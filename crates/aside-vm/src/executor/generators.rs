//! Generators transfer ordinary frame windows to a traceable heap record.
//!
//! Each copy step preserves the initialized bitmap and charges its own work.
//! Suspended state owns no unconditional roots, and collecting it runs no code.
use super::*;

const STATE: u32 = 0;
const FUNCTION: u32 = 1;
const PC: u32 = 2;
const MODULE: u32 = 3;
const CLOSURE: u32 = 4;
const EFFECTS: u32 = 5;
const CONTEXT: u32 = 6;
const RESULT: u32 = 7;
const WINDOWS: u32 = 8;
const CAUGHT: u32 = 10;
const LOGICAL_ID: u32 = 11;
const HEADER: usize = 12;

const NEW: i64 = 0;
const SUSPENDED: i64 = 1;
const RUNNING: i64 = 2;
const CLOSING: i64 = 3;
const COMPLETE: i64 = 4;
const CLOSED: i64 = 5;

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(super) enum GeneratorAction {
    Pair(Reg),
    Advance { done: Reg, value: Reg },
    Close(Option<Reg>),
}

/// Snapshot admission checks one frame or stored bitmap pair per restore unit.
pub(super) struct GeneratorValidation {
    generator: Value,
    state: i64,
    function: FunctionId,
    owner: Option<ScriptFrame>,
    task: usize,
    frame: usize,
    scanning: bool,
    part: u32,
    index: u32,
}

impl GeneratorValidation {
    pub(super) fn new(vm: &Vm, generator: Value) -> Result<Self, crate::snapshot::SnapshotError> {
        use crate::snapshot::SnapshotError;
        let invalid = || SnapshotError::Invalid("generator header".into());
        if vm.heap.info(generator)?.len != HEADER as u32 {
            return Err(invalid());
        }
        let integer = |field| {
            vm.generator_integer(generator, field)
                .map_err(|_| invalid())
        };
        let state = integer(STATE)?;
        let function = FunctionId(u32::try_from(integer(FUNCTION)?).map_err(|_| invalid())?);
        let definition = vm
            .program
            .package()
            .functions
            .get(function.index())
            .ok_or_else(invalid)?;
        let effects = integer(EFFECTS)?;
        let signature = &vm.program.package().signatures[definition.signature.index()];
        if !(NEW..=CLOSED).contains(&state)
            || definition.kind != FunctionKind::Generator
            || integer(PC)? < 0
            || integer(PC)? as usize >= definition.code.end_pc()
            || !definition.code.is_boundary(integer(PC)? as usize)
            || integer(MODULE)? != i64::from(definition.module.0)
            || !(0..=1).contains(&integer(CLOSURE)?)
            || effects < 0
            || effects as u64 & !u64::from(signature.effects.0) != 0
            || !(0..=7).contains(&integer(CONTEXT)?)
            || integer(LOGICAL_ID)? as u64 >= vm.next_id
        {
            return Err(invalid());
        }
        Ok(Self {
            generator,
            state,
            function,
            owner: None,
            task: 0,
            frame: 0,
            scanning: matches!(state, NEW | RUNNING | CLOSING),
            part: 0,
            index: 0,
        })
    }

