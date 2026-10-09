//! Fixed collections use only the existing B01 ordered descriptors.
use super::*;

impl FunctionBuilder<'_, '_> {
    pub(super) fn sequence(
        &mut self,
        n: h::NodeId,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Error> {
        self.l.b.work(self.l.m.hir().elements(n).len() as u64)?;
        let count = self.l.m.hir().children(n).count();
        self.l.b.work(count as u64)?;
        for child in self.l.m.hir().children(n) {
            if self.l.m.hir().node(child).kind == H::Spread {
                self.l.b.span = self.span(child);
                return Err(self.l.b.error(
                    ErrorKind::Unsupported,
                    "collection spread awaits ordered builder lowering",
                ));
            }
        }
        self.l.b.span = self.span(n);
        let window = self.registers.get(
            u32::try_from(count).map_err(|_| {
                self.l
                    .b
                    .error(ErrorKind::Limit, "collection length overflow")
            })?,
            &mut self.l.b,
        )?;
        self.l.b.work(count as u64)?;
        for (offset, child) in self.l.m.hir().children(n).enumerate() {
            let Some(value) = self.value(child, depth)? else {
                self.release_window(window, n)?;
                return Ok(None);
            };
            self.op(
                bc::Opcode::Move,
                window.start.0 + offset as u32,
                value.0,
                0,
                child,
            )?;
            self.release(value)?;
        }
        self.l.b.span = self.span(n);
        let descriptor = self.descriptor(bc::Descriptor::Sequence(window))?;
        let out = self.temp()?;
        self.op(
            if self.l.m.hir().node(n).kind == H::Tuple {
                bc::Opcode::MakeTuple
            } else {
                bc::Opcode::MakeArray
            },
            out.0,
            descriptor.0,
            0,
            n,
        )?;
        self.release_window(window, n)?;
        Ok(Some(out))
    }

    pub(super) fn table(&mut self, n: h::NodeId, depth: usize) -> Result<Option<bc::Reg>, Error> {
        self.l.b.work(self.l.m.hir().elements(n).len() as u64)?;
        let count = self.l.m.hir().children(n).count();
        self.l.b.work(count as u64)?;
        for field in self.l.m.hir().children(n) {
            if !matches!(self.l.m.hir().node(field).form, Form::Field { .. }) {
                self.l.b.span = self.span(field);
                return Err(self.l.b.error(
                    ErrorKind::Unsupported,
                    "table spread awaits ordered builder lowering",
                ));
            }
        }
        self.l.b.span = self.span(n);
        let len = count
            .checked_mul(2)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| {
                self.l
                    .b
                    .error(ErrorKind::Limit, "table window length overflow")
            })?;
        let window = self.registers.get(len, &mut self.l.b)?;
        let mut pairs = Vec::new();
        self.l.b.reserve(&mut pairs, count)?;
        self.l.b.work(count as u64)?;
        for (offset, field) in self.l.m.hir().children(n).enumerate() {
            let Form::Field { key, value } = self.l.m.hir().node(field).form else {
                unreachable!("preflight fixed fields");
            };
            self.l.b.span = self.span(field);
            let key = match key {
                h::FieldKey::Name(token) => {
                    let text = self
                        .l
                        .b
                        .string(self.l.m.hir().name(token.name.expect("field name")))?;
                    Some(self.constant(bc::Const::Utf8(text), field)?)
                }
                h::FieldKey::Expression(key) => self.value(key, depth)?,
            };
            let Some(key) = key else {
                self.l.b.release(pairs);
                self.release_window(window, n)?;
                return Ok(None);
            };
            let key_slot = bc::Reg(window.start.0 + offset as u32 * 2);
            let value_slot = bc::Reg(key_slot.0 + 1);
            self.op(bc::Opcode::Move, key_slot.0, key.0, 0, field)?;
            self.release(key)?;
            let Some(value) = self.value(value, depth)? else {
                self.l.b.release(pairs);
                self.release_window(window, n)?;
                return Ok(None);
            };
            self.op(bc::Opcode::Move, value_slot.0, value.0, 0, field)?;
            self.release(value)?;
            self.l.b.push(&mut pairs, [key_slot, value_slot])?;
        }
        // No key validation/IndexSet is emitted here. Construction (including
        // hash errors) starts only after every key and value has been evaluated.
        self.l.b.span = self.span(n);
        let descriptor = self.descriptor(bc::Descriptor::Table(pairs))?;
        let out = self.temp()?;
        self.op(bc::Opcode::MakeTable, out.0, descriptor.0, 0, n)?;
        self.release_window(window, n)?;
        Ok(Some(out))
    }
}
