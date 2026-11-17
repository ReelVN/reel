//! Bounded scalar blocks borrow one register mapping and retain canonical exits.
use super::*;
use crate::frame_slots::EnvironmentWindow;
use crate::frame_slots::RegisterWindow;
mod registers;
#[cfg(any(test, feature = "vm-validation-oracle"))]
mod residual;
#[cfg(any(test, feature = "vm-validation-oracle"))]
pub(super) use residual::State as ResidualState;
#[cfg(any(test, feature = "vm-validation-oracle"))]
mod suffixes;
mod synchronous;
use registers::Registers;

#[derive(Clone, Copy)]
pub(super) enum ScalarStep {
    Done,
    General,
    Body(Opcode),
    Preparation,
    NamedRead,
    Budget,
}

enum Exit {
    Done,
    General(Opcode),
    Unavailable,
    Budget,
    Fault(FaultKind),
}

/// A fetched instruction has not consumed its opcode's instruction/work grant.
/// The cursor retains its original PC until scalar or general execution commits.
#[derive(Clone, Copy)]
pub(super) struct BodyInstruction {
    pub(super) instruction: Instruction,
    pub(super) opcode: Opcode,
    pub(super) next: Pc,
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
    #[inline]
    fn fetch(
        &mut self,
        program: &VerifiedProgram,
        window: &mut crate::bytecode::CodeWindow,
        meter: &mut Meter,
        #[cfg(feature = "profiling")] profile: &step_costs::State,
    ) -> Result<Option<BodyInstruction>, VmError> {
        if !meter.admits(0, 1, 0) {
            return Ok(None);
        }
        let cached = self.instruction.filter(|_| self.instruction_pc == self.pc);
        let (instruction, next) = if let Some(instruction) = cached {
            (instruction, self.instruction_end)
        } else {
            // CodeStore allocation cannot collect Heap roots or relocate slots.
            let Some(decoded) = step_cost!(
                profile,
                Fetch,
                self.point(),
                program.package().functions[self.function.index()]
                    .code
                    .fetch_budgeted(self.pc.index(), window, &mut |work, allocation| meter
                        .take(0, work, allocation),)
            )?
            else {
                return Ok(None);
            };
            self.instruction = Some(decoded.0);
            self.instruction_pc = self.pc;
            self.instruction_end = Pc(decoded.1 as u32);
            (decoded.0, self.instruction_end)
        };
        Ok(Some(BodyInstruction {
            instruction,
            opcode: instruction_spec(instruction.opcode)
                .expect("verified spec")
                .opcode,
            next,
        }))
    }
    #[cfg(feature = "profiling")]
    fn point(&self) -> Option<step_costs::StepPoint> {
        Some(step_costs::StepPoint {
            function: self.function.0,
            pc: self.pc.0,
            native: None,
            mode: 0,
        })
    }
}

struct Block<'a> {
    program: &'a VerifiedProgram,
    code_window: &'a mut crate::bytecode::CodeWindow,
    registers: Registers<'a>,
    cursor: &'a mut Cursor,
    environment: Option<EnvironmentWindow<'a>>,
    header: Header,
    types: &'a crate::type_checks::TypeChecks,
    class_hints: &'a member_hints::MemberHints<{ member_hints::CLASS_SLOTS }>,
    #[cfg(test)]
    work_probe: &'a WorkProbe,
    #[cfg(feature = "profiling")]
    profile: &'a step_costs::State,
    #[cfg(feature = "execution-counts")]
    execution_counts: &'a execution_counts::State,
    #[cfg(test)]
    single: bool,
    #[cfg(test)]
    general_clears: bool,
    #[cfg(test)]
    legacy_suffixes: bool,
    #[cfg(any(test, feature = "vm-validation-oracle"))]
    residual: Option<&'a ResidualState>,
}

#[derive(Clone, Copy)]
struct Header {
    context: ContextFlags,
    effects: Effects,
    closure: bool,
}
impl Header {
    fn capture(frame: &ScriptFrame) -> Self {
        Self {
            context: frame.context,
            effects: frame.effects,
            closure: frame.closure,
        }
    }
}

