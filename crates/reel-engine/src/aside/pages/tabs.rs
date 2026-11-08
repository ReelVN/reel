//! Tab content is authored; option input and selection remain native.
use super::*;

impl SessionVm {
    pub(super) fn tabs_watch(
        &mut self,
        t: HostTicket,
        output: Value,
        s: Value,
    ) -> Result<(), AdapterError> {
        let root = get(self.vm.heap(), s, S::Root)?;
        self.vm
            .heap_mut()
            .observe_read(root, Some(TabField::Value.index() as u32))?;
        let key = field(self.vm.heap(), root, TabField::Value.index() as u32)?;
        let selected = if key.tag() == ValueTag::Null {
            Value::NULL
        } else {
            find(
                self.vm.heap(),
                get(self.vm.heap(), s, S::Sorted)?,
                key,
                self.read_limit,
            )?
            .ok_or(AdapterError::Input("unknown tab key"))?
        };
        let previous = get(self.vm.heap(), s, S::Selected)?;
        if !selected.same_bits(previous) {
            for (entry, active) in [(previous, false), (selected, true)] {
                if entry.tag() != ValueTag::Null {
                    let row = get(self.vm.heap(), entry, E::Row)?;
                    if row.tag() != ValueTag::Null {
                        observable::publish(self.vm.heap_mut(), row, 2, Value::bool(active))?;
                    }
                }
            }
            put(self.vm.heap_mut(), s, S::Selected, selected)?;
        }
        if get(self.vm.heap(), s, S::Initialized)?.as_bool() == Some(true) {
            self.vm.heap_mut().commit_derived(output, root)?;
            self.vm.heap_mut().end_derived(output)?;
            self.vm.respond(t, HostResponse::Ready(root))?;
        } else {
            // Factory reads are one-time construction; their nested derivations
            // keep independent captures. This frame retains the no-wait context.
            self.vm.heap_mut().commit_derived(output, root)?;
            self.vm.heap_mut().end_derived(output)?;
            self.vm.heap_mut().begin_untracked(output)?;
            self.page_continue(t, &[output, s, Value::int(0), Value::NULL], TAB_BUILD)?;
        }
        Ok(())
    }
    pub(super) fn tabs_build(
        &mut self,
        session: &mut Session,
        t: HostTicket,
        output: Value,
        s: Value,
        index: u32,
        returned: bool,
    ) -> Result<(), AdapterError> {
        let root = get(self.vm.heap(), s, S::Root)?;
        let entries = get(self.vm.heap(), s, S::Entries)?;
        if index == self.vm.heap().sequence_len(entries)? {
            let children = self.vm.heap_mut().alloc_array(&[])?;
            for i in 0..self.vm.heap().sequence_len(entries)? {
                let e = self.vm.heap().sequence_get(entries, i)?;
                let option = get(self.vm.heap(), e, E::Node)?;
                self.vm.heap_mut().array_push(children, option)?;
            }
            self.stage
                .reconcile_list(self.vm.heap_mut(), root, children, self.read_limit)?;
            let id = self.stage.id(self.vm.heap(), root)?;
            self.stage
                .sync_tree(self.vm.heap_mut(), id, self.read_limit)?;
            put(self.vm.heap_mut(), s, S::Initialized, Value::bool(true))?;
            self.vm.heap_mut().end_derived(output)?;
            self.vm.respond(t, HostResponse::Ready(root))?;
            return Ok(());
        }
        let e = self.vm.heap().sequence_get(entries, index)?;
        let build = field(self.vm.heap(), root, TabField::Build.index() as u32)?;
        if !returned {
            let option = self.tab_option(t, root, e, build.tag() == ValueTag::Null)?;
            let key = get(self.vm.heap(), e, E::Key)?;
            let label = get(self.vm.heap(), e, E::Label)?;
            let selected = keys::compare(
                self.vm.heap(),
                key,
                field(self.vm.heap(), root, 0)?,
                self.read_limit,
            )?
            .is_eq();
            let resolved = self.vm.heap().property_argument(label)?;
            let row = observable::record(
                self.vm.heap_mut(),
                &[
                    ("key", key),
                    ("label", resolved),
                    ("selected", Value::bool(selected)),
                    ("control", option),
                ],
                &[Value::NULL],
            )?;
            self.stage
                .types
                .mark(self.vm.heap_mut(), row, "reel::types::TabItem")?;
            if self.vm.heap().is_property_ref(label) {
                self.start_tab_label(session, root, row, label)?;
            }
            put(self.vm.heap_mut(), e, E::Row, row)?;
            if build.tag() != ValueTag::Null {
                self.vm.respond(
                    t,
                    HostResponse::CallScript {
                        closure: build,
                        arguments: &[row],
                        continuation: Continuation {
                            version: 1,
                            stage: TAB_BUILT,
                            values: &[output, s, Value::int(i64::from(index)), Value::NULL],
                            faults: FaultRetention::Preserve,
                        },
                    },
                )?;
                return Ok(());
            }
        }
        let content = if returned {
            self.vm.host_callback_value(t)?
        } else {
            Value::NULL
        };
        if content.tag() != ValueTag::Null {
            self.stage.id(self.vm.heap(), content).map_err(|_| {
                AdapterError::Input("tab content factory must return a Node or null")
            })?;
            let option = get(self.vm.heap(), e, E::Node)?;
            self.stage
                .add_node(self.vm.heap_mut(), option, content, None)?;
        }
        self.page_continue(
            t,
            &[output, s, Value::int(i64::from(index + 1)), Value::NULL],
            TAB_BUILD,
        )?;
        Ok(())
    }
    fn tab_option(
        &mut self,
        t: HostTicket,
        root: Value,
        e: Value,
        decorated: bool,
    ) -> Result<Value, AdapterError> {
        let value = self.vm.heap_mut().alloc_property_ref(
            PropertyId {
                host: root,
                field: 0,
            },
            true,
        )?;
        let mut args = vec![
            get(self.vm.heap(), e, E::Key)?,
            get(self.vm.heap(), e, E::Label)?,
            value,
            Value::NULL,
        ];
        let mut props = layout(self.vm.heap_mut())?;
        props[stage::NodeProperty::Decorated.index()] = Value::bool(decorated);
        args.extend(props);
        args.push(Value::NULL);
        let origin = self.stage_origin(t);
        let option = self.stage.dispatch(
            &mut self.vm,
            self.root_owner,
            "reel::radio",
            &args,
            self.read_limit,
            origin,
        )?;
        put(self.vm.heap_mut(), e, E::Node, option)?;
        Ok(option)
    }
    pub(super) fn tabs_abort_error(
        &mut self,
        t: HostTicket,
        output: Value,
        s: Value,
        error: AdapterError,
    ) -> Result<(), AdapterError> {
        let Some(kind) = error.native_fault_kind() else {
            return Err(error);
        };
        self.vm.heap_mut().end_derived(output)?;
        self.vm.heap_mut().disconnect_derived(output)?;
        let details = if kind == FaultKind::Resource {
            Value::NULL
        } else {
            text::alloc_string(self.vm.heap_mut(), &error.to_string(), self.read_limit)?
        };
        self.page_continue(
            t,
            &[output, s, Value::int(0), details],
            TAB_ABORT_ERROR + kind as u64,
        )
    }
    pub(super) fn tabs_abort(
        &mut self,
        t: HostTicket,
        output: Value,
        s: Value,
        index: u32,
        value: Value,
        error: Option<FaultKind>,
    ) -> Result<(), AdapterError> {
        let entries = get(self.vm.heap(), s, S::Entries)?;
        if index < self.vm.heap().sequence_len(entries)? {
            let e = self.vm.heap().sequence_get(entries, index)?;
            let option = get(self.vm.heap(), e, E::Node)?;
            if option.tag() != ValueTag::Null && self.stage.id(self.vm.heap(), option).is_ok() {
                self.stage.remove_node(self.vm.heap_mut(), option)?;
            }
            let phase = error.map_or(TAB_ABORT, |kind| TAB_ABORT_ERROR + kind as u64);
            self.page_continue(
                t,
                &[output, s, Value::int(i64::from(index + 1)), value],
                phase,
            )?;
        } else {
            let root = get(self.vm.heap(), s, S::Root)?;
            self.stage.discard_unmounted(self.vm.heap_mut(), root)?;
            if let Some(kind) = error {
                self.vm.respond_language_error(t, kind, value)?;
            } else {
                self.vm.respond(
                    t,
                    HostResponse::ForwardFault(self.vm.heap().fault_id(value)?),
                )?;
            }
        }
        Ok(())
    }
    fn start_tab_label(
        &mut self,
        session: &mut Session,
        root: Value,
        row: Value,
        label: Value,
    ) -> Result<(), AdapterError> {
        let output = self.vm.heap_mut().alloc_derived(Value::NULL)?;
        let s = self.vm.heap_mut().alloc_array(&[root, row, label])?;
        self.vm.heap_mut().set_derived_state(output, s)?;
        self.vm.heap_mut().set_host_payload(row, 0, output)?;
        let function = self.page_method(4)?;
        let closure = self.vm.bind_native(output, function)?;
        let node = self.stage.id(self.vm.heap(), root)?;
        self.start_owned_task_in_context(session, Some(node), closure, &[], None, Context::NoWait)?;
        Ok(())
    }
    pub(super) fn tab_label(&mut self, t: HostTicket) -> Result<(), AdapterError> {
        let output = self.vm.host_arguments(t)?[0];
        let s = self.vm.heap().derived_state(output)?;
        let root = get(self.vm.heap(), s, 0u32)?;
        if self.stage.id(self.vm.heap(), root).is_err() {
            self.vm.heap_mut().disconnect_derived(output)?;
            self.vm.respond(t, HostResponse::Ready(Value::NULL))?;
        } else if t.input == NativeInput::Entry {
            self.vm.begin_reactive(
                t,
                output,
                false,
                Continuation {
                    version: 1,
                    stage: 0,
                    values: &[output],
                    faults: FaultRetention::Preserve,
                },
            )?;
        } else {
            let row = get(self.vm.heap(), s, 1u32)?;
            let reference = get(self.vm.heap(), s, 2u32)?;
            let (property, _) = self.vm.heap().property_ref(reference)?;
            self.vm
                .heap_mut()
                .observe_read(property.host, Some(property.field))?;
            let label = self
                .vm
                .heap()
                .property_argument(get(self.vm.heap(), s, 2u32)?)?;
            observable::publish(self.vm.heap_mut(), row, 1, label)?;
            self.vm.heap_mut().commit_derived(output, row)?;
            self.vm.heap_mut().end_derived(output)?;
            self.vm.respond(t, HostResponse::Ready(row))?;
        }
        Ok(())
    }
}
