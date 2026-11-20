//! Class facts for the source predictor. A nominal type admits subclasses.

use aside_vm::bytecode::{self as bc, Opcode};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Member {
    Field(bc::TypeId),
    Method(bc::FunctionId),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Base {
    Root,
    Global(u32),
    Unknown,
}

struct Definition {
    members: HashMap<bc::SymbolId, Option<Member>>,
    base: Base,
}

#[derive(Default)]
pub(in crate::aside) struct Classes {
    definitions: HashMap<bc::TypeId, Definition>,
    globals: HashMap<u32, bc::TypeId>,
    descendants: Option<HashMap<bc::TypeId, Vec<bc::TypeId>>>,
}

#[derive(Clone, Copy)]
enum Origin {
    Class(bc::TypeId),
    Global(u32),
}

impl Classes {
    pub(in crate::aside) fn compile(program: &bc::VerifiedProgram, budget: &mut usize) -> Self {
        let mut classes = Self::default();
        let mut ambiguous = HashSet::new();
        let mut opaque_base = false;
        for function in &program.package().functions {
            if spend(budget, 1 + function.descriptors.len()).is_none() {
                return Self::default();
            }
            for descriptor in &function.descriptors {
                let bc::Descriptor::Class(descriptor) = descriptor else {
                    continue;
                };
                let Some(nominal) = descriptor.nominal else {
                    opaque_base |= descriptor.base.is_some();
                    continue;
                };
                let base = if descriptor.base.is_none() {
                    Base::Root
                } else {
                    Base::Unknown
                };
                if spend(budget, descriptor.fields.len() + descriptor.methods.len()).is_none() {
                    return Self::default();
                }
                let mut members = HashMap::new();
                for field in descriptor.fields.iter().filter(|field| !field.is_static) {
                    members.insert(field.name, Some(Member::Field(field.ty)));
                }
                for method in descriptor.methods.iter().filter(|method| !method.is_static) {
                    let member = method
                        .captures
                        .is_empty()
                        .then_some(Member::Method(method.function));
                    members.insert(method.name, member);
                }
                if classes
                    .definitions
                    .insert(nominal, Definition { members, base })
                    .is_some()
                {
                    ambiguous.insert(nominal);
                }
            }
        }
        for nominal in &ambiguous {
            classes.definitions.remove(nominal);
        }
        for module in &program.package().modules {
            if spend(budget, 1).is_none() {
                return Self::default();
            }
            let Some(initializer) = module.initializer else {
                continue;
            };
            let function = &program.package().functions[initializer.index()];
            if spend(budget, function.descriptors.len()).is_none() {
                return Self::default();
            }
            if !function
                .descriptors
                .iter()
                .any(|descriptor| matches!(descriptor, bc::Descriptor::Class(_)))
            {
                continue;
            }
            if let Some((globals, bases)) = initializer_classes(program, function, budget) {
                classes.globals.extend(
                    globals
                        .into_iter()
                        .filter(|(_, nominal)| classes.definitions.contains_key(nominal)),
                );
                for (nominal, base) in bases {
                    if let Some(definition) = classes.definitions.get_mut(&nominal) {
                        definition.base = base;
                    }
                }
            }
        }
        if !opaque_base && ambiguous.is_empty() {
            classes.descendants = classes.find_descendants(budget);
        }
        classes
    }

    pub(super) fn has(&self, ty: bc::TypeId) -> bool {
        self.definitions.contains_key(&ty)
    }

    /// The compiler can borrow parameter registers directly. A pause can start
    /// after any branch or loop, so seed only registers with no writes anywhere.
    pub(in crate::aside) fn stable_parameters(
        &self,
        function: &bc::RuntimeFunction,
        budget: &mut usize,
    ) -> Vec<(bc::Reg, bc::TypeId)> {
        if self.descendants.is_none() || self.definitions.is_empty() || *budget == 0 {
            return Vec::new();
        }
        self.prove_parameters(function, budget).unwrap_or_default()
    }

    fn prove_parameters(
        &self,
        function: &bc::RuntimeFunction,
        budget: &mut usize,
    ) -> Option<Vec<(bc::Reg, bc::TypeId)>> {
        spend(budget, function.bindings.len())?;
        let mut parameters: Vec<_> = function
            .bindings
            .iter()
            .filter_map(|binding| match (binding.initialization, binding.storage) {
                (bc::BindingInitialization::Parameter(_), bc::BindingStorage::Value(register))
                    if self.has(binding.ty) =>
                {
                    Some((register, binding.ty))
                }
                _ => None,
            })
            .take(super::FACT_LIMIT)
            .collect();
        if parameters.is_empty() {
            return Some(parameters);
        }
        for instruction in function.code.iter() {
            spend(budget, 1)?;
            let instruction = instruction.ok()?;
            bc::instruction_spec(instruction.opcode)?
                .visit_registers_budgeted(
                    function,
                    &instruction,
                    |units| spend(budget, units as usize).ok_or_else(work_limit),
                    |register, access| {
                        if access != bc::Access::Read {
                            parameters.retain(|&(parameter, _)| parameter != register);
                        }
                    },
                )
                .ok()?;
            if parameters.is_empty() {
                break;
            }
        }
        Some(parameters)
    }

    fn global(&self, canonical_offset: u32) -> Option<bc::TypeId> {
        self.globals.get(&canonical_offset).copied()
    }

    fn parent(&self, ty: bc::TypeId) -> Option<Option<bc::TypeId>> {
        match self.definitions.get(&ty)?.base {
            Base::Root => Some(None),
            Base::Global(offset) => Some(Some(self.global(offset)?)),
            Base::Unknown => None,
        }
    }

    fn find_descendants(&self, budget: &mut usize) -> Option<HashMap<bc::TypeId, Vec<bc::TypeId>>> {
        let mut descendants = HashMap::<bc::TypeId, Vec<bc::TypeId>>::new();
        for &ty in self.definitions.keys() {
            spend(budget, 1)?;
            let mut current = ty;
            let mut remaining = self.definitions.len();
            while let Some(parent) = self.parent(current)? {
                spend(budget, 1)?;
                remaining = remaining.checked_sub(1)?;
                descendants.entry(parent).or_default().push(ty);
                current = parent;
            }
        }
        Some(descendants)
    }

    pub(super) fn member(
        &self,
        ty: bc::TypeId,
        name: bc::SymbolId,
        budget: &mut usize,
    ) -> Option<Member> {
        let member = self.dispatch(ty, name, budget)?;
        let descendants = self.descendants.as_ref()?;
        for &child in descendants.get(&ty).into_iter().flatten() {
            if self.dispatch(child, name, budget)? != member {
                return None;
            }
        }
        Some(member)
    }

    fn dispatch(
        &self,
        mut ty: bc::TypeId,
        name: bc::SymbolId,
        budget: &mut usize,
    ) -> Option<Member> {
        loop {
            spend(budget, 1)?;
            if let Some(member) = self.definitions.get(&ty)?.members.get(&name) {
                return *member;
            }
            ty = self.parent(ty)??;
        }
    }
}

type GlobalClasses = HashMap<u32, bc::TypeId>;
type ClassBases = HashMap<bc::TypeId, Base>;

/// A straight-line initializer can prove which immutable global supplies a base.
/// Its temporary registers must never seed a later branch target.
fn initializer_classes(
    program: &bc::VerifiedProgram,
    function: &bc::RuntimeFunction,
    budget: &mut usize,
) -> Option<(GlobalClasses, ClassBases)> {
    if !function.regions.is_empty() {
        return None;
    }
    spend(budget, function.register_count as usize)?;
    let mut registers = vec![None; function.register_count as usize];
    let mut bases = ClassBases::new();
    let mut globals = GlobalClasses::new();
    for instruction in function.code.iter() {
        spend(budget, 1)?;
        let instruction = instruction.ok()?;
        let spec = bc::instruction_spec(instruction.opcode)?;
        if !matches!(spec.flow, bc::Flow::Next | bc::Flow::Return) {
            return None;
        }
        let [a, b, _] = instruction.operands.map(|value| value as usize);
        let value = match spec.opcode {
            Opcode::GlobalRead => global(program, function, b)
                .filter(|binding| !binding.mutable)
                .map(|binding| Origin::Global(binding.offset)),
            Opcode::MakeClass => {
                let bc::Descriptor::Class(descriptor) = &function.descriptors[b] else {
                    return None;
                };
                if let Some(nominal) = descriptor.nominal {
                    let base = match descriptor.base {
                        None => Base::Root,
                        Some(register) => match registers[register.index()] {
                            Some(Origin::Global(offset)) => Base::Global(offset),
                            _ => Base::Unknown,
                        },
                    };
                    bases
                        .entry(nominal)
                        .and_modify(|old| {
                            if *old != base {
                                *old = Base::Unknown;
                            }
                        })
                        .or_insert(base);
                    Some(Origin::Class(nominal))
                } else {
                    None
                }
            }
            Opcode::Move => registers[b],
            Opcode::BindRead => match function.bindings[b].storage {
                bc::BindingStorage::Value(register) => registers[register.index()],
                bc::BindingStorage::Cell(_) => None,
            },
            Opcode::GlobalInit => {
                if let Some(Origin::Class(ty)) = registers[a]
                    && let Some(binding) = global(program, function, b)
                    && !binding.mutable
                    && !binding.imported
                    && globals.insert(binding.offset, ty).is_some()
                {
                    return None;
                }
                None
            }
            _ => None,
        };
        let binding_write = match spec.opcode {
            Opcode::BindInit | Opcode::BindWrite => match function.bindings[a].storage {
                bc::BindingStorage::Value(register) => Some((register, registers[b])),
                bc::BindingStorage::Cell(_) => None,
            },
            _ => None,
        };
        spec.visit_registers_budgeted(
            function,
            &instruction,
            |units| spend(budget, units as usize).ok_or_else(work_limit),
            |register, access| {
                if access != bc::Access::Read {
                    registers[register.index()] = None;
                }
            },
        )
        .ok()?;
        if let Some(value) = value {
            registers[a] = Some(value);
        }
        if let Some((register, value)) = binding_write {
            registers[register.index()] = value;
        }
    }
    Some((globals, bases))
}

fn global(
    program: &bc::VerifiedProgram,
    function: &bc::RuntimeFunction,
    descriptor: usize,
) -> Option<bc::GlobalBinding> {
    let bc::Descriptor::Global { module, global } = function.descriptors[descriptor] else {
        return None;
    };
    program.global_binding(module, global).ok()
}

fn spend(budget: &mut usize, units: usize) -> Option<()> {
    let remaining = budget.checked_sub(units);
    *budget = remaining.unwrap_or(0);
    remaining.map(|_| ())
}

fn work_limit() -> bc::Error {
    bc::Error {
        kind: bc::ErrorKind::Limit,
        offset: None,
        function: None,
        pc: None,
        source: None,
        detail: String::new(),
    }
}
