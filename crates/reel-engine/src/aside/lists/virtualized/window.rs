use super::*;

impl SessionVm {
    fn virtual_window_fault(&self, s: Value) -> Result<(), AdapterError> {
        let fault = get(self.vm.heap(), s, S::Fault)?;
        if fault.as_bool() == Some(true) {
            return Err(AdapterError::Input(
                "virtual list rows must have positive natural height",
            ));
        }
        match fault.as_int() {
            Some(1) => Err(AdapterError::Input(
                "virtual list target cannot be reached along its scroll owner's vertical axis",
            )),
            Some(2) => Err(AdapterError::Input(
                "virtual list target is outside its scroll owner's reachable range",
            )),
            _ => Ok(()),
        }
    }
    pub(super) fn virtual_window(&mut self, t: HostTicket, s: Value) -> Result<(), AdapterError> {
        self.virtual_window_fault(s)?;
        let candidate = get(self.vm.heap(), s, S::Candidate)?;
        if candidate.tag() != ValueTag::Null {
            let mut values = [Value::NULL; 8];
            for (i, value) in values.iter_mut().enumerate() {
                *value = self.vm.heap().sequence_get(candidate, i as u32)?;
            }
            let w = Work::load(&values);
            self.vm.heap_mut().begin_untracked(w.output)?;
            return self.virtual_continue(t, &w, WINDOW);
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
                version: 1,
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
        self.virtual_window_fault(s)?;
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
        let reserve = (end - start) * OVERSCAN;
        let first = locate(self.vm.heap(), s, (start - reserve).max(0.))?;
        let last = if end > start {
            locate(self.vm.heap(), s, end + reserve)?
        } else {
            first
        };
        let candidates = self.vm.heap_mut().alloc_array(&[])?;
        let order = get(self.vm.heap(), s, S::Order)?;
        let pins = get(self.vm.heap(), s, S::Pinned)?;
        let nav = get(self.vm.heap(), s, S::Navigation)?;
        let visible = end > start || get(self.vm.heap(), s, S::Scroll)?.tag() == ValueTag::Null;
        let capacity = if visible {
            usize::try_from(last.saturating_sub(first) + 1).unwrap()
        } else {
            0
        } + self.vm.heap().sequence_len(pins)? as usize
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
        if n > 0 && visible {
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
        let constraints = [
            get(self.vm.heap(), s, S::Width)?,
            get(self.vm.heap(), s, S::Target)?,
            Value::int(i64::from(first)),
            Value::int(i64::from(last.min(n.saturating_sub(1)))),
            nav,
        ];
        let constraints = self.vm.heap_mut().alloc_tuple(&constraints)?;
        let w = Work {
            output,
            state: s,
            source: candidates,
            // Advance only on successful source publication, independently of
            // viewport requests, including edits to the same input array.
            entries: get(self.vm.heap(), s, S::SourceRevision)?,
            sorted: constraints,
            children,
            index: 0,
            version: int(self.vm.heap(), s, S::Request)? as u64,
        };
        let saved = self.vm.heap_mut().alloc_array(&w.values())?;
        put(self.vm.heap_mut(), s, S::Candidate, saved)?;
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
        put(self.vm.heap_mut(), w.state, S::Candidate, Value::NULL)?;
        Ok(())
    }
    fn virtual_candidate_covers(&self, w: &Work) -> Result<bool, AdapterError> {
        let h = self.vm.heap();
        let s = w.state;
        if !w.entries.same_bits(get(h, s, S::SourceRevision)?)
            || h.derived_is_queued(get(h, s, S::Source)?)?
            || !h.sequence_get(w.sorted, 0)?.same_bits(get(h, s, S::Width)?)
            || !h
                .sequence_get(w.sorted, 1)?
                .same_bits(get(h, s, S::Target)?)
            || !h
                .sequence_get(w.sorted, 4)?
                .same_bits(get(h, s, S::Navigation)?)
        {
            return Ok(false);
        }
        if w.version == int(h, s, S::Request)? as u64 {
            return Ok(true);
        }
        if num(h, s, S::End)? <= num(h, s, S::Start)?
            && get(h, s, S::Scroll)?.tag() != ValueTag::Null
        {
            return Ok(false);
        }
        let includes = |index: u32| -> Result<bool, AdapterError> {
            let mut lo = 0;
            let mut hi = h.sequence_len(w.source)?;
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let entry = h.sequence_get(w.source, mid)?;
                let found = h.sequence_get(entry, INDEX)?.as_int().unwrap() as u32;
                match found.cmp(&index) {
                    std::cmp::Ordering::Less => lo = mid + 1,
                    std::cmp::Ordering::Greater => hi = mid,
                    std::cmp::Ordering::Equal => return Ok(true),
                }
            }
            Ok(false)
        };
        if count(h, s)? > 0 && num(h, s, S::End)? > num(h, s, S::Start)? {
            let first = locate(h, s, num(h, s, S::Start)?.max(0.))?;
            let last = locate(h, s, num(h, s, S::End)?.max(0.))?;
            if i64::from(first) < h.sequence_get(w.sorted, 2)?.as_int().unwrap()
                || i64::from(last) > h.sequence_get(w.sorted, 3)?.as_int().unwrap()
                || !includes(first)?
                || !includes(last)?
            {
                return Ok(false);
            }
        }
        for i in 0..h.sequence_len(get(h, s, S::Pinned)?)? {
            let key = h.sequence_get(get(h, s, S::Pinned)?, i)?;
            if let Some(entry) = find(h, get(h, s, S::Sorted)?, key, self.read_limit)?
                && !includes(h.sequence_get(entry, INDEX)?.as_int().unwrap() as u32)?
            {
                return Ok(false);
            }
        }
        // The unchanged target was included at preparation. A replacement or
        // cancellation fails the identity constraint above.
        Ok(true)
    }
    pub(super) fn virtual_build_window(
        &mut self,
        t: HostTicket,
        mut w: Work,
        phase: u64,
    ) -> Result<(), AdapterError> {
        match phase {
            WINDOW => {
                if !self.virtual_candidate_covers(&w)? {
                    self.virtual_discard_candidate(&w)?;
                    self.vm.heap_mut().end_derived(w.output)?;
                    if self
                        .vm
                        .heap()
                        .derived_is_queued(get(self.vm.heap(), w.state, S::Source)?)?
                    {
                        self.vm
                            .respond(t, HostResponse::Ready(node(self.vm.heap(), w.state)?))?;
                    } else {
                        self.virtual_window(t, w.state)?;
                    }
                } else if w.index < self.vm.heap().sequence_len(w.source)? {
                    w.version = int(self.vm.heap(), w.state, S::Request)? as u64;
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
                        let kept = mounted_contains(self.vm.heap(), w.source, e)?;
                        if !kept {
                            let key = entry(self.vm.heap(), e, Entry::Key)?;
                            if let Some(current) = find(
                                self.vm.heap(),
                                get(self.vm.heap(), w.state, S::Sorted)?,
                                key,
                                self.read_limit,
                            )? {
                                let mounted = mounted_contains(self.vm.heap(), w.source, current)?;
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
                    put(self.vm.heap_mut(), w.state, S::Candidate, Value::NULL)?;
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
                if int(self.vm.heap(), w.state, S::Revision)? == 0 {
                    return self.virtual_continue(t, &w, WINDOW);
                }
                let saved = get(self.vm.heap(), w.state, S::Candidate)?;
                for (i, value) in w.values().into_iter().enumerate() {
                    self.vm.heap_mut().set_field(saved, i as u32, value)?;
                }
                // A whole row factory has returned. Drain ordinary observers
                // before continuing, without publishing detached candidate rows.
                self.vm.heap_mut().end_derived(w.output)?;
                self.vm.heap_mut().schedule_derived(w.output)?;
                self.vm
                    .respond(t, HostResponse::Ready(node(self.vm.heap(), w.state)?))?;
            }
            _ => unreachable!("window continuation phase"),
        }
        Ok(())
    }

    pub(in crate::aside) fn virtual_feedback_output(&self, output: Value) -> bool {
        let check = || -> Result<bool, AdapterError> {
            let h = self.vm.heap();
            let s = h.derived_state(output)?;
            if s.tag() == ValueTag::Null || !output.same_bits(get(h, s, S::Window)?) {
                return Ok(false);
            }
            let host = node(h, s)?;
            let id = self.stage.id(h, host)?;
            if !matches!(
                self.stage.scene.node(id).map(|n| &n.kind),
                Some(NodeKind::VirtualList(_))
            ) || get(h, s, S::Target)?.tag() != ValueTag::Null
                || get(h, s, S::Navigation)?.tag() != ValueTag::Null
                || h.derived_is_queued(get(h, s, S::Source)?)?
            {
                return Ok(false);
            }
            let projection = get(h, s, S::Projection)?;
            Ok(h.sequence_get(projection, 4)?
                .same_bits(get(h, s, S::SourceRevision)?)
                && int(h, s, S::Revision)? > 0)
        };
        check().unwrap_or(false)
    }
}

/// Candidate rows are ordered by their current logical index, including pinned
/// rows. Identity distinguishes an obsolete entry from its keyed replacement.
fn mounted_contains(heap: &Heap, rows: Value, needle: Value) -> Result<bool, HeapError> {
    let wanted = heap.sequence_get(needle, INDEX)?.as_int().unwrap();
    let mut lo = 0;
    let mut hi = heap.sequence_len(rows)?;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let entry = heap.sequence_get(rows, mid)?;
        match heap
            .sequence_get(entry, INDEX)?
            .as_int()
            .unwrap()
            .cmp(&wanted)
        {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => return Ok(entry.same_bits(needle)),
        }
    }
    Ok(false)
}