    pub(super) fn step(&mut self, vm: &Vm) -> Result<bool, crate::snapshot::SnapshotError> {
        use crate::snapshot::SnapshotError;
        let invalid = || SnapshotError::Invalid("generator suspended frame".into());
        if self.scanning {
            let task = if self.task == 0 {
                Some(&vm.active)
            } else {
                vm.tasks.data.get(self.task - 1)
            };
            if let Some(task) = task {
                if let Some(frame) = task.frames.data.get(self.frame) {
                    self.frame += 1;
                    if let Frame::Script(frame) = frame
                        && frame.code.function == self.function
                        && vm
                            .slots
                            .read(at(frame.env, vm.generator_slot(*frame)))
                            .is_ok_and(|value| value.same_bits(self.generator))
                    {
                        if self.owner.replace(*frame).is_some() {
                            return Err(invalid());
                        }
                        if frame.generator {
                            if !matches!(self.state, RUNNING | CLOSING) {
                                return Err(invalid());
                            }
                            let ReturnTo::Generator {
                                frame: caller,
                                call,
                                action,
                            } = frame.ret
                            else {
                                return Err(invalid());
                            };
                            let parent = task
                                .frames
                                .data
                                .iter()
                                .find(|entry| entry.id() == caller)
                                .ok_or_else(invalid)?;
                            let Frame::Script(parent) = parent else {
                                return Err(invalid());
                            };
                            if call.0 as usize
                                >= vm.program.package().functions[parent.code.function.index()]
                                    .code
                                    .len()
                                || !generator_action_valid(
                                    action,
                                    vm.heap.range_descriptor(parent.regs)?.len,
                                )
                                || !vm.slots.read(at(parent.env, 1))?.same_bits(self.generator)
                            {
                                return Err(invalid());
                            }
                            if matches!(action, GeneratorAction::Pair(_)) {
                                let pair = vm.slots.read(at(parent.env, 2))?;
                                if vm.heap.info(pair)?.kind != ObjectKind::Array
                                    || vm.heap.info(pair)?.len != 2
                                {
                                    return Err(invalid());
                                }
                            }
                        } else if self.state != NEW {
                            return Err(invalid());
                        }
                    }
                    return Ok(false);
                }
                self.task += 1;
                self.frame = 0;
                return Ok(false);
            }
            if matches!(self.state, RUNNING | CLOSING) && self.owner.is_none() {
                return Err(invalid());
            }
            self.scanning = false;
            return Ok(false);
        }
        if self.state >= COMPLETE {
            if self.part < 3 {
                if !vm
                    .heap
                    .get_field(self.generator, WINDOWS + self.part)?
                    .same_bits(Value::NULL)
                {
                    return Err(invalid());
                }
                self.part += 1;
                return Ok(false);
            }
            return Ok(true);
        }
        let code = &vm.program.package().functions[self.function.index()];
        let strict = self.state == SUSPENDED || (self.state == NEW && self.owner.is_none());
        if self.part < 2 {
            let array = vm.heap.get_field(self.generator, WINDOWS + self.part)?;
            if array.same_bits(Value::NULL) {
                if strict || self.state != NEW {
                    return Err(invalid());
                }
                self.part += 1;
                self.index = 0;
                return Ok(false);
            }
            let info = vm.heap.info(array)?;
            let expected = match self.part {
                0 => code.register_count,
                1 => 5 + code.regions.len() as u32 * 2,
                _ => unreachable!(),
            };
            if info.kind != ObjectKind::Array
                || info.len % 2 != 0
                || info.len > expected.saturating_mul(2)
                || (strict && info.len != expected.saturating_mul(2))
            {
                return Err(invalid());
            }
            if self.index < info.len {
                let flag = vm.heap.get_field(array, self.index)?;
                let value = vm.heap.get_field(array, self.index + 1)?;
                match flag.as_bool() {
                    Some(false) if !value.same_bits(Value::NULL) => return Err(invalid()),
                    Some(_) => (),
                    None if !strict && flag.same_bits(Value::NULL) => (),
                    None => return Err(invalid()),
                }
                self.index += 2;
            } else {
                self.part += 1;
                self.index = 0;
            }
            return Ok(false);
        }
        let caught = vm.heap.get_field(self.generator, CAUGHT)?;
        if caught.same_bits(Value::NULL) {
            return if self.state == NEW && self.owner.is_some() {
                Ok(true)
            } else {
                Err(invalid())
            };
        }
        let info = vm.heap.info(caught)?;
        if info.kind != ObjectKind::Array
            || info.len > (code.regions.len() as u32).saturating_mul(3)
            || (strict && info.len % 3 != 0)
        {
            return Err(invalid());
        }
        if self.index >= info.len {
            return Ok(true);
        }
        let region = vm
            .heap
            .get_field(caught, self.index)?
            .as_int()
            .ok_or_else(invalid)?;
        if region < 0
            || code
                .regions
                .get(region as usize)
                .is_none_or(|region| region.catch.is_none())
        {
            return Err(invalid());
        }
        if self.index + 1 < info.len {
            vm.heap
                .fault_id(vm.heap.get_field(caught, self.index + 1)?)?;
        }
        if self.index + 2 < info.len {
            let activation = vm
                .heap
                .get_field(caught, self.index + 2)?
                .as_int()
                .ok_or_else(invalid)? as u64;
            if activation >= vm.next_id {
                return Err(invalid());
            }
        }
        self.index += 3;
        Ok(false)
    }
}

