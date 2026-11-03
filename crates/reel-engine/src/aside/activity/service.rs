//! Native admission, producer controls, and independent await subscriptions.
use super::*;

impl Activities {
    pub fn next_tick(&mut self, vm: &mut Vm, ticket: HostTicket) -> Result<(), AdapterError> {
        let clock = vm.host_arguments(ticket)?[0];
        tagged(vm.heap(), clock, ACTIVITY)?;
        if action::kind(
            vm.heap(),
            root_get(vm.heap(), clock, RootSlot::Description)?,
        )? != action::Kind::Clock
        {
            return Err(AdapterError::Input("next requires a clock"));
        }
        if terminal(state(vm.heap(), clock)?) {
            vm.respond(ticket, HostResponse::Error(Self::tick_cancelled()))?;
        } else {
            let ticks = vm
                .heap()
                .host_property(PropertyId {
                    host: clock,
                    field: 0,
                })?
                .as_int()
                .expect("clock ticks are integers");
            self.waits
                .try_reserve(1)
                .map_err(|_| AdapterError::Allocation)?;
            let wait = ticket.wait_id();
            vm.respond(ticket, HostResponse::Pending(wait))?;
            self.waits.push((wait, clock, Some(ticks)));
            self.waits_dirty = true;
        }
        Ok(())
    }

    fn tick_cancelled() -> aside_vm::NewFault {
        aside_vm::NewFault {
            value: Value::NULL,
            code: aside_vm::FaultCode::core(FaultKind::Cancelled),
            cause: None,
        }
    }

    pub fn clock(
        &mut self,
        vm: &mut Vm,
        stage: &StageBinding,
        root_owner: RootOwner,
        ticket: HostTicket,
        args: [Value; 3],
        budget: u32,
    ) -> Result<(bool, u32), AdapterError> {
        let action = if self.native.is_none() {
            action::clock(vm.heap_mut(), args[0])?
        } else {
            Value::NULL
        };
        self.run(
            vm,
            stage,
            root_owner,
            ticket,
            [action, args[1], args[2]],
            budget,
        )
    }
    pub fn run(
        &mut self,
        vm: &mut Vm,
        stage: &StageBinding,
        root_owner: RootOwner,
        ticket: HostTicket,
        args: [Value; 3],
        budget: u32,
    ) -> Result<(bool, u32), AdapterError> {
        let [action, owner, paused] = args;
        if self.native.is_none() {
            let paused = paused
                .as_bool()
                .ok_or(AdapterError::Input("paused must be bool"))?;
            tagged(vm.heap(), action, ACTION)?;
            super::super::scopes::validate_owner(vm.heap(), stage, owner)?;
            let native = vm
                .program()
                .package()
                .natives
                .iter()
                .position(|n| {
                    vm.program().package().symbols[n.symbol.index()] == "reel::activity::await"
                })
                .ok_or(AdapterError::Input("activity await adapter missing"))?;
            let heap = vm.heap_mut();
            let graph = heap.alloc_array(&[])?;
            let results = heap.alloc_array(&[])?;
            for name in [
                "Completed",
                "Stopped",
                "animated property requires a numeric start value",
            ] {
                let value = heap.alloc_string(name)?;
                heap.array_push(results, value)?;
            }
            let fields = heap.alloc_table()?;
            let clock = action::kind(heap, action)? == action::Kind::Clock;
            if clock {
                for (index, (name, initial)) in
                    [("ticks", Value::int(0)), ("elapsed", Value::float(0.))]
                        .into_iter()
                        .enumerate()
                {
                    let key = heap.alloc_string(name)?;
                    heap.table_store(fields, index as u32, key, initial)?;
                }
            }
            let serial = self
                .serial
                .checked_add(1)
                .ok_or(AdapterError::Input("activity identity exhausted"))?;
            self.serial = serial;
            let mut slots = [Value::NULL; RootSlot::Forcing as usize + 1];
            slots[RootSlot::Tag as usize] = Value::int(ACTIVITY);
            slots[RootSlot::State as usize] = Value::int(if paused {
                State::Paused
            } else {
                State::Running
            } as i64);
            slots[RootSlot::Description as usize] = action;
            slots[RootSlot::Owner as usize] = owner;
            slots[RootSlot::Graph as usize] = graph;
            slots[RootSlot::Results as usize] = results;
            slots[RootSlot::Serial as usize] = Value::int(serial as i64);
            slots[RootSlot::PausedAt as usize] = Value::float(self.time);
            slots[RootSlot::Forcing as usize] = Value::bool(false);
            let activity =
                heap.alloc_host(fields, &slots, if clock { None } else { Some(&READONLY) })?;
            stage.types.mark(
                heap,
                activity,
                if clock {
                    "reel::types::Clock"
                } else {
                    "reel::types::Activity"
                },
            )?;
            if clock {
                heap.observe_host_properties(activity, 0, 2)?;
            }
            let root = heap.add_root(root_owner, RootKind::Host, activity)?;
            if let Err(error) =
                vm.bind_awaitable(activity, aside_vm::bytecode::NativeId(native as u32))
            {
                vm.heap_mut().release_root(root)?;
                return Err(error.into());
            }
            self.native = Some(NativeWork {
                task: ticket.task,
                root,
                build: Some(graph::Build::new(activity)),
                command: None,
            });
        }
        self.pump_native(vm, stage, root_owner, ticket, budget)
    }

