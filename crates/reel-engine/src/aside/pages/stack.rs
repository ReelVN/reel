//! Page tasks share the ordinary result, cancellation and snapshot machinery.
use super::*;

impl SessionVm {
    pub(super) fn attach_page_methods(&mut self, root: Value) -> Result<(), AdapterError> {
        for (index, name) in [
            (0, "get"),
            (1, "ensure"),
            (2, "evict"),
            (SELECT_METHOD, "select"),
        ] {
            let function = self.page_method(index)?;
            let method = self.vm.bind_native(root, function)?;
            let h = self.vm.heap_mut();
            let fields = h.host_fields(root)?;
            let key = h.alloc_string(name)?;
            h.table_store(fields, h.table_len(fields)?, key, method)?;
        }
        Ok(())
    }
    fn page_task(
        &mut self,
        session: &mut Session,
        root: Value,
        e: Value,
        retry: bool,
    ) -> Result<Value, AdapterError> {
        let task = get(self.vm.heap(), e, E::Task)?;
        if task.tag() != ValueTag::Null
            && (get(self.vm.heap(), e, E::Node)?.tag() != ValueTag::Null
                || field(self.vm.heap(), task, 0)?.as_bool() == Some(true)
                || !retry)
        {
            return Ok(task);
        }
        let task = self.results.create_task(&mut self.vm, root)?;
        let request = self.vm.heap_mut().alloc_array(&[root, e, task])?;
        let function = self.page_method(3)?;
        let closure = self.vm.bind_native(request, function)?;
        self.start_owned_task_in_context(session, None, closure, &[], Some(task), Context::NoWait)?;
        put(self.vm.heap_mut(), e, E::Task, task)?;
        Ok(task)
    }
    pub(super) fn page_operation(
        &mut self,
        session: &mut Session,
        t: HostTicket,
        symbol: &str,
    ) -> Result<(), AdapterError> {
        if symbol == "reel::page_stack::produce" {
            return self.page_produce(t);
        }
        if symbol == "reel::page_stack::commit_selection" {
            return self.commit_page_selection(t);
        }
        let args = self.vm.host_arguments(t)?.to_vec();
        let (root, key) = (args[0], args[1]);
        self.stage.id(self.vm.heap(), root)?;
        let s = state(self.vm.heap(), root)?;
        if get(self.vm.heap(), s, S::Tag)?.as_int() != Some(PAGE) {
            return Err(AdapterError::Input("expected page stack"));
        }
        let e = find(
            self.vm.heap(),
            get(self.vm.heap(), s, S::Sorted)?,
            key,
            self.read_limit,
        )?
        .ok_or(AdapterError::Input("unknown page key"))?;
        let node = get(self.vm.heap(), e, E::Node)?;
        let response = match symbol {
            "reel::page_stack::get" => node,
            "reel::page_stack::ensure" => self.page_task(session, root, e, true)?,
            "reel::page_stack::select" => self.select_page(session, root, s, e, key)?,
            "reel::page_stack::evict" => {
                if keys::compare(
                    self.vm.heap(),
                    field(self.vm.heap(), root, 0)?,
                    key,
                    self.read_limit,
                )?
                .is_eq()
                {
                    let message = self
                        .vm
                        .heap_mut()
                        .alloc_string("cannot evict the selected page")?;
                    self.vm
                        .respond_language_error(t, FaultKind::InvalidState, message)?;
                    return Ok(());
                }
                if node.tag() == ValueTag::Null {
                    Value::bool(false)
                } else {
                    let wrapper = get(self.vm.heap(), e, E::Wrapper)?;
                    self.stage.remove_node(self.vm.heap_mut(), wrapper)?;
                    if get(self.vm.heap(), s, S::Selected)?.same_bits(e) {
                        put(self.vm.heap_mut(), s, S::Selected, Value::NULL)?;
                    }
                    for f in [E::Node, E::Wrapper, E::Task] {
                        put(self.vm.heap_mut(), e, f, Value::NULL)?;
                    }
                    Value::bool(true)
                }
            }
            _ => return Err(AdapterError::Input("page operation")),
        };
        self.vm.respond(t, HostResponse::Ready(response))?;
        Ok(())
    }

