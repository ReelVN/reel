//! Viewport materialization shares the keyed list's VM graph and native frame.
//! The source observer indexes data; a second observer reacts only to layout.
use super::*;
use reel_ui::stage::{
    NodeId, NodeKind, NodeVirtualList, VirtualListNavigation, VirtualListView, VirtualRow,
};
use std::sync::Arc;
mod feedback;
mod index;
mod target;
mod window;
use index::*;
pub(in crate::aside) use target::presentation_current;

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
    Projection,
    Target,
    MeasuredRevision,
    LastNavigation,
    Fault,
    Candidate,
    SourceRevision,
}
const STATE_LEN: usize = S::SourceRevision as usize + 1;
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
pub(in crate::aside) fn project(h: &Heap, host: Value) -> Result<VirtualListView, AdapterError> {
    let s = state(h, host)?;
    let projection = get(h, s, S::Projection)?;
    let mut rows = Vec::new();
    for i in (5..h.sequence_len(projection)?).step_by(3) {
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
            request: h.sequence_get(nav, 3)?.as_int().unwrap() as u64,
            ready: h.sequence_get(nav, 4)?.as_bool().unwrap(),
            index: h.sequence_get(nav, 0)?.as_int().unwrap() as usize,
            forward: h.sequence_get(nav, 1)?.as_bool().unwrap(),
            resolved_index: h.sequence_get(nav, 2)?.as_int().unwrap() as usize,
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
        get(h, s, S::SourceRevision)?,
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
                version: 1,
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
                    version: 1,
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
                // A new source must not adopt detached rows from an obsolete
                // window. Retire that candidate before copying keyed entries.
                let candidate = get(self.vm.heap(), s, S::Candidate)?;
                if candidate.tag() != ValueTag::Null {
                    let mut values = [Value::NULL; 8];
                    for (i, value) in values.iter_mut().enumerate() {
                        *value = self.vm.heap().sequence_get(candidate, i as u32)?;
                    }
                    self.virtual_discard_candidate(&Work::load(&values))?;
                }
                (output, s)
            } else {
                let mut layout = args[4..].to_vec();
                layout.push(Value::NULL);
                let origin = self.stage_origin(t);
                let host = self.stage.dispatch(
                    &mut self.vm,
                    self.root_owner,
                    "reel::stack",
                    &layout,
                    self.read_limit,
                    origin,
                )?;
                let output = self.vm.heap_mut().alloc_derived(args[0])?;
                let window = self.vm.heap_mut().alloc_derived(args[0])?;
                let demand =
                    observable::record(self.vm.heap_mut(), &[("revision", Value::int(0))], &[])?;
                let empty = self.vm.heap_mut().alloc_array(&[])?;
                let tree = self.vm.heap_mut().alloc_array(&[
                    Value::int(0),
                    Value::int(0),
                    Value::float(0.),
                ])?;
                let projection = self.vm.heap_mut().alloc_array(&[
                    Value::int(0),
                    Value::float(0.),
                    Value::int(0),
                    Value::NULL,
                    Value::int(0),
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
                for f in [
                    S::Revision,
                    S::Epoch,
                    S::Request,
                    S::LastNavigation,
                    S::SourceRevision,
                ] {
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
                self.bind_list_scroll_to(host)?;
                (output, s)
            };
            self.virtual_target_invalidate(s)?;
            let w = Work {
                output,
                state: s,
                source: Value::NULL,
                entries: self.vm.heap_mut().alloc_array(&[])?,
                sorted: Value::NULL,
                children: self.vm.heap_mut().alloc_array(&[
                    Value::int(0),
                    Value::int(0),
                    Value::float(0.),
                ])?,
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
                        let epoch = int(self.vm.heap(), w.state, S::Epoch)?;
                        if self.vm.heap().sequence_get(e, EPOCH)?.as_int() == Some(epoch) {
                            let value = stage::number(self.vm.heap().sequence_get(e, HEIGHT)?)?;
                            add(self.vm.heap_mut(), w.children, n, w.index, epoch, 1, value)?;
                        }
                        w.index += 1;
                        self.virtual_continue(t, &w, INDEX_HEIGHTS)?;
                    } else {
                        let source_revision = int(self.vm.heap(), w.state, S::SourceRevision)? + 1;
                        put(
                            self.vm.heap_mut(),
                            w.state,
                            S::SourceRevision,
                            Value::int(source_revision),
                        )?;
                        put(self.vm.heap_mut(), w.state, S::Sorted, w.sorted)?;
                        put(self.vm.heap_mut(), w.state, S::Order, w.entries)?;
                        put(self.vm.heap_mut(), w.state, S::Tree, w.children)?;
                        refresh_estimate(self.vm.heap_mut(), w.state)?;
                        self.virtual_target_invalidate(w.state)?;
                        if !self.virtual_target_position(w.state)? {
                            self.virtual_anchor(w.state)?;
                        }
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
                WINDOW | BUILT => self.virtual_build_window(t, w, t.continuation.unwrap().stage)?,
                _ => return Err(AdapterError::Input("virtual list continuation")),
            }
        }
        self.issued = None;
        Ok(())
    }
}