    pub fn command(
        &mut self,
        vm: &mut Vm,
        stage: &StageBinding,
        root_owner: RootOwner,
        ticket: HostTicket,
        command: Command,
        budget: u32,
    ) -> Result<(bool, u32), AdapterError> {
        if self.native.is_none() {
            let activity = vm.host_arguments(ticket)?[0];
            tagged(vm.heap(), activity, ACTIVITY)?;
            let old = state(vm.heap(), activity)?;
            // Reject before state, claims, counters or property writes change.
            if command == Command::Finish
                && !terminal(old)
                && action::duration(
                    vm.heap(),
                    root_get(vm.heap(), activity, RootSlot::Description)?,
                )?
                .is_infinite()
            {
                return Err(AdapterError::Input(
                    "continuous activity has no final value; use stop",
                ));
            }
            let root = vm
                .heap_mut()
                .add_root(root_owner, RootKind::Host, activity)?;
            self.native = Some(NativeWork {
                task: ticket.task,
                root,
                build: None,
                command: Some(command),
            });
            if !terminal(old) {
                let heap = vm.heap_mut();
                let scan = match command {
                    Command::Pause if old == State::Running => {
                        root_set(
                            heap,
                            activity,
                            RootSlot::State,
                            Value::int(State::Paused as i64),
                        )?;
                        root_set(heap, activity, RootSlot::PausedAt, Value::float(self.time))?;
                        Some(0.)
                    }
                    Command::Resume if old == State::Paused => {
                        root_set(
                            heap,
                            activity,
                            RootSlot::State,
                            Value::int(State::Running as i64),
                        )?;
                        Some(
                            self.time
                                - root_get(heap, activity, RootSlot::PausedAt)?
                                    .as_float()
                                    .unwrap(),
                        )
                    }
                    Command::Finish => {
                        if old == State::Paused {
                            root_set(
                                heap,
                                activity,
                                RootSlot::State,
                                Value::int(State::Running as i64),
                            )?;
                        }
                        root_set(heap, activity, RootSlot::Forcing, Value::bool(true))?;
                        Some(0.)
                    }
                    Command::Stop => {
                        self.begin_cleanup(heap, activity, State::Stopped, None)?;
                        None
                    }
                    _ => None,
                };
                if let Some(shift) = scan {
                    self.control = Some(Control {
                        activity,
                        command,
                        index: 0,
                        shift,
                    });
                }
            }
            self.begin_round(vm.heap(), self.time)?;
        }
        self.pump_native(vm, stage, root_owner, ticket, budget)
    }

    fn pump_native(
        &mut self,
        vm: &mut Vm,
        stage: &StageBinding,
        owner: RootOwner,
        ticket: HostTicket,
        budget: u32,
    ) -> Result<(bool, u32), AdapterError> {
        let mut operation = self.native.take().unwrap();
        debug_assert_eq!(operation.task, ticket.task);
        let result = (|| -> Result<(bool, u32), AdapterError> {
            let activity = vm.heap().root_value(operation.root)?;
            let mut work = 0;
            if let Some(build) = &mut operation.build {
                let (done, cost) = build.step(vm.heap_mut(), stage, budget)?;
                work += cost;
                if !done {
                    return Ok((false, work));
                }
                // Allocate scheduling roots before committing any property takeover.
                let heap = vm.heap_mut();
                if self.jobs.is_none() {
                    self.jobs = Some(heap.add_root(owner, RootKind::Host, Value::NULL)?);
                }
                if self.head.is_none() {
                    self.head = Some(heap.add_root(owner, RootKind::Host, Value::NULL)?);
                }
                let first = self.first(heap)?;
                root_set(heap, activity, RootSlot::Next, first)?;
                if present(first) {
                    root_set(heap, first, RootSlot::Previous, activity)?;
                }
                heap.set_root(self.head.unwrap(), activity)?;
                operation.build = None;
                if state(heap, activity)? == State::Running {
                    self.queue(
                        heap,
                        graph::node(heap, activity, 0)?,
                        graph::Job::Activate,
                        self.time,
                        false,
                    )?;
                }
                self.begin_round(heap, self.time)?;
            }
            let progress = self.drain(vm, budget - work)?;
            work += progress.work;
            if progress.pending {
                return Ok((false, work));
            }
            if operation.command.is_none() && state(vm.heap(), activity)? == State::Failed {
                let Completion::Error(error) = Self::response(vm.heap(), activity)? else {
                    unreachable!()
                };
                vm.respond(ticket, HostResponse::Error(error))?;
            } else if operation.command == Some(Command::Await) {
                self.subscribe(vm, ticket, activity)?;
            } else {
                vm.respond(ticket, HostResponse::Ready(activity))?;
            }
            Ok((true, work))
        })();
        if matches!(result, Ok((true, _))) {
            vm.heap_mut().release_root(operation.root)?;
            if self.head.is_none()
                && !present(self.job_head(vm.heap())?)
                && let Some(root) = self.jobs.take()
            {
                vm.heap_mut().release_root(root)?;
            }
        } else {
            self.native = Some(operation);
        }
        result
    }