pub(super) fn generator_action_valid(action: GeneratorAction, registers: u32) -> bool {
    match action {
        GeneratorAction::Pair(reg) | GeneratorAction::Close(Some(reg)) => reg.0 < registers,
        GeneratorAction::Advance { done, value } => done.0 < registers && value.0 < registers,
        GeneratorAction::Close(None) => true,
    }
}

pub(super) fn validate_generator_frame(
    vm: &Vm,
    frame: ScriptFrame,
) -> Result<(), crate::snapshot::SnapshotError> {
    use crate::snapshot::SnapshotError;
    let invalid = || SnapshotError::Invalid("generator frame continuation".into());
    let code = &vm.program.package().functions[frame.code.function.index()];
    if frame.generator && (code.kind != FunctionKind::Generator || !frame.context.no_wait)
        || matches!(frame.ret, ReturnTo::Generator { .. }) != frame.generator
    {
        return Err(invalid());
    }
    if frame.generator {
        let generator = vm.generator_value(frame).map_err(|_| invalid())?;
        if !matches!(
            vm.generator_integer(generator, STATE)
                .map_err(|_| invalid())?,
            RUNNING | CLOSING
        ) {
            return Err(invalid());
        }
    }
    match frame.phase {
        ScriptPhase::GeneratorCreate => {
            if code.kind != FunctionKind::Generator || frame.generator {
                return Err(invalid());
            }
        }
        ScriptPhase::GeneratorCapture {
            part,
            index,
            created,
        } => {
            if code.kind != FunctionKind::Generator || created == frame.generator || part > 3 {
                return Err(invalid());
            }
            if part < 2 {
                let range = [frame.regs, frame.env][part as usize];
                if index > vm.heap.range_descriptor(range)?.len {
                    return Err(invalid());
                }
            } else if part == 2 {
                if index > vm.limits.max_unwinds {
                    return Err(invalid());
                }
            } else if index != 0 {
                return Err(invalid());
            }
            if !vm.is_text(
                vm.generator_value(frame).map_err(|_| invalid())?,
                ObjectKind::Generator,
            ) {
                return Err(invalid());
            }
        }
        ScriptPhase::GeneratorStart { action, call } => {
            if call.0.checked_add(1) != Some(frame.pc.0)
                || !generator_action_valid(action, code.register_count)
                || !vm.is_text(
                    vm.value(frame.env, 1).map_err(|_| invalid())?,
                    ObjectKind::Generator,
                )
            {
                return Err(invalid());
            }
            if let Ok(pair) = vm.slots.read(at(frame.env, 2))
                && (!matches!(action, GeneratorAction::Pair(_))
                    || vm.heap.info(pair)?.kind != ObjectKind::Array
                    || vm.heap.info(pair)?.len != 2)
            {
                return Err(invalid());
            }
        }
        ScriptPhase::GeneratorRestore { part, index, .. } => {
            if !frame.generator || part > 5 {
                return Err(invalid());
            }
            if part < 2 {
                let range = [frame.regs, frame.env][part as usize];
                if index > vm.heap.range_descriptor(range)?.len {
                    return Err(invalid());
                }
            } else if part < 5 {
                let generator = vm.generator_value(frame).map_err(|_| invalid())?;
                let array = vm.heap.get_field(
                    generator,
                    if part == 2 {
                        CAUGHT
                    } else {
                        WINDOWS + part - 3
                    },
                )?;
                if index > vm.heap.info(array)?.len || (part == 2 && index % 3 != 0) {
                    return Err(invalid());
                }
            } else if index != 0 {
                return Err(invalid());
            }
        }
        _ => (),
    }
    Ok(())
}

impl Vm {
    fn generator_slot(&self, s: ScriptFrame) -> u32 {
        4 + self.program.package().functions[s.code.function.index()]
            .regions
            .len() as u32
            * 2
    }

    fn generator_value(&self, s: ScriptFrame) -> Result<Value, VmError> {
        self.value(s.env, self.generator_slot(s))
    }

    fn generator_integer(&self, generator: Value, field: u32) -> Result<i64, VmError> {
        self.heap
            .get_field(generator, field)?
            .as_int()
            .ok_or(VmError::Contract("generator metadata"))
    }