    fn select_page(
        &mut self,
        session: &mut Session,
        root: Value,
        s: Value,
        e: Value,
        key: Value,
    ) -> Result<Value, AdapterError> {
        let property = PropertyId {
            host: root,
            field: PageField::Value.index() as u32,
        };
        if !self.vm.heap().property_source(property)?.1 {
            return Err(HeapError::ReadOnly.into());
        }
        let serial = Value::int(
            get(self.vm.heap(), s, S::SelectionRequest)?
                .as_int()
                .unwrap()
                .wrapping_add(1),
        );
        let producer = self.page_task(session, root, e, true)?;
        let task = self.results.create_task(&mut self.vm, root)?;
        let request = self
            .vm
            .heap_mut()
            .alloc_array(&[root, key, serial, producer])?;
        let commit = self.page_method(COMMIT_SELECTION)?;
        let closure = self.vm.bind_native(request, commit)?;
        self.start_owned_task_in_context(
            session,
            None,
            closure,
            &[],
            Some(task),
            Context::Ordinary,
        )?;
        put(self.vm.heap_mut(), s, S::SelectionRequest, serial)?;
        self.remember_page_selection(root, e, task);
        Ok(task)
    }

    fn commit_page_selection(&mut self, t: HostTicket) -> Result<(), AdapterError> {
        let request = self.vm.host_arguments(t)?[0];
        let root = get(self.vm.heap(), request, 0u32)?;
        let s = state(self.vm.heap(), root)?;
        let serial = get(self.vm.heap(), request, 2u32)?;
        if !get(self.vm.heap(), s, S::SelectionRequest)?.same_bits(serial) {
            self.vm.respond(t, HostResponse::Ready(Value::NULL))?;
        } else if t.input == NativeInput::Entry {
            let producer = get(self.vm.heap(), request, 3u32)?;
            let wait = self.page_method(AWAIT_TASK)?;
            let closure = self.vm.bind_native(producer, wait)?;
            self.vm.respond(
                t,
                HostResponse::CallScript {
                    closure,
                    arguments: &[],
                    continuation: Continuation {
                        version: 1,
                        stage: 0,
                        values: &[],
                        faults: FaultRetention::Preserve,
                    },
                },
            )?;
        } else if t.input == NativeInput::CallbackFault {
            self.vm.respond(
                t,
                HostResponse::ForwardFault(self.vm.host_callback_fault(t)?.unwrap().id),
            )?;
        } else {
            let node = self.vm.host_resume_value(t)?;
            let key = get(self.vm.heap(), request, 1u32)?;
            self.vm.heap_mut().write_host_property(
                PropertyId {
                    host: root,
                    field: PageField::Value.index() as u32,
                },
                key,
                true,
            )?;
            self.vm.respond(t, HostResponse::Ready(node))?;
        }
        Ok(())
    }
    fn page_produce(&mut self, t: HostTicket) -> Result<(), AdapterError> {
        let request = self.vm.host_arguments(t)?[0];
        let root = get(self.vm.heap(), request, 0u32)?;
        let e = get(self.vm.heap(), request, 1u32)?;
        self.stage.id(self.vm.heap(), root)?;
        if t.input == NativeInput::Entry {
            self.vm.respond(
                t,
                HostResponse::CallScript {
                    closure: get(self.vm.heap(), e, E::Build)?,
                    arguments: &[],
                    continuation: Continuation {
                        version: 1,
                        stage: 0,
                        values: &[request],
                        faults: FaultRetention::Preserve,
                    },
                },
            )?;
        } else if t.input == NativeInput::CallbackFault {
            self.vm.respond(
                t,
                HostResponse::ForwardFault(self.vm.host_callback_fault(t)?.unwrap().id),
            )?;
        } else {
            let node = self.vm.host_resume_value(t)?;
            let candidate = self.stage.id(self.vm.heap(), node);
            let unattached = candidate.as_ref().is_ok_and(|id| {
                self.stage
                    .scene
                    .node(*id)
                    .is_some_and(|node| node.parent.is_none())
                    && self.stage.scene.root() != Some(*id)
            });
            if !unattached {
                let id = self.stage.id(self.vm.heap(), root)?;
                let origin = self
                    .stage
                    .scene
                    .node(id)
                    .unwrap()
                    .props
                    .appearance_origin
                    .as_deref();
                let message = origin.map_or_else(
                    || "page factory must return an unattached live Node".to_owned(),
                    |origin| format!("{origin}: page factory must return an unattached live Node"),
                );
                let details = text::alloc_string(self.vm.heap_mut(), &message, self.read_limit)?;
                self.vm
                    .respond_language_error(t, FaultKind::Type, details)?;
                return Ok(());
            }
            let mut args = layout(self.vm.heap_mut())?;
            args[stage::NodeProperty::Visible.index()] = Value::bool(false);
            args[stage::NodeProperty::Stretch.index()] = self.vm.heap_mut().alloc_string("both")?;
            let children = self.vm.heap_mut().alloc_array(&[node])?;
            args.push(children);
            let origin = self.stage_origin(t);
            let wrapper = self.stage.dispatch(
                &mut self.vm,
                self.root_owner,
                "reel::stack",
                &args,
                self.read_limit,
                origin,
            )?;
            self.stage
                .add_node(self.vm.heap_mut(), root, wrapper, None)?;
            put(self.vm.heap_mut(), e, E::Node, node)?;
            put(self.vm.heap_mut(), e, E::Wrapper, wrapper)?;
            self.vm.respond(t, HostResponse::Ready(node))?;
        }
        Ok(())
    }
    pub(super) fn pages_watch(
        &mut self,
        session: &mut Session,
        t: HostTicket,
        output: Value,
        s: Value,
    ) -> Result<(), AdapterError> {
        let root = get(self.vm.heap(), s, S::Root)?;
        self.vm
            .heap_mut()
            .observe_read(root, Some(PageField::Value.index() as u32))?;
        let key = field(self.vm.heap(), root, PageField::Value.index() as u32)?;
        let selected = if key.tag() == ValueTag::Null {
            Value::NULL
        } else {
            find(
                self.vm.heap(),
                get(self.vm.heap(), s, S::Sorted)?,
                key,
                self.read_limit,
            )?
            .ok_or(AdapterError::Input("unknown page key"))?
        };
        let mut ready = true;
        if selected.tag() != ValueTag::Null {
            self.vm.heap_mut().observe_read(selected, None)?;
            let task = self.page_task(session, root, selected, false)?;
            self.vm.heap_mut().observe_read(task, Some(0))?;
            let pending = field(self.vm.heap(), task, 0)?.as_bool().unwrap();
            if pending {
                self.remember_page_selection(root, selected, task);
            }
            ready = !pending;
            if !pending && get(self.vm.heap(), selected, E::Node)?.tag() == ValueTag::Null {
                self.vm.heap_mut().end_derived(output)?;
                if let Some(VmCompletion::Error(error)) =
                    results::response(self.vm.heap_mut(), task)?
                {
                    self.vm.respond(t, HostResponse::Error(error))?;
                    return Ok(());
                }
            }
        }
        if ready {
            let previous = get(self.vm.heap(), s, S::Selected)?;
            if !previous.same_bits(selected) {
                for (entry, visible) in [(previous, false), (selected, true)] {
                    if entry.tag() != ValueTag::Null {
                        let wrapper = get(self.vm.heap(), entry, E::Wrapper)?;
                        self.vm.heap_mut().write_host_property(
                            PropertyId {
                                host: wrapper,
                                field: stage::NodeProperty::Visible.index() as u32,
                            },
                            Value::bool(visible),
                            false,
                        )?;
                    }
                }
                put(self.vm.heap_mut(), s, S::Selected, selected)?;
            }
        }
        put(self.vm.heap_mut(), s, S::Initialized, Value::bool(true))?;
        self.vm.heap_mut().commit_derived(output, root)?;
        self.vm.heap_mut().end_derived(output)?;
        self.vm.respond(t, HostResponse::Ready(root))?;
        Ok(())
    }
}

impl stage::StageBinding {
    pub(in crate::aside) fn pages_ready(&self, h: &Heap, limit: usize) -> bool {
        for &id in &self.page_stacks {
            let mut cursor = Some(id);
            let mut mounted = false;
            while let Some(id) = cursor {
                let Some(node) = self.scene.node(id) else {
                    break;
                };
                if !node.props.visible {
                    break;
                }
                if Some(id) == self.scene.root() {
                    mounted = true;
                    break;
                }
                cursor = node.parent;
            }
            if !mounted {
                continue;
            }
            let ready = (|| -> Result<bool, AdapterError> {
                let root = self.value(h, id)?;
                let s = state(h, root)?;
                let key = field(h, root, 0)?;
                let selected = get(h, s, S::Selected)?;
                if key.tag() == ValueTag::Null {
                    return Ok(selected.tag() == ValueTag::Null);
                }
                Ok(selected.tag() != ValueTag::Null
                    && keys::compare(h, key, get(h, selected, E::Key)?, limit)?.is_eq())
            })();
            if !ready.unwrap_or(false) {
                return false;
            }
        }
        true
    }
}
