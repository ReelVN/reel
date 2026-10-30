//! Scalar instructions use the same value operations without copying cold frame state.
use super::*;

pub(super) enum ScalarStep {
    Done,
    General,
    Budget,
}

impl Vm {
    pub(super) fn scalar_step(&mut self, meter: &mut Meter) -> Result<ScalarStep, VmError> {
        let Some(Frame::Script(s)) = self.active.frames.data.last() else {
            return Ok(ScalarStep::General);
        };
        if !matches!(s.phase, ScriptPhase::Body) {
            return Ok(ScalarStep::General);
        }
        if !meter.admits(0, 1, 0) {
            return Ok(ScalarStep::Budget);
        }
        let (function, pc, regs) = (s.code.function, s.pc, s.regs);
        let cached = s.instruction.filter(|_| s.instruction_pc == pc);
        let (instruction, next) = if let Some(instruction) = cached {
            (instruction, s.instruction_end.index())
        } else {
            let Some(decoded) = step_cost!(
                self,
                Fetch,
                self.program.package().functions[function.index()]
                    .code
                    .fetch_budgeted(
                        pc.index(),
                        &mut self.code_window,
                        &mut |work, allocation| meter.take(0, work, allocation),
                    )
            )?
            else {
                return Ok(ScalarStep::Budget);
            };
            let Frame::Script(frame) = self.active.frames.data.last_mut().unwrap() else {
                unreachable!()
            };
            frame.instruction = Some(decoded.0);
            frame.instruction_pc = pc;
            frame.instruction_end = Pc(decoded.1 as u32);
            decoded
        };
        let op = instruction_spec(instruction.opcode).unwrap().opcode;
        let [a, b, c] = instruction.operands;
        if matches!(op, Opcode::Clear | Opcode::Clear2 | Opcode::Clear3) {
            #[cfg(test)]
            if self.work_probe.general_clears {
                return Ok(ScalarStep::General);
            }
            return self.scalar_clear(regs, op, [a, b, c], next, meter);
        }
        if !matches!(
            op,
            Opcode::LoadConst
                | Opcode::Move
                | Opcode::BindRead
                | Opcode::CellRead
                | Opcode::Neg
                | Opcode::Pos
                | Opcode::Not
                | Opcode::BitNot
                | Opcode::Add
                | Opcode::Sub
                | Opcode::Mul
                | Opcode::Div
                | Opcode::Rem
                | Opcode::BitAnd
                | Opcode::BitOr
                | Opcode::BitXor
                | Opcode::Shl
                | Opcode::Shr
                | Opcode::Lt
                | Opcode::Le
                | Opcode::Gt
                | Opcode::Ge
        ) {
            return Ok(ScalarStep::General);
        }
        if !meter.admits(1, 1, 0) {
            return Ok(ScalarStep::Budget);
        }
        // Invalid operands retain the ordinary fault and source-location path.
        let Ok(Some(value)) = self.scalar_value(function, regs, op, b, c) else {
            return Ok(ScalarStep::General);
        };
        assert!(meter.take(1, 1, 0));
        if let Err(kind) = self.write(regs, Reg(a), value) {
            let Frame::Script(mut frame) = *self.active.frames.data.last().unwrap() else {
                unreachable!()
            };
            self.collection_fault(&mut frame, kind)?;
            *self.active.frames.data.last_mut().unwrap() = Frame::Script(frame);
            return Ok(ScalarStep::Done);
        }
        let Frame::Script(frame) = self.active.frames.data.last_mut().unwrap() else {
            unreachable!()
        };
        frame.pc = Pc(next as u32);
        frame.instruction = None;
        frame.member_symbol = None;
        Ok(ScalarStep::Done)
    }

    fn scalar_clear(
        &mut self,
        regs: RootRangeToken,
        op: Opcode,
        [a, b, c]: [u32; 3],
        next: usize,
        meter: &mut Meter,
    ) -> Result<ScalarStep, VmError> {
        let work = match op {
            Opcode::Clear => 1,
            Opcode::Clear2 => 2,
            _ => 3,
        };
        if !meter.take(1, work, 0) {
            return Ok(ScalarStep::Budget);
        }
        let result = match op {
            Opcode::Clear => self.slots.clear(&mut self.heap, at(regs, a)),
            Opcode::Clear2 => self.slots.clear_registers(&mut self.heap, regs, [a, b]),
            _ => self.slots.clear_registers(&mut self.heap, regs, [a, b, c]),
        };
        if let Err(error) = result {
            let Frame::Script(mut frame) = *self.active.frames.data.last().unwrap() else {
                unreachable!()
            };
            self.collection_fault(&mut frame, heap_fault(error))?;
            *self.active.frames.data.last_mut().unwrap() = Frame::Script(frame);
            return Ok(ScalarStep::Done);
        }
        let Frame::Script(frame) = self.active.frames.data.last_mut().unwrap() else {
            unreachable!()
        };
        frame.pc = Pc(next as u32);
        frame.instruction = None;
        frame.member_symbol = None;
        Ok(ScalarStep::Done)
    }

    fn scalar_value(
        &self,
        function: FunctionId,
        regs: RootRangeToken,
        op: Opcode,
        b: u32,
        c: u32,
    ) -> Result<Option<Value>, FaultKind> {
        Ok(Some(match op {
            Opcode::LoadConst => match self.program.package().constants[b as usize] {
                Const::Null => Value::NULL,
                Const::Bool(v) => Value::bool(v),
                Const::Int(v) => Value::int(v),
                Const::FloatBits(v) => Value::float(f64::from_bits(v)),
                _ => return Ok(None),
            },
            Opcode::Move => self.read(regs, Reg(b))?,
            Opcode::BindRead => {
                let storage =
                    self.program.package().functions[function.index()].bindings[b as usize].storage;
                let value = self.read(regs, storage.reg())?;
                if matches!(storage, BindingStorage::Cell(_)) {
                    self.heap.cell_read(value).map_err(heap_fault)?
                } else {
                    value
                }
            }
            Opcode::CellRead => self
                .heap
                .cell_read(self.read(regs, Reg(b))?)
                .map_err(heap_fault)?,
            Opcode::Neg | Opcode::Pos | Opcode::Not | Opcode::BitNot => {
                unary(op, self.read(regs, Reg(b))?)?
            }
            _ => {
                let (left, right) = self.read_pair(regs, b, c)?;
                if left.tag() == ValueTag::Object || right.tag() == ValueTag::Object {
                    return Ok(None);
                }
                binary(op, left, right)?
            }
        }))
    }
}
