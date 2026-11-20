//! A bounded read-only walk of the current story's bytecode. The value facts
//! belong to this query and never come from the running VM's registers or heap.

use arrayvec::ArrayVec;
use aside_vm::bytecode::{self as bc, Opcode};
mod catalogs;
pub(super) use catalogs::Catalogs;

const NESTING_LIMIT: usize = 8;
const ARGUMENT_LIMIT: usize = 32;
const FACT_LIMIT: usize = 32;
const PATH_LIMIT: usize = 1024;
const LAYER_LIMIT: usize = 8;
const GROUP_LIMIT: usize = 8;
const FIELD_LIMIT: usize = 64;
const CLOSURE_LIMIT: usize = 8;
const CAPTURE_LIMIT: usize = 8;
const BUILD_INSTRUCTION_LIMIT: usize = 1024;
const BUILD_DIALOGUE_LIMIT: usize = 6;

pub(super) const IMAGE_LIMIT: usize = LAYER_LIMIT;
pub(super) const RESUME_LOOKUP_LIMIT: usize = 16;

type Paths<'p> = ArrayVec<ImagePath<'p>, LAYER_LIMIT>;

#[derive(Clone, Copy)]
pub(super) struct ImagePath<'p> {
    pub(super) constant: bc::ConstId,
    pub(super) text: &'p str,
}

impl PartialEq for ImagePath<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fact {
    Unknown,
    Bool(bool),
    Constant(bc::ConstId),
    Function(bc::FunctionId),
    Closure(u8),
    Table { start: u8, end: u8 },
    Catalog(usize),
    Layers(u8),
}

type Captures = ArrayVec<Fact, CAPTURE_LIMIT>;

#[derive(Clone, Default)]
struct Facts {
    registers: ArrayVec<(u32, Fact), FACT_LIMIT>,
    captures: Captures,
}

impl Facts {
    fn get(&self, register: u32) -> Fact {
        self.registers
            .iter()
            .find_map(|&(key, fact)| (key == register).then_some(fact))
            .unwrap_or(Fact::Unknown)
    }

    fn set(&mut self, register: u32, fact: Fact) {
        if let Some(index) = self.registers.iter().position(|&(key, _)| key == register) {
            if fact == Fact::Unknown {
                self.registers.swap_remove(index);
            } else {
                self.registers[index].1 = fact;
            }
        } else if fact != Fact::Unknown {
            // Excess registers stay unknown for this walk.
            let _ = self.registers.try_push((register, fact));
        }
    }
}

enum Outcome<'p> {
    Images(Paths<'p>),
    Return(Fact),
}

/// Analyze a continuation using build-time work. `Some` with zero paths is a
/// proven ordinary return and allows the player to look at the outer caller.
pub(super) fn compile_site<'p>(
    program: &'p bc::VerifiedProgram,
    catalogs: &Catalogs<'p>,
    function: &bc::RuntimeFunction,
    next: usize,
    budget: &mut usize,
) -> Option<Paths<'p>> {
    let available = (*budget).min(BUILD_INSTRUCTION_LIMIT);
    let mut walk = Walk::new(program, catalogs, available, BUILD_DIALOGUE_LIMIT);
    let result = walk.function(function, next, Facts::default(), 0);
    *budget -= available - walk.remaining;
    match result? {
        Outcome::Images(paths) => Some(paths),
        Outcome::Return(_) => Some(Paths::new()),
    }
}

pub(super) fn image_path(program: &bc::VerifiedProgram, id: bc::ConstId) -> Option<&str> {
    let candidate = program.package().constants.get(id.index())?;
    let length = match candidate {
        bc::Const::Utf8(text) => text.len(),
        bc::Const::Text { len, .. } => *len as usize,
        _ => return None,
    };
    if !(1..=PATH_LIMIT).contains(&length) {
        return None;
    }
    std::str::from_utf8(program.constant_bytes(id)?).ok()
}

fn text(program: &bc::VerifiedProgram, id: bc::ConstId) -> Option<&str> {
    if !matches!(
        program.package().constants[id.index()],
        bc::Const::Utf8(_) | bc::Const::Text { .. }
    ) {
        return None;
    }
    std::str::from_utf8(program.constant_bytes(id)?).ok()
}

struct Walk<'p, 'c> {
    program: &'p bc::VerifiedProgram,
    catalogs: &'c Catalogs<'p>,
    remaining: usize,
    dialogue_remaining: usize,
    code: bc::ResidentCodeReader,
    layers: ArrayVec<ArrayVec<bc::ConstId, LAYER_LIMIT>, GROUP_LIMIT>,
    closures: ArrayVec<(bc::FunctionId, Captures), CLOSURE_LIMIT>,
    fields: ArrayVec<(bc::ConstId, Fact), FIELD_LIMIT>,
}