    fn generator_seed(&self, fault: Value) -> Result<FaultSeed, VmError> {
        let id = self.heap.fault_id(fault)?;
        let info = self.heap.fault_info(id)?;
        Ok(FaultSeed {
            kind: info.kind,
            location: info.location,
            fault: Some(id),
            explicit: None,
            author_value: true,
            awaited: None,
        })
    }

    pub(super) fn generator_begin(
        &mut self,
        s: &mut ScriptFrame,
        generator: Value,
        action: GeneratorAction,
    ) -> Result<(), FaultKind> {
        if !self.is_text(generator, ObjectKind::Generator) {
            return Err(FaultKind::Type);
        }
        if s.effects.0 & Effects::WRITE == 0 || s.context.derived {
            return Err(FaultKind::Effect);
        }
        let state = self.generator_integer(generator, STATE).map_err(vm_fault)?;
        if state == RUNNING || state == CLOSING {
            return Err(FaultKind::InvalidState);
        }
        self.put(s.env, 1, generator).map_err(vm_fault)?;
        self.clear(s.env, 2).map_err(vm_fault)?;
        s.phase = ScriptPhase::GeneratorStart { action, call: s.pc };
        s.pc = s.instruction_end;
        Ok(())
    }

    fn generator_deliver(
        &mut self,
        ret: ReturnTo,
        done: bool,
        value: Value,
        cause: Option<Value>,
        error: Option<FaultSeed>,
    ) -> Result<bool, VmError> {
        let ReturnTo::Generator {
            frame,
            call,
            action,
        } = ret
        else {
            return self.commit(ret, value, cause, error);
        };
        let i = self.index(frame)?;
        let Frame::Script(mut parent) = self.active.frames.data[i] else {
            return Err(VmError::Contract("generator caller"));
        };
        if error.is_some() {
            return self.commit(
                ReturnTo::Register {
                    frame,
                    call,
                    reg: Reg(0),
                },
                value,
                cause,
                error,
            );
        }
        match action {
            GeneratorAction::Pair(reg) => {
                // The private builder is admitted before resuming user code.
                let pair = self.value(parent.env, 2)?;
                self.heap.set_field(pair, 0, Value::bool(done))?;
                self.heap.set_field(pair, 1, value)?;
                self.heap.seal_tuple(pair)?;
                self.put(parent.regs, reg.0, pair)?;
            }
            GeneratorAction::Advance { done: d, value: v } => {
                self.put(parent.regs, d.0, Value::bool(done))?;
                self.put(parent.regs, v.0, value)?;
            }
            GeneratorAction::Close(reg) => {
                if let Some(reg) = reg {
                    self.put(parent.regs, reg.0, Value::NULL)?;
                }
            }
        }
        self.clear(parent.env, 1)?;
        self.clear(parent.env, 2)?;
        parent.phase = ScriptPhase::Body;
        self.active.frames.data[i] = Frame::Script(parent);
        Ok(true)
    }

    pub(super) fn generator_commit_error(
        &mut self,
        ret: ReturnTo,
        value: Value,
        cause: Option<Value>,
        error: Option<FaultSeed>,
    ) -> Result<bool, VmError> {
        self.generator_deliver(ret, true, value, cause, error)
    }

    pub(super) fn generator_create_step(
        &mut self,
        s: &mut ScriptFrame,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        if !meter.take(
            0,
            HEADER as u64 + 2,
            Values::allocation_bytes(HEADER as u64),
        ) {
            return Ok(false);
        }
        let context = i64::from(s.context.no_wait)
            | (i64::from(s.context.init) << 1)
            | (i64::from(s.context.derived) << 2);
        let mut fields = [Value::NULL; HEADER];
        fields[LOGICAL_ID as usize] = Value::int(self.serial()? as i64);
        for (index, value) in [
            (STATE, NEW),
            (FUNCTION, i64::from(s.code.function.0)),
            (PC, i64::from(s.pc.0)),
            (MODULE, i64::from(s.module.0)),
            (CLOSURE, i64::from(s.closure)),
            (EFFECTS, i64::from(s.effects.0)),
            (CONTEXT, context),
        ] {
            fields[index as usize] = Value::int(value);
        }
        match self.heap.alloc_record(ObjectKind::Generator, &fields) {
            Ok(generator) => {
                self.put(s.env, self.generator_slot(*s), generator)?;
                s.phase = ScriptPhase::GeneratorCapture {
                    part: 0,
                    index: 0,
                    created: true,
                };
            }
            Err(error) => self.preparation_fault(s, heap_fault(error))?,
        }
        Ok(true)
    }

