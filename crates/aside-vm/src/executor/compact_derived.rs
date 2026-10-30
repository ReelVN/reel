//! Standard derives reuse ordinary script execution and tail-transfer its roots
//! to the host commit continuation. Custom observers keep their native bridge.
use super::*;
use crate::heap::DerivedSlot;

impl Vm {
    pub(super) fn compact_output(&self, output: Value) -> Result<bool, VmError> {
        Ok(self
            .heap
            .host_payload(output, DerivedSlot::Compact as u32)?
            .as_bool()
            == Some(true))
    }
    pub(super) fn compact_derived_location(
        &self,
        output: Value,
    ) -> Result<(NativeId, Location), VmError> {
        let function = FunctionId(
            self.heap
                .host_payload(output, DerivedSlot::Function as u32)?
                .as_int()
                .unwrap() as u32,
        );
        let FunctionKind::Native(native) = self.program.package().functions[function.index()].kind
        else {
            unreachable!()
        };
        let function = FunctionId(
            self.heap
                .host_payload(output, DerivedSlot::Origin as u32)?
                .as_int()
                .unwrap() as u32,
        );
        let pc = Pc(self
            .heap
            .host_payload(output, DerivedSlot::CallPc as u32)?
            .as_int()
            .unwrap() as u32);
        Ok((
            native,
            Location {
                code: CodeRef {
                    function,
                    version: self.program.package().content_version,
                },
                pc,
                source: self.program.source_at(function, pc),
            },
        ))
    }
    pub(super) fn enter_compact_derived(
        &mut self,
        output: Value,
        module: ModuleInstanceId,
        effects: Effects,
    ) -> Result<(), VmError> {
        let top = self.active.frames.data.len() - 1;
        let Frame::Script(mut script) = self.active.frames.data[top] else {
            unreachable!()
        };
        let id = match self.serial() {
            Ok(id) => FrameId(id),
            Err(error) => {
                self.release_frame(Frame::Script(script))?;
                self.active.frames.data.pop();
                return Err(error);
            }
        };
        if let Err(error) = self.put(script.env, 4, output) {
            self.release_frame(Frame::Script(script))?;
            self.active.frames.data.pop();
            return Err(error);
        }
        script.derived_bridge = Some((script.id, module, effects));
        script.id = id;
        script.effects = Effects(script.effects.0 & effects.0);
        script.context.derived = true;
        script.context.no_wait = true;
        script.phase = ScriptPhase::CompactBegin;
        self.active.frames.data[top] = Frame::Script(script);
        self.derived_starts += 1;
        self.compact_derived_starts += 1;
        Ok(())
    }
    pub(super) fn compact_stack_entry(&self, script: ScriptFrame) -> Result<StackEntry, VmError> {
        let output = self.value(script.env, 4)?;
        let (native, location) = self.compact_derived_location(output)?;
        let version = self.program.package().content_version;
        Ok(StackEntry {
            frame: script.derived_bridge.unwrap().0,
            location,
            module: script.derived_bridge.unwrap().1,
            module_version: version,
            source_version: version,
            kind: StackFrameKind::Native {
                native,
                abi_version: self.program.package().natives[native.index()].abi_version,
                continuation: ContinuationState {
                    version: 1,
                    stage: 0,
                },
            },
        })
    }
    pub(super) fn compact_derived_commit(
        &mut self,
        script: ScriptFrame,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        // Root 1 already carries the callback closure in ordinary native frames.
        // Borrow it for the one supplied argument instead of copying it into the
        // two empty script argument windows. Reentrant callbacks materialize them.
        if !meter.take(0, self.release_cost(1) + self.program_lookup_work() + 10, 0) {
            return Ok(false);
        }
        let output = self.value(script.env, 4)?;
        let closure = self.value(script.env, 0)?;
        let value = self.value(script.env, 1)?;
        let (native, location) = self.compact_derived_location(output)?;
        // The original return register still roots value. Publish the closure's
        // destination before overwriting its old environment slot.
        self.put(script.env, 1, closure)?;
        self.put(script.env, 0, value)?;
        self.put(script.env, 2, output)?;
        self.put(script.env, 3, Value::bool(true))?;
        self.clear(script.env, 4)?;
        let signature = self.program.package().natives[native.index()].signature;
        let frame = NativeFrame {
            inline_argument: true,
            argument_plan: None,
            id: script.derived_bridge.unwrap().0,
            native,
            signature,
            args: script.args,
            bound: script.bound,
            bindings: None,
            module: script.derived_bridge.unwrap().1,
            has_defaults: false,
            roots: script.env,
            argument_count: 1,
            receiver: false,
            continuation_len: 2,
            continuation_active: true,
            continuation_version: 1,
            continuation_stage: 0,
            effects: script.derived_bridge.unwrap().2,
            context: script.context,
            owner: script.owner,
            location,
            ret: script.ret,
            callback_fault: None,
            derived_output: Some(output),
            fault_count: 0,
            pending_fault: None,
            phase: NativePhase::Resume(NativeInput::CallbackReturned),
            await_activation: None,
        };
        *self.active.frames.data.last_mut().unwrap() = Frame::Native(frame);
        self.slots.release(&mut self.heap, script.regs)?;
        execution_count!(self, CompactCommits, 1);
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
