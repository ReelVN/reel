//! Prove pure mask constructions without executing the story or changing VM calls.
//! Unknown values/control flow retain the ordinary dynamic mask implementation.
use super::*;
use aside_compiler::strings::{StringLiteral, StringPart};
use reel_ui::stage::{MaskChannel, MaskOp, MaskProgram};
use std::collections::HashSet;

#[derive(Clone)]
enum Static {
    Number(f64),
    Text(Arc<str>),
    Bool(bool),
    Mask(Arc<Tree>),
}
struct Tree {
    op: Operation,
    inputs: Vec<Arc<Tree>>,
}
enum Operation {
    Constant(u32),
    Parameter(u8),
    Sample {
        path: Arc<str>,
        channel: MaskChannel,
        repeat: u8,
    },
    Add,
    Subtract,
    Multiply,
    Pow(u32),
    Clamp(u32, u32),
    Smoothstep,
}
type Environment = HashMap<(usize, h::SymbolId), Static>;
struct Proof<'a> {
    modules: &'a [h::AnalyzedModule],
    initializers: &'a [HashMap<h::SymbolId, h::NodeId>],
    aliases: HashMap<(usize, h::SymbolId), Static>,
    active: HashSet<(usize, h::SymbolId)>,
    mask_contracts: Vec<aside_host::NativeContract>,
    work: usize,
}

