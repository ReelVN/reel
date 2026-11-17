//! Source-order calls using B01 windows or a private ordered argument builder.
use super::*;

impl FunctionBuilder<'_, '_> {
    pub(super) fn resume_nominal_guard(
        &mut self,
        value: bc::Reg,
        typed: h::NodeId,
        source: h::NodeId,
    ) -> Result<(), Box<Error>> {
        let summary = self.l.m.source_types().value(typed).cloned();
        if let Some(summary) = summary
            && let Some(ty) = self.resume_hint_type(&summary.ty)?
        {
            self.op(bc::Opcode::CheckType, value.0, ty.0, 0, source)?;
            return Ok(());
        }
        let ty = self.l.m.facts()[typed.0 as usize].ty;
        let nominal = match &self.l.m.types()[ty.0 as usize] {
            h::Type::Named(_) | h::Type::Instance(_) => true,
            h::Type::Union(types) => {
                self.l.b.work(types.len() as u64)?;
                types.iter().all(|id| {
                    matches!(
                        self.l.m.types()[id.0 as usize],
                        h::Type::Named(_) | h::Type::Instance(_) | h::Type::Null
                    )
                })
            }
            _ => false,
        };
        if nominal {
            let ty = self.l.ty(ty, 0)?;
            self.op(bc::Opcode::CheckType, value.0, ty.0, 0, source)?;
        }
        Ok(())
    }
    fn resume_hint_type(
        &mut self,
        hint: &crate::semantic::TypeHint,
    ) -> Result<Option<bc::TypeId>, Box<Error>> {
        use crate::semantic::TypeHint;
        match hint {
            TypeHint::Named(canonical) => {
                self.l.b.work(self.l.m.types().len() as u64)?;
                if let Some(index) = self.l.m.types().iter().position(
                    |ty| matches!(ty,h::Type::Named(name) if self.l.m.hir().name(*name)==canonical),
                ) {
                    return self.l.ty(h::TypeId(index as u32), 0).map(Some);
                }
                let Some((module, name)) = canonical.rsplit_once("::") else {
                    return Ok(None);
                };
                self.l.b.work(self.l.package.types.len() as u64)?;
                Ok(self.l.package.types.iter().position(|ty|matches!(ty,bc::TypeDef::Named{module:owner,name:field,..}
                    if self.l.package.symbols[self.l.package.modules[owner.index()].path.index()]==module && self.l.package.symbols[field.index()]==name)).map(|index|bc::TypeId(index as u32)))
            }
            TypeHint::Union(types) if types.iter().any(|ty| matches!(ty, TypeHint::Named(_))) => {
                self.l.b.work(types.len() as u64)?;
                if !types
                    .iter()
                    .all(|ty| matches!(ty, TypeHint::Named(_) | TypeHint::Null))
                {
                    return Ok(None);
                }
                let mut members = Vec::new();
                for hint in types {
                    let ty = if matches!(hint, TypeHint::Null) {
                        Some(self.l.builtin(bc::BuiltinType::Null)?)
                    } else {
                        self.resume_hint_type(hint)?
                    };
                    let Some(ty) = ty else {
                        self.l.b.release(members);
                        return Ok(None);
                    };
                    self.l.b.push(&mut members, ty)?;
                }
                self.l.b.work(self.l.package.types.len() as u64)?;
                if let Some(index) =
                    self.l.package.types.iter().position(
                        |ty| matches!(ty,bc::TypeDef::Union(current) if *current==members),
                    )
                {
                    self.l.b.release(members);
                    return Ok(Some(bc::TypeId(index as u32)));
                }
                let ty = bc::TypeId(self.l.package.types.len() as u32);
                self.l
                    .b
                    .push(&mut self.l.package.types, bc::TypeDef::Union(members))?;
                Ok(Some(ty))
            }
            _ => Ok(None),
        }
    }
    fn call_contract(&mut self, node: h::NodeId) -> Result<Option<bc::TypeId>, Box<Error>> {
        let Form::Call { callee, .. } = self.l.m.hir().node(node).form else {
            return Ok(None);
        };
        let mut ty = self.l.m.facts()[callee.0 as usize].ty;
        if let h::Type::Union(types) = &self.l.m.types()[ty.0 as usize] {
            let mut callable = types
                .iter()
                .copied()
                .filter(|id| !matches!(self.l.m.types()[id.0 as usize], h::Type::Null));
            if let Some(inner) = callable.next().filter(|_| callable.next().is_none()) {
                ty = inner;
            }
        }
        if matches!(self.l.m.types()[ty.0 as usize], h::Type::Callable(..)) {
            self.l.ty(ty, 0).map(Some)
        } else {
            Ok(None)
        }
    }

