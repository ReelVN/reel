//! Allocate selected function bodies and their default argument thunks.
use super::*;

impl Lowerer<'_> {
    pub(super) fn prepare_functions(&mut self) -> Result<(), Box<Error>> {
        for f in self.m.functions() {
            if self.active[f.node.0 as usize] == 0 {
                self.b.push(&mut self.outputs, None)?;
                self.b.push(&mut self.defaults, Vec::new())?;
                continue;
            }
            self.b.span = self.m.hir().node(f.node).span;
            if f.kind == h::FunctionKind::Async {
                return Err(self.b.error(
                    ErrorKind::Unsupported,
                    "async functions belong to later lowering",
                ));
            }
            let mut parameters = Vec::new();
            let mut rest = None;
            let mut defaults = Vec::new();
            self.b.work(f.parameters.len() as u64)?;
            for p in &f.parameters {
                let symbol = &self.m.symbols()[p.symbol.0 as usize];
                let ty = p
                    .annotation
                    .map(|id| self.ty(id, 0))
                    .transpose()?
                    .unwrap_or(self.builtin(bc::BuiltinType::Any)?);
                self.contracts[p.symbol.0 as usize] = ty;
                let name = self.name(self.m.hir().name(symbol.name))?;
                let parameter = bc::Parameter {
                    name,
                    ty,
                    mutable: symbol.mutable,
                    default: None,
                };
                if p.rest {
                    self.b.record()?;
                    rest = Some(parameter);
                } else {
                    self.b.push(&mut parameters, parameter)?;
                }
                self.b.push(&mut defaults, None)?;
            }
            let result = if matches!(
                self.m.hir().node(f.node).form,
                Form::Function {
                    result: Some(_),
                    ..
                }
            ) {
                self.ty(f.return_type, 0)?
            } else {
                self.builtin(bc::BuiltinType::Any)?
            };
            let context = if f.declared_context.derived && f.declared_context.init {
                return Err(self.b.error(
                    ErrorKind::Unsupported,
                    "B01 Context cannot encode combined Derived+Init declarations",
                ));
            } else if f.declared_context.derived {
                bc::Context::Derived
            } else if f.declared_context.init {
                bc::Context::Init
            } else if f.declared_context.no_wait {
                bc::Context::NoWait
            } else {
                bc::Context::Ordinary
            };
            let signature = self.signature(parameters, result, context)?;
            self.package.signatures[signature.index()].rest = rest;
            let name = match self.m.hir().node(f.node).form {
                Form::Function {
                    name: Some(token), ..
                } => self.m.hir().name(token.name.expect("function name")),
                _ => "$closure",
            };
            let captures = self.captures(f)?;
            let output = self.empty_function(name, signature, context, captures)?;
            if f.kind == h::FunctionKind::Generator {
                self.package.functions[output.index()].kind = bc::FunctionKind::Generator;
            }
            self.b.push(&mut self.outputs, Some(output))?;
            self.b.push(&mut self.defaults, defaults)?;
        }
        for (i, f) in self.m.functions().iter().enumerate() {
            let Some(output) = self.outputs[i] else {
                continue;
            };
            for (p, parameter) in f.parameters.iter().enumerate() {
                let Some(node) = parameter.default else {
                    continue;
                };
                self.b.span = self.m.hir().node(node).span;
                let body_signature = self.package.functions[output.index()].signature;
                if let Some(value) = self.literal_constant(node)? {
                    let mut existing = None;
                    for (index, constant) in self.package.constants.iter().enumerate() {
                        if source_constant_matches(constant, &value, &mut self.b)? {
                            existing = Some(bc::ConstId(index as u32));
                            break;
                        }
                    }
                    let id = if let Some(id) = existing {
                        self.b.release_bytes(match &value {
                            bc::Const::Utf8(s) => s.capacity(),
                            _ => 0,
                        });
                        id
                    } else {
                        let id = bc::ConstId(self.package.constants.len() as u32);
                        self.b.push(&mut self.package.constants, value)?;
                        id
                    };
                    self.package.signatures[body_signature.index()].parameters[p].default =
                        Some(bc::ParameterDefault::Constant(id));
                    continue;
                }
                self.b.work(p as u64)?;
                let mut prefix = Vec::new();
                self.b.reserve(&mut prefix, p)?;
                for param in &self.package.signatures[body_signature.index()].parameters[..p] {
                    let mut param = param.clone();
                    param.default = None;
                    prefix.push(param);
                }
                let result = self.package.signatures[body_signature.index()].parameters[p].ty;
                let signature = self.signature(prefix, result, bc::Context::NoWait)?;
                let captures = self.captures(f)?;
                let default =
                    self.empty_function("$default", signature, bc::Context::NoWait, captures)?;
                self.defaults[i][p] = Some(default);
                self.package.signatures[body_signature.index()].parameters[p].default =
                    Some(bc::ParameterDefault::Function(default));
            }
        }
        Ok(())
    }
}
