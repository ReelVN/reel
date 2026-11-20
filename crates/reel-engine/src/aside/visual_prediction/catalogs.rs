//! Literal module tables whose mutable containers never escape a read-only projection.
use super::{PATH_LIMIT, text};
use aside_vm::bytecode::{self as bc, InstructionSequence, Opcode};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const MAX_REGISTERS: u32 = 4096;
const MAX_FIELDS: usize = 32_768;
const TABLE_ALIASES: u8 = 0b11;
const LOOKUP_ALIAS: u8 = 1 << 2;
const LOOKUP_RESULT_SHIFT: u8 = 3;

type Table<'p> = BTreeMap<&'p str, Value>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Lookup {
    Get,
    HasOwn,
}

impl Lookup {
    pub(super) fn named(name: &str) -> Option<Self> {
        match name {
            "get" => Some(Self::Get),
            "has_own" => Some(Self::HasOwn),
            _ => None,
        }
    }

    fn alias(self, receiver: u8) -> u8 {
        let result = match self {
            Self::Get => receiver >> 1,
            Self::HasOwn => 0,
        };
        LOOKUP_ALIAS | (result << LOOKUP_RESULT_SHIFT)
    }
}

#[derive(Clone, Copy)]
pub(super) enum Value {
    Constant(bc::ConstId),
    Table(usize),
}

struct Root<'p> {
    table: usize,
    row_members: Option<BTreeSet<&'p str>>,
}

impl Root<'_> {
    fn depth(&self) -> u8 {
        if self.row_members.is_some() { 2 } else { 1 }
    }
}

#[derive(Default)]
pub(in crate::aside) struct Catalogs<'p> {
    tables: Vec<Table<'p>>,
    roots: HashMap<u32, Root<'p>>,
}

impl<'p> Catalogs<'p> {
    pub(in crate::aside) fn compile(program: &'p bc::VerifiedProgram, budget: &mut usize) -> Self {
        let mut catalogs = Self::default();
        let mut fields = MAX_FIELDS;
        for module in &program.package().modules {
            let Some(initializer) = module.initializer else {
                continue;
            };
            let function = &program.package().functions[initializer.index()];
            let start = catalogs.tables.len();
            let remaining_fields = fields;
            if let Some(roots) = catalogs.initializer(program, function, budget, &mut fields) {
                catalogs.roots.extend(roots);
            } else {
                catalogs.tables.truncate(start);
                fields = remaining_fields;
            }
            if *budget == 0 {
                return Self::default();
            }
        }
        catalogs.certify_uses(program, budget);
        catalogs
    }

