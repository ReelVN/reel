//! One serialized native-window request, with portable state supplied by its host.
use super::{AdapterError, HostResponse, HostTicket, SessionVm, observable, results, stage, text};
use aside_vm::{Heap, RootKind, RootToken, Value, ValueTag};
use reel_core::window::{WindowConfiguration, WindowInfo};
use reel_ui::input::FocusVisibility;
use reel_ui::stage::NodeId;
use std::sync::Arc;

#[derive(Clone, Copy, Debug)]
pub enum WindowCommand {
    Configure(WindowConfiguration),
    MovePointer {
        position: [f64; 2],
        owner: NodeId,
    },
    /// Focus a control after the frame carrying this request has been accepted.
    Focus {
        target: NodeId,
        visibility: FocusVisibility,
    },
}
pub enum WindowReply {
    Configured(Arc<WindowInfo>),
    PointerMoved(bool),
    Focused(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowRequestId {
    pub session: crate::SessionIdentity,
    pub serial: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct WindowRequest {
    pub id: WindowRequestId,
    pub command: WindowCommand,
}
struct Pending {
    request: WindowRequest,
    root: RootToken,
    claimed: bool,
    presentation: Option<u64>,
}
#[derive(Default)]
pub(super) struct Window {
    info: Arc<WindowInfo>,
    pointer: Option<[f64; 2]>,
    pub(super) pointer_revision: Option<RootToken>,
    serial: u64,
    pending: Option<Pending>,
}
impl Window {
    pub fn active(&self) -> bool {
        self.pending.is_some()
    }
}

fn table(heap: &mut Heap, fields: &[(&str, Value)]) -> Result<Value, AdapterError> {
    let value = heap.alloc_table()?;
    for (index, (key, field)) in fields.iter().enumerate() {
        let key = heap.alloc_string(key)?;
        heap.table_store(value, index as u32, key, *field)?;
    }
    Ok(value)
}
fn value(heap: &mut Heap, info: &WindowInfo) -> Result<Value, AdapterError> {
    let platform = heap.alloc_string(info.platform.as_str())?;
    let capabilities = table(
        heap,
        &[
            ("resize", Value::bool(info.capabilities.resize)),
            ("fullscreen", Value::bool(info.capabilities.fullscreen)),
            ("vsync", Value::bool(info.capabilities.vsync)),
            ("pointer_move", Value::bool(info.capabilities.pointer_move)),
        ],
    )?;
    let mut resolutions = Vec::with_capacity(info.resolutions.len());
    for resolution in &*info.resolutions {
        resolutions.push(table(
            heap,
            &[
                ("width", Value::float(resolution.width)),
                ("height", Value::float(resolution.height)),
            ],
        )?);
    }
    let resolutions = heap.alloc_array(&resolutions)?;
    table(
        heap,
        &[
            ("platform", platform),
            ("width", Value::float(info.size.width)),
            ("height", Value::float(info.size.height)),
            ("windowed_width", Value::float(info.windowed_size.width)),
            ("windowed_height", Value::float(info.windowed_size.height)),
            ("scale_factor", Value::float(info.scale_factor)),
            ("fullscreen", Value::bool(info.fullscreen)),
            ("vsync", Value::bool(info.vsync)),
            ("capabilities", capabilities),
            ("resolutions", resolutions),
        ],
    )
}

impl SessionVm {
    pub fn set_window_info(&mut self, info: Arc<WindowInfo>) {
        self.window.info = info;
    }
    pub fn window_info(&self) -> &Arc<WindowInfo> {
        &self.window.info
    }
    /// Publish ephemeral accepted stage coordinates. Returns whether the position changed.
    /// Host updates compare coordinates and notify existing subscriptions;
    /// the first derived read creates the source.
    pub fn set_pointer_position(
        &mut self,
        position: Option<[f64; 2]>,
    ) -> Result<bool, AdapterError> {
        if self.window.pointer == position {
            return Ok(false);
        }
        self.invalidate_pointer_observation()?;
        self.window.pointer = position;
        Ok(true)
    }
    pub(super) fn invalidate_pointer_observation(&mut self) -> Result<(), AdapterError> {
        if let Some(root) = self.window.pointer_revision {
            let heap = self.vm.heap_mut();
            let host = heap.root_value(root)?;
            let previous = heap
                .host_property(aside_vm::PropertyId { host, field: 0 })?
                .as_bool()
                .unwrap();
            observable::publish(heap, host, 0, Value::bool(!previous))?;
        }
        Ok(())
    }
    fn observe_pointer(&mut self) -> Result<(), AdapterError> {
        let heap = self.vm.heap_mut();
        let host = if let Some(root) = self.window.pointer_revision {
            heap.root_value(root)?
        } else {
            let host = observable::record(heap, &[("revision", Value::bool(false))], &[])?;
            self.window.pointer_revision =
                Some(heap.add_root(self.root_owner, RootKind::Host, host)?);
            host
        };
        heap.observe_read(host, Some(0))?;
        Ok(())
    }
    pub fn has_window_requests(&self) -> bool {
        !self.revoked
            && self.window.pending.as_ref().is_some_and(|pending| {
                !pending.claimed
                    && pending
                        .presentation
                        .is_none_or(|request| self.results.presentation_reached(request))
            })
    }
    pub fn pointer_owner_permits_input(&self, owner: NodeId) -> Result<bool, AdapterError> {
        self.stage.permits_input(self.vm.heap(), owner)
    }
    /// Check current script ownership and input flags before applying focus.
    /// A target retired while its request waits returns false.
    pub fn focus_target_permits_input(&self, target: NodeId) -> Result<bool, AdapterError> {
        if self.stage.scene.node(target).is_none() {
            return Ok(false);
        }
        self.stage.permits_input(self.vm.heap(), target)
    }
    pub(super) fn dispatch_window(
        &mut self,
        ticket: HostTicket,
        symbol: &str,
    ) -> Result<(), AdapterError> {
        if symbol == "reel::window::info" {
            let state = value(self.vm.heap_mut(), &self.window.info)?;
            self.vm.respond(ticket, HostResponse::Ready(state))?;
            self.issued = None;
            return Ok(());
        }
        if symbol == "reel::stage::pointer" {
            if self.vm.heap().reactive_evaluating() {
                self.observe_pointer()?;
            }
            let state = match self.window.pointer {
                Some([x, y]) => table(
                    self.vm.heap_mut(),
                    &[("x", Value::float(x)), ("y", Value::float(y))],
                )?,
                None => Value::NULL,
            };
            self.vm.respond(ticket, HostResponse::Ready(state))?;
            self.issued = None;
            return Ok(());
        }
        self.maintain_window_result()?;
        if self.window.pending.is_some() {
            return Err(AdapterError::Input("window request is Busy"));
        }
        let args = self.vm.host_arguments(ticket)?;
        if symbol == "reel::stage::focus" {
            let target = self.stage.id(self.vm.heap(), args[0])?;
            let visibility = match text::read_string(self.vm.heap(), args[2], 16)?.as_str() {
                "show" => FocusVisibility::Show,
                "preserve" => FocusVisibility::Preserve,
                _ => {
                    return Err(AdapterError::Input(
                        "focus visibility must be show or preserve",
                    ));
                }
            };
            let owner = if args[1].tag() == ValueTag::Null {
                self.default_activity_owner(ticket.task)?
            } else {
                args[1]
            };
            super::scopes::validate_owner(self.vm.heap(), &self.stage, owner)?;
            self.submit_window(ticket, WindowCommand::Focus { target, visibility }, owner)?;
            return Ok(());
        }
        if symbol == "reel::stage::move_pointer" {
            let position = [stage::number(args[0])?, stage::number(args[1])?];
            let owner = args[2];
            let id = self.stage.id(self.vm.heap(), owner)?;
            self.submit_window(
                ticket,
                WindowCommand::MovePointer {
                    position,
                    owner: id,
                },
                owner,
            )?;
            return Ok(());
        }
        let dimension = |value: Value| -> Result<Option<f64>, AdapterError> {
            if value.tag() == ValueTag::Null {
                return Ok(None);
            }
            let value = stage::number(value)?;
            if value <= 0. {
                return Err(AdapterError::Input("window dimensions must be positive"));
            }
            Ok(Some(value))
        };
        let boolean = |value: Value| -> Result<Option<bool>, AdapterError> {
            if value.tag() == ValueTag::Null {
                Ok(None)
            } else {
                value
                    .as_bool()
                    .map(Some)
                    .ok_or(AdapterError::Input("window flags must be bool"))
            }
        };
        let configuration = WindowConfiguration {
            width: dimension(args[0])?,
            height: dimension(args[1])?,
            fullscreen: boolean(args[2])?,
            vsync: boolean(args[3])?,
        };
        let caps = self.window.info.capabilities;
        if ((configuration.width.is_some() || configuration.height.is_some()) && !caps.resize)
            || (configuration.fullscreen.is_some() && !caps.fullscreen)
            || (configuration.vsync.is_some() && !caps.vsync)
        {
            return Err(AdapterError::Input(
                "requested window configuration is unsupported by this host",
            ));
        }
        if !caps.resize && !caps.fullscreen && !caps.vsync {
            return Err(AdapterError::Input(
                "window configuration is unsupported by this host",
            ));
        }
        let owner = if args[4].tag() == ValueTag::Null {
            self.default_activity_owner(ticket.task)?
        } else {
            args[4]
        };
        if owner.tag() != ValueTag::Null {
            super::scopes::validate_owner(self.vm.heap(), &self.stage, owner)?;
        }
        self.submit_window(ticket, WindowCommand::Configure(configuration), owner)
    }
    fn submit_window(
        &mut self,
        ticket: HostTicket,
        command: WindowCommand,
        owner: Value,
    ) -> Result<(), AdapterError> {
        let result = if matches!(command, WindowCommand::Focus { .. }) {
            self.results.create_focus(&mut self.vm, owner)?
        } else {
            self.results.create_window(&mut self.vm, owner)?
        };
        if matches!(command, WindowCommand::MovePointer { .. })
            && !self.window.info.capabilities.pointer_move
        {
            results::finish_task(
                self.vm.heap_mut(),
                result,
                results::TaskResult::Returned(Value::bool(false)),
            )?;
            self.vm.respond(ticket, HostResponse::Ready(result))?;
            self.issued = None;
            return Ok(());
        }
        let presentation = if let WindowCommand::Focus { target, .. } = command {
            let target = self.stage.value(self.vm.heap(), target)?;
            results::attach(self.vm.heap_mut(), result, target)?;
            Some(self.results.request_presentation()?)
        } else {
            None
        };
        let root = self
            .vm
            .heap_mut()
            .add_root(self.root_owner, RootKind::Host, result)?;
        self.window.serial += 1;
        self.window.pending = Some(Pending {
            request: WindowRequest {
                id: WindowRequestId {
                    session: self.identity,
                    serial: self.window.serial,
                },
                command,
            },
            root,
            claimed: false,
            presentation,
        });
        self.vm.respond(ticket, HostResponse::Ready(result))?;
        self.issued = None;
        Ok(())
    }
    pub fn take_window_request(&mut self) -> Result<Option<WindowRequest>, AdapterError> {
        self.maintain_window_result()?;
        let Some(pending) = self.window.pending.as_mut().filter(|pending| {
            !pending.claimed
                && pending
                    .presentation
                    .is_none_or(|request| self.results.presentation_reached(request))
        }) else {
            return Ok(None);
        };
        pending.claimed = true;
        Ok(Some(pending.request))
    }
    pub fn complete_window_request(
        &mut self,
        id: WindowRequestId,
        result: Result<WindowReply, String>,
    ) -> Result<bool, AdapterError> {
        if self.revoked || id.session != self.identity {
            return Ok(false);
        }
        let Some(pending) = self.window.pending.as_ref().filter(|p| p.request.id == id) else {
            return Ok(false);
        };
        let root = pending.root;
        let object = self.vm.heap().root_value(root)?;
        if results::pending(self.vm.heap_mut(), object)? {
            let outcome = match result {
                Ok(WindowReply::Configured(info)) => {
                    let result = value(self.vm.heap_mut(), &info)?;
                    self.window.info = info;
                    results::TaskResult::Returned(result)
                }
                Ok(WindowReply::PointerMoved(applied) | WindowReply::Focused(applied)) => {
                    results::TaskResult::Returned(Value::bool(applied))
                }
                Err(message) => results::TaskResult::Rejected(text::alloc_string(
                    self.vm.heap_mut(),
                    &message,
                    self.read_limit,
                )?),
            };
            results::finish_task(self.vm.heap_mut(), object, outcome)?;
        }
        if matches!(pending.request.command, WindowCommand::Focus { .. }) {
            results::attach(self.vm.heap_mut(), object, Value::NULL)?;
        }
        self.vm.heap_mut().release_root(root)?;
        self.window.pending = None;
        self.results.changed();
        Ok(true)
    }
    fn maintain_window_result(&mut self) -> Result<(), AdapterError> {
        if let Some(pending) = &self.window.pending {
            let object = self.vm.heap().root_value(pending.root)?;
            if !pending.claimed && !results::pending(self.vm.heap_mut(), object)? {
                if matches!(pending.request.command, WindowCommand::Focus { .. }) {
                    results::attach(self.vm.heap_mut(), object, Value::NULL)?;
                }
                self.vm.heap_mut().release_root(pending.root)?;
                self.window.pending = None;
                self.results.changed();
            }
        }
        Ok(())
    }
}

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Return the host's current portable window information immediately.
        /// Fields include platform, logical width/height, remembered windowed dimensions,
        /// scale_factor, fullscreen, vsync, supported resolutions, and capabilities for
        /// resize, fullscreen, vsync and pointer_move. This reads host state without a request.
        "reel::window::info";
        /// Request changes to logical window dimensions, fullscreen or vsync.
        /// Null fields preserve existing settings; dimensions must be positive and finite.
        /// Unsupported capabilities and a concurrent pending window request are rejected.
        /// Returns an awaitable result whose value is the actual host window information,
        /// rather than a prediction of the requested settings. `owner` defaults to the current
        /// scope owner; retirement cancels the result. Host application of a claimed native
        /// request is not reversed by cancelling its result.
        "reel::window::configure";
        /// Return the last accepted stage-space pointer position as {x, y}, or null when
        /// the host has no pointer position. Inside derive, tracks actual position changes,
        /// including transitions to or from null; ordinary calls return independent snapshots.
        /// Updates run when the host publishes a changed position. Restored bindings read
        /// null until the host publishes current input; returned snapshots save normally.
        "reel::stage::pointer";
        /// Request a host pointer move to finite stage-space x/y coordinates.
        /// `owner` is a required node and must still be permitted to receive input when the
        /// host applies the request. Returns an awaitable bool: false when pointer movement
        /// is unsupported or declined, true when applied. Owner retirement cancels the result;
        /// a concurrent pending window request is rejected.
        "reel::stage::move_pointer";
        /// Request keyboard/controller focus on an author control. Waits for an accepted
        /// frame containing stage changes made before this call, then applies focus through
        /// the current input/modal scope. Returns a FocusOperation, a WindowOperation
        /// subtype whose awaited result is bool: true when focused,
        /// false when the target is hidden, disabled, retired or not focusable in that frame.
        /// A declined request preserves the existing focus. `visibility: "show"` (default)
        /// displays the focus indicator; "preserve" keeps its state when the handoff is
        /// applied. Navigation or confirmation presses still display it normally.
        /// Owner defaults to the current scope; retirement
        /// cancels an unfinished request. Shares the bounded window-request slot.
        "reel::stage::focus";
    }
}
pub(super) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{named, nullable, number};
    api.refine("reel::window::info", TypeHint::Table, &[]);
    api.refine(
        "reel::window::configure",
        named("reel::types::WindowOperation"),
        &[
            ("width", nullable(number())),
            ("height", nullable(number())),
            ("fullscreen", nullable(TypeHint::Bool)),
            ("vsync", nullable(TypeHint::Bool)),
        ],
    );
    api.refine("reel::stage::pointer", nullable(TypeHint::Table), &[]);
    api.refine(
        "reel::stage::focus",
        named("reel::types::FocusOperation"),
        &[
            ("target", named("reel::types::Node")),
            ("visibility", TypeHint::String),
        ],
    );
    api.refine(
        "reel::stage::move_pointer",
        named("reel::types::WindowOperation"),
        &[
            ("x", number()),
            ("y", number()),
            ("owner", named("reel::types::Node")),
        ],
    );
}