    fn check_call_result(
        &mut self,
        node: h::NodeId,
        value: bc::Reg,
        contract: Option<bc::TypeId>,
    ) -> Result<(), Box<Error>> {
        if let Some(contract) = contract {
            let bc::TypeDef::Function { return_type, .. } = self.l.package.types[contract.index()]
            else {
                unreachable!()
            };
            if self.l.package.types[return_type.index()]
                != bc::TypeDef::Builtin(bc::BuiltinType::Any)
            {
                self.op(bc::Opcode::CheckType, value.0, return_type.0, 0, node)?;
            }
        }
        Ok(())
    }
    /// The caller owns the callee register across every source-order argument.
    pub(super) fn call_with_target(
        &mut self,
        n: h::NodeId,
        arguments: Option<h::NodeId>,
        children: Option<h::NodeId>,
        target: bc::CallTarget,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        if self.l.resume_calls[n.0 as usize]
            && let bc::CallTarget::Register(value) = target
            && let Form::Call { callee, .. } = self.l.m.hir().node(n).form
        {
            self.resume_nominal_guard(value, callee, n)?;
        }
        if let Some(expansion) = self.expansion(n)? {
            return self.expanded_call(n, expansion, target, depth);
        }
        self.l
            .b
            .work(arguments.map_or(0, |args| self.l.m.hir().elements(args).len()) as u64)?;
        let mut count = arguments.map_or(0, |args| self.l.m.hir().children(args).count())
            + usize::from(children.is_some());
        self.l.b.work(count as u64)?;
        let mut named_tail = None;
        if arguments.is_some_and(|args| {
            self.l
                .m
                .hir()
                .children(args)
                .any(|arg| self.l.m.hir().node(arg).kind == H::Spread)
        }) {
            let arguments = arguments.expect("spread argument list");
            named_tail = self.static_named_tail(arguments, children.is_some())?;
            if let Some((_, fields)) = named_tail {
                count = count - 1 + fields;
            } else {
                return self.packed_call(n, arguments, children, target, depth);
            }
        }
        let window = self.registers.get(
            u32::try_from(count).map_err(|_| {
                self.l
                    .b
                    .error(ErrorKind::Limit, "call window length overflow")
            })?,
            &mut self.l.b,
        )?;
        let mut names = Vec::new();
        if let Some(arguments) = arguments {
            let hir = self.l.m.hir();
            let ordinary = hir
                .children(arguments)
                .filter(|&arg| hir.node(arg).kind != H::Spread);
            let fields = named_tail
                .into_iter()
                .flat_map(|(table, _)| hir.children(table));
            for (offset, arg) in ordinary.chain(fields).enumerate() {
                let (name, value) = match hir.node(arg).form {
                    Form::Argument { name, value } => (name, value),
                    Form::Field {
                        key: h::FieldKey::Name(name),
                        value,
                    } => (Some(name), value),
                    _ => unreachable!("admitted call argument"),
                };
                let Some(value) = self.value(value, depth)? else {
                    self.l.b.release(names);
                    self.release_window(window, n)?;
                    return Ok(None);
                };
                self.op(
                    bc::Opcode::Move,
                    window.start.0 + offset as u32,
                    value.0,
                    0,
                    arg,
                )?;
                self.release(value)?;
                if let Some(name) = name {
                    let name = self.l.name(hir.name(name.name.expect("argument name")))?;
                    self.l.b.push(
                        &mut names,
                        bc::NamedArg {
                            name,
                            offset: offset as u32,
                        },
                    )?;
                }
            }
        }
        if let Some(children) = children {
            let Some(value) = self.children_value(children, depth)? else {
                self.l.b.release(names);
                self.release_window(window, n)?;
                return Ok(None);
            };
            let offset = count as u32 - 1;
            self.op(
                bc::Opcode::Move,
                window.start.0 + offset,
                value.0,
                0,
                children,
            )?;
            self.release(value)?;
            let name = self.l.name("children")?;
            self.l.b.push(&mut names, bc::NamedArg { name, offset })?;
        }
        let contract = self.call_contract(n)?;
        let descriptor = self.descriptor(bc::Descriptor::Call(bc::CallDesc {
            contract,
            target,
            arguments: bc::Arguments::Window(window),
            names,
        }))?;
        let out = self.temp()?;
        self.op(self.call_opcode(), out.0, descriptor.0, 0, n)?;
        self.check_call_result(n, out, contract)?;
        self.release_window(window, n)?;
        Ok(Some(out))
    }