    /// Only a straight literal initializer can publish a table. This also
    /// rules out retaining its construction aliases in closures or mutations
    /// elsewhere in the initializer.
    fn initializer(
        &mut self,
        program: &'p bc::VerifiedProgram,
        function: &bc::RuntimeFunction,
        budget: &mut usize,
        fields: &mut usize,
    ) -> Option<Vec<(u32, Root<'p>)>> {
        if !function.regions.is_empty() || function.register_count > MAX_REGISTERS {
            return None;
        }
        let mut registers = vec![None; function.register_count as usize];
        let mut roots = Vec::new();
        let mut published = BTreeSet::new();
        for instruction in function.code.iter() {
            spend(budget, 1)?;
            let instruction = instruction.ok()?;
            let op = bc::instruction_spec(instruction.opcode)?.opcode;
            let [a, b, c] = instruction.operands.map(|reg| reg as usize);
            match op {
                Opcode::LoadConst => registers[a] = Some(Value::Constant(bc::ConstId(b as u32))),
                Opcode::Move => registers[a] = registers[b],
                Opcode::Clear | Opcode::Clear2 | Opcode::Clear3 => {
                    registers[a] = None;
                    if op != Opcode::Clear {
                        registers[b] = None;
                    }
                    if op == Opcode::Clear3 {
                        registers[c] = None;
                    }
                }
                Opcode::BindInit | Opcode::BindWrite | Opcode::BindRead => {
                    let binding = if op == Opcode::BindRead { b } else { a };
                    let bc::BindingStorage::Value(reg) = function.bindings[binding].storage else {
                        return None;
                    };
                    let register = reg.index();
                    if op == Opcode::BindRead {
                        registers[a] = registers[register];
                    } else {
                        registers[register] = registers[b];
                    }
                }
                Opcode::MakeClosure => {
                    let bc::Descriptor::Closure { captures, .. } = &function.descriptors[b] else {
                        return None;
                    };
                    if !captures.is_empty() {
                        return None;
                    }
                    registers[a] = None;
                }
                Opcode::GlobalRead => registers[a] = None,
                Opcode::MakeClass => {
                    let bc::Descriptor::Class(class) = &function.descriptors[b] else {
                        return None;
                    };
                    spend(budget, class.fields.len() + class.methods.len())?;
                    if class.fields.iter().any(|field| !field.captures.is_empty())
                        || class
                            .methods
                            .iter()
                            .any(|method| !method.captures.is_empty())
                    {
                        return None;
                    }
                    registers[a] = None;
                }
                Opcode::MakeTable => {
                    let descriptor = &function.descriptors[b];
                    let count = match descriptor {
                        bc::Descriptor::Table(pairs) => pairs.len(),
                        bc::Descriptor::TableSources(sources) => sources.len(),
                        _ => return None,
                    };
                    spend(budget, count)?;
                    spend(fields, count.max(1))?;
                    let mut table = BTreeMap::new();
                    for index in 0..count {
                        let [key, value] = match descriptor {
                            bc::Descriptor::Table(pairs) => pairs[index],
                            bc::Descriptor::TableSources(sources) => match sources[index] {
                                bc::TableSource::Pair(pair) => pair,
                                bc::TableSource::Spread(_) => return None,
                            },
                            _ => unreachable!("table descriptor"),
                        };
                        let Value::Constant(key) = registers[key.index()]? else {
                            return None;
                        };
                        let key = text(program, key)?;
                        if key.len() > PATH_LIMIT {
                            return None;
                        }
                        table.insert(key, registers[value.index()]?);
                    }
                    registers[a] = Some(Value::Table(self.tables.len()));
                    self.tables.push(table);
                }
                Opcode::GlobalInit => {
                    if let Some(Value::Table(table)) = registers[a] {
                        let binding = binding(program, function, b)?;
                        // A table identity reachable through two globals could
                        // be mutated from either spelling of the reference.
                        let root = self.root(table, &mut published, budget)?;
                        if !binding.mutable && !binding.imported {
                            roots.push((binding.offset, root));
                        }
                    }
                }
                Opcode::CheckType => {}
                Opcode::Return => {
                    if matches!(registers[a], Some(Value::Table(_))) {
                        return None;
                    }
                    return Some(roots);
                }
                _ => return None,
            }
        }
        None
    }