impl Vm {
    #[inline]
    pub(super) fn body_handoff_enabled(&self) -> bool {
        #[cfg(test)]
        if self.legacy_body_dispatch {
            return false;
        }
        #[cfg(test)]
        if self.work_probe.single_scalar_step || self.work_probe.legacy_script_publication {
            return false;
        }
        true
    }
    fn body_context_enabled(&self) -> bool {
        #[cfg(test)]
        if self.legacy_body_context {
            return false;
        }
        #[cfg(test)]
        if self.work_probe.general_class_reads || self.work_probe.checked_class_metadata {
            return false;
        }
        self.body_handoff_enabled()
    }
    #[cfg(test)]
    pub(super) fn scalar_step(&mut self, meter: &mut Meter) -> Result<ScalarStep, VmError> {
        self.scalar_step_inner(meter, None)
    }

    pub(super) fn scalar_step_clocked(
        &mut self,
        meter: &mut Meter,
        now: &mut dyn FnMut() -> u64,
    ) -> Result<ScalarStep, VmError> {
        self.scalar_step_inner(meter, Some(now))
    }

    fn scalar_step_inner(
        &mut self,
        meter: &mut Meter,
        now: Option<&mut dyn FnMut() -> u64>,
    ) -> Result<ScalarStep, VmError> {
        #[cfg(feature = "profiling")]
        let driver = self.step_costs.start_driver(step_costs::Phase::Scalar);
        let result = self.scalar_block(meter, now);
        #[cfg(feature = "profiling")]
        self.step_costs.finish_driver(driver);
        result
    }

    fn scalar_block(
        &mut self,
        meter: &mut Meter,
        now: Option<&mut dyn FnMut() -> u64>,
    ) -> Result<ScalarStep, VmError> {
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
        let mut cursor = Cursor::capture(frame);
        let result = self.scalar_body(&mut cursor, None, meter, now);
        let Some(Frame::Script(frame)) = self.active.frames.data.last_mut() else {
            unreachable!("scalar blocks retain the current frame")
        };
        cursor.publish(frame);
        self.finish_scalar(result?, None)
    }

    /// Body32 already owns this frame. Keep its cursor through scalar/general
    /// handoff while publishing the same canonical cursor before leaving a block.
    pub(super) fn scalar_working_step(
        &mut self,
        working: &mut ScriptFrame,
        meter: &mut Meter,
        now: &mut dyn FnMut() -> u64,
    ) -> Result<ScalarStep, VmError> {
        #[cfg(feature = "profiling")]
        let driver = self.step_costs.start_driver(step_costs::Phase::Scalar);
        let mut cursor = Cursor::capture(working);
        let result = self.scalar_body(&mut cursor, Some(working), meter, Some(now));
        cursor.publish(working);
        let Some(Frame::Script(stored)) = self.active.frames.data.last_mut() else {
            unreachable!("scalar blocks retain the Body frame")
        };
        cursor.publish(stored);
        let result = result.and_then(|exit| self.finish_scalar(exit, Some(working)));
        #[cfg(feature = "profiling")]
        self.step_costs.finish_driver(driver);
        result
    }

