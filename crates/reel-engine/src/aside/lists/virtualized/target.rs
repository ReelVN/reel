//! Programmatic positioning and edge navigation own one keyed target per list.
use super::*;
use aside_vm::bytecode::{FunctionId, FunctionKind};
#[repr(u32)]
enum T {
    Key,
    Align,
    Result,
    Navigation,
    Expected,
    Armed,
}
fn field(h: &Heap, t: Value, f: T) -> Result<Value, HeapError> {
    h.sequence_get(t, f as u32)
}

/// Validate only this producer when its accepted barrier is about to settle.
pub(in crate::aside) fn presentation_current(
    heap: &mut Heap,
    stage: &stage::StageBinding,
    result: Value,
    owner: Value,
    evaluating: Option<Value>,
) -> Result<bool, AdapterError> {
    if owner.tag() == ValueTag::Null || crate::aside::scopes::is_scope(heap, owner) {
        return Ok(true);
    }
    let id = stage.id(heap, owner)?;
    if !matches!(
        stage.scene.node(id).map(|n| &n.kind),
        Some(NodeKind::VirtualList(_))
    ) {
        return Ok(true);
    }
    let s = state(heap, owner)?;
    let target = get(heap, s, S::Target)?;
    if target.tag() == ValueTag::Null || !field(heap, target, T::Result)?.same_bits(result) {
        return Ok(true);
    }
    if heap.derived_is_current(get(heap, s, S::Window)?, evaluating)? {
        return Ok(true);
    }
    heap.set_field(target, T::Armed as u32, Value::int(-1))?;
    // Rebuild even when an upstream derive publishes the same value, so this
    // invalidated result always gets a new measured window and accepted frame.
    let request = int(heap, s, S::Request)? + 1;
    put(heap, s, S::Request, Value::int(request))?;
    observable::publish(heap, get(heap, s, S::Demand)?, 0, Value::int(request))?;
    Ok(false)
}

