//! Typed editor operations share the input journal and ordinary native continuation.
use super::*;
use crate::aside::{NativeBudget, NativeDispatch};
use aside_compiler::semantic::TypeHint;
use aside_host::{
    NativeContract, NativeParameter,
    source::{Api, Member, named, nullable},
};
use aside_vm::{
    bytecode::{BuiltinType, Const, Effects},
    executor::{Continuation, FaultRetention, HostResponse, HostTicket, NativeInput, Vm},
};

aside_host::native_documentation! {
    pub(in crate::aside) fn documentation {
        /// Select UTF-8 byte endpoints without moving keyboard focus. Null focus collapses
        /// at anchor; out-of-range or non-UTF-8 boundaries fail at this call. The shared
        /// editor snaps display endpoints to cluster starts on the next presentation.
        /// Return the same TextInputNode. Selection changes split undo transactions.
        "reel::text_input::select_bytes";
        /// Select the complete committed text without requesting focus. Return this input.
        "reel::text_input::select_all";
        /// Restore the preceding edit transaction and its selection. Continuous typing
        /// and same-direction deletion merge; selection moves and IME commits split them.
        /// on_change observes the committed value once. No history leaves the value alone.
        /// Read-only inputs reject mutation. Return the same TextInputNode.
        "reel::text_input::undo";
        /// Reapply the next undone transaction and its selection. A new edit or external
        /// value replacement discards redo. Return the same TextInputNode.
        "reel::text_input::redo";
    }
}

pub(in crate::aside) fn contracts() -> Vec<NativeContract> {
    ["select_bytes", "select_all", "undo", "redo"]
        .into_iter()
        .map(|name| {
            let mut parameters = vec![NativeParameter {
                name: "receiver".into(),
                ty: BuiltinType::Any,
                default: None,
            }];
            if name == "select_bytes" {
                parameters.push(NativeParameter {
                    name: "anchor".into(),
                    ty: BuiltinType::Int,
                    default: None,
                });
                parameters.push(NativeParameter {
                    name: "focus".into(),
                    ty: BuiltinType::Any,
                    default: Some(Const::Null),
                });
            }
            NativeContract {
                symbol: format!("reel::text_input::{name}"),
                abi_version: 1,
                parameters,
                rest: None,
                result: BuiltinType::Any,
                effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
                required_capabilities: vec!["reel.stage".into()],
            }
        })
        .collect()
}

pub(in crate::aside::stage) fn describe(api: &mut Api, members: &mut Vec<Member>) {
    let owner = "reel::types::TextInputNode";
    for name in ["select_bytes", "select_all", "undo", "redo"] {
        let symbol = format!("reel::text_input::{name}");
        api.refine(&symbol, named(owner), &[("receiver", named(owner))]);
        if name == "select_bytes" {
            api.refine(
                &symbol,
                named(owner),
                &[
                    ("anchor", TypeHint::Int),
                    ("focus", nullable(TypeHint::Int)),
                ],
            );
        }
        api.native_mut(&symbol).returns_parameter("receiver");
        members.push(Member::method(api.native(&symbol).clone().bound(name)));
    }
    for (index, name) in METRICS.into_iter().enumerate() {
        members.push(Member::field(
            owner,
            name,
            if index < 2 {
                TypeHint::Int
            } else {
                TypeHint::Bool
            },
            None,
            match index {
                0 => "The last committed selection anchor, as a UTF-8 byte offset.",
                1 => "The last committed selection focus, as a UTF-8 byte offset.",
                2 => "Whether the current value has an earlier edit transaction to undo.",
                _ => "Whether the current value has an undone transaction to reapply.",
            },
            Some(if index < 2 {
                Const::Int(0)
            } else {
                Const::Bool(false)
            }),
        ));
    }
}

pub(in crate::aside) type Change = (reel_ui::stage::NodeId, Value, Value, Value);

fn defer(
    vm: &mut Vm,
    ticket: HostTicket,
    stage: u64,
    values: &[Value],
    work: u32,
) -> Result<NativeDispatch, AdapterError> {
    vm.respond(
        ticket,
        HostResponse::Continue(Continuation {
            version: 1,
            stage,
            values,
            faults: FaultRetention::Preserve,
        }),
    )?;
    Ok(NativeDispatch::Decoding { work })
}