    fn scalar_body(
        &mut self,
        cursor: &mut Cursor,
        mut working: Option<&mut ScriptFrame>,
        meter: &mut Meter,
        mut now: Option<&mut dyn FnMut() -> u64>,
    ) -> Result<Exit, VmError> {
        #[cfg(test)]
        if self.legacy_body_clock {
            now = None;
        }
        let frame = working.as_deref().unwrap_or_else(|| {
            let Some(Frame::Script(frame)) = self.active.frames.data.last() else {
                unreachable!("Body execution retains its current frame")
            };
            frame
        });
        let synchronous = self.body_context_enabled() && frame.table_work.is_none();
        let first = if self.body_handoff_enabled() {
            let fetched = cursor.fetch(
                &self.program,
                &mut self.code_window,
                meter,
                #[cfg(feature = "profiling")]
                &self.step_costs,
            )?;
            let Some(instruction) = fetched else {
                return Ok(Exit::Budget);
            };
            if !scalar_opcode(instruction.opcode)
                && !(synchronous && synchronous_opcode(instruction.opcode))
            {
                return Ok(Exit::General(instruction.opcode));
            }
            Some(instruction)
        } else {
            None
        };
        // A fetched handoff needs only the cursor. Capture root/context fields
        // after classification, when an instruction will actually use a block.
        let header = Header::capture(frame);
        let range = frame.regs;
        let environment_range = frame.env;
        #[cfg(test)]
        self.work_probe
            .scalar_windows
            .set(self.work_probe.scalar_windows.get() + 1);
        #[cfg(test)]
        let (registers, environment) = if self.work_probe.single_scalar_step {
            (
                Registers::Checked {
                    slots: &mut self.slots,
                    heap: &mut self.heap,
                    range,
                },
                None,
            )
        } else if synchronous && header.closure {
            let Ok((registers, environment)) =
                self.slots
                    .body_registers_vm(&mut self.heap, range, environment_range)
            else {
                return Ok(first.map_or(Exit::Unavailable, |instruction| {
                    Exit::General(instruction.opcode)
                }));
            };
            (Registers::Window(registers), Some(environment))
        } else {
            let Ok(registers) = self.slots.registers_vm(&mut self.heap, range) else {
                return Ok(first.map_or(Exit::Unavailable, |instruction| {
                    Exit::General(instruction.opcode)
                }));
            };
            (
                Registers::Window(registers),
                synchronous.then(EnvironmentWindow::empty),
            )
        };
        #[cfg(not(test))]
        let (registers, environment) = if synchronous && header.closure {
            let Ok((registers, environment)) =
                self.slots
                    .body_registers_vm(&mut self.heap, range, environment_range)
            else {
                return Ok(first.map_or(Exit::Unavailable, |instruction| {
                    Exit::General(instruction.opcode)
                }));
            };
            (Registers::Window(registers), Some(environment))
        } else {
            let Ok(registers) = self.slots.registers_vm(&mut self.heap, range) else {
                return Ok(first.map_or(Exit::Unavailable, |instruction| {
                    Exit::General(instruction.opcode)
                }));
            };
            (
                Registers::Window(registers),
                synchronous.then(EnvironmentWindow::empty),
            )
        };
        let mut block = Block {
            program: &self.program,
            code_window: &mut self.code_window,
            registers,
            cursor,
            environment,
            header,
            types: &self.type_checks,
            class_hints: &self.class_member_hints,
            #[cfg(test)]
            work_probe: &self.work_probe,
            #[cfg(feature = "profiling")]
            profile: &self.step_costs,
            #[cfg(feature = "execution-counts")]
            execution_counts: &self.execution_counts,
            #[cfg(test)]
            single: self.work_probe.single_scalar_step,
            #[cfg(test)]
            general_clears: self.work_probe.general_clears,
            #[cfg(test)]
            legacy_suffixes: self.legacy_scalar_suffixes,
            #[cfg(any(test, feature = "vm-validation-oracle"))]
            residual: Some(&self.residual_body),
        };
        let Some(Frame::Script(stored)) = self.active.frames.data.last_mut() else {
            unreachable!("a borrowed Body window retains its frame")
        };
        let mut check_clock = |cursor: &Cursor, meter: &mut Meter| {
            let now = now.as_deref_mut()?;
            // The exclusive Vm borrow excludes collection, relocation and
            // reentrant mutation during now(). Publish before this host call,
            // including when it unwinds; root values already use their barrier.
            cursor.publish(stored);
            if let Some(working) = working.as_deref_mut() {
                cursor.publish(working);
            }
            Some(meter.deadline_reached(now))
        };
        block.run(meter, first, &mut check_clock)
    }