impl<'p, 'c> Walk<'p, 'c> {
    fn new(
        program: &'p bc::VerifiedProgram,
        catalogs: &'c Catalogs<'p>,
        remaining: usize,
        dialogue_remaining: usize,
    ) -> Self {
        Self {
            program,
            catalogs,
            remaining,
            dialogue_remaining,
            code: bc::ResidentCodeReader::default(),
            layers: ArrayVec::new(),
            closures: ArrayVec::new(),
            fields: ArrayVec::new(),
        }
    }

    fn instruction(
        &mut self,
        function: &bc::RuntimeFunction,
        pc: usize,
    ) -> Option<(bc::Instruction, usize)> {
        self.remaining = self.remaining.checked_sub(1)?;
        self.code.read(&function.code, pc).ok()?
    }

    fn function(
        &mut self,
        function: &bc::RuntimeFunction,
        mut pc: usize,
        mut facts: Facts,
        nesting: usize,
    ) -> Option<Outcome<'p>> {
        if nesting == NESTING_LIMIT
            || function.kind != bc::FunctionKind::Plain
            || !function.regions.is_empty()
        {
            return None;
        }
        loop {
            let (instruction, next) = self.instruction(function, pc)?;
            let op = bc::instruction_spec(instruction.opcode)?.opcode;
            let [a, b, c] = instruction.operands;
            match op {
                Opcode::Call | Opcode::CallNoWait => {
                    let bc::Descriptor::Call(call) = &function.descriptors[b as usize] else {
                        return None;
                    };
                    match self.call(call, &facts, nesting)? {
                        images @ Outcome::Images(_) => return Some(images),
                        Outcome::Return(value) => facts.set(a, value),
                    }
                }
                Opcode::GlobalRead => {
                    let bc::Descriptor::Global { module, global } =
                        function.descriptors[b as usize]
                    else {
                        return None;
                    };
                    let binding = self.program.global_binding(module, global).ok()?;
                    let value = if let Some(table) = self.catalogs.get(binding.offset) {
                        Fact::Catalog(table)
                    } else {
                        self.global_function(module, global)
                            .map_or(Fact::Unknown, Fact::Function)
                    };
                    facts.set(a, value);
                }
                Opcode::MakeTable => {
                    let table = self.table(&function.descriptors[b as usize], &facts);
                    facts.set(a, table.unwrap_or(Fact::Unknown));
                }
                Opcode::IndexGet | Opcode::MemberGet => {
                    let key = if op == Opcode::IndexGet {
                        let Fact::Constant(key) = facts.get(c) else {
                            return None;
                        };
                        self.text(key)?
                    } else {
                        self.program.package().symbols[c as usize].as_str()
                    };
                    let value = self.field(facts.get(b), key)?;
                    facts.set(a, value);
                }
                Opcode::MakeArray => {
                    let group = self.layer_group(&function.descriptors[b as usize], &facts);
                    facts.set(a, group.map_or(Fact::Unknown, Fact::Layers));
                }
                Opcode::MakeClosure => {
                    let closure = self.closure(function, &function.descriptors[b as usize], &facts);
                    facts.set(a, closure.unwrap_or(Fact::Unknown));
                }
                Opcode::CaptureRead => {
                    facts.set(
                        a,
                        facts
                            .captures
                            .get(b as usize)
                            .copied()
                            .unwrap_or(Fact::Unknown),
                    );
                }
                Opcode::Eq | Opcode::Ne => {
                    let equal = match (facts.get(b), facts.get(c)) {
                        (Fact::Constant(left), Fact::Constant(right)) => self
                            .text(left)
                            .zip(self.text(right))
                            .and_then(|(left, right)| {
                                (left.len().max(right.len()) <= PATH_LIMIT).then(|| left == right)
                            }),
                        _ => None,
                    };
                    facts.set(
                        a,
                        equal.map_or(Fact::Unknown, |equal| {
                            Fact::Bool(equal == (op == Opcode::Eq))
                        }),
                    );
                }
                Opcode::Is => {
                    let bc::TypeDef::StringLiteral(pattern) =
                        &self.program.package().types[c as usize]
                    else {
                        return None;
                    };
                    let matches = match facts.get(b) {
                        Fact::Constant(id) => self.text(id).and_then(|text| {
                            (text.len().max(pattern.len()) <= PATH_LIMIT).then(|| text == pattern)
                        }),
                        _ => None,
                    };
                    facts.set(a, matches.map_or(Fact::Unknown, Fact::Bool));
                }
                Opcode::Jump => {
                    pc = forward(pc, a)?;
                    continue;
                }
                Opcode::BranchBool | Opcode::BranchNull => {
                    if let Some(taken) = self.branch(op, facts.get(a))? {
                        pc = forward(pc, if taken { b } else { c })?;
                        continue;
                    }
                    let left =
                        self.function(function, forward(pc, b)?, facts.clone(), nesting + 1)?;
                    let right = self.function(function, forward(pc, c)?, facts, nesting + 1)?;
                    return match (left, right) {
                        (Outcome::Images(left), Outcome::Images(right)) if left == right => {
                            Some(Outcome::Images(left))
                        }
                        (Outcome::Return(left), Outcome::Return(right)) => {
                            Some(Outcome::Return(if left == right {
                                left
                            } else {
                                Fact::Unknown
                            }))
                        }
                        _ => None,
                    };
                }
                Opcode::Return => return Some(Outcome::Return(facts.get(a))),
                _ => update_facts(function, op, [a, b, c], &mut facts)?,
            }
            pc = next;
        }
    }

    fn branch(&self, opcode: Opcode, fact: Fact) -> Option<Option<bool>> {
        let constant = match fact {
            Fact::Unknown => return Some(None),
            Fact::Bool(value) => return Some(Some(opcode == Opcode::BranchBool && value)),
            Fact::Constant(id) => Some(&self.program.package().constants[id.index()]),
            Fact::Function(_)
            | Fact::Closure(_)
            | Fact::Table { .. }
            | Fact::Catalog(_)
            | Fact::Layers(_) => None,
        };
        match (opcode, constant) {
            (Opcode::BranchBool, Some(bc::Const::Bool(value))) => Some(Some(*value)),
            (Opcode::BranchNull, constant) => Some(Some(matches!(constant, Some(bc::Const::Null)))),
            _ => None,
        }
    }

    fn call(&mut self, call: &bc::CallDesc, facts: &Facts, nesting: usize) -> Option<Outcome<'p>> {
        let package = self.program.package();
        let (target, captures) = match call.target {
            bc::CallTarget::Register(reg) => {
                let (id, captures) = self.resolve_closure(facts.get(reg.0))?;
                (bc::CallTarget::Function(id), captures)
            }
            target => (target, Captures::new()),
        };
        let native = match target {
            bc::CallTarget::Native(native) => Some(native),
            bc::CallTarget::Function(id) => match package.functions[id.index()].kind {
                bc::FunctionKind::Native(native) => Some(native),
                _ => None,
            },
            bc::CallTarget::Register(_) => return None,
        };
        if let Some(native) = native {
            let native = &package.natives[native.index()];
            let symbol = package.symbols[native.symbol.index()].as_str();
            if symbol == "reel::tr" {
                return Some(Outcome::Return(Fact::Unknown));
            }
            if matches!(
                symbol,
                "reel::narrator"
                    | "reel::say"
                    | "reel::character::speak"
                    | "reel::speech::wait"
                    | "reel::speech::revealed"
            ) {
                self.dialogue_remaining = self.dialogue_remaining.checked_sub(1)?;
                return Some(Outcome::Return(Fact::Unknown));
            }
            let signature = &package.signatures[native.signature.index()];
            if symbol == "reel::stage::transition" {
                let arguments = self.arguments(signature, call, facts)?;
                let change = signature
                    .parameters
                    .iter()
                    .position(|parameter| package.symbols[parameter.name.index()] == "change")?;
                let (id, captures) = self.resolve_closure(arguments[change]?)?;
                return self.invoke(id, [None; ARGUMENT_LIMIT], captures, nesting);
            }
            if !matches!(symbol, "reel::image" | "reel::layered_image") {
                return None;
            }
            let bc::Arguments::Window(window) = call.arguments else {
                return None;
            };
            let positional = (window.len as usize).checked_sub(call.names.len())?;
            if window.len as usize > ARGUMENT_LIMIT {
                return None;
            }
            let source = signature.parameters.first()?.name;
            let named = call.names.iter().find(|arg| arg.name == source);
            let offset = match (positional > 0, named) {
                (true, None) => 0,
                (false, Some(named)) => named.offset,
                _ => return None,
            };
            let mut paths = Paths::new();
            match (symbol, facts.get(window.start.0 + offset)) {
                ("reel::image", Fact::Constant(id)) => paths.push(self.path(id)?),
                ("reel::layered_image", Fact::Layers(group)) => {
                    for &id in &self.layers[usize::from(group)] {
                        let path = self.path(id)?;
                        if !paths.contains(&path) {
                            paths.push(path);
                        }
                    }
                }
                _ => return None,
            };
            return Some(Outcome::Images(paths));
        }

        let bc::CallTarget::Function(id) = target else {
            return None;
        };
        let function = &package.functions[id.index()];
        let signature = &package.signatures[function.signature.index()];
        let arguments = self.arguments(signature, call, facts)?;
        self.invoke(id, arguments, captures, nesting)
    }

    fn invoke(
        &mut self,
        id: bc::FunctionId,
        arguments: [Option<Fact>; ARGUMENT_LIMIT],
        captures: Captures,
        nesting: usize,
    ) -> Option<Outcome<'p>> {
        let package = self.program.package();
        let function = &package.functions[id.index()];
        let signature = &package.signatures[function.signature.index()];
        if signature.parameters.len() > ARGUMENT_LIMIT || function.captures.len() != captures.len()
        {
            return None;
        }
        let mut parameters = Facts {
            captures,
            ..Facts::default()
        };
        for (index, parameter) in signature.parameters.iter().enumerate() {
            let fact = if let Some(provided) = arguments[index] {
                provided
            } else if let Some(bc::ParameterDefault::Constant(id)) = parameter.default {
                Fact::Constant(id)
            } else {
                return None;
            };
            parameters.set(index as u32, fact);
        }
        self.function(function, 0, parameters, nesting + 1)
    }

    fn closure(
        &mut self,
        owner: &bc::RuntimeFunction,
        descriptor: &bc::Descriptor,
        facts: &Facts,
    ) -> Option<Fact> {
        let bc::Descriptor::Closure { function, captures } = descriptor else {
            return None;
        };
        if captures.is_empty() {
            return Some(Fact::Function(*function));
        }
        if captures.len() > CAPTURE_LIMIT {
            return None;
        }
        self.remaining = self.remaining.checked_sub(captures.len())?;
        let mut values = Captures::new();
        for source in captures {
            let value = match *source {
                bc::CaptureSource::Binding(id) => {
                    let bc::BindingStorage::Value(reg) = owner.bindings[id.index()].storage else {
                        return None;
                    };
                    facts.get(reg.0)
                }
                bc::CaptureSource::Capture(id) => *facts.captures.get(id.index())?,
            };
            values.push(value);
        }
        let index = self.closures.len() as u8;
        self.closures.try_push((*function, values)).ok()?;
        Some(Fact::Closure(index))
    }

    fn resolve_closure(&self, fact: Fact) -> Option<(bc::FunctionId, Captures)> {
        match fact {
            Fact::Function(id) => Some((id, Captures::new())),
            Fact::Closure(index) => self.closures.get(usize::from(index)).cloned(),
            _ => None,
        }
    }

    fn text(&self, id: bc::ConstId) -> Option<&'p str> {
        text(self.program, id)
    }

    fn path(&self, id: bc::ConstId) -> Option<ImagePath<'p>> {
        Some(ImagePath {
            constant: id,
            text: image_path(self.program, id)?,
        })
    }

    fn table(&mut self, descriptor: &bc::Descriptor, facts: &Facts) -> Option<Fact> {
        let count = match descriptor {
            bc::Descriptor::Table(pairs) => pairs.len(),
            bc::Descriptor::TableSources(entries) => entries.len(),
            _ => return None,
        };
        let start = self.fields.len();
        if count > FIELD_LIMIT - start {
            return None;
        }
        self.remaining = self.remaining.checked_sub(count)?;
        let built = (0..count).try_for_each(|index| {
            let [key, value] = match descriptor {
                bc::Descriptor::Table(pairs) => pairs[index],
                bc::Descriptor::TableSources(entries) => match entries[index] {
                    bc::TableSource::Pair(pair) => pair,
                    bc::TableSource::Spread(_) => return None,
                },
                _ => unreachable!("table descriptor"),
            };
            let Fact::Constant(key) = facts.get(key.0) else {
                return None;
            };
            if self.text(key)?.len() > PATH_LIMIT {
                return None;
            }
            self.fields.push((key, facts.get(value.0)));
            Some(())
        });
        if built.is_none() {
            self.fields.truncate(start);
            return None;
        }
        Some(Fact::Table {
            start: start as u8,
            end: self.fields.len() as u8,
        })
    }

    fn field(&mut self, table: Fact, key: &str) -> Option<Fact> {
        if key.len() > PATH_LIMIT {
            return None;
        }
        if let Fact::Catalog(table) = table {
            self.remaining = self.remaining.checked_sub(1)?;
            return Some(match self.catalogs.field(table, key)? {
                catalogs::Value::Constant(value) => Fact::Constant(value),
                catalogs::Value::Table(table) => Fact::Catalog(table),
            });
        }
        let Fact::Table { start, end } = table else {
            return None;
        };
        let fields = &self.fields[start as usize..end as usize];
        self.remaining = self.remaining.checked_sub(fields.len())?;
        fields
            .iter()
            .rev()
            .find_map(|&(name, value)| (self.text(name) == Some(key)).then_some(value))
    }

    fn layer_group(&mut self, descriptor: &bc::Descriptor, facts: &Facts) -> Option<u8> {
        let bc::Descriptor::Sequence(window) = descriptor else {
            return None;
        };
        if window.len as usize > LAYER_LIMIT {
            return None;
        }
        self.remaining = self.remaining.checked_sub(window.len as usize)?;
        let mut group = ArrayVec::new();
        for offset in 0..window.len {
            match facts.get(window.start.0 + offset) {
                table @ Fact::Table { .. } => {
                    let Fact::Constant(id) = self.field(table, "source")? else {
                        return None;
                    };
                    group.push(id);
                }
                Fact::Constant(id)
                    if self.program.package().constants[id.index()] == bc::Const::Null => {}
                _ => return None,
            }
        }
        let index = self.layers.len() as u8;
        self.layers.try_push(group).ok()?;
        Some(index)
    }

    fn arguments(
        &self,
        signature: &bc::Signature,
        call: &bc::CallDesc,
        facts: &Facts,
    ) -> Option<[Option<Fact>; ARGUMENT_LIMIT]> {
        let bc::Arguments::Window(window) = call.arguments else {
            return None;
        };
        let len = window.len as usize;
        if len > ARGUMENT_LIMIT
            || signature.parameters.len() > ARGUMENT_LIMIT
            || signature.rest.is_some()
            || signature.property_bag_tail.is_some()
        {
            return None;
        }
        let positional = len.checked_sub(call.names.len())?;
        if positional > signature.parameters.len() {
            return None;
        }
        let mut arguments = [None; ARGUMENT_LIMIT];
        for (index, argument) in arguments.iter_mut().take(positional).enumerate() {
            *argument = Some(facts.get(window.start.0 + index as u32));
        }
        for named in &call.names {
            let index = signature
                .parameters
                .iter()
                .position(|p| p.name == named.name)?;
            if arguments[index].is_some() || named.offset as usize >= len {
                return None;
            }
            arguments[index] = Some(facts.get(window.start.0 + named.offset));
        }
        Some(arguments)
    }

    fn global_function(
        &mut self,
        module: bc::ModuleId,
        global: bc::GlobalId,
    ) -> Option<bc::FunctionId> {
        let package = self.program.package();
        let owner = &package.modules[module.index()];
        let declaration = owner.globals.get(global.index())?;
        if declaration.mutable {
            return None;
        }
        if let Some(function) = declaration.linked_function {
            return Some(function);
        }
        // Reel finishes module initialization before executing story tasks.
        // Prove a closure in the unconditional prefix of its owning initializer.
        // An immutable cell cannot be reinitialized or written after that point.
        let initializer = &package.functions[owner.initializer?.index()];
        if !initializer.regions.is_empty() {
            return None;
        }
        let mut facts = Facts::default();
        let mut pc = 0;
        loop {
            let (instruction, next) = self.instruction(initializer, pc)?;
            let op = bc::instruction_spec(instruction.opcode)?.opcode;
            let [a, b, c] = instruction.operands;
            if op == Opcode::GlobalInit {
                let bc::Descriptor::Global {
                    module: target,
                    global: slot,
                } = initializer.descriptors[b as usize]
                else {
                    return None;
                };
                if (target, slot) == (module, global) {
                    return match facts.get(a) {
                        Fact::Function(function) => Some(function),
                        _ => None,
                    };
                }
            } else {
                update_facts(initializer, op, [a, b, c], &mut facts)?;
            }
            pc = next;
        }
    }
}