    pub(super) fn generator_capture_step(
        &mut self,
        s: &mut ScriptFrame,
        part: u32,
        index: u32,
        created: bool,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let generator = self.generator_value(*s)?;
        if part < 2 {
            let range = [s.regs, s.env][part as usize];
            let len = self.heap.range_descriptor(range)?.len;
            let array = self.heap.get_field(generator, WINDOWS + part)?;
            if array.same_bits(Value::NULL) {
                if !meter.take(0, 2, 0) {
                    return Ok(false);
                }
                match self.heap.alloc_array(&[]) {
                    Ok(array) => self.heap.set_field(generator, WINDOWS + part, array)?,
                    Err(error) => {
                        self.generator_capture_fault(s, created, error)?;
                        return Ok(true);
                    }
                }
                return Ok(true);
            }
            if index < len {
                let stored = self.heap.info(array)?.len;
                let extra = Values::growth_bytes(
                    u64::from(stored),
                    if stored <= index * 2 { 2 } else { 0 },
                );
                if !meter.take(0, 4, extra) {
                    return Ok(false);
                }
                let input = self.slots.read(at(range, index));
                let (initialized, value) = match input {
                    Ok(value) => (true, value),
                    Err(HeapError::UninitializedSlot) => (false, Value::NULL),
                    Err(error) => return Err(error.into()),
                };
                let copied = if stored <= index * 2 {
                    self.heap
                        .array_push(array, Value::bool(initialized))
                        .and_then(|()| self.heap.array_push(array, value))
                } else {
                    self.heap
                        .set_field(array, index * 2, Value::bool(initialized))
                        .and_then(|()| self.heap.set_field(array, index * 2 + 1, value))
                };
                if let Err(error) = copied {
                    self.generator_capture_fault(s, created, error)?;
                    return Ok(true);
                }
                s.phase = ScriptPhase::GeneratorCapture {
                    part,
                    index: index + 1,
                    created,
                };
            } else {
                if !meter.take(0, 1, 0) {
                    return Ok(false);
                }
                s.phase = ScriptPhase::GeneratorCapture {
                    part: part + 1,
                    index: 0,
                    created,
                };
            }
            return Ok(true);
        }
        if part == 2 {
            let caught = self.heap.get_field(generator, CAUGHT)?;
            if caught.same_bits(Value::NULL) {
                if !meter.take(0, 2, 0) {
                    return Ok(false);
                }
                match self.heap.alloc_array(&[]) {
                    Ok(array) => self.heap.set_field(generator, CAUGHT, array)?,
                    Err(error) => self.generator_capture_fault(s, created, error)?,
                }
                return Ok(true);
            }
            let len = self.heap.info(caught)?.len;
            let record = self.active.caught.data.get(index as usize).copied();
            let extra = if record.is_some_and(|record| record.frame == s.id) {
                Values::growth_bytes(len.into(), 3)
            } else {
                0
            };
            if !meter.take(0, 5, extra) {
                return Ok(false);
            }
            if let Some(record) = record {
                if record.frame != s.id {
                    s.phase = ScriptPhase::GeneratorCapture {
                        part,
                        index: index + 1,
                        created,
                    };
                    return Ok(true);
                }
                let saved = self
                    .heap
                    .array_push(caught, Value::int(i64::from(record.region.0)))
                    .and_then(|()| {
                        self.heap.array_push(
                            caught,
                            record.info.fault.map_or(Value::NULL, FaultId::as_value),
                        )
                    })
                    .and_then(|()| {
                        self.heap
                            .array_push(caught, Value::int(record.activation as i64))
                    });
                if let Err(error) = saved {
                    self.generator_capture_fault(s, created, error)?;
                    return Ok(true);
                }
                s.phase = ScriptPhase::GeneratorCapture {
                    part,
                    index: index + 1,
                    created,
                };
            } else {
                s.phase = ScriptPhase::GeneratorCapture {
                    part: 3,
                    index: 0,
                    created,
                };
            }
            return Ok(true);
        }
        let catches = self.active.caught.data.len() as u64;
        if !meter.take(
            0,
            catches + (catches + 2).max(self.release_cost(4) + self.commit_work(s.ret) + 4),
            0,
        ) {
            return Ok(false);
        }
        if let Some(index) = self
            .active
            .caught
            .data
            .iter()
            .position(|record| record.frame == s.id)
        {
            self.active.caught.data.remove(index);
            return Ok(true);
        }
        self.heap
            .set_field(generator, PC, Value::int(i64::from(s.pc.0)))?;
        self.heap.set_field(
            generator,
            STATE,
            Value::int(if created { NEW } else { SUSPENDED }),
        )?;
        let value = if created {
            generator
        } else {
            self.heap.get_field(generator, RESULT)?
        };
        let delivered = if created {
            self.commit(s.ret, value, None, None)?
        } else {
            self.generator_deliver(s.ret, false, value, None, None)?
        };
        if !delivered {
            return Ok(true);
        }
        self.heap.set_field(generator, RESULT, Value::NULL)?;
        let mut detached = *s;
        detached.generator = false;
        self.release_frame(Frame::Script(detached))?;
        self.active.frames.data.pop();
        Ok(true)
    }

