//! Viewport materialization shares the keyed list's VM graph and native frame.
//! The source observer indexes data; a second observer reacts only to layout.
use super::*;
use reel_ui::stage::{
    NodeId, NodeKind, NodeVirtualList, VirtualListNavigation, VirtualListView, VirtualRow,
};
use std::sync::Arc;

#[derive(Clone, Copy)]
#[repr(u32)]
enum S {
    Node,
    Sorted,
    Order,
    Tree,
    Source,
    Window,
    Demand,
    Mounted,
    Revision,
    Width,
    Start,
    End,
    Offset,
    Scroll,
    Estimate,
    Epoch,
    AnchorKey,
    AnchorIndex,
    AnchorOffset,
    Pinned,
    Navigation,
    Request,
    AtEnd,
    Initialized,
    EstimateReady,
    Projection,
    Fault,
}
const STATE_LEN: usize = S::Fault as usize + 1;
const HEIGHT: u32 = 4;
const INDEX: u32 = 5;
const EPOCH: u32 = 6;
const SOURCE: u64 = 0;
const KEY: u64 = 1;
const INDEX_ROWS: u64 = 2;
const INDEX_HEIGHTS: u64 = 3;
const WINDOW: u64 = 4;
const BUILT: u64 = 5;
const PREPARE_WINDOW: u64 = 6;

fn get(h: &Heap, s: Value, f: S) -> Result<Value, HeapError> {
    h.sequence_get(s, f as u32)
}
fn put(h: &mut Heap, s: Value, f: S, v: Value) -> Result<(), HeapError> {
    h.set_field(s, f as u32, v)
}
fn num(h: &Heap, s: Value, f: S) -> Result<f64, AdapterError> {
    stage::number(get(h, s, f)?)
}
fn int(h: &Heap, s: Value, f: S) -> Result<i64, HeapError> {
    Ok(get(h, s, f)?.as_int().unwrap())
}
fn node(h: &Heap, s: Value) -> Result<Value, HeapError> {
    get(h, s, S::Node)
}
fn state(h: &Heap, host: Value) -> Result<Value, HeapError> {
    h.derived_state(h.host_payload(host, 3)?)
}
fn count(h: &Heap, s: Value) -> Result<u32, HeapError> {
    h.sequence_len(get(h, s, S::Order)?)
}
fn gap(h: &Heap, s: Value) -> Result<f64, AdapterError> {
    stage::number(
        h.table_entry(
            h.host_fields(node(h, s)?)?,
            stage::NodeProperty::Gap.index() as u32,
        )?
        .1,
    )
}
fn height(h: &Heap, s: Value, e: Value) -> Result<f64, AdapterError> {
    if h.sequence_get(e, EPOCH)?.as_int() == Some(int(h, s, S::Epoch)?) {
        let value = stage::number(h.sequence_get(e, HEIGHT)?)?;
        if value > 0. {
            return Ok(value);
        }
    }
    num(h, s, S::Estimate)
}
// A Fenwick tree of measured-height corrections. Epoch tags invalidate width
// dependent measurements in O(1), without visiting unmounted author data.
fn sum(h: &Heap, tree: Value, mut n: u32, epoch: i64) -> Result<f64, AdapterError> {
    let mut value = 0.;
    while n > 0 {
        if h.sequence_get(tree, n * 2)?.as_int() == Some(epoch) {
            value += stage::number(h.sequence_get(tree, n * 2 + 1)?)?;
        }
        n &= n - 1;
    }
    Ok(value)
}
fn add(
    h: &mut Heap,
    tree: Value,
    count: u32,
    mut i: u32,
    epoch: i64,
    delta: f64,
) -> Result<(), AdapterError> {
    i += 1;
    while i <= count {
        let old = if h.sequence_get(tree, i * 2)?.as_int() == Some(epoch) {
            stage::number(h.sequence_get(tree, i * 2 + 1)?)?
        } else {
            0.
        };
        h.set_field(tree, i * 2, Value::int(epoch))?;
        h.set_field(tree, i * 2 + 1, Value::float(old + delta))?;
        i += i & i.wrapping_neg();
    }
    Ok(())
}
fn prefix(h: &Heap, s: Value, n: u32) -> Result<f64, AdapterError> {
    Ok(f64::from(n) * (num(h, s, S::Estimate)? + gap(h, s)?)
        + sum(h, get(h, s, S::Tree)?, n, int(h, s, S::Epoch)?)?)
}
fn total(h: &Heap, s: Value) -> Result<f64, AdapterError> {
    let n = count(h, s)?;
    Ok(if n == 0 {
        0.
    } else {
        prefix(h, s, n)? - gap(h, s)?
    })
}
fn locate(h: &Heap, s: Value, offset: f64) -> Result<u32, AdapterError> {
    let n = count(h, s)?;
    if n == 0 {
        return Ok(0);
    }
    let tree = get(h, s, S::Tree)?;
    let epoch = int(h, s, S::Epoch)?;
    let base = num(h, s, S::Estimate)? + gap(h, s)?;
    let mut step = 1u32 << (31 - n.leading_zeros());
    let (mut i, mut correction) = (0, 0.);
    while step != 0 {
        let next = i + step;
        if next <= n {
            let part = if h.sequence_get(tree, next * 2)?.as_int() == Some(epoch) {
                stage::number(h.sequence_get(tree, next * 2 + 1)?)?
            } else {
                0.
            };
            if f64::from(next) * base + correction + part <= offset {
                i = next;
                correction += part;
            }
        }
        step >>= 1;
    }
    Ok(i.min(n - 1))
}