fn forward(pc: usize, target: u32) -> Option<usize> {
    let target = target as usize;
    (target > pc).then_some(target)
}

fn update_facts(
    function: &bc::RuntimeFunction,
    opcode: Opcode,
    [a, b, c]: [u32; 3],
    facts: &mut Facts,
) -> Option<()> {
    match opcode {
        Opcode::LoadConst => facts.set(a, Fact::Constant(bc::ConstId(b))),
        Opcode::Move => facts.set(a, facts.get(b)),
        Opcode::Clear | Opcode::Clear2 | Opcode::Clear3 => {
            facts.set(a, Fact::Unknown);
            if opcode != Opcode::Clear {
                facts.set(b, Fact::Unknown);
            }
            if opcode == Opcode::Clear3 {
                facts.set(c, Fact::Unknown);
            }
        }
        Opcode::BindRead | Opcode::BindInit | Opcode::BindWrite => {
            let binding = if opcode == Opcode::BindRead { b } else { a };
            let bc::BindingStorage::Value(reg) = function.bindings[binding as usize].storage else {
                return None;
            };
            if opcode == Opcode::BindRead {
                facts.set(a, facts.get(reg.0));
            } else {
                facts.set(reg.0, facts.get(b));
            }
        }
        Opcode::MakeClosure => {
            let bc::Descriptor::Closure { function, captures } = &function.descriptors[b as usize]
            else {
                return None;
            };
            facts.set(
                a,
                if captures.is_empty() {
                    Fact::Function(*function)
                } else {
                    Fact::Unknown
                },
            );
        }
        Opcode::CheckType => {}
        _ => return None,
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::super::visual_plan::VisualPlan;
    use super::*;
    use aside_vm::{
        bytecode::InstructionSequence,
        executor::{CodeRef, HostCallSite},
    };

    fn next_images(
        program: &bc::VerifiedProgram,
        sites: impl Iterator<Item = HostCallSite>,
    ) -> Option<ArrayVec<&str, IMAGE_LIMIT>> {
        let prepared = VisualPlan::compile(program, 64 * 1024)?;
        let decoded = VisualPlan::decode(prepared.as_bytes().into(), program).unwrap();
        decoded.next_images(program, sites)
    }

    fn next_image(
        program: &bc::VerifiedProgram,
        sites: impl Iterator<Item = HostCallSite>,
    ) -> Option<&str> {
        next_images(program, sites).map(|paths| {
            assert_eq!(paths.len(), 1);
            paths[0]
        })
    }

    fn compile(source: &str) -> bc::VerifiedProgram {
        super::super::source::compile_story(source, "prediction", 1, "opening")
            .unwrap()
            .program
    }

    fn first_call(program: &bc::VerifiedProgram, name: &str) -> HostCallSite {
        let package = program.package();
        let (id, function) = package
            .functions
            .iter()
            .enumerate()
            .find(|(_, function)| package.symbols[function.name.index()] == name)
            .unwrap();
        let (range, _) = function
            .code
            .ranged_iter()
            .map(Result::unwrap)
            .find(|(_, instruction)| {
                matches!(
                    bc::instruction_spec(instruction.opcode).unwrap().opcode,
                    Opcode::Call | Opcode::CallNoWait | Opcode::Await
                )
            })
            .unwrap();
        HostCallSite {
            code: CodeRef {
                function: bc::FunctionId(id as u32),
                version: package.content_version,
            },
            pc: bc::Pc(range.start as u32),
        }
    }

    #[test]
    fn predicts_positional_and_named_image_literals_in_release_bytecode() {
        for expression in [
            "image(\"next.png\")",
            "image(width: 200, source: \"next.png\")",
        ] {
            let compiled = compile(&format!(
                "fn opening() {{ narrator(\"wait\"); let _ = {expression}; }}"
            ));
            let mut release = compiled.to_unpacked().unwrap();
            for function in &mut release.functions {
                function.source_map.clear();
            }
            let text = bc::externalize_text(&mut release).unwrap();
            let program = bc::verify(release, bc::Limits::default())
                .unwrap()
                .bind_text_pool(text)
                .unwrap();
            let site = first_call(&program, "opening");
            assert_eq!(next_image(&program, [site].into_iter()), Some("next.png"));
        }
    }

    #[test]
    fn follows_an_immutable_helper_and_forwards_its_named_argument() {
        let program = compile(
            r#"
                fn picture(width: int, path: string) { let _ = image(path, width: width); }
                fn opening() { narrator("wait"); picture(path: "portrait.png", width: 128); }
            "#,
        );
        let site = first_call(&program, "opening");
        assert_eq!(
            next_image(&program, [site].into_iter()),
            Some("portrait.png")
        );
    }

    #[test]
    fn branches_require_the_same_first_image() {
        for (right, expected) in [("shared.png", Some("shared.png")), ("different.png", None)] {
            let program = compile(&format!(
                r#"fn opening(route: string = "hall") {{
                    narrator("wait");
                    if route == "hall" {{ let _ = image("shared.png"); }} else {{ let _ = image("{right}"); }}
                }}"#,
            ));
            let site = first_call(&program, "opening");
            assert_eq!(next_image(&program, [site].into_iter()), expected);
        }
    }

    #[test]
    fn localized_dialogue_can_look_ahead_to_a_known_table_key() {
        let program = compile(
            r#"
                fn room(name: string) {
                    let rooms = {hall: {path: "old.png"}, cafe: {path: "cafe.png"},
                                 ["hall"]: {path: "hall.png"}};
                    let _ = image(rooms[name].path);
                }
                fn opening() {
                    narrator("wait");
                    narrator(tr("Yet another sentence.", {en: "One more sentence."}));
                    room("hall");
                }
                fn changed() {
                    narrator("wait");
                    let rooms = {hall: "old.png"};
                    rooms["hall"] = "new.png";
                    let _ = image(rooms["hall"]);
                }
            "#,
        );
        for (name, expected) in [("opening", Some("hall.png")), ("changed", None)] {
            let site = first_call(&program, name);
            assert_eq!(next_image(&program, [site].into_iter()), expected);
        }
    }

    #[test]
    fn predicts_one_entry_from_a_large_imported_catalog() {
        use aside_compiler::semantic::SourceModule;
        let rows = (0..96)
            .map(|index| format!(r#"room{index}: {{source: "room{index}.png", width: 1280}}"#))
            .collect::<Vec<_>>()
            .join(", ");
        let inputs = [
            SourceModule {
                canonical_path: "game::rooms".into(),
                source_path: Some("src/rooms.aside".into()),
                version: "1".into(),
                source: format!("pub let rooms = {{{rows}}};").into(),
            },
            SourceModule {
                canonical_path: "game".into(),
                source_path: Some("src/main.aside".into()),
                version: "1".into(),
                source: r#"
                    use game::rooms::rooms;
                    fn paint(key: string, wide: bool) {
                        let part = rooms[key];
                        if wide {
                            let _ = image(part.source, width: part.width);
                        } else {
                            let _ = image(part.source);
                        }
                    }
                    fn opening() { narrator("wait"); paint("room90", true); }
                "#
                .into(),
            },
        ];
        let compiled =
            super::super::source::compile_project(&inputs, &Default::default(), 1).unwrap();
        let program = &compiled.compilation.program;
        let site = first_call(program, "opening");
        assert_eq!(next_image(program, [site].into_iter()), Some("room90.png"));
    }

    #[test]
    fn declines_catalogs_with_other_writers_or_escaping_rows() {
        for other_use in [
            "fn change() { let alias = rooms.hall; alias.source = \"changed.png\"; }",
            "fn capture() -> fn() -> table { let alias = rooms.hall; || alias }",
            "fn escape() -> table { let alias = rooms.hall; alias }",
        ] {
            let program = compile(&format!(
                r#"
                    let rooms = {{hall: {{source: "hall.png"}}}};
                    {other_use}
                    fn opening() {{ narrator("wait"); let _ = image(rooms.hall.source); }}
                "#
            ));
            let site = first_call(&program, "opening");
            assert!(next_image(&program, [site].into_iter()).is_none());
        }
    }

    #[test]
    fn predicts_layer_sources_forwarded_through_a_helper() {
        let program = compile(
            r#"
                fn portrait(layers: array) { let _ = layered_image(layers, width: 128); }
                fn opening() {
                    narrator("wait");
                    let body = {source: "body.png", width: 128};
                    portrait([body, null, {source: "face.png"}, body]);
                }
            "#,
        );
        let site = first_call(&program, "opening");
        let paths = next_images(&program, [site].into_iter()).unwrap();
        assert_eq!(paths.as_slice(), ["body.png", "face.png"]);
    }

    #[test]
    fn skips_layer_groups_with_runtime_sources_or_unknown_spreads() {
        for layers in ["[{source: path}]", "[{source: \"known.png\", ...changes}]"] {
            let program = compile(&format!(
                r#"fn opening(path: string = "runtime.png", changes: table = {{}}) {{
                    narrator("wait");
                    let _ = layered_image({layers});
                }}"#
            ));
            let site = first_call(&program, "opening");
            assert!(next_images(&program, [site].into_iter()).is_none());
        }
    }

    #[test]
    fn predicts_the_named_expression_and_shared_body_from_the_same_factory() {
        let program = compile(
            r#"
                fn portrait(expression: string) -> Node {
                    let face = match expression {
                        "smile" => "smile.png",
                        _ => "neutral.png",
                    };
                    layered_image([{source: "body.png"}, {source: face}])
                }
                fn opening(expression: string = "smile") {
                    narrator("wait");
                    let _ = portrait("smile");
                }
                fn other() {
                    narrator("wait");
                    let _ = portrait("other");
                }
                fn dynamic(expression: string) {
                    narrator("wait");
                    let _ = portrait(expression);
                }
            "#,
        );
        for (name, paths) in [
            ("opening", ["body.png", "smile.png"]),
            ("other", ["body.png", "neutral.png"]),
        ] {
            let site = first_call(&program, name);
            let predicted = next_images(&program, [site].into_iter()).unwrap();
            assert_eq!(predicted.as_slice(), paths);
        }
        let site = first_call(&program, "dynamic");
        assert!(next_images(&program, [site].into_iter()).is_none());
    }

    #[test]
    fn only_an_ordinary_return_reaches_an_outer_continuation() {
        for (body, expected) in [
            ("narrator(\"wait\");", Some("outer.png")),
            (
                "narrator(\"wait\"); narrator(\"another line\"); narrator(\"last line\");",
                Some("outer.png"),
            ),
            (
                concat!(
                    "narrator(\"wait\"); narrator(\"one\"); narrator(\"two\"); narrator(\"three\");",
                    "narrator(\"four\"); narrator(\"five\"); narrator(\"six\"); narrator(\"seven\");"
                ),
                None,
            ),
            ("narrator(\"wait\"); choose([{text: \"Next\"}]);", None),
            (
                "try { narrator(\"wait\"); } finally { let value = 1; }",
                None,
            ),
        ] {
            let program = compile(&format!(
                "fn pause() {{ {body} }} fn opening() {{ pause(); let _ = image(\"outer.png\"); }}"
            ));
            let sites = [
                first_call(&program, "pause"),
                first_call(&program, "opening"),
            ];
            assert_eq!(next_image(&program, sites.into_iter()), expected);
        }
    }

    #[test]
    fn follows_a_transition_callback_with_known_value_captures() {
        let program = compile(
            r#"
                fn transition_to(path: string) {
                    let change = || { let _ = image(path); };
                    stage.transition(duration: 0.1, change: change);
                }
                fn opening() {
                    narrator("wait");
                    narrator("next line");
                    transition_to("school.png");
                }
                fn mutable_capture() {
                    narrator("wait");
                    let mut path = "before.png";
                    let change = || { let _ = image(path); };
                    path = "after.png";
                    stage.transition(change: change);
                }
            "#,
        );
        for (function, expected) in [("opening", Some("school.png")), ("mutable_capture", None)] {
            let site = first_call(&program, function);
            assert_eq!(next_image(&program, [site].into_iter()), expected);
        }
    }

    #[test]
    fn compiled_plan_reaches_a_cut_through_multiple_dialogue_lines() {
        let program = compile(
            r#"fn opening() {
                narrator("wait");
                narrator("first");
                narrator("second");
                narrator("third");
                narrator("fourth");
                let _ = image("next.png");
            }"#,
        );
        let site = first_call(&program, "opening");
        assert_eq!(next_image(&program, [site].into_iter()), Some("next.png"));
        let prepared = VisualPlan::compile(&program, 1024).unwrap();
        let mut bytes = prepared.as_bytes().to_vec();
        // The decoder must reject a record outside the verified function table.
        bytes[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(VisualPlan::decode(bytes.into_boxed_slice(), &program).is_none());
    }

    #[test]
    fn stops_at_unknown_callee_or_work_limit() {
        let program = compile(
            r#"fn opening(callee: fn() -> null = || {}) {
                narrator("wait");
                callee();
                let _ = image("later.png");
            }"#,
        );
        let site = first_call(&program, "opening");
        assert_eq!(next_image(&program, [site].into_iter()), None);

        let program = compile(r#"fn opening() { narrator("wait"); let _ = image("later.png"); }"#);
        let site = first_call(&program, "opening");
        let function = &program.package().functions[site.code.function.index()];
        let next = function.code.next_pc(site.pc.index()).unwrap().unwrap();
        let catalogs = Catalogs::default();
        let mut walk = Walk::new(&program, &catalogs, 1, BUILD_DIALOGUE_LIMIT);
        assert!(walk.function(function, next, Facts::default(), 0).is_none());
        assert_eq!(walk.remaining, 0);
    }
}
