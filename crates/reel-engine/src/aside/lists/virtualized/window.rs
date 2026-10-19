use super::*;

impl SessionVm {
    pub(super) fn virtual_window(&mut self, t: HostTicket, s: Value) -> Result<(), AdapterError> {
        if get(self.vm.heap(), s, S::Fault)?.as_bool() == Some(true) {
            return Err(AdapterError::Input(
                "virtual list rows must have positive natural height",
            ));
        }
        let output = get(self.vm.heap(), s, S::Window)?;
        let w = Work {
            output,
            state: s,
            source: Value::NULL,
            entries: Value::NULL,
            sorted: Value::NULL,
            children: Value::NULL,
            index: 0,
            version: 0,
        };
        self.vm.begin_reactive(
            t,
            output,
            false,
            Continuation {
                version: 2,
                stage: PREPARE_WINDOW,
                values: &w.values(),
                faults: FaultRetention::Preserve,
            },
        )?;
        Ok(())
    }
    pub(super) fn virtual_prepare_window(
        &mut self,
        t: HostTicket,
        s: Value,
    ) -> Result<(), AdapterError> {
        self.virtual_target_position(s)?;
        let output = get(self.vm.heap(), s, S::Window)?;
        let source = get(self.vm.heap(), s, S::Source)?;
        let demand = get(self.vm.heap(), s, S::Demand)?;
        self.vm.heap_mut().observe_read(source, Some(0))?;
        self.vm.heap_mut().observe_read(demand, Some(0))?;
        let host = node(self.vm.heap(), s)?;
        self.vm.heap_mut().commit_derived(output, host)?;
        self.vm.heap_mut().begin_untracked(output)?;
        let n = count(self.vm.heap(), s)?;
        let start = num(self.vm.heap(), s, S::Start)?;
        let end = num(self.vm.heap(), s, S::End)?;
        let first = locate(self.vm.heap(), s, (start - (end - start)).max(0.))?;
        let last = if end > start {
            locate(self.vm.heap(), s, end + (end - start))?
        } else {
            first
        };
        let candidates = self.vm.heap_mut().alloc_array(&[])?;
        let order = get(self.vm.heap(), s, S::Order)?;
        let pins = get(self.vm.heap(), s, S::Pinned)?;
        let nav = get(self.vm.heap(), s, S::Navigation)?;
        let capacity = usize::try_from(last.saturating_sub(first) + 1).unwrap()
            + self.vm.heap().sequence_len(pins)? as usize
            + 1;
        let _reservation = self
            .vm
            .heap()
            .budget_usage()
            .reserve((capacity * size_of::<u32>()) as u64)?;
        let mut indices = Vec::new();
        indices
            .try_reserve_exact(capacity)
            .map_err(|_| AdapterError::Allocation)?;
        if n > 0 {
            indices.extend(first..=last.min(n - 1));
        }
        for i in 0..self.vm.heap().sequence_len(pins)? {
            let key = self.vm.heap().sequence_get(pins, i)?;
            if let Some(e) = find(
                self.vm.heap(),
                get(self.vm.heap(), s, S::Sorted)?,
                key,
                self.read_limit,
            )? {
                indices.push(self.vm.heap().sequence_get(e, INDEX)?.as_int().unwrap() as u32);
            }
        }
        if nav.tag() != ValueTag::Null {
            let i = self.vm.heap().sequence_get(nav, 2)?.as_int().unwrap() as u32;
            if i < n {
                indices.push(i);
            }
        }
        if let Some(e) = self.virtual_target_entry(s)? {
            indices.push(self.vm.heap().sequence_get(e, INDEX)?.as_int().unwrap() as u32);
        }
        indices.sort_unstable();
        indices.dedup();
        for i in indices {
            let e = self.vm.heap().sequence_get(order, i)?;
            self.vm.heap_mut().array_push(candidates, e)?;
        }
        let children = self.vm.heap_mut().alloc_array(&[])?;
        let w = Work {
            output,
            state: s,
            source: candidates,
            entries: Value::NULL,
            sorted: Value::NULL,
            children,
            index: 0,
            version: int(self.vm.heap(), s, S::Request)? as u64,
        };
        self.virtual_continue(t, &w, WINDOW)
    }
    pub(super) fn virtual_discard_candidate(&mut self, w: &Work) -> Result<(), AdapterError> {
        for i in 0..self.vm.heap().sequence_len(w.source)? {
            let e = self.vm.heap().sequence_get(w.source, i)?;
            let child = entry(self.vm.heap(), e, Entry::Node)?;
            if child.tag() != ValueTag::Null
                && self.stage.discard_unmounted(self.vm.heap_mut(), child)?
            {
                self.vm
                    .heap_mut()
                    .set_field(e, Entry::Row as u32, Value::NULL)?;
                self.vm
                    .heap_mut()
                    .set_field(e, Entry::Node as u32, Value::NULL)?;
            }
        }
        Ok(())
    }
    pub(super) fn virtual_build_window(
        &mut self,
        t: HostTicket,
        mut w: Work,
        phase: u64,
    ) -> Result<(), AdapterError> {
        match phase {
            WINDOW => {
                if w.version != int(self.vm.heap(), w.state, S::Request)? as u64 {
                    self.virtual_discard_candidate(&w)?;
                    self.vm.heap_mut().end_derived(w.output)?;
                    self.virtual_window(t, w.state)?;
                } else if w.index < self.vm.heap().sequence_len(w.source)? {
                    let e = self.vm.heap().sequence_get(w.source, w.index)?;
                    let child = entry(self.vm.heap(), e, Entry::Node)?;
                    if child.tag() != ValueTag::Null {
                        self.vm.heap_mut().array_push(w.children, child)?;
                        w.index += 1;
                        self.virtual_continue(t, &w, WINDOW)?;
                    } else {
                        let key = entry(self.vm.heap(), e, Entry::Key)?;
                        let item = entry(self.vm.heap(), e, Entry::Item)?;
                        let index = self.vm.heap().sequence_get(e, INDEX)?;
                        let row = observable::record(
                            self.vm.heap_mut(),
                            &[("key", key), ("value", item), ("index", index)],
                            &[],
                        )?;
                        self.vm.heap_mut().set_field(e, Entry::Row as u32, row)?;
                        self.virtual_call(t, &w, BUILT, self.vm.host_arguments(t)?[2], row)?;
                    }
                } else {
                    let old = get(self.vm.heap(), w.state, S::Mounted)?;
                    for i in 0..self.vm.heap().sequence_len(w.source)? {
                        let e = self.vm.heap().sequence_get(w.source, i)?;
                        let row = entry(self.vm.heap(), e, Entry::Row)?;
                        let item = entry(self.vm.heap(), e, Entry::Item)?;
                        let index = self.vm.heap().sequence_get(e, INDEX)?;
                        observable::publish(self.vm.heap_mut(), row, 1, item)?;
                        observable::publish(self.vm.heap_mut(), row, 2, index)?;
                    }
                    self.vm.heap_mut().end_derived(w.output)?;
                    let host = node(self.vm.heap(), w.state)?;
                    self.stage.reconcile_list(
                        self.vm.heap_mut(),
                        host,
                        w.children,
                        self.read_limit,
                    )?;
                    for i in 0..self.vm.heap().sequence_len(old)? {
                        let e = self.vm.heap().sequence_get(old, i)?;
                        let kept = (0..self.vm.heap().sequence_len(w.source)?).any(|j| {
                            self.vm
                                .heap()
                                .sequence_get(w.source, j)
                                .is_ok_and(|v| v.same_bits(e))
                        });
                        if !kept {
                            let key = entry(self.vm.heap(), e, Entry::Key)?;
                            if let Some(current) = find(
                                self.vm.heap(),
                                get(self.vm.heap(), w.state, S::Sorted)?,
                                key,
                                self.read_limit,
                            )? {
                                let mounted =
                                    (0..self.vm.heap().sequence_len(w.source)?).any(|j| {
                                        self.vm
                                            .heap()
                                            .sequence_get(w.source, j)
                                            .is_ok_and(|v| v.same_bits(current))
                                    });
                                if !mounted {
                                    self.vm.heap_mut().set_field(
                                        current,
                                        Entry::Row as u32,
                                        Value::NULL,
                                    )?;
                                    self.vm.heap_mut().set_field(
                                        current,
                                        Entry::Node as u32,
                                        Value::NULL,
                                    )?;
                                }
                            }
                            self.vm
                                .heap_mut()
                                .set_field(e, Entry::Row as u32, Value::NULL)?;
                            self.vm
                                .heap_mut()
                                .set_field(e, Entry::Node as u32, Value::NULL)?;
                        }
                    }
                    put(self.vm.heap_mut(), w.state, S::Mounted, w.source)?;
                    let revision = int(self.vm.heap(), w.state, S::Revision)? + 1;
                    put(
                        self.vm.heap_mut(),
                        w.state,
                        S::Revision,
                        Value::int(revision),
                    )?;
                    commit_projection(self.vm.heap_mut(), w.state)?;
                    self.stage.publish_virtual(self.vm.heap(), host)?;
                    self.virtual_target_arm(w.state)?;
                    self.vm.respond(t, HostResponse::Ready(host))?;
                }
            }
            BUILT => {
                let child = self.vm.host_callback_value(t)?;
                if child.tag() == ValueTag::Null {
                    return Err(AdapterError::Input("virtual list build must return a node"));
                }
                self.stage.id(self.vm.heap(), child)?;
                let e = self.vm.heap().sequence_get(w.source, w.index)?;
                self.vm.heap_mut().set_field(e, Entry::Node as u32, child)?;
                self.vm.heap_mut().array_push(w.children, child)?;
                w.index += 1;
                self.virtual_continue(t, &w, WINDOW)?;
            }
            _ => unreachable!("window continuation phase"),
        }
        Ok(())
    }
}
