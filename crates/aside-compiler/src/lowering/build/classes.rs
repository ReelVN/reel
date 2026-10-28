//! Classes retain the analyzed function/capture identities; methods and field
//! initializers use the ordinary function emitter and argument contracts.
use super::*;

impl Lowerer<'_> {
    pub(super) fn external_class_type(
        &mut self,
        name: h::NameId,
    ) -> Result<bc::TypeId, Box<Error>> {
        let name = self.m.hir().name(name);
        let Some((path, name)) = name.rsplit_once("::") else {
            return Err(self.b.error(
                ErrorKind::MissingLink,
                "named type lacks canonical module identity",
            ));
        };
        self.b.work(self.package.modules.len() as u64)?;
        let module = self
            .package
            .modules
            .iter()
            .position(|m| self.package.symbols[m.path.index()] == path)
            .ok_or_else(|| {
                self.b
                    .error(ErrorKind::MissingLink, "named type module is not linked")
            })?;
        let name = self.name(name)?;
        let def = bc::TypeDef::Named {
            module: bc::ModuleId(module as u32),
            name,
            version: self.options.content_version,
        };
        self.b.work(self.package.types.len() as u64)?;
        if let Some(index) = self.package.types.iter().position(|t| *t == def) {
            return Ok(bc::TypeId(index as u32));
        }
        let id = bc::TypeId(self.package.types.len() as u32);
        self.b.push(&mut self.package.types, def)?;
        Ok(id)
    }
    pub(super) fn class_type(&mut self, node: h::NodeId) -> Result<bc::TypeId, Box<Error>> {
        let symbol = self.m.facts()[node.0 as usize].symbol;
        let name = if let Some(symbol) = symbol
            .filter(|s| self.m.symbols()[s.0 as usize].origin == h::BindingOrigin::ModuleGlobal)
        {
            self.m
                .hir()
                .name(self.m.symbols()[symbol.0 as usize].name)
                .to_owned()
        } else {
            format!("$class@{}", node.0)
        };
        self.b.work(self.package.symbols.len() as u64)?;
        let name = if let Some(index) = self.package.symbols.iter().position(|s| s == &name) {
            bc::SymbolId(index as u32)
        } else {
            let id = bc::SymbolId(self.package.symbols.len() as u32);
            let name = self.b.string(&name)?;
            self.b.push(&mut self.package.symbols, name)?;
            id
        };
        let def = bc::TypeDef::Named {
            module: self.module,
            name,
            version: self.options.content_version,
        };
        self.b.work(self.package.types.len() as u64)?;
        if let Some(index) = self.package.types.iter().position(|t| *t == def) {
            return Ok(bc::TypeId(index as u32));
        }
        let id = bc::TypeId(self.package.types.len() as u32);
        self.b.push(&mut self.package.types, def)?;
        Ok(id)
    }
}

impl FunctionBuilder<'_, '_> {
    pub(super) fn class_value(
        &mut self,
        node: h::NodeId,
        base: Option<h::NodeId>,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        let base = match base {
            Some(base) => match self.value(base, depth)? {
                Some(value) => Some(value),
                None => return Ok(None),
            },
            None => None,
        };
        let nominal = Some(self.l.class_type(node)?);
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        for member in self.l.m.hir().children(node) {
            let (member, is_static) = match self.l.m.hir().node(member).form {
                Form::Modified {
                    declaration,
                    static_member,
                    ..
                } => (declaration, static_member),
                _ => (member, false),
            };
            match self.l.m.hir().node(member).form {
                Form::Function {
                    name: Some(name), ..
                } => {
                    let function = self.l.m.facts()[member.0 as usize]
                        .function
                        .expect("analyzed class method");
                    let name = self.l.name(self.l.m.hir().name(name.name.unwrap()))?;
                    let captures = self.capture_sources(function)?;
                    self.l.b.push(
                        &mut methods,
                        bc::ClassMethod {
                            name,
                            function: self.l.outputs[function.0 as usize].expect("selected method"),
                            is_static,
                            captures,
                        },
                    )?;
                }
                Form::Binding {
                    pattern,
                    annotation,
                    initializer,
                    mutable,
                } => {
                    let token = self
                        .l
                        .m
                        .hir()
                        .tokens(pattern)
                        .find(|t| t.name.is_some())
                        .expect("analyzed field name");
                    let name = self.l.name(self.l.m.hir().name(token.name.unwrap()))?;
                    let ty = if let Some(annotation) = annotation {
                        self.l.ty(self.l.m.facts()[annotation.0 as usize].ty, 0)?
                    } else {
                        self.l.builtin(bc::BuiltinType::Any)?
                    };
                    let function = initializer.map(|_| {
                        self.l.m.facts()[member.0 as usize]
                            .function
                            .expect("analyzed field initializer")
                    });
                    let captures = function
                        .map(|f| self.capture_sources(f))
                        .transpose()?
                        .unwrap_or_default();
                    self.l.b.push(
                        &mut fields,
                        bc::ClassField {
                            name,
                            ty,
                            mutable,
                            is_static,
                            initializer: function.map(|f| {
                                self.l.outputs[f.0 as usize].expect("selected initializer")
                            }),
                            captures,
                        },
                    )?;
                }
                _ => {}
            }
        }
        let descriptor = self.descriptor(bc::Descriptor::Class(bc::ClassDesc {
            nominal,
            base,
            fields,
            methods,
        }))?;
        let out = self.temp()?;
        self.op(bc::Opcode::MakeClass, out.0, descriptor.0, 0, node)?;
        if let Some(base) = base {
            self.release(base)?;
        }
        if let Some(symbol) = self.l.m.facts()[node.0 as usize].symbol {
            self.write(symbol, out, true, node)?;
            self.release(out)?;
            Ok(None)
        } else {
            Ok(Some(out))
        }
    }
}
