//! Native function values use VM-rooted callable registrations. Calls enter
//! the ordinary native frame directly, including defaults and host callbacks.
use super::*;

impl Lowerer<'_> {
    fn native_value(&mut self, native: bc::NativeId) -> Result<bc::FunctionId, Box<Error>> {
        if let Some(function) = self.native_values[native.index()] {
            return Ok(function);
        }
        let signature = self.package.natives[native.index()].signature;
        let mut source_map = Vec::new();
        self.b.push(
            &mut source_map,
            bc::SourceMapEntry {
                pc: bc::Pc(0),
                span: bc::SourceSpan {
                    file: self.source,
                    start: self.b.span.start().into(),
                    end: self.b.span.end().into(),
                },
            },
        )?;
        let f = bc::FunctionDef {
            module: self.module,
            name: self.package.natives[native.index()].symbol,
            kind: bc::FunctionKind::Native(native),
            signature,
            context: bc::Context::Ordinary,
            captures: Vec::new(),
            register_count: 0,
            bindings: Vec::new(),
            descriptors: Vec::new(),
            code: Vec::new(),
            regions: Vec::new(),
            source_map,
        };
        let function = bc::FunctionId(self.package.functions.len() as u32);
        self.b.push(&mut self.package.functions, f)?;
        self.native_values[native.index()] = Some(function);
        Ok(function)
    }
}

impl FunctionBuilder<'_, '_> {
    pub(super) fn native_reference(
        &mut self,
        native: bc::NativeId,
        node: h::NodeId,
    ) -> Result<bc::Reg, Box<Error>> {
        let function = self.l.native_value(native)?;
        let desc = self.descriptor(bc::Descriptor::Closure {
            function,
            captures: Vec::new(),
        })?;
        let output = self.temp()?;
        self.op(bc::Opcode::MakeClosure, output.0, desc.0, 0, node)?;
        Ok(output)
    }
}