    fn finish_scalar(
        &mut self,
        result: Exit,
        working: Option<&mut ScriptFrame>,
    ) -> Result<ScalarStep, VmError> {
        Ok(match result {
            Exit::Done => ScalarStep::Done,
            Exit::Unavailable => ScalarStep::General,
            Exit::General(opcode) => {
                if self.body_handoff_enabled() {
                    #[cfg(test)]
                    self.work_probe
                        .body_handoffs
                        .set(self.work_probe.body_handoffs.get() + 1);
                    ScalarStep::Body(opcode)
                } else {
                    ScalarStep::General
                }
            }
            Exit::Budget => ScalarStep::Budget,
            Exit::Fault(kind) => {
                if let Some(working) = working {
                    self.collection_fault(working, kind)?;
                    let top = self.active.frames.data.len() - 1;
                    self.publish_script_frame(top, working);
                    return Ok(ScalarStep::Done);
                }
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
    #[inline(always)]
    fn run(
        &mut self,
        meter: &mut Meter,
        mut first: Option<BodyInstruction>,
        check_clock: &mut dyn FnMut(&Cursor, &mut Meter) -> Option<bool>,
    ) -> Result<Exit, VmError> {
        #[cfg(any(test, feature = "vm-validation-oracle"))]
        self.residual_begin();
        let result = self.run_inner(meter, first.take(), check_clock);
        #[cfg(any(test, feature = "vm-validation-oracle"))]
        self.residual_finish(matches!(result, Ok(Exit::Done | Exit::General(_))));
        result
    }

    #[inline(always)]
    fn run_inner(
        &mut self,
        meter: &mut Meter,
        mut first: Option<BodyInstruction>,
        check_clock: &mut dyn FnMut(&Cursor, &mut Meter) -> Option<bool>,
    ) -> Result<Exit, VmError> {
        let limit = 32;
        #[cfg(test)]
        let limit = if self.single { 1 } else { limit };
        let mut step = 0;
        while step < limit {
            // The outer loop checked the first step. Keep exactly the same
            // clock boundaries without rebuilding a still-current root window.
            if step != 0 && !meter.next_short_step() {
                match check_clock(self.cursor, meter) {
                    Some(false) => {}
                    Some(true) => return Ok(Exit::Budget),
                    None => break,
                }
            }
            step += 1;
            #[cfg(any(test, feature = "vm-validation-oracle"))]
            let remaining = limit - step;
            match step_cost!(
                self.profile,
                Scalar,
                self.point(),
                self.step(
                    meter,
                    first.take(),
                    #[cfg(any(test, feature = "vm-validation-oracle"))]
                    remaining,
                    #[cfg(any(test, feature = "vm-validation-oracle"))]
                    &mut step,
                )
            )? {
                Exit::Done => {}
                exit => return Ok(exit),
            }
        }
        Ok(Exit::Done)
    }

    #[cfg(feature = "profiling")]
    fn point(&self) -> Option<step_costs::StepPoint> {
        self.cursor.point()
    }

    fn step(
        &mut self,
        meter: &mut Meter,
        first: Option<BodyInstruction>,
        #[cfg(any(test, feature = "vm-validation-oracle"))] remaining: u8,
        #[cfg(any(test, feature = "vm-validation-oracle"))] step: &mut u8,
    ) -> Result<Exit, VmError> {
        let decoded = if let Some(first) = first {
            first
        } else {
            let Some(instruction) = self.cursor.fetch(
                self.program,
                self.code_window,
                meter,
                #[cfg(feature = "profiling")]
                self.profile,
            )?
            else {
                return Ok(Exit::Budget);
            };
            instruction
        };
        let BodyInstruction {
            instruction,
            opcode: op,
            next,
        } = decoded;
        #[cfg(any(test, feature = "vm-validation-oracle"))]
        if let Some(exit) = self.residual_step(meter, decoded, remaining, step) {
            return Ok(exit);
        }
        let [a, b, c] = instruction.operands;
        if self.environment.is_some() && synchronous_opcode(op) {
            return self.synchronous_step(meter, decoded);
        }
        if matches!(op, Opcode::Clear | Opcode::Clear2 | Opcode::Clear3) {
            #[cfg(test)]
            if self.general_clears {
                return Ok(Exit::General(op));
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
                return Ok(Exit::General(op));
            }
            if !meter.admits(1, 1, 0) {
                return Ok(Exit::Budget);
            }
            let Ok(target) = branch_target(op, [a, b, c], |r| {
                self.registers.read(r).map_err(heap_fault)
            }) else {
                return Ok(Exit::General(op));
            };
            assert!(meter.take(1, 1, 0));
            instruction_admitted!(self, op);
            execution_count!(self, BytecodeExecutions, 1);
            self.cursor.advance(target);
            return Ok(Exit::Done);
        } else {
            if !supported(op) {
                return Ok(Exit::General(op));
            }
            if !meter.admits(1, 1, 0) {
                return Ok(Exit::Budget);
            }
            // Invalid values retain the ordinary source-located fault path.
            let Ok(Some(value)) = self.value(op, b, c) else {
                return Ok(Exit::General(op));
            };
            #[cfg(any(test, feature = "vm-validation-oracle"))]
            if let Some(exit) = self.value_suffix(meter, op, value, remaining, step) {
                return Ok(exit);
            }
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

fn scalar_opcode(op: Opcode) -> bool {
    supported(op)
        || matches!(
            op,
            Opcode::Clear
                | Opcode::Clear2
                | Opcode::Clear3
                | Opcode::Jump
                | Opcode::BranchBool
                | Opcode::BranchNull
        )
}

fn synchronous_opcode(op: Opcode) -> bool {
    matches!(
        op,
        Opcode::BindInit
            | Opcode::BindWrite
            | Opcode::CaptureRead
            | Opcode::Eq
            | Opcode::Ne
            | Opcode::MemberGet
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