/// The editor uses the same per-call property proposal as node configuration.
/// Its builder and plan remain ordinary continuation roots through GC and saving.
fn publication<R>(
    vm: &mut Vm,
    ticket: HostTicket,
    phase: u64,
    mut roots: [Value; 4],
    writes: &dyn aside_vm::PropertyWrites,
    budget: NativeBudget,
    extra_work: u64,
    prepare: impl FnOnce(&Heap) -> Result<R, AdapterError>,
) -> Result<(Option<R>, u32), AdapterError> {
    use aside_vm::{PropertyBatch, PropertyBatchProgress};
    let mut plan = (roots[3].tag() != ValueTag::Null).then(|| PropertyBatch::from_value(roots[3]));
    let required = match plan {
        Some(p) => vm.heap_mut().property_batch_required_work(p, writes)?,
        None => PropertyBatch::INITIAL_WORK,
    };
    let grant =
        u64::from(budget.with_minimum(
            u32::try_from(required + extra_work).map_err(|_| HeapError::OutOfBounds)?,
        ));
    if grant < required + extra_work {
        defer(vm, ticket, phase, &roots, 0)?;
        return Ok((None, 0));
    }
    let initial = if plan.is_none() {
        PropertyBatch::INITIAL_WORK
    } else {
        0
    };
    if plan.is_none() {
        plan = Some(vm.heap_mut().begin_host_publication()?);
    }
    let plan = plan.unwrap();
    roots[3] = plan.value();
    let progress = vm
        .heap_mut()
        .step_property_batch(plan, writes, grant - initial)?;
    let mut prepared = None;
    let (used, done) = match progress {
        PropertyBatchProgress::Prepared {
            work,
            required_commit_work,
        } if grant - initial - work >= required_commit_work + extra_work => {
            prepared = Some(prepare(vm.heap())?);
            match vm
                .heap_mut()
                .commit_property_batch(plan, writes, grant - initial - work)?
            {
                PropertyBatchProgress::Committed { work: commit } => {
                    (work + commit + initial + extra_work, true)
                }
                PropertyBatchProgress::Prepared { work: commit, .. }
                | PropertyBatchProgress::Preparing { work: commit, .. } => {
                    (work + commit + initial, false)
                }
            }
        }
        PropertyBatchProgress::Committed { work } => (initial + work, true),
        PropertyBatchProgress::Preparing { work, .. }
        | PropertyBatchProgress::Prepared { work, .. } => (initial + work, false),
    };
    let used = u32::try_from(used).map_err(|_| HeapError::OutOfBounds)?;
    if !done {
        defer(vm, ticket, phase, &roots, used)?;
    }
    Ok((if done { prepared } else { None }, used))
}

fn writes(
    host: Value,
    value: Option<Value>,
    selected: TextSelection,
    undo: bool,
    redo: bool,
) -> [(PropertyId, Value); 5] {
    let field = metrics();
    [
        (
            PropertyId { host, field },
            Value::int(selected.anchor as i64),
        ),
        (
            PropertyId {
                host,
                field: field + 1,
            },
            Value::int(selected.focus as i64),
        ),
        (
            PropertyId {
                host,
                field: field + 2,
            },
            Value::bool(undo),
        ),
        (
            PropertyId {
                host,
                field: field + 3,
            },
            Value::bool(redo),
        ),
        (PropertyId { host, field: 0 }, value.unwrap_or(Value::NULL)),
    ]
}