    fn generator_capture_fault(
        &mut self,
        s: &mut ScriptFrame,
        created: bool,
        error: HeapError,
    ) -> Result<(), VmError> {
        if created {
            self.preparation_fault(s, heap_fault(error))
        } else {
            s.pc = Pc(self.program.package().functions[s.code.function.index()]
                .code
                .previous_pc(s.pc.index())
                .expect("generator resume PC") as u32);
            self.collection_fault(s, heap_fault(error))
        }
    }

    pub(super) fn generator_start_step(
        &mut self,
        s: &mut ScriptFrame,
        action: GeneratorAction,
        call: Pc,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let generator = self.value(s.env, 1)?;
        let state = self.generator_integer(generator, STATE)?;
        let ret = ReturnTo::Generator {
            frame: s.id,
            call,
            action,
        };
        if matches!(action, GeneratorAction::Pair(_)) && self.slots.read(at(s.env, 2)).is_err() {
            if !meter.take(0, 3, Values::allocation_bytes(2)) {
                return Ok(false);
            }
            match self.heap.alloc_array(&[Value::NULL, Value::NULL]) {
                Ok(pair) => self.put(s.env, 2, pair)?,
                Err(error) => {
                    s.pc = call;
                    self.collection_fault(s, heap_fault(error))?;
                }
            }
            return Ok(true);
        }
        if state >= COMPLETE {
            if !meter.take(0, 5, 0) {
                return Ok(false);
            }
            self.generator_deliver(ret, true, Value::NULL, None, None)?;
            // The caller is the current frame in this branch.
            let i = self.index(s.id)?;
            let Frame::Script(current) = self.active.frames.data[i] else {
                unreachable!()
            };
            *s = current;
            return Ok(true);
        }
        if state == RUNNING || state == CLOSING {
            if !meter.take(0, 1, 0) {
                return Ok(false);
            }
            self.collection_fault(s, FaultKind::InvalidState)?;
            return Ok(true);
        }
        if state == NEW && matches!(action, GeneratorAction::Close(_)) {
            if !meter.take(0, 10, 0) {
                return Ok(false);
            }
            self.generator_terminal(generator, CLOSED)?;
            self.generator_deliver(ret, true, Value::NULL, None, None)?;
            let Frame::Script(current) = self.active.frames.data[self.index(s.id)?] else {
                unreachable!()
            };
            *s = current;
            return Ok(true);
        }
        let mut lengths = [0u32; 4];
        for (part, length) in lengths.iter_mut().enumerate().take(2) {
            *length = self
                .heap
                .info(self.heap.get_field(generator, WINDOWS + part as u32)?)?
                .len
                / 2;
        }
        let values = lengths.iter().map(|&n| u64::from(n)).sum();
        let (work, allocation) = self.window_push_cost(4, values);
        if !meter.take(0, work + 20, allocation) {
            return Ok(false);
        }
        if self.active.frames.data.len() == self.limits.max_frames as usize {
            self.collection_fault(s, FaultKind::Resource)?;
            return Ok(true);
        }
        let function = FunctionId(
            u32::try_from(self.generator_integer(generator, FUNCTION)?)
                .map_err(|_| VmError::Contract("generator function"))?,
        );
        let pc = Pc(u32::try_from(self.generator_integer(generator, PC)?)
            .map_err(|_| VmError::Contract("generator pc"))?);
        let flags = self.generator_integer(generator, CONTEXT)?;
        let saved = ContextFlags {
            no_wait: true,
            init: flags & 2 != 0,
            derived: flags & 4 != 0,
        };
        if let Err(error) = self.slots.admit_windows(&mut self.heap, 4, values) {
            s.pc = call;
            self.collection_fault(s, heap_fault(error))?;
            return Ok(true);
        }
        let mut ranges = [s.regs; 4];
        for part in 0..4 {
            match self.open(s.owner, RootKind::TaskFrame, lengths[part]) {
                Ok(range) => ranges[part] = range,
                Err(error) => {
                    for range in ranges.iter().take(part) {
                        self.slots.release(&mut self.heap, *range)?;
                    }
                    self.collection_fault(s, vm_fault(error))?;
                    return Ok(true);
                }
            }
        }
        let id = FrameId(self.serial()?);
        let mut frame = ScriptFrame {
            derived_bridge: None,
            id,
            code: CodeRef {
                function,
                version: self.program.package().content_version,
            },
            module: ModuleInstanceId(self.generator_integer(generator, MODULE)? as u32),
            pc,
            instruction_end: Pc(0),
            member_symbol: None,
            instruction: None,
            instruction_pc: Pc(0),
            regs: ranges[0],
            env: ranges[1],
            args: ranges[2],
            bound: ranges[3],
            argument_count: 0,
            closure: self.generator_integer(generator, CLOSURE)? != 0,
            shared_parameters: false,
            parameter_module: ModuleInstanceId(
                self.program.package().functions[function.index()].module.0,
            ),
            effects: Effects(self.generator_integer(generator, EFFECTS)? as u32 & s.effects.0),
            context: ContextFlags {
                no_wait: true,
                init: saved.init || s.context.init,
                derived: saved.derived || s.context.derived,
            },
            owner: s.owner,
            ret,
            phase: ScriptPhase::GeneratorRestore {
                part: 0,
                index: 0,
                closing: matches!(action, GeneratorAction::Close(_)),
            },
            table_work: None,
            generator: true,
        };
        frame.context = inherit(frame.context, Context::NoWait);
        self.put(frame.env, self.generator_slot(frame), generator)?;
        self.heap.set_field(
            generator,
            STATE,
            Value::int(if matches!(action, GeneratorAction::Close(_)) {
                CLOSING
            } else {
                RUNNING
            }),
        )?;
        self.heap.set_field(generator, RESULT, Value::NULL)?;
        s.phase = ScriptPhase::Body;
        let parent = self.index(s.id)?;
        self.active.frames.data[parent] = Frame::Script(*s);
        self.active.frames.data.push(Frame::Script(frame));
        Ok(true)
    }

