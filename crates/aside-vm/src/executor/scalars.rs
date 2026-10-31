//! Bounded scalar blocks borrow one register mapping and retain canonical exits.
use super::*;
use crate::frame_slots::RegisterWindow;
mod registers;
use registers::Registers;

pub(super) enum ScalarStep {
    Done,
    General,
    Preparation,
    NamedRead,
    Budget,
}

enum Exit {
    Done,
    General,
    Budget,
    Fault(FaultKind),
}

#[derive(Clone, Copy)]
struct Cursor {
    function: FunctionId,
    pc: Pc,
    instruction_end: Pc,
    member_symbol: Option<SymbolId>,
    instruction: Option<Instruction>,
    instruction_pc: Pc,
}

impl Cursor {
    fn capture(frame: &ScriptFrame) -> Self {
        Self {
            function: frame.code.function,
            pc: frame.pc,
            instruction_end: frame.instruction_end,
            member_symbol: frame.member_symbol,
            instruction: frame.instruction,
            instruction_pc: frame.instruction_pc,
        }
    }
    fn publish(self, frame: &mut ScriptFrame) {
        frame.pc = self.pc;
        frame.instruction_end = self.instruction_end;
        frame.member_symbol = self.member_symbol;
        frame.instruction = self.instruction;
        frame.instruction_pc = self.instruction_pc;
    }
    fn advance(&mut self, next: Pc) {
        self.pc = next;
        self.instruction = None;
        self.member_symbol = None;
    }
}

struct Block<'a> {
    program: &'a VerifiedProgram,
    code_window: &'a mut crate::bytecode::CodeWindow,
    registers: Registers<'a>,
    cursor: Cursor,
    #[cfg(feature = "profiling")]
    profile: &'a step_costs::State,
    #[cfg(feature = "execution-counts")]
    execution_counts: &'a execution_counts::State,
    #[cfg(test)]
    single: bool,
    #[cfg(test)]
    general_clears: bool,
}

impl Vm {
    pub(super) fn scalar_step(&mut self, meter: &mut Meter) -> Result<ScalarStep, VmError> {
        #[cfg(feature = "profiling")]
        let driver = self.step_costs.start_driver(step_costs::Phase::Scalar);
        let result = self.scalar_block(meter);
        #[cfg(feature = "profiling")]
        self.step_costs.finish_driver(driver);
        result
    }

    fn scalar_block(&mut self, meter: &mut Meter) -> Result<ScalarStep, VmError> {
        let Some(Frame::Script(frame)) = self.active.frames.data.last() else {
            return step_cost!(
                self.step_costs,
                Scalar,
                self.step_profile_point(),
                Ok(ScalarStep::General)
            );
        };
        if !matches!(frame.phase, ScriptPhase::Body) {
            let preparation = script_preparation::is_preparation(&frame.phase);
            #[cfg(test)]
            let preparation = preparation && !self.work_probe.general_script_preparation;
            let named = !preparation && named_reads::eligible(frame, meter);
            #[cfg(test)]
            let named = named && !self.work_probe.single_named_read_step;
            return step_cost!(
                self.step_costs,
                Scalar,
                self.step_profile_point(),
                Ok(if preparation {
                    ScalarStep::Preparation
                } else if named {
                    ScalarStep::NamedRead
                } else {
                    ScalarStep::General
                })
            );
        }
        let cursor = Cursor::capture(frame);
        let range = frame.regs;
        #[cfg(test)]
        let registers = if self.work_probe.single_scalar_step {
            Registers::Checked {
                slots: &mut self.slots,
                heap: &mut self.heap,
                range,
            }
        } else {
            let Ok(registers) = self.slots.registers(&mut self.heap, range) else {
                return Ok(ScalarStep::General);
            };
            Registers::Window(registers)
        };
        #[cfg(not(test))]
        let registers = {
            let Ok(registers) = self.slots.registers(&mut self.heap, range) else {
                return Ok(ScalarStep::General);
            };
            Registers::Window(registers)
        };
        let mut block = Block {
            program: &self.program,
            code_window: &mut self.code_window,
            registers,
            cursor,
            #[cfg(feature = "profiling")]
            profile: &self.step_costs,
            #[cfg(feature = "execution-counts")]
            execution_counts: &self.execution_counts,
            #[cfg(test)]
            single: self.work_probe.single_scalar_step,
            #[cfg(test)]
            general_clears: self.work_probe.general_clears,
        };
        let result = block.run(meter);
        let cursor = block.cursor;
        // This last use ends the pool/heap borrow before fault publication.
        // Every value has already reached its ordinary rooted register.
        let Some(Frame::Script(frame)) = self.active.frames.data.last_mut() else {
            unreachable!("scalar blocks retain the current frame")
        };
        cursor.publish(frame);
        Ok(match result? {
            Exit::Done => ScalarStep::Done,
            Exit::General => ScalarStep::General,
            Exit::Budget => ScalarStep::Budget,
            Exit::Fault(kind) => {
                let Frame::Script(mut frame) = *self.active.frames.data.last().unwrap() else {
                    unreachable!()
                };
                self.collection_fault(&mut frame, kind)?;
                let top = self.active.frames.data.len() - 1;
                self.publish_script_frame(top, &frame);
                ScalarStep::Done
            }
        })
    }
}

