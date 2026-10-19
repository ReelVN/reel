//! One serialized native-window request, with portable state supplied by its host.
use super::{AdapterError, HostResponse, HostTicket, SessionVm, results, stage, text};
use aside_vm::{Heap, RootKind, RootToken, Value, ValueTag};
use reel_core::window::{WindowConfiguration, WindowInfo};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowRequestId {
    pub session: crate::SessionIdentity,
    pub serial: u64,
}
#[derive(Clone, Copy, Debug)]
pub struct WindowRequest {
    pub id: WindowRequestId,
    pub configuration: WindowConfiguration,
}
struct Pending {
    request: WindowRequest,
    root: RootToken,
    claimed: bool,
}
#[derive(Default)]
pub(super) struct Window {
    info: Arc<WindowInfo>,
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
    pub fn has_window_requests(&self) -> bool {
        !self.revoked && self.window.pending.as_ref().is_some_and(|p| !p.claimed)
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
        self.maintain_window_result()?;
        if self.window.pending.is_some() {
            return Err(AdapterError::Input("window configuration is Busy"));
        }
        let args = self.vm.host_arguments(ticket)?;
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
        let result = self.results.create_window(&mut self.vm, owner)?;
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
                configuration,
            },
            root,
            claimed: false,
        });
        self.vm.respond(ticket, HostResponse::Ready(result))?;
        self.issued = None;
        Ok(())
    }
    pub fn take_window_request(&mut self) -> Result<Option<WindowRequest>, AdapterError> {
        self.maintain_window_result()?;
        let Some(pending) = self.window.pending.as_mut().filter(|p| !p.claimed) else {
            return Ok(None);
        };
        pending.claimed = true;
        Ok(Some(pending.request))
    }
    pub fn complete_window_request(
        &mut self,
        id: WindowRequestId,
        result: Result<Arc<WindowInfo>, String>,
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
                Ok(info) => {
                    let result = value(self.vm.heap_mut(), &info)?;
                    self.window.info = info;
                    results::TaskResult::Returned(result)
                }
                Err(message) => results::TaskResult::Rejected(text::alloc_string(
                    self.vm.heap_mut(),
                    &message,
                    self.read_limit,
                )?),
            };
            results::finish_task(self.vm.heap_mut(), object, outcome)?;
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
                self.vm.heap_mut().release_root(pending.root)?;
                self.window.pending = None;
                self.results.changed();
            }
        }
        Ok(())
    }
}