    pub(super) fn generator_restore_step(
        &mut self,
        s: &mut ScriptFrame,
        part: u32,
        index: u32,
        closing: bool,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let generator = self.generator_value(*s)?;
        if part < 2 {
            if !meter.take(0, 4, 0) {
                return Ok(false);
            }
            let range = [s.regs, s.env][part as usize];
            let len = self.heap.range_descriptor(range)?.len;
            if index < len {
                let array = self.heap.get_field(generator, WINDOWS + part)?;
                if self
                    .heap
                    .get_field(array, index * 2)?
                    .as_bool()
                    .ok_or(VmError::Contract("generator bitmap"))?
                {
                    self.put(range, index, self.heap.get_field(array, index * 2 + 1)?)?;
                } else if !(part == 1 && index == self.generator_slot(*s)) {
                    self.clear(range, index)?;
                }
                s.phase = ScriptPhase::GeneratorRestore {
                    part,
                    index: index + 1,
                    closing,
                };
            } else {
                s.phase = ScriptPhase::GeneratorRestore {
                    part: part + 1,
                    index: 0,
                    closing,
                };
            }
            return Ok(true);
        }
        let caught = self.heap.get_field(generator, CAUGHT)?;
        if part == 2 && index < self.heap.info(caught)?.len {
            if !meter.take(0, 7, 0) {
                return Ok(false);
            }
            if self.active.caught.data.len() == self.limits.max_unwinds as usize {
                self.collection_fault(s, FaultKind::Resource)?;
                return Ok(true);
            }
            let region = RegionId(
                self.heap
                    .get_field(caught, index)?
                    .as_int()
                    .ok_or(VmError::Contract("generator catch"))? as u32,
            );
            let info = self.generator_seed(self.heap.get_field(caught, index + 1)?)?;
            let activation =
                self.heap
                    .get_field(caught, index + 2)?
                    .as_int()
                    .ok_or(VmError::Contract("generator catch activation"))? as u64;
            self.active.caught.data.push(CaughtFault {
                frame: s.id,
                region,
                info,
                activation,
            });
            s.phase = ScriptPhase::GeneratorRestore {
                part,
                index: index + 3,
                closing,
            };
            return Ok(true);
        }
        if !meter.take(0, 8, 0) {
            return Ok(false);
        }
        // The arrays remain allocated for the next suspension, with stale edges
        // cleared incrementally by the next stage before executing author code.
        s.phase = if part < 3 {
            ScriptPhase::GeneratorRestore {
                part: 3,
                index: 0,
                closing,
            }
        } else if part < 5 {
            let array = self.heap.get_field(generator, WINDOWS + part - 3)?;
            if index < self.heap.info(array)?.len {
                self.heap.set_field(array, index, Value::NULL)?;
                ScriptPhase::GeneratorRestore {
                    part,
                    index: index + 1,
                    closing,
                }
            } else {
                ScriptPhase::GeneratorRestore {
                    part: part + 1,
                    index: 0,
                    closing,
                }
            }
        } else if self.heap.info(caught)?.len > 0 {
            self.heap.array_pop(caught)?;
            ScriptPhase::GeneratorRestore {
                part,
                index: 0,
                closing,
            }
        } else if closing {
            self.put(s.env, 1, Value::NULL)?;
            self.clear(s.env, 2)?;
            ScriptPhase::Exit {
                origin: Pc(self.program.package().functions[s.code.function.index()]
                    .code
                    .previous_pc(s.pc.index())
                    .expect("generator origin PC") as u32),
                reason: ExitReason::GeneratorClose {
                    logical_id: self.generator_integer(generator, LOGICAL_ID)? as u64,
                    activation: s.id.0,
                },
                skip: None,
            }
        } else {
            ScriptPhase::Body
        };
        Ok(true)
    }