pub(super) fn collect(
    modules: &[h::AnalyzedModule],
) -> Result<Vec<CompiledEffect>, EffectDiagnostic> {
    let initializers: Vec<_> = modules
        .iter()
        .map(|module| {
            module
                .hir()
                .nodes
                .iter()
                .filter_map(|node| {
                    let Form::Binding {
                        pattern,
                        initializer: Some(value),
                        mutable: false,
                        ..
                    } = node.form
                    else {
                        return None;
                    };
                    let symbol = module.facts()[pattern.0 as usize].symbol?;
                    (!module.symbols()[symbol.0 as usize].mutable).then_some((symbol, value))
                })
                .collect::<HashMap<_, _>>()
        })
        .collect();
    let mut output = Vec::new();
    let mut keys = HashSet::new();
    let mut proof = Proof {
        modules,
        initializers: &initializers,
        aliases: HashMap::new(),
        active: HashSet::new(),
        mask_contracts: super::super::super::masks::contracts(),
        work: 0,
    };
    for (module_index, module) in modules.iter().enumerate() {
        let selected = aside_compiler::lowering::select_branches(module, Default::default())
            .map_err(|error| EffectDiagnostic {
                module: module.source_path().into(),
                span: error.span,
                message: error.detail,
            })?;
        let mut function_nodes = vec![None; module.hir().nodes.len()];
        for (index, function) in module.functions().iter().enumerate() {
            function_nodes[function.node.0 as usize] = Some(h::FunctionId(index as u32));
        }
        for (index, node) in module.hir().nodes.iter().enumerate() {
            if !selected.contains(h::NodeId(index as u32))
                || !matches!(node.form, Form::Call { .. })
            {
                continue;
            }
            proof.work = 0;
            let Some(Static::Mask(tree)) = proof.value(
                module_index,
                h::NodeId(index as u32),
                &mut Environment::new(),
                0,
            ) else {
                continue;
            };
            // Almost all calls in a story are unrelated to masks. Resolve the
            // owner only after the pure-construction proof succeeds.
            let mut owner = Some(h::NodeId(index as u32));
            let mut function = None;
            while let Some(node) = owner {
                if let Some(found) = function_nodes[node.0 as usize] {
                    function = Some(found);
                    break;
                }
                owner = module.hir().node(node).parent;
            }
            let Some(function) = function else {
                continue;
            };
            let Some(mask) = project(tree) else {
                continue;
            };
            let Ok(key) = super::products::encode_mask_template(&mask) else {
                continue;
            };
            if !keys.insert(key) {
                continue;
            }
            let Some(program) = reel_ui::stage::effect::lower_mask(&mask) else {
                continue;
            };
            output.push(CompiledEffect {
                entry: EffectFunction {
                    module: module_index,
                    function,
                },
                mask: Some(mask),
                node_sources: vec![
                    EffectSource {
                        module: module_index,
                        span: node.span
                    };
                    program.nodes().len()
                ],
                source_files: modules
                    .iter()
                    .map(|module| Arc::from(module.source_path()))
                    .collect(),
                program,
            });
        }
    }
    Ok(output)
}
impl Proof<'_> {
    fn value(
        &mut self,
        module_index: usize,
        node: h::NodeId,
        env: &mut Environment,
        depth: usize,
    ) -> Option<Static> {
        if depth > 64 || self.work >= 100_000 {
            return None;
        }
        self.work += 1;
        let module = &self.modules[module_index];
        let hir = module.hir();
        if let Some(literal) = StringLiteral::new(hir, node) {
            let mut bytes = Vec::new();
            for part in literal.parts() {
                let StringPart::Text(text) = part else {
                    return None;
                };
                text.append_to(&mut bytes).ok()?;
            }
            return Some(Static::Text(Arc::from(String::from_utf8(bytes).ok()?)));
        }
        match hir.node(node).form {
            Form::Single { child }
            | Form::Statement {
                expression: child, ..
            } => self.value(module_index, child, env, depth + 1),
            Form::Literal { token } => match token.kind {
                K::Number => {
                    let text = hir.text(token.span).replace('_', "");
                    let number = if let Some(digits) =
                        text.strip_prefix("0x").or_else(|| text.strip_prefix("0X"))
                    {
                        i64::from_str_radix(digits, 16).ok()? as f64
                    } else if let Some(digits) =
                        text.strip_prefix("0b").or_else(|| text.strip_prefix("0B"))
                    {
                        i64::from_str_radix(digits, 2).ok()? as f64
                    } else if let Some(digits) =
                        text.strip_prefix("0o").or_else(|| text.strip_prefix("0O"))
                    {
                        i64::from_str_radix(digits, 8).ok()? as f64
                    } else {
                        text.parse::<f64>().ok()?
                    };
                    Some(Static::Number(number))
                }
                K::True => Some(Static::Bool(true)),
                K::False => Some(Static::Bool(false)),
                _ => None,
            },
            Form::Name { .. } => {
                let symbol = module.facts()[node.0 as usize].symbol?;
                let key = (module_index, symbol);
                if let Some(value) = env.get(&key) {
                    return Some(value.clone());
                }
                // A helper's local initializer can depend on its arguments.
                // Share only proofs evaluated without a call-local environment.
                let cacheable = env.is_empty();
                if cacheable && let Some(value) = self.aliases.get(&key) {
                    return Some(value.clone());
                }
                let value = *self.initializers[module_index].get(&symbol)?;
                if !self.active.insert(key) {
                    return None;
                }
                let result = self.value(module_index, value, env, depth + 1);
                self.active.remove(&key);
                if cacheable && let Some(value) = &result {
                    self.aliases.insert(key, value.clone());
                }
                result
            }
            Form::Unary { operator, value } if matches!(operator.kind, K::Plus | K::Minus) => {
                let Static::Number(value) = self.value(module_index, value, env, depth + 1)? else {
                    return None;
                };
                Some(Static::Number(if operator.kind == K::Minus {
                    -value
                } else {
                    value
                }))
            }
            Form::Call {
                callee,
                arguments,
                children: None,
                optional: false,
                ..
            } => {
                let reference = module.canonical_reference(callee);
                let mask_name = reference
                    .filter(|reference| {
                        reference.module == "reel::mask" && reference.version == "1"
                    })
                    .and_then(|reference| reference.export.as_deref());
                // Resolve before inspecting arguments: ordinary UI, dialogue and
                // platform calls cannot form a pure mask construction.
                let helper = if mask_name.is_none() {
                    let target = resolve_effect_function(self.modules, module_index, callee)?;
                    let function =
                        &self.modules[target.module].functions()[target.function.0 as usize];
                    if !function.captures.is_empty()
                        || function.kind != h::FunctionKind::Plain
                        || function.parameters.iter().any(|p| p.rest)
                    {
                        return None;
                    }
                    Some(target)
                } else {
                    None
                };
                let mut arguments_out = Vec::new();
                for argument in arguments
                    .into_iter()
                    .flat_map(|arguments| hir.children(arguments))
                {
                    let Form::Argument { name, value } = hir.node(argument).form else {
                        return None;
                    };
                    arguments_out.push((
                        name.map(|name| hir.name(name.name.unwrap()).to_string()),
                        self.value(module_index, value, env, depth + 1)?,
                    ));
                }
                if let Some(name) = mask_name {
                    return self.mask_call(name, arguments_out);
                }
                let target = helper?;
                let target_module = &self.modules[target.module];
                let function = &target_module.functions()[target.function.0 as usize];
                let names: Vec<_> = function
                    .parameters
                    .iter()
                    .map(|p| {
                        target_module
                            .hir()
                            .name(target_module.symbols()[p.symbol.0 as usize].name)
                            .to_owned()
                    })
                    .collect();
                let values = order(&names, arguments_out)?;
                let mut local = Environment::new();
                for (index, parameter) in function.parameters.iter().enumerate() {
                    let value = if let Some(value) = values[index].clone() {
                        value
                    } else {
                        self.value(target.module, parameter.default?, &mut local, depth + 1)?
                    };
                    local.insert((target.module, parameter.symbol), value);
                }
                self.body(target.module, function.body, &mut local, depth + 1)
            }
            Form::Block { .. } => self.body(module_index, node, env, depth + 1),
            Form::Condition {
                pattern: None,
                condition,
                body,
                otherwise,
                ..
            } => {
                let Static::Bool(condition) =
                    self.value(module_index, condition, env, depth + 1)?
                else {
                    return None;
                };
                self.value(
                    module_index,
                    if condition { body } else { otherwise? },
                    env,
                    depth + 1,
                )
            }
            _ => None,
        }
    }
    fn body(
        &mut self,
        module: usize,
        body: h::NodeId,
        env: &mut Environment,
        depth: usize,
    ) -> Option<Static> {
        let hir = self.modules[module].hir();
        let Form::Block { tail } = hir.node(body).form else {
            return self.value(module, body, env, depth);
        };
        for node in hir.children(body) {
            match hir.node(node).form {
                Form::Binding {
                    pattern,
                    initializer: Some(value),
                    mutable: false,
                    ..
                } => {
                    let value = self.value(module, value, env, depth + 1)?;
                    env.insert(
                        (
                            module,
                            self.modules[module].facts()[pattern.0 as usize].symbol?,
                        ),
                        value,
                    );
                }
                Form::Transfer {
                    value: Some(value), ..
                } if hir.node(node).kind == H::Return => {
                    return self.value(module, value, env, depth + 1);
                }
                _ if Some(node) == tail => return self.value(module, node, env, depth + 1),
                _ => return None,
            }
        }
        None
    }
    fn mask_call(&self, name: &str, arguments: Vec<(Option<String>, Static)>) -> Option<Static> {
        let contract = self
            .mask_contracts
            .iter()
            .find(|contract| contract.symbol.strip_prefix("reel::mask::") == Some(name))?;
        let names: Vec<_> = contract.parameters.iter().map(|p| p.name.clone()).collect();
        let mut values = order(&names, arguments)?;
        for (value, parameter) in values.iter_mut().zip(&contract.parameters) {
            if value.is_none() {
                *value = Some(match parameter.default.as_ref()? {
                    aside_vm::bytecode::Const::Int(v) => Static::Number(*v as f64),
                    aside_vm::bytecode::Const::FloatBits(v) => Static::Number(f64::from_bits(*v)),
                    aside_vm::bytecode::Const::Utf8(v) => Static::Text(Arc::from(v.as_str())),
                    _ => return None,
                });
            }
        }
        let values: Vec<_> = values.into_iter().collect::<Option<_>>()?;
        let number = |index: usize| match &values[index] {
            Static::Number(v) if (*v as f32).is_finite() => Some((*v as f32).to_bits()),
            _ => None,
        };
        let text = |index: usize| match &values[index] {
            Static::Text(v) => Some(v.clone()),
            _ => None,
        };
        let input = |index: usize| match &values[index] {
            Static::Mask(v) => Some(v.clone()),
            Static::Number(v) if (*v as f32).is_finite() => Some(Arc::new(Tree {
                op: Operation::Constant((*v as f32).to_bits()),
                inputs: vec![],
            })),
            _ => None,
        };
        let (op, inputs) = match name {
            "alpha" | "gray" => {
                let path = text(0)?;
                if path.is_empty() || path.ends_with(".reel-image") {
                    return None;
                }
                let repeat = match text(5)?.as_ref() {
                    "none" => 0,
                    "x" => 1,
                    "y" => 2,
                    "both" => 3,
                    _ => return None,
                };
                (
                    Operation::Sample {
                        path,
                        channel: if name == "alpha" {
                            MaskChannel::Alpha
                        } else {
                            MaskChannel::Gray
                        },
                        repeat,
                    },
                    (1..5).map(input).collect::<Option<_>>()?,
                )
            }
            "param" => (
                Operation::Parameter(match text(0)?.as_ref() {
                    "progress" => 0,
                    "softness" => 1,
                    "x" => 2,
                    "y" => 3,
                    _ => return None,
                }),
                vec![],
            ),
            "add" => (Operation::Add, vec![input(0)?, input(1)?]),
            "subtract" => (Operation::Subtract, vec![input(0)?, input(1)?]),
            "multiply" => (Operation::Multiply, vec![input(0)?, input(1)?]),
            "pow" => (Operation::Pow(number(1)?), vec![input(0)?]),
            "clamp" => (Operation::Clamp(number(1)?, number(2)?), vec![input(0)?]),
            "smoothstep" => (Operation::Smoothstep, vec![input(0)?, input(1)?, input(2)?]),
            _ => return None,
        };
        Some(Static::Mask(Arc::new(Tree { op, inputs })))
    }
}
fn order(
    names: &[String],
    arguments: Vec<(Option<String>, Static)>,
) -> Option<Vec<Option<Static>>> {
    let mut output = vec![None; names.len()];
    let mut positional = 0;
    for (name, value) in arguments {
        let index = if let Some(name) = name {
            names.iter().position(|candidate| *candidate == name)?
        } else {
            let index = positional;
            positional += 1;
            index
        };
        if output.get_mut(index)?.replace(value).is_some() {
            return None;
        }
    }
    Some(output)
}
fn project(root: Arc<Tree>) -> Option<Arc<MaskProgram>> {
    let mut operations = Vec::new();
    let mut sources = Vec::<Arc<str>>::new();
    let mut nodes = HashMap::new();
    let mut constants = HashMap::new();
    let mut pending = vec![(root, false)];
    while let Some((node, ready)) = pending.pop() {
        let key = Arc::as_ptr(&node);
        if nodes.contains_key(&key) {
            continue;
        }
        if let Operation::Constant(bits) = node.op {
            let slot = *constants.entry(bits).or_insert_with(|| {
                let index = operations.len() as u32;
                operations.push(MaskOp::Constant(bits));
                index
            });
            nodes.insert(key, slot);
            continue;
        }
        if !ready {
            pending.push((node.clone(), true));
            pending.extend(node.inputs.iter().rev().map(|node| (node.clone(), false)));
            continue;
        }
        let at = |index: usize| nodes[&Arc::as_ptr(&node.inputs[index])];
        let op = match &node.op {
            Operation::Constant(_) => unreachable!(),
            Operation::Parameter(index) => MaskOp::Parameter(*index),
            Operation::Sample {
                path,
                channel,
                repeat,
            } => {
                let image = sources
                    .iter()
                    .position(|source| source == path)
                    .unwrap_or_else(|| {
                        sources.push(path.clone());
                        sources.len() - 1
                    });
                MaskOp::Sample {
                    image: image as u32,
                    channel: *channel,
                    coordinates: [at(0), at(1), at(2), at(3)],
                    repeat: *repeat,
                }
            }
            Operation::Add => MaskOp::Add(at(0), at(1)),
            Operation::Subtract => MaskOp::Subtract(at(0), at(1)),
            Operation::Multiply => MaskOp::Multiply(at(0), at(1)),
            Operation::Pow(power) => MaskOp::Pow(at(0), *power),
            Operation::Clamp(low, high) => MaskOp::Clamp(at(0), *low, *high),
            Operation::Smoothstep => MaskOp::Smoothstep(at(0), at(1), at(2)),
        };
        nodes.insert(key, operations.len() as u32);
        operations.push(op);
        if operations.len() > 32 || sources.len() > 4 {
            return None;
        }
    }
    MaskProgram::new(operations, sources).ok()
}
