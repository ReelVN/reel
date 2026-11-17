//! Turn an accepted viewport measurement into an ordinary offset write or tween.
use super::*;
use crate::aside::SessionVm;
use aside_vm::executor::{Continuation, FaultRetention, HostResponse, HostTicket, NativeInput};

aside_host::native_documentation! {
    pub(in crate::aside) fn documentation {
        /// Wait for an accepted frame and show a descendant of this scroll viewport.
        /// Margins inset each scrolling axis in logical pixels. A target larger than
        /// the usable area aligns its start. The nearest scroll ancestor must be this viewport.
        /// Return null for an immediate move or an already visible target. A nonzero
        /// duration returns an Activity, owned by the viewport unless an owner is given.
        /// Newer offset writes take priority over the accepted request's captured offset.
        "reel::scroll::reveal_child";
    }
}

impl SessionVm {
    pub(in crate::aside) fn dispatch_scroll_reveal(
        &mut self,
        ticket: HostTicket,
    ) -> Result<(), AdapterError> {
        let args = self.vm.host_arguments(ticket)?;
        let viewport = args[0];
        match ticket.input {
            NativeInput::Entry => {
                let node = args[1];
                let margins = [nonnegative(args[2])?, nonnegative(args[3])?];
                let owner = if args[6].tag() == ValueTag::Null {
                    viewport
                } else {
                    args[6]
                };
                super::super::super::scopes::validate_owner(self.vm.heap(), &self.stage, owner)?;
                nonnegative(args[4])?;
                let viewport_id = self.stage.id(self.vm.heap(), viewport)?;
                let target = self.stage.id(self.vm.heap(), node)?;
                let mut cursor = self.stage.scene.node(target).unwrap().parent;
                let mut nearest = None;
                while let Some(id) = cursor {
                    let ancestor = self.stage.scene.node(id).unwrap();
                    if matches!(ancestor.kind, NodeKind::Scroll { .. }) {
                        nearest = Some(id);
                        break;
                    }
                    cursor = ancestor.parent;
                }
                if nearest != Some(viewport_id) {
                    return Err(AdapterError::Input(
                        "reveal requires a node in this scroll viewport",
                    ));
                }
                let request = self.request_geometry(node, viewport, owner, Some(margins))?;
                self.scroll_reveal_call(ticket, "reel::completion::await", request, &[], 0)?;
            }
            NativeInput::CallbackFault => self.vm.respond(
                ticket,
                HostResponse::ForwardFault(self.vm.host_callback_fault(ticket)?.unwrap().id),
            )?,
            _ if ticket.continuation.unwrap().stage == 1 => {
                let activity = self.vm.host_resume_value(ticket)?;
                self.vm.respond(ticket, HostResponse::Ready(activity))?;
            }
            _ => {
                let duration = number(args[4])?;
                let easing = args[5];
                let owner = if args[6].tag() == ValueTag::Null {
                    viewport
                } else {
                    args[6]
                };
                let plan = self.vm.host_resume_value(ticket)?;
                let heap = self.vm.heap_mut();
                let mut goals = [Value::NULL; 6];
                let mut count = 0;
                for (i, field) in [ScrollField::X, ScrollField::Y].into_iter().enumerate() {
                    let property = PropertyId {
                        host: viewport,
                        field: field.index() as u32,
                    };
                    let current = number(heap.host_property(property)?)?;
                    let requested = number(heap.sequence_get(plan, i as u32)?)?;
                    let rendered = number(heap.sequence_get(plan, i as u32 + 2)?)?;
                    let target = number(heap.sequence_get(plan, i as u32 + 4)?)?;
                    if current != target && (current == requested || current == rendered) {
                        if duration == 0. {
                            heap.write_host_property(property, Value::float(target), true)?;
                        } else {
                            goals[count..count + 3].copy_from_slice(&[
                                Value::int(i64::from(property.field)),
                                Value::NULL,
                                Value::float(target),
                            ]);
                            count += 3;
                        }
                    }
                }
                if count == 0 {
                    self.vm.respond(ticket, HostResponse::Ready(Value::NULL))?;
                } else {
                    let action = super::super::super::activity::action::scroll_tween(
                        heap,
                        viewport,
                        duration,
                        easing,
                        &goals[..count],
                        self.read_limit,
                    )?;
                    self.scroll_reveal_call(
                        ticket,
                        "reel::run",
                        action,
                        &[owner, Value::bool(false)],
                        1,
                    )?;
                }
            }
        }
        self.issued = None;
        Ok(())
    }

    fn scroll_reveal_call(
        &mut self,
        ticket: HostTicket,
        name: &str,
        receiver: Value,
        arguments: &[Value],
        stage: u64,
    ) -> Result<(), AdapterError> {
        use aside_vm::bytecode::{FunctionId, FunctionKind};
        let package = self.vm.program().package();
        let function = package
            .functions
            .iter()
            .enumerate()
            .find_map(|(i, f)| {
                let FunctionKind::Native(id) = f.kind else {
                    return None;
                };
                (package.symbols[package.natives[id.index()].symbol.index()] == name)
                    .then_some(FunctionId(i as u32))
            })
            .ok_or(AdapterError::Input("scroll reveal adapter missing"))?;
        let closure = self.vm.bind_native(receiver, function)?;
        self.vm.respond(
            ticket,
            HostResponse::CallScript {
                closure,
                arguments,
                continuation: Continuation {
                    version: 1,
                    stage,
                    values: &[],
                    faults: FaultRetention::Preserve,
                },
            },
        )?;
        Ok(())
    }
}