    fn generator_terminal(&mut self, generator: Value, state: i64) -> Result<(), VmError> {
        self.heap.set_field(generator, STATE, Value::int(state))?;
        self.heap.set_field(generator, RESULT, Value::NULL)?;
        for field in WINDOWS..=CAUGHT {
            self.heap.set_field(generator, field, Value::NULL)?;
        }
        Ok(())
    }

    pub(super) fn generator_release(
        &mut self,
        s: ScriptFrame,
        value: Value,
        cause: Option<Value>,
        error: Option<FaultSeed>,
    ) -> Result<bool, VmError> {
        let generator = self.generator_value(s)?;
        let state = if error.is_some() || self.generator_integer(generator, STATE)? == CLOSING {
            CLOSED
        } else {
            COMPLETE
        };
        self.generator_terminal(generator, state)?;
        self.generator_deliver(s.ret, true, value, cause, error)
    }

    pub(super) fn generator_abandon(&mut self, s: ScriptFrame) -> Result<(), VmError> {
        let generator = self.generator_value(s)?;
        if matches!(self.generator_integer(generator, STATE)?, RUNNING | CLOSING) {
            self.generator_terminal(generator, CLOSED)?;
        }
        Ok(())
    }

    pub(super) fn generator_yield(
        &mut self,
        s: &mut ScriptFrame,
        result: Reg,
        item: Reg,
    ) -> Result<(), FaultKind> {
        if !s.generator || self.active.unwinds.data.iter().any(|u| u.frame == s.id) {
            return Err(FaultKind::InvalidState);
        }
        let generator = self.generator_value(*s).map_err(vm_fault)?;
        let value = self.read(s.regs, item)?;
        self.heap
            .set_field(generator, RESULT, value)
            .map_err(heap_fault)?;
        self.write(s.regs, result, Value::NULL)?;
        s.pc = s.instruction_end;
        s.phase = ScriptPhase::GeneratorCapture {
            part: 0,
            index: 0,
            created: false,
        };
        Ok(())
    }
}