impl Block<'_> {
    fn run(&mut self, meter: &mut Meter) -> Result<Exit, VmError> {
        let limit = 32;
        #[cfg(test)]
        let limit = if self.single { 1 } else { limit };
        for step in 0..limit {
            // The outer loop checked the first step. Stop before a required
            // clock query; every opcode retains its individual admission.
            if step != 0 && !meter.next_short_step() {
                break;
            }
            match step_cost!(self.profile, Scalar, self.point(), self.step(meter))? {
                Exit::Done => {}
                exit => return Ok(exit),
            }
        }
        Ok(Exit::Done)
    }

    #[cfg(feature = "profiling")]
    fn point(&self) -> Option<step_costs::StepPoint> {
        Some(step_costs::StepPoint {
            function: self.cursor.function.0,
            pc: self.cursor.pc.0,
            native: None,
            mode: 0,
        })
    }

    fn step(&mut self, meter: &mut Meter) -> Result<Exit, VmError> {
        if !meter.admits(0, 1, 0) {
            return Ok(Exit::Budget);
        }
        let cached = self
            .cursor
            .instruction
            .filter(|_| self.cursor.instruction_pc == self.cursor.pc);
        let (instruction, next) = if let Some(instruction) = cached {
            (instruction, self.cursor.instruction_end)
        } else {
            // Lazy code allocation belongs to CodeStore; it cannot run Heap GC
            // or relocate this borrowed register window.
            let Some(decoded) = step_cost!(
                self.profile,
                Fetch,
                self.point(),
                self.program.package().functions[self.cursor.function.index()]
                    .code
                    .fetch_budgeted(
                        self.cursor.pc.index(),
                        self.code_window,
                        &mut |work, allocation| meter.take(0, work, allocation),
                    )
            )?
            else {
                return Ok(Exit::Budget);
            };
            self.cursor.instruction = Some(decoded.0);
            self.cursor.instruction_pc = self.cursor.pc;
            self.cursor.instruction_end = Pc(decoded.1 as u32);
            (decoded.0, Pc(decoded.1 as u32))
        };
        let op = instruction_spec(instruction.opcode).unwrap().opcode;
        let [a, b, c] = instruction.operands;
        if matches!(op, Opcode::Clear | Opcode::Clear2 | Opcode::Clear3) {
            #[cfg(test)]
            if self.general_clears {
                return Ok(Exit::General);
            }
            let work = match op {
                Opcode::Clear => 1,
                Opcode::Clear2 => 2,
                _ => 3,
            };
            if !meter.take(1, work, 0) {
                return Ok(Exit::Budget);
            }
            instruction_admitted!(self, op);
            execution_count!(self, BytecodeExecutions, 1);
            let result = match op {
                Opcode::Clear => self.registers.clear([a]),
                Opcode::Clear2 => self.registers.clear([a, b]),
                _ => self.registers.clear([a, b, c]),
            };
            if let Err(error) = result {
                return Ok(Exit::Fault(heap_fault(error)));
            }
        } else if matches!(op, Opcode::Jump | Opcode::BranchBool | Opcode::BranchNull) {
            #[cfg(test)]
            if self.single {
                return Ok(Exit::General);
            }
            if !meter.admits(1, 1, 0) {
                return Ok(Exit::Budget);
            }
            let Ok(target) = branch_target(op, [a, b, c], |r| {
                self.registers.read(r).map_err(heap_fault)
            }) else {
                return Ok(Exit::General);
            };
            assert!(meter.take(1, 1, 0));
            instruction_admitted!(self, op);
            execution_count!(self, BytecodeExecutions, 1);
            self.cursor.advance(target);
            return Ok(Exit::Done);
        } else {
            if !supported(op) {
                return Ok(Exit::General);
            }
            if !meter.admits(1, 1, 0) {
                return Ok(Exit::Budget);
            }
            // Invalid values retain the ordinary source-located fault path.
            let Ok(Some(value)) = self.value(op, b, c) else {
                return Ok(Exit::General);
            };
            assert!(meter.take(1, 1, 0));
            instruction_admitted!(self, op);
            execution_count!(self, BytecodeExecutions, 1);
            if let Err(error) = self.registers.write(a, value) {
                return Ok(Exit::Fault(heap_fault(error)));
            }
        }
        self.cursor.advance(next);
        Ok(Exit::Done)
    }

    fn value(&self, op: Opcode, b: u32, c: u32) -> Result<Option<Value>, FaultKind> {
        Ok(Some(match op {
            Opcode::LoadConst => match self.program.package().constants[b as usize] {
                Const::Null => Value::NULL,
                Const::Bool(v) => Value::bool(v),
                Const::Int(v) => Value::int(v),
                Const::FloatBits(v) => Value::float(f64::from_bits(v)),
                _ => return Ok(None),
            },
            Opcode::Move => self.registers.read(b).map_err(heap_fault)?,
            Opcode::BindRead => {
                let storage = self.program.package().functions[self.cursor.function.index()]
                    .bindings[b as usize]
                    .storage;
                let value = self.registers.read(storage.reg().0).map_err(heap_fault)?;
                if matches!(storage, BindingStorage::Cell(_)) {
                    self.registers.cell_read(value).map_err(heap_fault)?
                } else {
                    value
                }
            }
            Opcode::CellRead => self
                .registers
                .cell_read(self.registers.read(b).map_err(heap_fault)?)
                .map_err(heap_fault)?,
            Opcode::Neg | Opcode::Pos | Opcode::Not | Opcode::BitNot => {
                unary(op, self.registers.read(b).map_err(heap_fault)?)?
            }
            _ => {
                let (left, right) = self.registers.read_pair(b, c).map_err(heap_fault)?;
                if left.tag() == ValueTag::Object || right.tag() == ValueTag::Object {
                    return Ok(None);
                }
                binary(op, left, right)?
            }
        }))
    }
}

fn supported(op: Opcode) -> bool {
    matches!(
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
    )
}

pub(super) fn branch_target(
    op: Opcode,
    [a, b, c]: [u32; 3],
    read: impl FnOnce(u32) -> Result<Value, FaultKind>,
) -> Result<Pc, FaultKind> {
    Ok(Pc(match op {
        Opcode::Jump => a,
        Opcode::BranchBool => {
            if read(a)?.as_bool().ok_or(FaultKind::Type)? {
                b
            } else {
                c
            }
        }
        Opcode::BranchNull => {
            if read(a)?.tag() == ValueTag::Null {
                b
            } else {
                c
            }
        }
        _ => unreachable!("only control-flow instructions use branch_target"),
    }))
}

#[cfg(test)]
mod tests;