pub(in crate::aside) fn project(h: &Heap, host: Value) -> Result<VirtualListView, AdapterError> {
    let s = state(h, host)?;
    let projection = get(h, s, S::Projection)?;
    let mut rows = Vec::new();
    for i in (4..h.sequence_len(projection)?).step_by(3) {
        let child = h.sequence_get(projection, i)?;
        rows.push(VirtualRow {
            node: NodeId(h.host_payload(child, 0)?.as_int().unwrap() as u64),
            index: h.sequence_get(projection, i + 1)?.as_int().unwrap() as usize,
            offset: stage::number(h.sequence_get(projection, i + 2)?)?,
        });
    }
    let nav = h.sequence_get(projection, 3)?;
    let navigation = if nav.tag() == ValueTag::Null {
        None
    } else {
        Some(VirtualListNavigation {
            index: h.sequence_get(nav, 0)?.as_int().unwrap() as usize,
            forward: h.sequence_get(nav, 1)?.as_bool().unwrap(),
        })
    };
    Ok(VirtualListView {
        revision: h.sequence_get(projection, 0)?.as_int().unwrap() as u64,
        total_height: stage::number(h.sequence_get(projection, 1)?)?,
        total_rows: h.sequence_get(projection, 2)?.as_int().unwrap() as usize,
        rows: Arc::from(rows),
        navigation,
    })
}

// Keep the last committed geometry separate from a source/window continuation.
// Restoring while a source shrinks or a row factory runs must publish neither
// its new index nor its detached candidates before reconciliation succeeds.
fn commit_projection(h: &mut Heap, s: Value) -> Result<(), AdapterError> {
    let header = [
        get(h, s, S::Revision)?,
        Value::float(total(h, s)?),
        Value::int(i64::from(count(h, s)?)),
        get(h, s, S::Navigation)?,
    ];
    let projection = h.alloc_array(&header)?;
    let mounted = get(h, s, S::Mounted)?;
    for i in 0..h.sequence_len(mounted)? {
        let e = h.sequence_get(mounted, i)?;
        let index = h.sequence_get(e, INDEX)?;
        for value in [
            entry(h, e, Entry::Node)?,
            index,
            Value::float(prefix(h, s, index.as_int().unwrap() as u32)?),
        ] {
            h.array_push(projection, value)?;
        }
    }
    put(h, s, S::Projection, projection)?;
    Ok(())
}

