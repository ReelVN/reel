//! Host Awaitable dispatch shares native frames, roots, wait identities and cancellation.
use super::*;

impl Vm {
    /// Version of pending-wait invalidation by completion or task cancellation.
    /// Hosts can reuse subscription checks until this version changes. New host
    /// subscriptions must independently invalidate their own cached checks.
    pub fn wait_revision(&self) -> u64 {
        self.wait_revision
    }

    /// Bind a host object to a trusted, unary native adapter in this loaded package.
    /// The adapter receives the object itself. Script properties cannot replace it.
    pub fn bind_awaitable(&mut self, object: Value, native: NativeId) -> Result<(), VmError> {
        let p = self.program.package();
        let definition = p
            .natives
            .get(native.index())
            .ok_or(VmError::Contract("unknown await adapter"))?;
        let sig = &p.signatures[definition.signature.index()];
        if sig.parameters.len() != 1
            || sig.rest.is_some()
            || sig.parameters[0].default.is_some()
            || p.types[sig.parameters[0].ty.index()] != TypeDef::Builtin(BuiltinType::Any)
            || sig.effects.0 & Effects::WAIT == 0
        {
            return Err(VmError::Contract(
                "await adapter requires one any parameter and WAIT",
            ));
        }
        self.heap.set_host_await(object, native)?;
        Ok(())
    }

    /// Hosts use this when retiring subscriptions. A completed/cancelled consumer
    /// cannot receive another result; removing it does not cancel its producer.
    pub fn wait_is_pending(&self, wait: WaitId) -> bool {
        self.task(wait.task).is_ok_and(|task| {
            !task.cancelling && task.frames.data.last().is_some_and(|frame| {
                matches!(frame, Frame::Native(n) if matches!(n.phase, NativePhase::Waiting(w) if w == wait))
            })
        })
    }

    pub(super) fn start_await(
        &mut self,
        s: &mut ScriptFrame,
        dest: Reg,
        value: Reg,
    ) -> Result<(), FaultKind> {
        if no_wait(s.context)
            || s.effects.0 & Effects::WAIT == 0
            || self.active.unwinds.data.iter().any(|u| u.frame == s.id)
        {
            return Err(FaultKind::Effect);
        }
        let native =
            self.heap
                .host_await(self.read(s.regs, value)?)
                .map_err(|error| match error {
                    HeapError::TypeMismatch | HeapError::WrongKind => FaultKind::NotAwaitable,
                    other => heap_fault(other),
                })?;
        let call = s.pc;
        let site = NativeSite {
            owner: s.owner,
            module: s.module,
            context: s.context,
            effects: s.effects,
            location: self.loc(s.code, call),
            ret: ReturnTo::Register {
                frame: s.id,
                reg: dest,
                call,
            },
        };
        let index = self.index(s.id).map_err(vm_fault)?;
        s.pc = s.instruction_end;
        self.publish_script_cursor(index, s);
        if let Err(error) =
            self.push_native(native, ArgSource::Await { frame: s.id, value }, 1, site)
        {
            s.pc = call;
            return Err(vm_fault(error));
        }
        Ok(())
    }

    pub(super) fn forwarded_fault(
        &self,
        n: NativeFrame,
        id: FaultId,
        crossing_await: bool,
    ) -> Result<FaultSeed, VmError> {
        let info = self.heap.fault_info(id)?;
        let activation = n.await_activation.filter(|_| crossing_await);
        Ok(FaultSeed {
            kind: info.kind,
            location: if activation.is_some() {
                n.location
            } else {
                info.location
            },
            fault: activation.is_none().then_some(id),
            explicit: None,
            author_value: true,
            awaited: activation.map(|activation| (id, activation)),
        })
    }
}