    fn root(
        &self,
        table: usize,
        published: &mut BTreeSet<usize>,
        budget: &mut usize,
    ) -> Option<Root<'p>> {
        if !published.insert(table) {
            return None;
        }
        let fields = &self.tables[table];
        spend(budget, fields.len())?;
        let mut row_members: Option<BTreeSet<_>> = None;
        let mut scalars = false;
        for &field in fields.values() {
            match field {
                Value::Constant(_) => scalars = true,
                Value::Table(row) => {
                    if !published.insert(row) {
                        return None;
                    }
                    let fields = &self.tables[row];
                    spend(budget, fields.len())?;
                    if fields
                        .values()
                        .any(|value| matches!(value, Value::Table(_)))
                    {
                        return None;
                    }
                    if let Some(common) = &mut row_members {
                        common.retain(|key| fields.contains_key(key));
                    } else {
                        row_members = Some(fields.keys().copied().collect());
                    }
                }
            }
        }
        if scalars && row_members.is_some() {
            return None;
        }
        Some(Root { table, row_members })
    }

    fn certify_uses(&mut self, program: &bc::VerifiedProgram, budget: &mut usize) {
        let mut instructions = bc::ResidentCodeReader::default();
        for function in &program.package().functions {
            if self.roots.is_empty() {
                break;
            }
            if spend(budget, function.descriptors.len().max(1)).is_none() {
                self.roots.clear();
                return;
            }
            // Functions with no reference to a candidate cannot introduce its
            // aliases. Canonical offsets include imported and reexported names.
            let possible = function
                .descriptors
                .iter()
                .enumerate()
                .any(|(index, descriptor)| {
                    matches!(descriptor, bc::Descriptor::Global { .. })
                        && binding(program, function, index)
                            .is_some_and(|binding| self.roots.contains_key(&binding.offset))
                });
            if !possible {
                continue;
            }
            for instruction in function.code.ranged_iter() {
                let Ok((range, instruction)) = instruction else {
                    self.roots.clear();
                    return;
                };
                if spend(budget, 1).is_none() {
                    self.roots.clear();
                    return;
                }
                let Some(Opcode::GlobalRead | Opcode::GlobalWrite) =
                    bc::instruction_spec(instruction.opcode).map(|spec| spec.opcode)
                else {
                    continue;
                };
                let [a, b, _] = instruction.operands;
                let Some(binding) = binding(program, function, b as usize) else {
                    self.roots.clear();
                    return;
                };
                let Some(root) = self.roots.get(&binding.offset) else {
                    continue;
                };
                if instruction.opcode == Opcode::GlobalWrite as u16
                    || !self.projection_uses(
                        program,
                        function,
                        &mut instructions,
                        (range.end, a),
                        root,
                        budget,
                    )
                {
                    self.roots.remove(&binding.offset);
                }
                if *budget == 0 {
                    self.roots.clear();
                    return;
                }
            }
        }
    }

    fn projection_uses(
        &self,
        program: &bc::VerifiedProgram,
        function: &bc::RuntimeFunction,
        instructions: &mut bc::ResidentCodeReader,
        (start, register): (usize, u32),
        root: &Root<'p>,
        budget: &mut usize,
    ) -> bool {
        if !function.regions.is_empty()
            || function.register_count > MAX_REGISTERS
            || spend(budget, function.register_count as usize).is_none()
        {
            return false;
        }
        let mut aliases = vec![0; function.register_count as usize];
        aliases[register as usize] = root.depth();
        let mut pending = BTreeMap::from([(start, aliases)]);
        let mut writes = Vec::new();
        while let Some((pc, mut aliases)) = pending.pop_first() {
            if spend(budget, 1).is_none() {
                return false;
            }
            let Ok(Some((instruction, next))) = instructions.read(&function.code, pc) else {
                return false;
            };
            let spec = bc::instruction_spec(instruction.opcode).expect("verified opcode");
            let [a, b, c] = instruction.operands.map(|reg| reg as usize);
            let mut copy = None;
            let allowed = match spec.opcode {
                Opcode::Move => {
                    copy = Some((a, aliases[b]));
                    Some(b)
                }
                Opcode::BindRead => {
                    if let bc::BindingStorage::Value(reg) = function.bindings[b].storage {
                        copy = Some((a, aliases[reg.index()]));
                        Some(reg.index())
                    } else {
                        None
                    }
                }
                Opcode::BindInit | Opcode::BindWrite => {
                    if let bc::BindingStorage::Value(reg) = function.bindings[a].storage {
                        copy = Some((reg.index(), aliases[b]));
                        Some(b)
                    } else {
                        None
                    }
                }
                Opcode::IndexGet if aliases[c] == 0 && aliases[b] & LOOKUP_ALIAS == 0 => {
                    copy = Some((a, aliases[b] >> 1));
                    Some(b)
                }
                Opcode::MemberGet if aliases[b] != 0 && aliases[b] & LOOKUP_ALIAS == 0 => {
                    let key = program.package().symbols[c].as_str();
                    let table = &self.tables[root.table];
                    let root_alias = aliases[b] & root.depth() != 0;
                    let row_alias = aliases[b] & 1 != 0 && root.row_members.is_some();
                    let root_owns = root_alias && table.contains_key(key);
                    let all_rows_own = root
                        .row_members
                        .as_ref()
                        .is_some_and(|fields| fields.contains(key));
                    let result = if (!root_alias || root_owns) && (!row_alias || all_rows_own) {
                        aliases[b] >> 1
                    } else {
                        // A missing key can bind a builtin to the mutable receiver.
                        // Accept readonly methods only if no possible receiver
                        // has an own key that could shadow the builtin.
                        let Some(lookup) = Lookup::named(key) else {
                            return false;
                        };
                        if root_owns {
                            return false;
                        }
                        if row_alias
                            && (spend(budget, table.len()).is_none()
                                || table.values().any(|value| match value {
                                    Value::Table(row) => self.tables[*row].contains_key(key),
                                    Value::Constant(_) => false,
                                }))
                        {
                            return false;
                        }
                        lookup.alias(aliases[b])
                    };
                    copy = Some((a, result));
                    Some(b)
                }
                Opcode::Call | Opcode::CallNoWait => {
                    let bc::Descriptor::Call(call) = &function.descriptors[b] else {
                        return false;
                    };
                    match (call.target, call.arguments) {
                        (bc::CallTarget::Register(target), bc::Arguments::Window(args))
                            if aliases[target.index()] & LOOKUP_ALIAS != 0
                                && aliases[target.index()] & TABLE_ALIASES == 0
                                && !(args.start.0..args.start.0 + args.len).contains(&target.0) =>
                        {
                            copy = Some((a, aliases[target.index()] >> LOOKUP_RESULT_SHIFT));
                            Some(target.index())
                        }
                        _ => None,
                    }
                }
                Opcode::CheckType | Opcode::BranchNull => Some(a),
                _ => None,
            };
            writes.clear();
            let mut escapes = false;
            let visit = spec.visit_registers_budgeted(
                function,
                &instruction,
                |units| spend(budget, units as usize).ok_or_else(work_limit),
                |reg, access| {
                    let register = reg.index();
                    match access {
                        bc::Access::Write => writes.push(register),
                        bc::Access::Read | bc::Access::ReadWrite => {
                            escapes |= aliases[register] != 0 && allowed != Some(register);
                        }
                        bc::Access::ConditionalWrite => {}
                    }
                },
            );
            if visit.is_err() || escapes {
                return false;
            }
            for &register in &writes {
                aliases[register] = 0;
            }
            if let Some((register, depth)) = copy {
                aliases[register] = depth;
            }
            if aliases.iter().all(|&depth| depth == 0) {
                continue;
            }
            let destinations = match spec.flow {
                bc::Flow::Next => [Some(next), None],
                bc::Flow::Jump => [Some(a), None],
                bc::Flow::Branch => [Some(b), Some(c)],
                bc::Flow::Return | bc::Flow::Throw => continue,
                bc::Flow::Leave | bc::Flow::EndFinally => return false,
            };
            for destination in destinations.into_iter().flatten() {
                if destination <= pc || spend(budget, aliases.len()).is_none() {
                    return false;
                }
                let state = pending
                    .entry(destination)
                    .or_insert_with(|| vec![0; aliases.len()]);
                for (old, &new) in state.iter_mut().zip(&aliases) {
                    *old |= new;
                }
            }
        }
        true
    }

    pub(super) fn get(&self, offset: u32) -> Option<usize> {
        Some(self.roots.get(&offset)?.table)
    }

    pub(super) fn field(&self, table: usize, key: &str) -> Option<Value> {
        self.tables.get(table)?.get(key).copied()
    }
}

fn spend(budget: &mut usize, units: usize) -> Option<()> {
    match budget.checked_sub(units) {
        Some(remaining) => {
            *budget = remaining;
            Some(())
        }
        None => {
            *budget = 0;
            None
        }
    }
}

fn binding(
    program: &bc::VerifiedProgram,
    function: &bc::RuntimeFunction,
    descriptor: usize,
) -> Option<bc::GlobalBinding> {
    let bc::Descriptor::Global { module, global } = function.descriptors[descriptor] else {
        return None;
    };
    program.global_binding(module, global).ok()
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