pub(in crate::aside) fn dispatch(
    stage: &mut StageBinding,
    vm: &mut Vm,
    ticket: HostTicket,
    name: &str,
    budget: NativeBudget,
) -> Result<(NativeDispatch, Option<Change>), AdapterError> {
    let receiver = vm.host_arguments(ticket)?[0];
    let id = stage.id(vm.heap(), receiver)?;
    if !matches!(
        stage.scene.node(id).unwrap().kind,
        NodeKind::TextInput { .. }
    ) {
        return Err(HeapError::TypeMismatch.into());
    }
    let current = vm.heap().host_property(PropertyId {
        host: receiver,
        field: 0,
    })?;
    let stale = stage.handles[&id]
        .text_history
        .as_ref()
        .is_some_and(|history| vm.heap().handle(current).ok() != Some(history.expected));
    if stale {
        let (requested, plan) = if let NativeInput::Continue {
            version: 1,
            stage: 5,
        } = ticket.input
        {
            let roots = vm.host_continuation(ticket)?;
            if !roots[0].same_bits(current) {
                return Err(HeapError::ConcurrentModification.into());
            }
            (
                TextSelection {
                    anchor: roots[1].as_int().unwrap() as usize,
                    focus: roots[2].as_int().unwrap() as usize,
                },
                roots[3],
            )
        } else {
            let mut selected = selection(vm.heap(), receiver)?;
            for offset in [&mut selected.anchor, &mut selected.focus] {
                *offset = (*offset).min(vm.heap().info(current)?.len as usize);
                while *offset < vm.heap().info(current)?.len as usize
                    && vm.heap().byte_segment(current, *offset as u32)?[0] & 0xc0 == 0x80
                {
                    *offset -= 1;
                }
            }
            (selected, Value::NULL)
        };
        let updates = writes(receiver, None, requested, false, false);
        let roots = [
            current,
            Value::int(requested.anchor as i64),
            Value::int(requested.focus as i64),
            plan,
        ];
        let (done, work) = publication(
            vm,
            ticket,
            5,
            roots,
            &[updates[0], updates[1], updates[2], updates[3]],
            budget,
            0,
            |_| Ok(()),
        )?;
        if done.is_none() {
            return Ok((NativeDispatch::Decoding { work }, None));
        }
        stage.handles.get_mut(&id).unwrap().text_history = None;
        stage.mark_text_projection(id);
        return Ok((defer(vm, ticket, 0, &[], work)?, None));
    }
    if name == "select_bytes" || name == "select_all" {
        let (original, requested, plan) = match ticket.input {
            NativeInput::Entry
            | NativeInput::Continue {
                version: 1,
                stage: 0,
            } => {
                let requested = if name == "select_all" {
                    TextSelection {
                        anchor: 0,
                        focus: vm.heap().info(current)?.len as usize,
                    }
                } else {
                    let args = vm.host_arguments(ticket)?;
                    let anchor = usize::try_from(args[1].as_int().ok_or(HeapError::TypeMismatch)?)
                        .map_err(|_| HeapError::OutOfBounds)?;
                    let focus = if args[2].tag() == ValueTag::Null {
                        anchor
                    } else {
                        usize::try_from(args[2].as_int().ok_or(HeapError::TypeMismatch)?)
                            .map_err(|_| HeapError::OutOfBounds)?
                    };
                    for offset in [anchor, focus] {
                        let len = vm.heap().info(current)?.len as usize;
                        if offset > len
                            || offset < len
                                && vm.heap().byte_segment(current, offset as u32)?[0] & 0xc0 == 0x80
                        {
                            return Err(HeapError::OutOfBounds.into());
                        }
                    }
                    TextSelection { anchor, focus }
                };
                (current, requested, Value::NULL)
            }
            NativeInput::Continue {
                version: 1,
                stage: 4,
            } => {
                let r = vm.host_continuation(ticket)?;
                (
                    r[0],
                    TextSelection {
                        anchor: r[1].as_int().unwrap() as usize,
                        focus: r[2].as_int().unwrap() as usize,
                    },
                    r[3],
                )
            }
            _ => return Err(AdapterError::Input("selection continuation")),
        };
        if !original.same_bits(current) {
            return Err(HeapError::ConcurrentModification.into());
        }
        let h = stage.handles[&id].text_history.as_ref();
        let (undo, redo) = h.map_or((false, false), |h| (h.can_undo(), h.can_redo()));
        let updates = writes(receiver, None, requested, undo, redo);
        let roots = [
            original,
            Value::int(requested.anchor as i64),
            Value::int(requested.focus as i64),
            plan,
        ];
        let (done, work) = publication(
            vm,
            ticket,
            4,
            roots,
            &[updates[0], updates[1], updates[2], updates[3]],
            budget,
            0,
            |_| Ok(()),
        )?;
        if done.is_none() {
            return Ok((NativeDispatch::Decoding { work }, None));
        }
        if let Some(h) = stage.handles.get_mut(&id).unwrap().text_history.as_mut() {
            h.split();
        }
        let revision = revision(vm.heap(), receiver)?.wrapping_add(1);
        vm.heap_mut()
            .set_host_payload(receiver, 4, Value::int(revision as i64))?;
        stage.mark_text_projection(id);
        vm.respond(ticket, HostResponse::Ready(receiver))?;
        return Ok((NativeDispatch::Ready { work }, None));
    }
    if vm
        .heap()
        .host_property(PropertyId {
            host: receiver,
            field: TextInputField::ReadOnly.index() as u32,
        })?
        .as_bool()
        == Some(true)
    {
        return Err(HeapError::ReadOnly.into());
    }
    let queue = stage.ensure_text_queue(vm.heap(), id)?;
    if queue.active.is_some_and(|task| task != ticket.task)
        || queue.active.is_none() && !queue.entries.is_empty()
    {
        return Ok((defer(vm, ticket, 2, &[], 1)?, None));
    }
    queue.active = Some(ticket.task);
    stage.mark_text_projection(id);
    let redo = name == "redo";
    let history = stage.handles[&id].text_history.as_ref().unwrap();
    let Some(delta) = history.delta(redo) else {
        stage.finish_text_operation(vm.heap_mut(), id, ticket.task, None)?;
        vm.respond(ticket, HostResponse::Ready(receiver))?;
        return Ok((NativeDispatch::Ready { work: 1 }, None));
    };
    let total = vm.heap().info(current)?.len as usize - delta.replaced_len(redo)
        + delta.replacement_len(redo);
    let (original, builder, mut offset, history_revision, plan) = match ticket.input {
        NativeInput::Entry
        | NativeInput::Continue {
            version: 1,
            stage: 0 | 2,
        } => {
            if budget.with_minimum(64) < 64 {
                return Ok((defer(vm, ticket, 0, &[], 0)?, None));
            }
            (
                current,
                vm.heap_mut().alloc_byte_buffer(&[])?,
                0usize,
                history.revision,
                Value::NULL,
            )
        }
        NativeInput::Continue {
            version: 1,
            stage: 1,
        } => {
            let r = vm.host_continuation(ticket)?;
            (
                r[0],
                r[1],
                r[2].as_int().unwrap() as usize,
                r[3].as_int().unwrap() as u64,
                Value::NULL,
            )
        }
        NativeInput::Continue {
            version: 1,
            stage: 3,
        } => {
            let r = vm.host_continuation(ticket)?;
            (r[0], r[1], total, r[2].as_int().unwrap() as u64, r[3])
        }
        _ => return Err(AdapterError::Input("editor continuation")),
    };
    if !original.same_bits(current) || history_revision != history.revision {
        return Err(HeapError::ConcurrentModification.into());
    }
    if !matches!(ticket.input, NativeInput::Continue { stage: 3, .. }) {
        let work = budget.with_minimum(64);
        let mut used = 32;
        let mut scratch = [0; 1024];
        while offset < total && work.saturating_sub(used) > 16 {
            let count = scratch
                .len()
                .min((work - used - 16) as usize)
                .min(total - offset);
            let length = if offset < delta.start {
                let bytes = vm.heap().byte_segment(current, offset as u32)?;
                let n = count.min(bytes.len()).min(delta.start - offset);
                scratch[..n].copy_from_slice(&bytes[..n]);
                n
            } else if offset < delta.start + delta.replacement_len(redo) {
                delta.read_replacement(redo, offset - delta.start, &mut scratch[..count])
            } else {
                let source = offset - delta.replacement_len(redo) + delta.replaced_len(redo);
                let bytes = vm.heap().byte_segment(current, source as u32)?;
                let n = count.min(bytes.len());
                scratch[..n].copy_from_slice(&bytes[..n]);
                n
            };
            vm.heap_mut().bytes_append(builder, &scratch[..length])?;
            offset += length;
            used += length as u32 + 16;
        }
        if offset != total {
            return Ok((
                defer(
                    vm,
                    ticket,
                    1,
                    &[
                        original,
                        builder,
                        Value::int(offset as i64),
                        Value::int(history_revision as i64),
                    ],
                    used,
                )?,
                None,
            ));
        }
        vm.heap_mut().seal_string(builder)?;
        return Ok((
            defer(
                vm,
                ticket,
                3,
                &[
                    original,
                    builder,
                    Value::int(history_revision as i64),
                    Value::NULL,
                ],
                used,
            )?,
            None,
        ));
    }
    let selected = delta.selection(redo);
    let replacement = (
        delta.start,
        delta.replaced_len(redo),
        delta.replacement_len(redo),
    );
    let next = if redo {
        history.position + 1
    } else {
        history.position - 1
    };
    let updates = writes(
        receiver,
        Some(builder),
        selected,
        next != 0,
        next < history.entries.len(),
    );
    // Prepare immutable selection replacements before the atomic publication. Old
    // frames keep their original replay values; the final queue swap allocates nothing.
    let roots = [original, builder, Value::int(history_revision as i64), plan];
    let (remap, work) = publication(
        vm,
        ticket,
        3,
        roots,
        &updates,
        budget,
        history.queue.as_ref().unwrap().entries.len() as u64 * 8,
        |heap| {
            history.queue.as_ref().unwrap().prepare_remap(
                heap,
                replacement.0,
                replacement.1,
                replacement.2,
            )
        },
    )?;
    let Some(remap) = remap else {
        return Ok((NativeDispatch::Decoding { work }, None));
    };
    let expected = vm.heap().handle(builder)?;
    stage
        .handles
        .get_mut(&id)
        .unwrap()
        .text_history
        .as_mut()
        .unwrap()
        .moved(redo, expected);
    stage.finish_text_operation(vm.heap_mut(), id, ticket.task, Some(remap))?;
    let callback = vm.heap().host_property(PropertyId {
        host: receiver,
        field: TextInputField::OnChange.index() as u32,
    })?;
    vm.respond(ticket, HostResponse::Ready(receiver))?;
    Ok((
        NativeDispatch::Ready { work },
        Some((id, receiver, callback, builder)),
    ))
}
