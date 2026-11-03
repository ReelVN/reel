//! User replacements run through the existing editor task, journal and publication.
use super::*;
use crate::aside::{NativeBudget, NativeDispatch, SessionVm};
use aside_compiler::semantic::TypeHint;
use aside_host::{
    NativeContract, NativeParameter,
    source::{Api, Member, NamedType, named, nullable},
};
use aside_vm::{
    bytecode::{BuiltinType, Context, Effects, FunctionId, FunctionKind},
    executor::{Continuation, FaultRetention, HostResponse, HostTicket, NativeInput, Vm},
};
use reel_ui::widgets::{TextEdit, TextEditKind};

pub(in crate::aside) const SYMBOL: &str = "reel::text_input::filter";
const TYPE: &str = "reel::types::TextEdit";
const FIELDS: [&str; 5] = ["previous", "value", "text", "start", "end"];

pub(super) fn source_type() -> TypeHint {
    nullable(TypeHint::Callable(
        vec![named(TYPE)],
        Box::new(nullable(TypeHint::String)),
    ))
}
pub(super) fn describe(api: &mut Api) {
    let members = FIELDS
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            Member::field(
                TYPE,
                name,
                if i < 3 {
                    TypeHint::String
                } else {
                    TypeHint::Int
                },
                None,
                match i {
                    0 => "The complete committed text before this edit.",
                    1 => "The complete proposed text before filtering.",
                    2 => "The text replacing previous[start..end].",
                    3 => "The inclusive UTF-8 byte start in previous.",
                    _ => "The exclusive UTF-8 byte end in previous.",
                },
                None,
            )
        })
        .collect();
    api.types.push(NamedType::new(
        TYPE,
        "A readonly proposed user replacement. start/end are UTF-8 byte offsets into previous. Return replacement text from edit_filter, or null to reject it.",
        members,
    ));
}
pub(super) fn contract() -> NativeContract {
    NativeContract {
        standard_derived_commit: false,
        read_only_leaf: None,
        symbol: SYMBOL.into(),
        abi_version: 1,
        parameters: ["receiver", "edit", "filter"]
            .into_iter()
            .map(|name| NativeParameter {
                name: name.into(),
                ty: BuiltinType::Any,
                default: None,
            })
            .collect(),
        rest: None,
        result: BuiltinType::Any,
        effects: Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST),
        required_capabilities: vec!["reel.stage".into()],
    }
}
fn get(heap: &Heap, edit: Value, field: usize) -> Result<Value, HeapError> {
    Ok(heap.table_entry(heap.host_fields(edit)?, field as u32)?.1)
}
fn internal(heap: &Heap, edit: Value) -> Result<TextEdit, HeapError> {
    let value = |i| -> Result<usize, HeapError> {
        Ok(heap.host_payload(edit, i)?.as_int().unwrap() as usize)
    };
    Ok(TextEdit {
        range: get(heap, edit, 3)?.as_int().unwrap() as usize
            ..get(heap, edit, 4)?.as_int().unwrap() as usize,
        before: TextSelection {
            anchor: value(1)?,
            focus: value(2)?,
        },
        after: TextSelection {
            anchor: value(3)?,
            focus: value(4)?,
        },
        kind: match value(5)? {
            0 => TextEditKind::Insert,
            1 => TextEditKind::Backspace,
            2 => TextEditKind::Delete,
            _ => TextEditKind::Transaction,
        },
        revision: value(0)? as u64,
        replay: None,
    })
}
impl SessionVm {
    /// The ordinary no-filter input path does not allocate a journal or a task.
    pub(in crate::aside) fn start_filtered_edit(
        &mut self,
        session: &mut crate::Session,
        input: &NodeText,
    ) -> Result<Option<bool>, AdapterError> {
        let TextInputEvent::Changed {
            previous,
            value,
            edit,
        } = &input.event
        else {
            return Ok(None);
        };
        if !self
            .stage
            .scene
            .node(input.node)
            .is_some_and(|node| matches!(node.kind, NodeKind::TextInput { .. }))
        {
            return Ok(Some(false));
        }
        let target = self.stage.value(self.vm.heap(), input.node)?;
        let filter = self.vm.heap().host_property(PropertyId {
            host: target,
            field: TextInputField::EditFilter.index() as u32,
        })?;
        if filter.tag() == ValueTag::Null {
            return Ok(None);
        }
        if !self
            .stage
            .text_action(self.vm.heap(), input.node, previous, edit.revision)?
        {
            return Ok(Some(false));
        }
        let (_, inserted) = replacement(previous, value, edit)?;
        let current = self.vm.heap().host_property(PropertyId {
            host: target,
            field: 0,
        })?;
        // Reuse the authoritative old value and allocate the ordinary candidate once.
        let candidate = crate::aside::text::alloc_string(self.vm.heap_mut(), value, 1024)?;
        let text = if inserted.len() == value.len() {
            candidate
        } else {
            crate::aside::text::alloc_string(self.vm.heap_mut(), inserted, 1024)?
        };
        let fields = self.vm.heap_mut().alloc_table()?;
        for (i, value) in [
            current,
            candidate,
            text,
            Value::int(edit.range.start as i64),
            Value::int(edit.range.end as i64),
        ]
        .into_iter()
        .enumerate()
        {
            let key = self.vm.heap_mut().alloc_string(FIELDS[i])?;
            self.vm
                .heap_mut()
                .table_store(fields, i as u32, key, value)?;
        }
        let kind = match edit.kind {
            TextEditKind::Insert => 0,
            TextEditKind::Backspace => 1,
            TextEditKind::Delete => 2,
            TextEditKind::Transaction => 3,
        };
        let proposal = self.vm.heap_mut().alloc_immutable_host(
            fields,
            &[
                Value::int(edit.revision as i64),
                Value::int(edit.before.anchor as i64),
                Value::int(edit.before.focus as i64),
                Value::int(edit.after.anchor as i64),
                Value::int(edit.after.focus as i64),
                Value::int(kind),
            ],
        )?;
        self.stage.types.mark(self.vm.heap_mut(), proposal, TYPE)?;
        let function = if let Some(function) = self.stage.text_filter_function {
            function
        } else {
            let p = self.vm.program().package();
            let function = p
                .functions
                .iter()
                .enumerate()
                .find_map(|(i, f)| {
                    let FunctionKind::Native(n) = f.kind else {
                        return None;
                    };
                    (p.symbols[p.natives[n.index()].symbol.index()] == SYMBOL)
                        .then_some(FunctionId(i as u32))
                })
                .ok_or(AdapterError::Input("missing editor filter adapter"))?;
            self.stage.text_filter_function = Some(function);
            function
        };
        let method = self.vm.bind_native(target, function)?;
        self.stage.ensure_text_queue(self.vm.heap(), input.node)?;
        let task = self.start_owned_task_in_context(
            session,
            Some(input.node),
            method,
            &[proposal, filter],
            None,
            Context::NoWait,
        )?;
        let queue = self.stage.ensure_text_queue(self.vm.heap(), input.node)?;
        queue.active = Some(task);
        queue.active_replay = edit.replay;
        self.stage.mark_text_projection(input.node);
        self.sync_stage_properties(false)?;
        Ok(Some(true))
    }
}
fn defer(
    vm: &mut Vm,
    ticket: HostTicket,
    phase: u64,
    roots: &[Value; 4],
    work: u32,
) -> Result<(NativeDispatch, Option<runtime::Change>), AdapterError> {
    vm.respond(
        ticket,
        HostResponse::Continue(Continuation {
            version: 1,
            stage: phase,
            values: roots,
            faults: FaultRetention::Preserve,
        }),
    )?;
    Ok((NativeDispatch::Decoding { work }, None))
}
pub(super) fn dispatch(
    stage: &mut StageBinding,
    vm: &mut Vm,
    ticket: HostTicket,
    budget: NativeBudget,
) -> Result<(NativeDispatch, Option<runtime::Change>), AdapterError> {
    if matches!(ticket.input, NativeInput::CallbackFault) {
        let fault = vm.host_callback_fault(ticket)?.unwrap();
        vm.respond(ticket, HostResponse::ForwardFault(fault.id))?;
        return Ok((NativeDispatch::Ready { work: 1 }, None));
    }
    let args = vm.host_arguments(ticket)?;
    let (receiver, proposal, filter) = (args[0], args[1], args[2]);
    let id = stage.id(vm.heap(), receiver)?;
    let original = get(vm.heap(), proposal, 0)?;
    let candidate = get(vm.heap(), proposal, 1)?;
    let text = get(vm.heap(), proposal, 2)?;
    let mut edit = internal(vm.heap(), proposal)?;
    let current = vm.heap().host_property(PropertyId {
        host: receiver,
        field: 0,
    })?;
    let read_only = vm
        .heap()
        .host_property(PropertyId {
            host: receiver,
            field: TextInputField::ReadOnly.index() as u32,
        })?
        .as_bool()
        == Some(true);
    if !original.same_bits(current) || revision(vm.heap(), receiver)? != edit.revision || read_only
    {
        vm.respond(ticket, HostResponse::Ready(receiver))?;
        return Ok((NativeDispatch::Ready { work: 1 }, None));
    }
    if matches!(ticket.input, NativeInput::Entry) {
        vm.respond(
            ticket,
            HostResponse::CallScript {
                closure: filter,
                arguments: &[proposal],
                continuation: Continuation {
                    version: 1,
                    stage: 0,
                    values: &[],
                    faults: FaultRetention::Preserve,
                },
            },
        )?;
        return Ok((NativeDispatch::Ready { work: 1 }, None));
    }
    let (phase, mut roots) = if matches!(ticket.input, NativeInput::CallbackReturned) {
        let result = vm.host_callback_value(ticket)?;
        if result.tag() == ValueTag::Null {
            stage.finish_text_operation(vm.heap_mut(), id, ticket.task, None)?;
            vm.respond(ticket, HostResponse::Ready(receiver))?;
            return Ok((NativeDispatch::Ready { work: 1 }, None));
        }
        if vm.heap().info(result)?.kind != ObjectKind::String {
            return Err(HeapError::TypeMismatch.into());
        }
        (1, [result, Value::NULL, Value::int(0), Value::NULL])
    } else if let NativeInput::Continue {
        version: 1,
        stage: phase,
    } = ticket.input
    {
        let values = vm.host_continuation(ticket)?;
        (phase, [values[0], values[1], values[2], values[3]])
    } else {
        return Err(AdapterError::Input("editor filter continuation"));
    };
    let replacement = roots[0];
    let replacement_len = vm.heap().info(replacement)?.len as usize;
    let proposed_len = vm.heap().info(text)?.len as usize;
    if phase == 1 {
        let work = budget.with_minimum(64);
        if work < 64 {
            return defer(vm, ticket, 1, &roots, 0);
        }
        let mut used = 16;
        let mut offset = roots[2].as_int().unwrap() as usize;
        let mut equal = replacement_len == proposed_len;
        while equal && offset < replacement_len && work - used > 16 {
            let a = vm.heap().byte_segment(replacement, offset as u32)?;
            let b = vm.heap().byte_segment(text, offset as u32)?;
            let count = a.len().min(b.len()).min((work - used - 16) as usize);
            equal = a[..count] == b[..count];
            offset += count;
            used += count as u32 + 16;
        }
        if equal && offset < replacement_len {
            roots[2] = Value::int(offset as i64);
            return defer(vm, ticket, 1, &roots, used);
        }
        roots[1] = if equal {
            candidate
        } else {
            vm.heap_mut().alloc_byte_buffer(&[])?
        };
        roots[2] = Value::bool(equal);
        return defer(vm, ticket, if equal { 3 } else { 2 }, &roots, used);
    }
    if phase == 2 {
        let work = budget.with_minimum(64);
        if work < 64 {
            return defer(vm, ticket, 2, &roots, 0);
        }
        let total = vm.heap().info(original)?.len as usize - edit.range.len() + replacement_len;
        let mut offset = if roots[2].as_bool().is_some() {
            0
        } else {
            roots[2].as_int().unwrap() as usize
        };
        let mut used = 16;
        let mut scratch = [0; 1024];
        while offset < total && work - used > 16 {
            let (source, source_offset, stop) = if offset < edit.range.start {
                (original, offset, edit.range.start)
            } else if offset < edit.range.start + replacement_len {
                (
                    replacement,
                    offset - edit.range.start,
                    edit.range.start + replacement_len,
                )
            } else {
                (original, offset - replacement_len + edit.range.len(), total)
            };
            let bytes = vm.heap().byte_segment(source, source_offset as u32)?;
            let count = bytes
                .len()
                .min(scratch.len())
                .min(stop - offset)
                .min((work - used - 16) as usize);
            scratch[..count].copy_from_slice(&bytes[..count]);
            vm.heap_mut().bytes_append(roots[1], &scratch[..count])?;
            offset += count;
            used += count as u32 + 16;
        }
        if offset < total {
            roots[2] = Value::int(offset as i64);
            return defer(vm, ticket, 2, &roots, used);
        }
        vm.heap_mut().seal_string(roots[1])?;
        roots[2] = Value::bool(false);
        return defer(vm, ticket, 3, &roots, used);
    }
    if phase != 3 {
        return Err(AdapterError::Input("editor filter phase"));
    }
    if roots[2].as_bool() != Some(true) {
        let caret = edit.range.start + replacement_len;
        edit.after = TextSelection {
            anchor: caret,
            focus: caret,
        };
    }
    let value = roots[1];
    let expected = vm.heap().handle(value)?;
    let history = stage
        .handles
        .get_mut(&id)
        .unwrap()
        .text_history
        .as_mut()
        .unwrap();
    let removed = history::EditText::Vm {
        heap: vm.heap(),
        value: original,
        start: edit.range.start,
        length: edit.range.len(),
    };
    let inserted = history::EditText::Vm {
        heap: vm.heap(),
        value: replacement,
        start: 0,
        length: replacement_len,
    };
    let extra = (removed.len() + inserted.len()) as u64 * 2
        + history.queue.as_ref().unwrap().entries.len() as u64 * 8
        + (history.entries.len() - history.position) as u64 * 8
        + 16;
    let updates = runtime::writes(receiver, Some(value), edit.after, true, false);
    let mut prepared = None;
    let publication =
        runtime::publication(vm, ticket, (3, roots), &updates, budget, extra, |heap| {
            let removed = history::EditText::Vm {
                heap,
                value: original,
                start: edit.range.start,
                length: edit.range.len(),
            };
            let inserted = history::EditText::Vm {
                heap,
                value: replacement,
                start: 0,
                length: replacement_len,
            };
            let remap = history.queue.as_ref().unwrap().prepare_remap(
                heap,
                edit.range.start,
                edit.range.len(),
                replacement_len,
            )?;
            prepared = Some(history.prepare(&edit, removed, inserted)?);
            Ok(remap)
        });
    let (remap, work) = match publication {
        Ok(done) => done,
        Err(error) => {
            if let Some(prepared) = prepared {
                history.abort(prepared);
            }
            return Err(error);
        }
    };
    let Some(remap) = remap else {
        if let Some(prepared) = prepared {
            history.abort(prepared);
        }
        return Ok((NativeDispatch::Decoding { work }, None));
    };
    let removed = history::EditText::Vm {
        heap: vm.heap(),
        value: original,
        start: edit.range.start,
        length: edit.range.len(),
    };
    let inserted = history::EditText::Vm {
        heap: vm.heap(),
        value: replacement,
        start: 0,
        length: replacement_len,
    };
    history.commit(prepared.unwrap(), &edit, removed, inserted, expected);
    stage.finish_text_operation(vm.heap_mut(), id, ticket.task, Some(remap))?;
    let callback = vm.heap().host_property(PropertyId {
        host: receiver,
        field: TextInputField::OnChange.index() as u32,
    })?;
    vm.respond(ticket, HostResponse::Ready(receiver))?;
    Ok((
        NativeDispatch::Ready { work },
        Some((id, receiver, callback, value)),
    ))
}
