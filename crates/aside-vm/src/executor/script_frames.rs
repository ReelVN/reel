//! Script frame admission and task allocation.
use super::*;

impl Vm {
    // All construction inputs are explicit so failed root publication rolls back
    // as one transaction before a frame becomes visible.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn push_script(
        &mut self,
        function: FunctionId,
        closure: Option<Value>,
        source: ArgSource,
        count: u32,
        owner: RootOwner,
        context: ContextFlags,
        effects: Effects,
        ret: ReturnTo,
    ) -> Result<(), VmError> {
        if self.active.frames.data.len() == self.limits.max_frames as usize
            || count > self.limits.max_arguments
        {
            return Err(VmError::Limit);
        }
        let f = &self.program.package().functions[function.index()];
        if let FunctionKind::Native(native) = f.kind {
            let site = self.native_site(function, owner, context, effects, ret)?;
            let receiver = closure
                .map(|value| self.callable_receiver(value))
                .transpose()?
                .flatten();
            return self.push_native_bound(native, source, count, site, receiver);
        }
        let parameters = &self.program.package().signatures[f.signature.index()];
        let values = u64::from(f.register_count)
            + 4
            + u64::from(self.program.compact_derived(function))
            + u64::from(f.kind == FunctionKind::Generator)
            + 2 * f.regions.len() as u64
            + u64::from(count) * 2
            + parameters.parameters.len() as u64
            + u64::from(parameters.rest.is_some());
        self.slots.admit_windows(&mut self.heap, 4, values)?;
        if !matches!(source, ArgSource::Prefix { .. })
            && let Some(closure) = closure
        {
            self.enter_initializer(closure, function)?;
        }
        let f = &self.program.package().functions[function.index()];
        let registers = f.register_count;
        // Slot 4 roots a compact observer independently of ordinary call scratch
        // (class construction and builtin comparators use slot 3). Eligible
        // functions have no exception regions or generator environment.
        let env_len = 4
            + f.regions.len() as u32 * 2
            + u32::from(f.kind == FunctionKind::Generator)
            + u32::from(self.program.compact_derived(function));
        let sig = &self.program.package().signatures[f.signature.index()];
        let n = sig.parameters.len() as u32 + u32::from(sig.rest.is_some());
        let context = inherit(context, f.context);
        let effects = Effects(
            effects.0
                & self.program.package().signatures[f.signature.index()]
                    .effects
                    .0,
        );
        let shared_parameters = matches!(source, ArgSource::Prefix { .. });
        let module = ModuleInstanceId(f.module.0);
        let parameter_module = if let ArgSource::Prefix { frame } = source {
            match self.active.frames.data[self.index(frame)?] {
                Frame::Script(parent) => parent.parameter_module,
                Frame::Native(parent) => parent.module,
            }
        } else {
            module
        };
        let id = FrameId(self.serial()?);
        // Failed construction rolls back published roots and does not push a partial frame.
        let regs = self.open(owner, RootKind::TaskFrame, registers)?;
        let env = match self.open(owner, RootKind::TaskFrame, env_len) {
            Ok(r) => r,
            Err(e) => {
                self.slots.release(&mut self.heap, regs)?;
                return Err(e);
            }
        };
        let args = match self.open(owner, RootKind::Arguments, count * 2) {
            Ok(r) => r,
            Err(e) => {
                self.slots.release(&mut self.heap, env)?;
                self.slots.release(&mut self.heap, regs)?;
                return Err(e);
            }
        };
        let bound = match self.open(owner, RootKind::Arguments, n) {
            Ok(r) => r,
            Err(e) => {
                self.slots.release(&mut self.heap, args)?;
                self.slots.release(&mut self.heap, env)?;
                self.slots.release(&mut self.heap, regs)?;
                return Err(e);
            }
        };
        self.put(env, 0, closure.unwrap_or(Value::NULL))?;
        if !shared_parameters
            && let Some(receiver) = closure
                .map(|v| self.script_receiver(v))
                .transpose()?
                .flatten()
        {
            self.put(bound, 0, receiver)?;
        }
        let s = ScriptFrame {
            derived_bridge: None,
            id,
            code: CodeRef {
                function,
                version: self.program.package().content_version,
            },
            module,
            pc: Pc(0),
            instruction_end: Pc(0),
            member_symbol: None,
            instruction: None,
            instruction_pc: Pc(0),
            regs,
            env,
            args,
            bound,
            argument_count: count,
            closure: closure.is_some(),
            shared_parameters,
            parameter_module,
            effects,
            context,
            owner,
            ret,
            phase: ScriptPhase::Copy { source, index: 0 },
            table_work: None,
            generator: false,
        };
        self.active.frames.data.push(Frame::Script(s));
        Ok(())
    }
    pub fn start(
        &mut self,
        function: FunctionId,
        args: &[Value],
        owner: RootOwner,
        context: Context,
    ) -> Result<TaskId, VmError> {
        self.start_impl(function, None, args, owner, context)
    }
    /// Whether a callable declares any parameters beyond its prebound receiver.
    /// Rest parameters count; this does not relax ordinary argument validation.
    pub fn callable_has_parameters(&self, callable: Value) -> Result<bool, VmError> {
        let function = self.closure_function(callable)?;
        let receiver = self.script_receiver(callable)?.is_some()
            || self.callable_receiver(callable)?.is_some();
        let package = self.program.package();
        let signature = &package.signatures[package.functions[function.index()].signature.index()];
        Ok(signature.parameters.len() > usize::from(receiver) || signature.rest.is_some())
    }

    pub fn start_closure(
        &mut self,
        closure: Value,
        args: &[Value],
        owner: RootOwner,
        context: Context,
    ) -> Result<TaskId, VmError> {
        let function = self.closure_function(closure)?;
        self.start_impl(function, Some(closure), args, owner, context)
    }
    pub(super) fn start_impl(
        &mut self,
        function: FunctionId,
        closure: Option<Value>,
        args: &[Value],
        owner: RootOwner,
        context: Context,
    ) -> Result<TaskId, VmError> {
        if function.index() >= self.program.package().functions.len()
            || args.len() > self.limits.max_arguments as usize
            || owner == RootOwner(u64::MAX)
        {
            return Err(VmError::Limit);
        }
        let f = &self.program.package().functions[function.index()];
        if !f.captures.is_empty() && closure.is_none() {
            return Err(VmError::Contract("entry requires closure environment"));
        }
        for &v in args {
            self.heap.root_insertion_barrier(v)?;
        }
        let serial = self.serial()?;
        let task = TaskId {
            heap: self.heap.id(),
            epoch: self.heap.epoch(),
            serial,
        };
        self.restore_active();
        let slot = match self.tasks.data.iter().position(|s| s.task_id.is_none()) {
            Some(slot) => {
                self.slots.reserve_task(&mut self.heap, 0)?;
                slot
            }
            None => self.grow_task_pool()?,
        };
        self.select_slot(slot);
        self.active.task_id = Some(task);
        self.task_index.data.push((serial, slot));
        self.active.task_owner = Some(owner);
        self.active.root_owner = RootOwner(u64::MAX.checked_sub(serial + 1).ok_or(VmError::Limit)?);
        if let Err(e) = self.push_script(
            function,
            closure,
            ArgSource::None,
            args.len() as u32,
            owner,
            ContextFlags::from(context),
            Effects::ALL,
            ReturnTo::Delivery,
        ) {
            self.forget_active_task();
            self.active.task_owner = None;
            return Err(e);
        }
        let mut frame = *self.active.frames.data.last().unwrap();
        let arguments = match frame {
            Frame::Script(s) => s.args,
            Frame::Native(n) => n.args,
        };
        for (i, &v) in args.iter().enumerate() {
            self.put(arguments, i as u32 * 2, Value::int(-1))?;
            self.put(arguments, i as u32 * 2 + 1, v)?;
        }
        let signature = self.program.package().functions[function.index()].signature;
        let rest = self.program.package().signatures[signature.index()]
            .rest
            .is_some();
        match &mut frame {
            Frame::Script(s) => {
                s.phase = if rest {
                    ScriptPhase::Rest
                } else {
                    ScriptPhase::Bind(ArgumentBinding::new(self.receiver_prefix(*s)?))
                }
            }
            Frame::Native(n) => {
                n.phase = if rest {
                    NativePhase::Rest
                } else {
                    NativePhase::Bind(ArgumentBinding::new(u32::from(n.receiver)))
                }
            }
        };
        *self.active.frames.data.last_mut().unwrap() = frame;
        self.active.task_owner = Some(owner);
        self.active.task_id = Some(task);
        Ok(task)
    }
    pub(super) fn grow_task_pool(&mut self) -> Result<usize, VmError> {
        let slot = self.tasks.data.len();
        if slot >= self.limits.max_tasks as usize {
            return Err(VmError::Limit);
        }
        let usage = self.heap.budget_usage();
        if slot == self.tasks.data.capacity() {
            let capacity = slot.saturating_mul(2).min(self.limits.max_tasks as usize);
            let mut next = ChargedVec::new(&usage, capacity)?;
            next.data.append(&mut self.tasks.data);
            self.tasks = next;
        }
        if slot == self.task_index.data.capacity() {
            let capacity = slot.saturating_mul(2).min(self.limits.max_tasks as usize);
            let mut next = ChargedVec::new(&usage, capacity)?;
            next.data.append(&mut self.task_index.data);
            self.task_index = next;
        }
        let roots = self
            .limits
            .max_unwinds
            .checked_mul(2)
            .and_then(|n| n.checked_add(2))
            .ok_or(VmError::Limit)?;
        self.slots.reserve_task(&mut self.heap, roots)?;
        let task = TaskState::new(&usage, &mut self.heap, &mut self.slots, self.limits, true)?;
        self.tasks.data.push(task);
        Ok(slot)
    }
}