impl SessionVm {
    fn virtual_continue(
        &mut self,
        t: HostTicket,
        w: &Work,
        phase: u64,
    ) -> Result<(), AdapterError> {
        self.vm.respond(
            t,
            HostResponse::Continue(Continuation {
                version: 2,
                stage: phase,
                values: &w.values(),
                faults: FaultRetention::Preserve,
            }),
        )?;
        Ok(())
    }
    fn virtual_call(
        &mut self,
        t: HostTicket,
        w: &Work,
        phase: u64,
        closure: Value,
        arg: Value,
    ) -> Result<(), AdapterError> {
        self.vm.respond(
            t,
            HostResponse::CallScript {
                closure,
                arguments: &[arg],
                continuation: Continuation {
                    version: 2,
                    stage: phase,
                    values: &w.values(),
                    faults: FaultRetention::Preserve,
                },
            },
        )?;
        Ok(())
    }
    fn virtual_key(&mut self, t: HostTicket, mut w: Work) -> Result<(), AdapterError> {
        w.check_source(self.vm.heap())?;
        if w.index < self.vm.heap().sequence_len(w.source)? {
            self.virtual_call(
                t,
                &w,
                KEY,
                self.vm.host_arguments(t)?[1],
                self.vm.heap().sequence_get(w.source, w.index)?,
            )
        } else {
            w.sorted = sorted(self.vm.heap_mut(), w.entries, self.read_limit)?;
            w.index = 0;
            self.virtual_continue(t, &w, INDEX_ROWS)
        }
    }
    fn virtual_window(&mut self, t: HostTicket, s: Value) -> Result<(), AdapterError> {
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
    fn virtual_prepare_window(&mut self, t: HostTicket, s: Value) -> Result<(), AdapterError> {
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
            0
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
            let i = self.vm.heap().sequence_get(nav, 0)?.as_int().unwrap() as u32;
            if i < n {
                indices.push(i);
            }
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
    fn virtual_discard_candidate(&mut self, w: &Work) -> Result<(), AdapterError> {
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
    fn virtual_anchor(&mut self, s: Value) -> Result<(), AdapterError> {
        let scroll = get(self.vm.heap(), s, S::Scroll)?;
        if scroll.tag() == ValueTag::Null {
            return Ok(());
        }
        let property = aside_vm::PropertyId {
            host: scroll,
            field: stage::scroll::ScrollField::Y.index() as u32,
        };
        let previous = num(self.vm.heap(), s, S::Offset)?;
        if stage::number(self.vm.heap().host_property(property)?)? != previous {
            return Ok(());
        }
        let n = count(self.vm.heap(), s)?;
        let old_index = int(self.vm.heap(), s, S::AnchorIndex)?;
        let start = num(self.vm.heap(), s, S::Start)?;
        let end = num(self.vm.heap(), s, S::End)?;
        let next = if get(self.vm.heap(), s, S::AtEnd)?.as_bool() == Some(true) {
            let fields = self.vm.heap().host_fields(node(self.vm.heap(), s)?)?;
            let padding = stage::number(
                self.vm
                    .heap()
                    .table_entry(fields, stage::NodeProperty::Padding.index() as u32)?
                    .1,
            )?;
            let offset = (total(self.vm.heap(), s)? + padding * 2. - (end - start)).max(0.);
            start + offset - previous
        } else if n > 0 && old_index >= 0 {
            let key = get(self.vm.heap(), s, S::AnchorKey)?;
            let found = find(
                self.vm.heap(),
                get(self.vm.heap(), s, S::Sorted)?,
                key,
                self.read_limit,
            )?;
            let i = if let Some(e) = found {
                self.vm.heap().sequence_get(e, INDEX)?.as_int().unwrap() as u32
            } else {
                (old_index as u32).min(n - 1)
            };
            prefix(self.vm.heap(), s, i)? + num(self.vm.heap(), s, S::AnchorOffset)?
        } else {
            0.
        };
        let delta = next - start;
        let offset = (previous + delta).max(0.);
        self.vm
            .heap_mut()
            .write_host_property(property, Value::float(offset), true)?;
        put(self.vm.heap_mut(), s, S::Offset, Value::float(offset))?;
        put(self.vm.heap_mut(), s, S::Start, Value::float(next))?;
        put(self.vm.heap_mut(), s, S::End, Value::float(end + delta))?;
        Ok(())
    }

    pub(in crate::aside) fn virtual_feedback(
        &mut self,
        event: &NodeVirtualList,
    ) -> Result<bool, AdapterError> {
        let Some(scene) = self.stage.scene.node(event.node) else {
            return Ok(false);
        };
        let NodeKind::VirtualList(view) = &scene.kind else {
            return Ok(false);
        };
        if view.revision != event.revision {
            return Ok(false);
        }
        let host = self.stage.value(self.vm.heap(), event.node)?;
        let s = state(self.vm.heap(), host)?;
        let scroll = self.stage.value(self.vm.heap(), event.scroll)?;
        let scroll_y = aside_vm::PropertyId {
            host: scroll,
            field: stage::scroll::ScrollField::Y.index() as u32,
        };
        if stage::number(self.vm.heap().host_property(scroll_y)?)? != event.offset.y {
            return Ok(false);
        }
        let follow = matches!(
            self.stage.scene.node(event.scroll).map(|n| &n.kind),
            Some(NodeKind::Scroll {
                follow_end: true,
                ..
            })
        );
        let mut changed = false;
        let moved = num(self.vm.heap(), s, S::Start)? != event.visible_start
            || num(self.vm.heap(), s, S::End)? != event.visible_end;
        for (field, value) in [
            (S::Start, event.visible_start),
            (S::End, event.visible_end),
            (S::Offset, event.offset.y),
        ] {
            if num(self.vm.heap(), s, field)? != value {
                put(self.vm.heap_mut(), s, field, Value::float(value))?;
            }
        }
        put(self.vm.heap_mut(), s, S::Scroll, scroll)?;
        let at_end = follow && event.visible_end >= total(self.vm.heap(), s)? - 0.01;
        put(self.vm.heap_mut(), s, S::AtEnd, Value::bool(at_end))?;
        if count(self.vm.heap(), s)? > 0 {
            let i = locate(self.vm.heap(), s, event.visible_start)?;
            let e = self
                .vm
                .heap()
                .sequence_get(get(self.vm.heap(), s, S::Order)?, i)?;
            let key = entry(self.vm.heap(), e, Entry::Key)?;
            let offset = event.visible_start - prefix(self.vm.heap(), s, i)?;
            put(self.vm.heap_mut(), s, S::AnchorKey, key)?;
            put(
                self.vm.heap_mut(),
                s,
                S::AnchorIndex,
                Value::int(i64::from(i)),
            )?;
            put(self.vm.heap_mut(), s, S::AnchorOffset, Value::float(offset))?;
        }
        if num(self.vm.heap(), s, S::Width)? != event.width {
            let epoch = int(self.vm.heap(), s, S::Epoch)? + 1;
            put(self.vm.heap_mut(), s, S::Epoch, Value::int(epoch))?;
            put(self.vm.heap_mut(), s, S::Width, Value::float(event.width))?;
            put(self.vm.heap_mut(), s, S::EstimateReady, Value::bool(false))?;
            changed = true;
        }
        if get(self.vm.heap(), s, S::EstimateReady)?.as_bool() != Some(true)
            && let Some(first) = event
                .measurements
                .iter()
                .find(|m| m.height.is_finite() && m.height > 0.)
        {
            put(
                self.vm.heap_mut(),
                s,
                S::Estimate,
                Value::float(first.height),
            )?;
            put(self.vm.heap_mut(), s, S::EstimateReady, Value::bool(true))?;
        }
        let mounted = get(self.vm.heap(), s, S::Mounted)?;
        let mut retained = Vec::new();
        for i in 0..self.vm.heap().sequence_len(mounted)? {
            let e = self.vm.heap().sequence_get(mounted, i)?;
            let child = entry(self.vm.heap(), e, Entry::Node)?;
            let id = NodeId(self.vm.heap().host_payload(child, 0)?.as_int().unwrap() as u64);
            if event.retained.contains(&id) {
                retained.push(entry(self.vm.heap(), e, Entry::Key)?);
            }
            if let Some(measurement) = event.measurements.iter().find(|m| m.node == id) {
                if !measurement.height.is_finite() || measurement.height <= 0. {
                    put(self.vm.heap_mut(), s, S::Fault, Value::bool(true))?;
                    changed = true;
                    continue;
                }
                let index = self.vm.heap().sequence_get(e, INDEX)?.as_int().unwrap() as u32;
                let old = height(self.vm.heap(), s, e)?;
                let epoch = int(self.vm.heap(), s, S::Epoch)?;
                if old != measurement.height
                    || self.vm.heap().sequence_get(e, EPOCH)?.as_int() != Some(epoch)
                {
                    let tree = get(self.vm.heap(), s, S::Tree)?;
                    let count = count(self.vm.heap(), s)?;
                    add(
                        self.vm.heap_mut(),
                        tree,
                        count,
                        index,
                        epoch,
                        measurement.height - old,
                    )?;
                    self.vm
                        .heap_mut()
                        .set_field(e, HEIGHT, Value::float(measurement.height))?;
                    self.vm.heap_mut().set_field(e, EPOCH, Value::int(epoch))?;
                    changed = true;
                }
            }
        }
        // Scroll within the buffered window only updates the anchor. It does
        // not schedule a native observer or allocate a replacement component.
        let first = locate(self.vm.heap(), s, event.visible_start.max(0.))?;
        let last = locate(self.vm.heap(), s, event.visible_end.max(0.))?;
        if count(self.vm.heap(), s)? > 0
            && (first..=last).any(|index| !view.rows.iter().any(|r| r.index == index as usize))
        {
            changed = true;
        }
        let pins = get(self.vm.heap(), s, S::Pinned)?;
        let same = self.vm.heap().sequence_len(pins)? as usize == retained.len()
            && retained.iter().enumerate().all(|(i, v)| {
                self.vm
                    .heap()
                    .sequence_get(pins, i as u32)
                    .is_ok_and(|x| x.same_bits(*v))
            });
        if !same {
            let pins = self.vm.heap_mut().alloc_array(&retained)?;
            put(self.vm.heap_mut(), s, S::Pinned, pins)?;
            changed = true;
        }
        if let Some(nav) = event.navigation {
            let nav = self
                .vm
                .heap_mut()
                .alloc_array(&[Value::int(nav.index as i64), Value::bool(nav.forward)])?;
            put(self.vm.heap_mut(), s, S::Navigation, nav)?;
            changed = true;
        } else if get(self.vm.heap(), s, S::Navigation)?.tag() != ValueTag::Null {
            put(self.vm.heap_mut(), s, S::Navigation, Value::NULL)?;
            changed = true;
        }
        if changed {
            self.virtual_anchor(s)?;
        }
        if changed || moved {
            let request = int(self.vm.heap(), s, S::Request)? + 1;
            put(self.vm.heap_mut(), s, S::Request, Value::int(request))?;
            if changed {
                let demand = get(self.vm.heap(), s, S::Demand)?;
                observable::publish(self.vm.heap_mut(), demand, 0, Value::int(request))?;
            }
        }
        Ok(changed)
    }

    pub(super) fn dispatch_virtual_list(&mut self, t: HostTicket) -> Result<(), AdapterError> {
        let result = self.dispatch_virtual_list_work(t);
        if result.is_err() && t.input != NativeInput::Entry {
            let w = Work::load(self.vm.host_continuation(t)?);
            if w.output.same_bits(get(self.vm.heap(), w.state, S::Window)?)
                && w.source.tag() != ValueTag::Null
            {
                self.virtual_discard_candidate(&w)?;
            }
        }
        result
    }

    fn dispatch_virtual_list_work(&mut self, t: HostTicket) -> Result<(), AdapterError> {
        if t.input == NativeInput::Entry {
            let args = self.vm.host_arguments(t)?.to_vec();
            let (output, s) = if let Some(output) = self.vm.reactive_output(t)? {
                let s = self.vm.heap().derived_state(output)?;
                if self
                    .stage
                    .id(self.vm.heap(), node(self.vm.heap(), s)?)
                    .is_err()
                {
                    self.vm.heap_mut().disconnect_derived(output)?;
                    self.vm.respond(t, HostResponse::Ready(Value::NULL))?;
                    self.issued = None;
                    return Ok(());
                }
                if output.same_bits(get(self.vm.heap(), s, S::Window)?) {
                    self.virtual_window(t, s)?;
                    self.issued = None;
                    return Ok(());
                }
                (output, s)
            } else {
                let mut layout = args[4..].to_vec();
                layout.push(Value::NULL);
                let origin = self.stage_origin(t);
                let host = self.stage.dispatch(
                    &mut self.vm,
                    self.root_owner,
                    "reel::column",
                    &layout,
                    self.read_limit,
                    origin,
                )?;
                let output = self.vm.heap_mut().alloc_derived(args[0])?;
                let window = self.vm.heap_mut().alloc_derived(args[0])?;
                let demand =
                    observable::record(self.vm.heap_mut(), &[("revision", Value::int(0))], &[])?;
                let empty = self.vm.heap_mut().alloc_array(&[])?;
                let tree = self
                    .vm
                    .heap_mut()
                    .alloc_array(&[Value::int(0), Value::float(0.)])?;
                let projection = self.vm.heap_mut().alloc_array(&[
                    Value::int(0),
                    Value::float(0.),
                    Value::int(0),
                    Value::NULL,
                ])?;
                let mut values = [Value::NULL; STATE_LEN];
                for (f, v) in [
                    (S::Node, host),
                    (S::Sorted, empty),
                    (S::Order, empty),
                    (S::Tree, tree),
                    (S::Source, output),
                    (S::Window, window),
                    (S::Demand, demand),
                    (S::Mounted, empty),
                    (S::Pinned, empty),
                    (S::Estimate, Value::float(48.)),
                    (S::AnchorIndex, Value::int(-1)),
                ] {
                    values[f as usize] = v;
                }
                for f in [S::Revision, S::Epoch, S::Request] {
                    values[f as usize] = Value::int(0);
                }
                for f in [S::Width, S::Start, S::End, S::Offset, S::AnchorOffset] {
                    values[f as usize] = Value::float(0.);
                }
                values[S::Projection as usize] = projection;
                let s = self.vm.heap_mut().alloc_array(&values)?;
                self.vm.heap_mut().set_derived_state(output, s)?;
                self.vm.heap_mut().set_derived_state(window, s)?;
                self.vm.heap_mut().set_host_payload(host, 3, window)?;
                self.stage.make_virtual(self.vm.heap(), host)?;
                (output, s)
            };
            let w = Work {
                output,
                state: s,
                source: Value::NULL,
                entries: self.vm.heap_mut().alloc_array(&[])?,
                sorted: Value::NULL,
                children: self
                    .vm
                    .heap_mut()
                    .alloc_array(&[Value::int(0), Value::float(0.)])?,
                index: 0,
                version: 0,
            };
            self.vm
                .call_reactive(t, output, args[0], false, &w.values())?;
        } else {
            let mut w = Work::load(self.vm.host_continuation(t)?);
            if t.input == NativeInput::CallbackFault {
                if w.output.same_bits(get(self.vm.heap(), w.state, S::Window)?) {
                    self.virtual_discard_candidate(&w)?;
                }
                self.vm.heap_mut().end_derived(w.output)?;
                self.vm.respond(
                    t,
                    HostResponse::ForwardFault(self.vm.host_callback_fault(t)?.unwrap().id),
                )?;
                self.issued = None;
                return Ok(());
            }
            match t.continuation.unwrap().stage {
                PREPARE_WINDOW => self.virtual_prepare_window(t, w.state)?,
                SOURCE => {
                    w.source = self.vm.host_callback_value(t)?;
                    if self.vm.heap().info(w.source)?.kind != ObjectKind::Array {
                        return Err(AdapterError::Input("list source must return an array"));
                    }
                    w.version = self.vm.heap().info(w.source)?.content_version;
                    self.vm.heap_mut().observe_read(w.source, None)?;
                    self.virtual_key(t, w)?;
                }
                KEY => {
                    w.check_source(self.vm.heap())?;
                    let key = self.vm.host_callback_value(t)?;
                    let item = self.vm.heap().sequence_get(w.source, w.index)?;
                    let e = self.vm.heap_mut().alloc_array(&[
                        key,
                        item,
                        Value::NULL,
                        Value::NULL,
                        Value::float(0.),
                        Value::int(i64::from(w.index)),
                        Value::int(-1),
                    ])?;
                    self.vm.heap_mut().array_push(w.entries, e)?;
                    w.index += 1;
                    self.virtual_key(t, w)?;
                }
                INDEX_ROWS => {
                    w.check_source(self.vm.heap())?;
                    if w.index < self.vm.heap().sequence_len(w.entries)? {
                        let e = self.vm.heap().sequence_get(w.entries, w.index)?;
                        let key = entry(self.vm.heap(), e, Entry::Key)?;
                        if let Some(old) = find(
                            self.vm.heap(),
                            get(self.vm.heap(), w.state, S::Sorted)?,
                            key,
                            self.read_limit,
                        )? {
                            for field in [Entry::Row as u32, Entry::Node as u32, HEIGHT, EPOCH] {
                                let value = self.vm.heap().sequence_get(old, field)?;
                                self.vm.heap_mut().set_field(e, field, value)?;
                            }
                            if !entry(self.vm.heap(), e, Entry::Item)?.same_bits(entry(
                                self.vm.heap(),
                                old,
                                Entry::Item,
                            )?) {
                                self.vm.heap_mut().set_field(e, EPOCH, Value::int(-1))?;
                            }
                        }
                        self.vm.heap_mut().array_push(w.children, Value::int(0))?;
                        self.vm
                            .heap_mut()
                            .array_push(w.children, Value::float(0.))?;
                        w.index += 1;
                        self.virtual_continue(t, &w, INDEX_ROWS)?;
                    } else {
                        w.index = 0;
                        self.virtual_continue(t, &w, INDEX_HEIGHTS)?;
                    }
                }
                INDEX_HEIGHTS => {
                    let n = self.vm.heap().sequence_len(w.entries)?;
                    if w.index < n {
                        let e = self.vm.heap().sequence_get(w.entries, w.index)?;
                        let delta = height(self.vm.heap(), w.state, e)?
                            - num(self.vm.heap(), w.state, S::Estimate)?;
                        let epoch = int(self.vm.heap(), w.state, S::Epoch)?;
                        add(self.vm.heap_mut(), w.children, n, w.index, epoch, delta)?;
                        w.index += 1;
                        self.virtual_continue(t, &w, INDEX_HEIGHTS)?;
                    } else {
                        put(self.vm.heap_mut(), w.state, S::Sorted, w.sorted)?;
                        put(self.vm.heap_mut(), w.state, S::Order, w.entries)?;
                        put(self.vm.heap_mut(), w.state, S::Tree, w.children)?;
                        self.virtual_anchor(w.state)?;
                        let request = int(self.vm.heap(), w.state, S::Request)? + 1;
                        put(self.vm.heap_mut(), w.state, S::Request, Value::int(request))?;
                        self.vm
                            .heap_mut()
                            .commit_derived(w.output, Value::int(request))?;
                        if get(self.vm.heap(), w.state, S::Initialized)?.as_bool() != Some(true) {
                            put(
                                self.vm.heap_mut(),
                                w.state,
                                S::Initialized,
                                Value::bool(true),
                            )?;
                            self.virtual_window(t, w.state)?;
                        } else {
                            self.vm
                                .respond(t, HostResponse::Ready(node(self.vm.heap(), w.state)?))?;
                        }
                    }
                }
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
                _ => return Err(AdapterError::Input("virtual list continuation")),
            }
        }
        self.issued = None;
        Ok(())
    }
}
