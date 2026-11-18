//! A bounded read-only walk of the current story's bytecode. The value facts
//! belong to this query and never come from the running VM's registers or heap.

use arrayvec::ArrayVec;
use aside_vm::{
    bytecode::{self as bc, Opcode},
    executor::HostCallSite,
};

const INSTRUCTION_LIMIT: usize = 512;
const NESTING_LIMIT: usize = 8;
const CALLER_LIMIT: usize = 16;
const ARGUMENT_LIMIT: usize = 32;
const FACT_LIMIT: usize = 32;
const PATH_LIMIT: usize = 1024;
const LAYER_LIMIT: usize = 8;
const GROUP_LIMIT: usize = 8;
const FIELD_LIMIT: usize = 64;

type Paths<'p> = ArrayVec<&'p str, LAYER_LIMIT>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fact {
    Unknown,
    Constant(bc::ConstId),
    Function(bc::FunctionId),
    Layer(bc::ConstId),
    Layers(u8),
}

#[derive(Clone, Default)]
struct Facts(ArrayVec<(u32, Fact), FACT_LIMIT>);

impl Facts {
    fn get(&self, register: u32) -> Fact {
        self.0
            .iter()
            .find_map(|&(key, fact)| (key == register).then_some(fact))
            .unwrap_or(Fact::Unknown)
    }

    fn set(&mut self, register: u32, fact: Fact) {
        if let Some(index) = self.0.iter().position(|&(key, _)| key == register) {
            if fact == Fact::Unknown {
                self.0.swap_remove(index);
            } else {
                self.0[index].1 = fact;
            }
        } else if fact != Fact::Unknown {
            // Excess registers stay unknown for this walk.
            let _ = self.0.try_push((register, fact));
        }
    }
}

enum Outcome<'p> {
    Images(Paths<'p>),
    Return(Fact),
}

/// Examine ordinary suspended callers, starting with the innermost one. A
/// successful return can expose the next caller. An unresolved path cannot.
pub(super) fn next_images(
    program: &bc::VerifiedProgram,
    sites: impl Iterator<Item = HostCallSite>,
) -> Option<Paths<'_>> {
    let mut walk = Walk {
        program,
        remaining: INSTRUCTION_LIMIT,
        code: bc::ResidentCodeReader::default(),
        layers: ArrayVec::new(),
    };
    for site in sites.take(CALLER_LIMIT) {
        if site.code.version != program.package().content_version {
            return None;
        }
        let function = program
            .package()
            .functions
            .get(site.code.function.index())?;
        let (call, next) = walk.instruction(function, site.pc.index())?;
        let opcode = bc::instruction_spec(call.opcode)?.opcode;
        if !matches!(opcode, Opcode::Call | Opcode::CallNoWait | Opcode::Await) {
            return None;
        }
        match walk.function(function, next, Facts::default(), 0)? {
            Outcome::Images(paths) => return Some(paths),
            Outcome::Return(_) => {}
        }
    }
    None
}

struct Walk<'p> {
    program: &'p bc::VerifiedProgram,
    remaining: usize,
    code: bc::ResidentCodeReader,
    layers: ArrayVec<ArrayVec<bc::ConstId, LAYER_LIMIT>, GROUP_LIMIT>,
}

impl<'p> Walk<'p> {
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
                    let value = self
                        .global_function(module, global)
                        .map_or(Fact::Unknown, Fact::Function);
                    facts.set(a, value);
                }
                Opcode::MakeTable => {
                    let source = self.layer_source(&function.descriptors[b as usize], &facts);
                    facts.set(a, source.map_or(Fact::Unknown, Fact::Layer));
                }
                Opcode::MakeArray => {
                    let group = self.layer_group(&function.descriptors[b as usize], &facts);
                    facts.set(a, group.map_or(Fact::Unknown, Fact::Layers));
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
            Fact::Constant(id) => Some(&self.program.package().constants[id.index()]),
            Fact::Function(_) | Fact::Layer(_) | Fact::Layers(_) => None,
        };
        match (opcode, constant) {
            (Opcode::BranchBool, Some(bc::Const::Bool(value))) => Some(Some(*value)),
            (Opcode::BranchNull, constant) => Some(Some(matches!(constant, Some(bc::Const::Null)))),
            _ => None,
        }
    }

    fn call(&mut self, call: &bc::CallDesc, facts: &Facts, nesting: usize) -> Option<Outcome<'p>> {
        let package = self.program.package();
        let target = match call.target {
            bc::CallTarget::Register(reg) => match facts.get(reg.0) {
                Fact::Function(function) => bc::CallTarget::Function(function),
                _ => return None,
            },
            target => target,
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
            if !matches!(symbol, "reel::image" | "reel::layered_image") {
                return None;
            }
            let signature = &package.signatures[native.signature.index()];
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
        if !function.captures.is_empty() {
            return None;
        }
        let signature = &package.signatures[function.signature.index()];
        let arguments = self.arguments(signature, call, facts)?;
        let mut parameters = Facts::default();
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

    fn text(&self, id: bc::ConstId) -> Option<&'p str> {
        if !matches!(
            self.program.package().constants[id.index()],
            bc::Const::Utf8(_) | bc::Const::Text { .. }
        ) {
            return None;
        }
        std::str::from_utf8(self.program.constant_bytes(id)?).ok()
    }

    fn path(&self, id: bc::ConstId) -> Option<&'p str> {
        let path = self.text(id)?;
        (!path.is_empty() && path.len() <= PATH_LIMIT).then_some(path)
    }

    fn layer_source(&mut self, descriptor: &bc::Descriptor, facts: &Facts) -> Option<bc::ConstId> {
        let count = match descriptor {
            bc::Descriptor::Table(pairs) => pairs.len(),
            bc::Descriptor::TableSources(entries) => entries.len(),
            _ => return None,
        };
        if count > FIELD_LIMIT {
            return None;
        }
        self.remaining = self.remaining.checked_sub(count)?;
        let mut source = Fact::Unknown;
        for index in 0..count {
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
            if self.text(key)? == "source" {
                source = facts.get(value.0);
            }
        }
        match source {
            Fact::Constant(id) => Some(id),
            _ => None,
        }
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
                Fact::Layer(id) => group.push(id),
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
    use super::*;
    use aside_vm::{bytecode::InstructionSequence, executor::CodeRef};

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
                r#"fn opening(flag: bool = true) {{
                    narrator("wait");
                    if flag {{ let _ = image("shared.png"); }} else {{ let _ = image("{right}"); }}
                }}"#,
            ));
            let site = first_call(&program, "opening");
            assert_eq!(next_image(&program, [site].into_iter()), expected);
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
    fn only_an_ordinary_return_reaches_an_outer_continuation() {
        for (body, expected) in [
            ("narrator(\"wait\");", Some("outer.png")),
            ("narrator(\"wait\"); narrator(\"another wait\");", None),
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
        let mut walk = Walk {
            program: &program,
            remaining: 1,
            code: bc::ResidentCodeReader::default(),
            layers: ArrayVec::new(),
        };
        assert!(walk.function(function, next, Facts::default(), 0).is_none());
        assert_eq!(walk.remaining, 0);
    }
}