    fn static_named_tail(
        &mut self,
        arguments: h::NodeId,
        trailing_children: bool,
    ) -> Result<Option<(h::NodeId, usize)>, Box<Error>> {
        let hir = self.l.m.hir();
        let Some(tail) = hir.children(arguments).last() else {
            return Ok(None);
        };
        let Form::Spread { value: table } = hir.node(tail).form else {
            return Ok(None);
        };
        if hir.node(table).kind != H::Object {
            return Ok(None);
        }
        let mut keys = std::collections::BTreeSet::new();
        for arg in hir.children(arguments).take_while(|&arg| arg != tail) {
            let Form::Argument { name, .. } = hir.node(arg).form else {
                return Ok(None);
            };
            if let Some(name) = name
                && !keys.insert(name.name.expect("argument name"))
            {
                return Ok(None);
            }
        }
        self.l.b.work(hir.elements(table).len() as u64)?;
        let mut count = 0;
        for field in hir.children(table) {
            let Form::Field {
                key: h::FieldKey::Name(key),
                ..
            } = hir.node(field).form
            else {
                return Ok(None);
            };
            let name = key.name.expect("field name");
            if (trailing_children && hir.name(name) == "children") || !keys.insert(name) {
                return Ok(None);
            }
            count += 1;
        }
        Ok(Some((table, count)))
    }
    fn packed_call(
        &mut self,
        n: h::NodeId,
        arguments: h::NodeId,
        children: Option<h::NodeId>,
        target: bc::CallTarget,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        self.l.b.span = self.span(n);
        let builder = self.temp()?;
        self.op(bc::Opcode::ArgsNew, builder.0, 0, 0, n)?;
        self.l
            .b
            .work(self.l.m.hir().elements(arguments).len() as u64)?;
        for arg in self.l.m.hir().children(arguments) {
            let (value, name, spread) = match self.l.m.hir().node(arg).form {
                Form::Argument { name, value } => (value, name, false),
                Form::Spread { value } => (value, None, true),
                _ => {
                    return Err(self
                        .l
                        .b
                        .error(ErrorKind::InvalidIr, "invalid analyzed argument"));
                }
            };
            let Some(value) = self.value(value, depth)? else {
                self.release(builder)?;
                return Ok(None);
            };
            let (opcode, symbol) = if spread {
                (bc::Opcode::ArgsSpread, 0)
            } else if let Some(name) = name {
                let symbol = self
                    .l
                    .name(self.l.m.hir().name(name.name.expect("argument name")))?;
                (bc::Opcode::ArgsNamed, symbol.0)
            } else {
                (bc::Opcode::ArgsPush, 0)
            };
            self.op(opcode, builder.0, value.0, symbol, arg)?;
            self.release(value)?;
        }
        if let Some(children) = children {
            let Some(value) = self.children_value(children, depth)? else {
                self.release(builder)?;
                return Ok(None);
            };
            let name = self.l.name("children")?;
            self.op(bc::Opcode::ArgsNamed, builder.0, value.0, name.0, children)?;
            self.release(value)?;
        }
        self.l.b.span = self.span(n);
        let contract = self.call_contract(n)?;
        let descriptor = self.descriptor(bc::Descriptor::Call(bc::CallDesc {
            contract,
            target,
            arguments: bc::Arguments::Packed(builder),
            names: Vec::new(),
        }))?;
        let out = self.temp()?;
        self.op(self.call_opcode(), out.0, descriptor.0, 0, n)?;
        self.check_call_result(n, out, contract)?;
        self.release(builder)?;
        Ok(Some(out))
    }
}
