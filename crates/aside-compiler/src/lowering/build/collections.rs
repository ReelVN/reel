//! Ordered collection construction. Spreads capture before later expressions;
//! table key validation waits until every expression has been evaluated.
use super::*;

impl FunctionBuilder<'_, '_> {
    pub(super) fn sequence(
        &mut self,
        n: h::NodeId,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        self.l.b.work(self.l.m.hir().elements(n).len() as u64)?;
        let count = self.l.m.hir().children(n).count();
        self.l.b.work(count as u64)?;
        for child in self.l.m.hir().children(n) {
            if self.l.m.hir().node(child).kind == H::Spread {
                return self.spread_array(n, depth);
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
        self.make_sequence(n, out, descriptor)?;
        self.release_window(window, n)?;
        Ok(Some(out))
    }

    pub(super) fn table(
        &mut self,
        n: h::NodeId,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        self.l.b.work(self.l.m.hir().elements(n).len() as u64)?;
        let count = self.l.m.hir().children(n).count();
        self.l.b.work(count as u64)?;
        let spread = self
            .l
            .m
            .hir()
            .children(n)
            .any(|field| self.l.m.hir().node(field).kind == H::Spread);
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
        let mut sources = Vec::new();
        if spread {
            self.l.b.reserve(&mut sources, count)?;
        } else {
            self.l.b.reserve(&mut pairs, count)?;
        }
        self.l.b.work(count as u64)?;
        for (offset, field) in self.l.m.hir().children(n).enumerate() {
            let key_slot = bc::Reg(window.start.0 + offset as u32 * 2);
            let value_slot = bc::Reg(key_slot.0 + 1);
            if let Form::Spread { value } = self.l.m.hir().node(field).form {
                let Some(source) = self.value(value, depth)? else {
                    self.l.b.release(sources);
                    self.l.b.release(pairs);
                    self.release_window(window, n)?;
                    return Ok(None);
                };
                let empty = self.descriptor(bc::Descriptor::Table(Vec::new()))?;
                self.op(bc::Opcode::MakeTable, key_slot.0, empty.0, 0, field)?;
                self.op(bc::Opcode::TableSpread, key_slot.0, source.0, 0, field)?;
                self.release(source)?;
                self.l
                    .b
                    .push(&mut sources, bc::TableSource::Spread(key_slot))?;
                continue;
            }
            let Form::Field { key, value } = self.l.m.hir().node(field).form else {
                unreachable!("table entry");
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
                self.l.b.release(sources);
                self.l.b.release(pairs);
                self.release_window(window, n)?;
                return Ok(None);
            };
            self.op(bc::Opcode::Move, key_slot.0, key.0, 0, field)?;
            self.release(key)?;
            let Some(value) = self.value(value, depth)? else {
                self.l.b.release(sources);
                self.l.b.release(pairs);
                self.release_window(window, n)?;
                return Ok(None);
            };
            self.op(bc::Opcode::Move, value_slot.0, value.0, 0, field)?;
            self.release(value)?;
            if spread {
                self.l
                    .b
                    .push(&mut sources, bc::TableSource::Pair([key_slot, value_slot]))?;
            } else {
                self.l.b.push(&mut pairs, [key_slot, value_slot])?;
            }
        }
        // No key validation/IndexSet is emitted here. Construction (including
        // hash errors) starts only after every key and value has been evaluated.
        self.l.b.span = self.span(n);
        let descriptor = if spread {
            self.descriptor(bc::Descriptor::TableSources(sources))?
        } else {
            self.descriptor(bc::Descriptor::Table(pairs))?
        };
        let out = self.temp()?;
        self.op(bc::Opcode::MakeTable, out.0, descriptor.0, 0, n)?;
        self.release_window(window, n)?;
        Ok(Some(out))
    }

    fn spread_array(&mut self, n: h::NodeId, depth: usize) -> Result<Option<bc::Reg>, Box<Error>> {
        let out = self.temp()?;
        let empty = self.descriptor(bc::Descriptor::Sequence(bc::RegWindow {
            start: out,
            len: 0,
        }))?;
        self.make_sequence(n, out, empty)?;
        for child in self.l.m.hir().children(n) {
            let (input, opcode) = match self.l.m.hir().node(child).form {
                Form::Spread { value } => (value, bc::Opcode::ArraySpread),
                _ => (child, bc::Opcode::ArrayPush),
            };
            let Some(value) = self.value(input, depth)? else {
                self.release(out)?;
                return Ok(None);
            };
            self.op(opcode, out.0, value.0, 0, child)?;
            self.release(value)?;
        }
        Ok(Some(out))
    }

    fn make_sequence(
        &mut self,
        node: h::NodeId,
        out: bc::Reg,
        descriptor: bc::DescriptorId,
    ) -> Result<(), Box<Error>> {
        let ty = self.l.m.facts()[node.0 as usize].ty;
        let (opcode, item) = if self.l.m.hir().node(node).kind == H::Tuple {
            (bc::Opcode::MakeTuple, 0)
        } else if let h::Type::TypedArray(item) = self.l.m.types()[ty.0 as usize] {
            (bc::Opcode::MakeTypedArray, self.l.ty(item, 0)?.0)
        } else {
            (bc::Opcode::MakeArray, 0)
        };
        self.op(opcode, out.0, descriptor.0, item, node)
    }
}
