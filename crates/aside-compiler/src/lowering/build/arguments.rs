//! Source-order calls using B01 windows or a private ordered argument builder.
use super::*;

impl FunctionBuilder<'_, '_> {
    /// The caller owns the callee register across every source-order argument.
    pub(super) fn call_with_target(
        &mut self,
        n: h::NodeId,
        arguments: Option<h::NodeId>,
        target: bc::CallTarget,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Error> {
        self.l
            .b
            .work(arguments.map_or(0, |args| self.l.m.hir().elements(args).len()) as u64)?;
        let count = arguments.map_or(0, |args| self.l.m.hir().children(args).count());
        self.l.b.work(count as u64)?;
        if arguments.is_some_and(|args| {
            self.l
                .m
                .hir()
                .children(args)
                .any(|arg| self.l.m.hir().node(arg).kind == H::Spread)
        }) {
            return self.packed_call(n, arguments.expect("spread argument list"), target, depth);
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
            for (i, arg) in self.l.m.hir().children(arguments).enumerate() {
                let Form::Argument { name, value } = self.l.m.hir().node(arg).form else {
                    return Err(self.l.b.error(
                        ErrorKind::Unsupported,
                        "spread requires ordered argument builder lowering",
                    ));
                };
                let Some(value) = self.value(value, depth)? else {
                    self.l.b.release(names);
                    self.release_window(window, n)?;
                    return Ok(None);
                };
                self.op(bc::Opcode::Move, window.start.0 + i as u32, value.0, 0, arg)?;
                self.release(value)?;
                if let Some(name) = name {
                    let name = self
                        .l
                        .name(self.l.m.hir().name(name.name.expect("argument name")))?;
                    self.l.b.push(
                        &mut names,
                        bc::NamedArg {
                            name,
                            offset: i as u32,
                        },
                    )?;
                }
            }
        }
        let descriptor = self.descriptor(bc::Descriptor::Call(bc::CallDesc {
            target,
            arguments: bc::Arguments::Window(window),
            names,
        }))?;
        let out = self.temp()?;
        self.op(bc::Opcode::Call, out.0, descriptor.0, 0, n)?;
        self.release_window(window, n)?;
        Ok(Some(out))
    }
    fn packed_call(
        &mut self,
        n: h::NodeId,
        arguments: h::NodeId,
        target: bc::CallTarget,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Error> {
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
        self.l.b.span = self.span(n);
        let descriptor = self.descriptor(bc::Descriptor::Call(bc::CallDesc {
            target,
            arguments: bc::Arguments::Packed(builder),
            names: Vec::new(),
        }))?;
        let out = self.temp()?;
        self.op(bc::Opcode::Call, out.0, descriptor.0, 0, n)?;
        self.release(builder)?;
        Ok(Some(out))
    }
}