impl SessionVm {
    fn virtual_scroll_state(&self, scroll: NodeId) -> Result<Option<Value>, AdapterError> {
        let list = self.stage.scene.node(scroll).and_then(|n| {
            n.children
                .iter()
                .find(|id| {
                    matches!(
                        self.stage.scene.node(**id).map(|n| &n.kind),
                        Some(NodeKind::VirtualList(_))
                    )
                })
                .copied()
        });
        list.map(|id| {
            let host = self.stage.value(self.vm.heap(), id)?;
            Ok(state(self.vm.heap(), host)?)
        })
        .transpose()
    }
    pub(in crate::aside) fn virtual_scroll_input(
        &mut self,
        scroll: NodeId,
    ) -> Result<(), AdapterError> {
        if let Some(s) = self.virtual_scroll_state(scroll)?
            && get(self.vm.heap(), s, S::Target)?.tag() != ValueTag::Null
        {
            self.virtual_target_cancel(s)?;
            self.virtual_target_demand(s)?;
        }
        Ok(())
    }
    /// Called only after the scroll property accepted this layout's requested
    /// offset. A clamp adjusts our prediction; it is not a newer author input.
    pub(in crate::aside) fn virtual_scroll_layout(
        &mut self,
        event: &reel_ui::stage::NodeScroll,
    ) -> Result<(), AdapterError> {
        let Some(s) = self.virtual_scroll_state(event.node)? else {
            return Ok(());
        };
        let t = get(self.vm.heap(), s, S::Target)?;
        if t.tag() == ValueTag::Null {
            return Ok(());
        }
        let expected = field(self.vm.heap(), t, T::Expected)?;
        if expected.tag() != ValueTag::Null
            && stage::number(expected)? == event.requested.y
            && event.requested.y != event.offset.y
        {
            self.virtual_target_invalidate(s)?;
            self.vm
                .heap_mut()
                .set_field(t, T::Expected as u32, Value::float(event.offset.y))?;
        }
        Ok(())
    }
    pub(in crate::aside) fn restore_virtual_presentation(
        &mut self,
        id: NodeId,
    ) -> Result<(), AdapterError> {
        if !matches!(
            self.stage.scene.node(id).unwrap().kind,
            NodeKind::VirtualList(_)
        ) {
            return Ok(());
        }
        let host = self.stage.value(self.vm.heap(), id)?;
        let s = state(self.vm.heap(), host)?;
        put(self.vm.heap_mut(), s, S::MeasuredRevision, Value::NULL)?;
        put(self.vm.heap_mut(), s, S::LastNavigation, Value::int(0))?;
        let t = get(self.vm.heap(), s, S::Target)?;
        if t.tag() == ValueTag::Null {
            return Ok(());
        }
        if field(self.vm.heap(), t, T::Navigation)?.tag() != ValueTag::Null {
            self.virtual_target_cancel(s)?;
        } else {
            self.virtual_target_invalidate(s)?;
            self.vm
                .heap_mut()
                .set_field(t, T::Expected as u32, Value::NULL)?;
        }
        Ok(())
    }
    pub(super) fn bind_list_scroll_to(&mut self, host: Value) -> Result<(), AdapterError> {
        let p = self.vm.program().package();
        let function = p
            .functions
            .iter()
            .enumerate()
            .find_map(|(i, f)| match f.kind {
                FunctionKind::Native(n)
                    if p.symbols[p.natives[n.index()].symbol.index()]
                        == "reel::list::scroll_to" =>
                {
                    Some(FunctionId(i as u32))
                }
                _ => None,
            })
            .ok_or(AdapterError::Input("list scroll_to binding missing"))?;
        let method = self.vm.bind_native(host, function)?;
        let h = self.vm.heap_mut();
        let fields = h.host_fields(host)?;
        let key = h.alloc_string("scroll_to")?;
        h.table_store(fields, h.table_len(fields)?, key, method)?;
        Ok(())
    }
    pub(in crate::aside) fn dispatch_list_scroll_to(
        &mut self,
        ticket: HostTicket,
    ) -> Result<(), AdapterError> {
        if ticket.context.no_wait || ticket.context.init || ticket.context.derived {
            return Err(AdapterError::Vm(aside_vm::executor::VmError::Effect));
        }
        let args = self.vm.host_arguments(ticket)?;
        let (host, key) = (args[0], args[1]);
        let align = match text::read_string(self.vm.heap(), args[2], self.read_limit)?.as_str() {
            "nearest" => 0,
            "start" => 1,
            "center" => 2,
            "end" => 3,
            _ => {
                return Err(AdapterError::Input(
                    "scroll_to align must be nearest, start, center or end",
                ));
            }
        };
        let id = self.stage.id(self.vm.heap(), host)?;
        if !matches!(
            self.stage.scene.node(id).unwrap().kind,
            NodeKind::VirtualList(_)
        ) {
            return Err(AdapterError::Input("scroll_to requires a virtual list"));
        }
        let s = state(self.vm.heap(), host)?;
        self.virtual_target_cancel(s)?;
        let result = self
            .results
            .create_presentation(&mut self.vm, self.root_owner, host, None)?;
        let target = self.vm.heap_mut().alloc_array(&[
            key,
            Value::int(align),
            result,
            Value::NULL,
            Value::NULL,
            Value::int(-1),
        ])?;
        put(self.vm.heap_mut(), s, S::Target, target)?;
        self.virtual_target_demand(s)?;
        self.vm.respond(ticket, HostResponse::Ready(result))?;
        self.issued = None;
        Ok(())
    }
    pub(super) fn virtual_target_demand(&mut self, s: Value) -> Result<(), AdapterError> {
        let request = int(self.vm.heap(), s, S::Request)? + 1;
        put(self.vm.heap_mut(), s, S::Request, Value::int(request))?;
        let demand = get(self.vm.heap(), s, S::Demand)?;
        observable::publish(self.vm.heap_mut(), demand, 0, Value::int(request))?;
        Ok(())
    }
    pub(super) fn virtual_target_cancel(&mut self, s: Value) -> Result<(), AdapterError> {
        let t = get(self.vm.heap(), s, S::Target)?;
        if t.tag() != ValueTag::Null {
            let result = field(self.vm.heap(), t, T::Result)?;
            if result.tag() != ValueTag::Null && results::pending(self.vm.heap_mut(), result)? {
                results::finish_task(self.vm.heap_mut(), result, results::TaskResult::Cancelled)?;
                self.results.changed();
            }
            put(self.vm.heap_mut(), s, S::Target, Value::NULL)?;
            put(self.vm.heap_mut(), s, S::Navigation, Value::NULL)?;
        }
        Ok(())
    }
    pub(in crate::aside) fn virtual_result_cancelled(
        &mut self,
        owner: Value,
        result: Value,
    ) -> Result<(), AdapterError> {
        let Ok(id) = self.stage.id(self.vm.heap(), owner) else {
            return Ok(());
        };
        if !matches!(
            self.stage.scene.node(id).unwrap().kind,
            NodeKind::VirtualList(_)
        ) {
            return Ok(());
        }
        let s = state(self.vm.heap(), owner)?;
        let t = get(self.vm.heap(), s, S::Target)?;
        if t.tag() != ValueTag::Null && field(self.vm.heap(), t, T::Result)?.same_bits(result) {
            self.virtual_target_cancel(s)?;
            self.virtual_target_demand(s)?;
        }
        Ok(())
    }
    pub(super) fn virtual_target_entry(&mut self, s: Value) -> Result<Option<Value>, AdapterError> {
        let t = get(self.vm.heap(), s, S::Target)?;
        if t.tag() == ValueTag::Null {
            return Ok(None);
        }
        let result = field(self.vm.heap(), t, T::Result)?;
        if result.tag() != ValueTag::Null && !results::pending(self.vm.heap_mut(), result)? {
            self.virtual_target_cancel(s)?;
            return Ok(None);
        }
        let key = field(self.vm.heap(), t, T::Key)?;
        let e = find(
            self.vm.heap(),
            get(self.vm.heap(), s, S::Sorted)?,
            key,
            self.read_limit,
        )?;
        if e.is_none() {
            self.virtual_target_cancel(s)?;
        }
        Ok(e)
    }
    pub(super) fn virtual_target_invalidate(&mut self, s: Value) -> Result<(), AdapterError> {
        let t = get(self.vm.heap(), s, S::Target)?;
        if t.tag() == ValueTag::Null {
            return Ok(());
        }
        let result = field(self.vm.heap(), t, T::Result)?;
        if result.tag() != ValueTag::Null && results::pending(self.vm.heap_mut(), result)? {
            self.results.disarm_presentation(&mut self.vm, result)?;
        }
        self.vm
            .heap_mut()
            .set_field(t, T::Armed as u32, Value::int(-1))?;
        Ok(())
    }
    /// A target replaces reading-anchor correction until it completes or is cancelled.
    pub(super) fn virtual_target_position(&mut self, s: Value) -> Result<bool, AdapterError> {
        let Some(e) = self.virtual_target_entry(s)? else {
            return Ok(false);
        };
        let t = get(self.vm.heap(), s, S::Target)?;
        let scroll = get(self.vm.heap(), s, S::Scroll)?;
        let old_offset = num(self.vm.heap(), s, S::Offset)?;
        let expected = field(self.vm.heap(), t, T::Expected)?;
        if scroll.tag() != ValueTag::Null && expected.tag() != ValueTag::Null {
            let actual = self.vm.heap().host_property(aside_vm::PropertyId {
                host: scroll,
                field: stage::scroll::ScrollField::Y.index() as u32,
            })?;
            if stage::number(actual)? != stage::number(expected)? {
                self.virtual_target_cancel(s)?;
                return Ok(false);
            }
        }
        let index = self.vm.heap().sequence_get(e, INDEX)?.as_int().unwrap() as u32;
        let top = prefix(self.vm.heap(), s, index)?;
        let row_height = height(self.vm.heap(), s, e)?;
        let start = num(self.vm.heap(), s, S::Start)?;
        let viewport = num(self.vm.heap(), s, S::End)? - start;
        let bottom = top + row_height;
        let align = field(self.vm.heap(), t, T::Align)?.as_int().unwrap();
        let next = match align {
            1 => top,
            2 => top + (row_height - viewport) / 2.,
            3 => bottom - viewport,
            _ if (top >= start && bottom <= start + viewport)
                || (top <= start && bottom >= start + viewport) =>
            {
                start
            }
            _ if (top - start).abs() <= (bottom - viewport - start).abs() => top,
            _ => bottom - viewport,
        };
        let padding = gap_padding(self.vm.heap(), s)?;
        let offset = (old_offset + next - start).clamp(
            0.,
            (total(self.vm.heap(), s)? + padding * 2. - viewport).max(0.),
        );
        let next = start + offset - old_offset;
        if scroll.tag() != ValueTag::Null {
            self.vm.heap_mut().write_host_property(
                aside_vm::PropertyId {
                    host: scroll,
                    field: stage::scroll::ScrollField::Y.index() as u32,
                },
                Value::float(offset),
                true,
            )?;
            self.vm
                .heap_mut()
                .set_field(t, T::Expected as u32, Value::float(offset))?;
        }
        put(self.vm.heap_mut(), s, S::Start, Value::float(next))?;
        put(self.vm.heap_mut(), s, S::End, Value::float(next + viewport))?;
        put(self.vm.heap_mut(), s, S::Offset, Value::float(offset))?;
        let nav = field(self.vm.heap(), t, T::Navigation)?;
        if nav.tag() != ValueTag::Null {
            let requested = self.vm.heap().sequence_get(nav, 0)?;
            let forward = self.vm.heap().sequence_get(nav, 1)?;
            let serial = self.vm.heap().sequence_get(nav, 2)?;
            let projected = self.vm.heap_mut().alloc_array(&[
                requested,
                forward,
                Value::int(index as i64),
                serial,
                Value::bool(false),
            ])?;
            put(self.vm.heap_mut(), s, S::Navigation, projected)?;
        }
        Ok(true)
    }
    pub(super) fn virtual_target_feedback(
        &mut self,
        s: Value,
        event: &NodeVirtualList,
    ) -> Result<bool, AdapterError> {
        let mut changed = false;
        let t = get(self.vm.heap(), s, S::Target)?;
        if t.tag() != ValueTag::Null {
            let expected = field(self.vm.heap(), t, T::Expected)?;
            if expected.tag() != ValueTag::Null && stage::number(expected)? != event.offset.y {
                self.virtual_target_cancel(s)?;
                changed = true;
            }
        }
        let t = get(self.vm.heap(), s, S::Target)?;
        let old = if t.tag() == ValueTag::Null {
            Value::NULL
        } else {
            field(self.vm.heap(), t, T::Navigation)?
        };
        if let Some(nav) = event.navigation {
            let same = int(self.vm.heap(), s, S::LastNavigation)? as u64 == nav.request;
            if !same && nav.index < count(self.vm.heap(), s)? as usize {
                put(
                    self.vm.heap_mut(),
                    s,
                    S::LastNavigation,
                    Value::int(nav.request as i64),
                )?;
                self.virtual_target_cancel(s)?;
                let e = self
                    .vm
                    .heap()
                    .sequence_get(get(self.vm.heap(), s, S::Order)?, nav.index as u32)?;
                let key = entry(self.vm.heap(), e, Entry::Key)?;
                let nav = self.vm.heap_mut().alloc_array(&[
                    Value::int(nav.index as i64),
                    Value::bool(nav.forward),
                    Value::int(nav.request as i64),
                ])?;
                let t = self.vm.heap_mut().alloc_array(&[
                    key,
                    Value::int(0),
                    Value::NULL,
                    nav,
                    Value::float(event.offset.y),
                    Value::int(-1),
                ])?;
                put(self.vm.heap_mut(), s, S::Target, t)?;
                changed = true;
            }
        } else if old.tag() != ValueTag::Null {
            self.virtual_target_cancel(s)?;
            changed = true;
        }
        Ok(changed)
    }
    pub(super) fn virtual_target_arm(&mut self, s: Value) -> Result<bool, AdapterError> {
        let Some(e) = self.virtual_target_entry(s)? else {
            return Ok(false);
        };
        let t = get(self.vm.heap(), s, S::Target)?;
        let result = field(self.vm.heap(), t, T::Result)?;
        let revision = get(self.vm.heap(), s, S::Revision)?;
        if field(self.vm.heap(), t, T::Armed)?.same_bits(revision)
            || !get(self.vm.heap(), s, S::MeasuredRevision)?.same_bits(revision)
            || self.vm.heap().sequence_get(e, EPOCH)?.as_int()
                != Some(int(self.vm.heap(), s, S::Epoch)?)
        {
            return Ok(false);
        }
        let window = get(self.vm.heap(), s, S::Window)?;
        if !self.vm.heap_mut().derived_is_current(window, None)? {
            return Ok(false);
        }
        let child = entry(self.vm.heap(), e, Entry::Node)?;
        if child.tag() == ValueTag::Null {
            return Ok(false);
        }
        if result.tag() == ValueTag::Null {
            let nav = get(self.vm.heap(), s, S::Navigation)?;
            self.vm.heap_mut().set_field(nav, 4, Value::bool(true))?;
            commit_projection(self.vm.heap_mut(), s)?;
            let host = node(self.vm.heap(), s)?;
            self.stage.publish_virtual(self.vm.heap(), host)?;
        } else {
            self.results.arm_presentation(&mut self.vm, result, child)?;
        }
        self.vm.heap_mut().set_field(t, T::Armed as u32, revision)?;
        Ok(true)
    }
}
fn gap_padding(h: &Heap, s: Value) -> Result<f64, AdapterError> {
    stage::number(
        h.table_entry(
            h.host_fields(node(h, s)?)?,
            stage::NodeProperty::Padding.index() as u32,
        )?
        .1,
    )
}