    pub fn cancel_native(&mut self, heap: &mut Heap, task: TaskId) -> Result<(), AdapterError> {
        if self
            .native
            .as_ref()
            .is_some_and(|operation| operation.task == task)
        {
            let operation = self.native.take().unwrap();
            heap.release_root(operation.root)?;
        }
        // Committed producers and in-progress controls are independently owned;
        // their bounded work continues even if this native caller is cancelled.
        Ok(())
    }

    fn response(heap: &Heap, activity: Value) -> Result<Completion, AdapterError> {
        let status = state(heap, activity)?;
        let results = root_get(heap, activity, RootSlot::Results)?;
        Ok(match status {
            State::Cancelled | State::Failed => Completion::Error(aside_vm::NewFault {
                value: if status == State::Failed {
                    heap.sequence_get(results, 2)?
                } else {
                    Value::NULL
                },
                code: aside_vm::FaultCode::core(if status == State::Cancelled {
                    FaultKind::Cancelled
                } else {
                    FaultKind::Type
                }),
                cause: None,
            }),
            State::Completed | State::Stopped => {
                Completion::Ready(heap.sequence_get(results, u32::from(status == State::Stopped))?)
            }
            _ => unreachable!("only terminal activities deliver results"),
        })
    }

    fn subscribe(
        &mut self,
        vm: &mut Vm,
        ticket: HostTicket,
        activity: Value,
    ) -> Result<(), AdapterError> {
        if terminal(state(vm.heap(), activity)?) {
            match Self::response(vm.heap(), activity)? {
                Completion::Ready(value) => vm.respond(ticket, HostResponse::Ready(value))?,
                Completion::Error(error) => vm.respond(ticket, HostResponse::Error(error))?,
                _ => unreachable!(),
            }
        } else {
            let wait = ticket.wait_id();
            self.waits
                .try_reserve(1)
                .map_err(|_| AdapterError::Allocation)?;
            vm.respond(ticket, HostResponse::Pending(wait))?;
            self.waits.push((wait, activity, None));
            self.waits_dirty = true;
        }
        Ok(())
    }

    /// A native frame roots its awaited value; a cancelled consumer never stops
    /// the shared producer. Deliver only after a complete logical-clock round.
    pub fn maintain(&mut self, vm: &mut Vm) -> Result<bool, AdapterError> {
        if self.round.is_some() {
            return Ok(false);
        }
        if !self.waits_dirty && self.checked_wait_revision == vm.wait_revision() {
            return Ok(false);
        }
        let mut woke = false;
        let mut failure = None;
        #[cfg(test)]
        {
            self.wait_checks += self.waits.len() as u64;
        }
        self.waits.retain(|&(wait, activity, tick)| {
            if failure.is_some() {
                return true;
            }
            if !vm.wait_is_pending(wait) {
                return false;
            }
            match (|| -> Result<bool, AdapterError> {
                let status = state(vm.heap(), activity)?;
                if let Some(tick) = tick {
                    let ticks = vm
                        .heap()
                        .host_property(PropertyId {
                            host: activity,
                            field: 0,
                        })?
                        .as_int()
                        .expect("clock ticks are integers");
                    let result = if terminal(status) {
                        Completion::Error(Self::tick_cancelled())
                    } else if ticks > tick {
                        Completion::Ready(Value::int(ticks))
                    } else {
                        return Ok(true);
                    };
                    vm.complete(wait, result)?;
                    woke = true;
                    Ok(false)
                } else if terminal(status) {
                    vm.complete(wait, Self::response(vm.heap(), activity)?)?;
                    woke = true;
                    Ok(false)
                } else {
                    Ok(true)
                }
            })() {
                Ok(keep) => keep,
                Err(error) => {
                    failure = Some(error);
                    true
                }
            }
        });
        if let Some(error) = failure {
            return Err(error);
        }
        self.waits_dirty = false;
        self.checked_wait_revision = vm.wait_revision();
        Ok(woke)
    }

    pub fn revoke(&mut self, heap: &mut Heap) -> Result<(), AdapterError> {
        if let Some(operation) = self.native.take() {
            heap.release_root(operation.root)?;
        }
        for root in [self.head.take(), self.jobs.take()].into_iter().flatten() {
            heap.release_root(root)?;
        }
        self.job_tail = None;
        self.events.clear();
        self.deadline_cache.set(None);
        self.sampling = 0;
        self.clock_events = 0;
        self.cleanup = None;
        self.control = None;
        self.round = None;
        self.waits.clear();
        Ok(())
    }
}
