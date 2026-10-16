//! Frame capture requests. The host supplies pixels; the language owns no GPU handles.
use super::{AdapterError, HostResponse, HostTicket, SessionVm, data, results, text};
use aside_vm::{HeapReservation, RootKind, RootToken, Value, ValueTag};
use std::{collections::VecDeque, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureId {
    pub session: crate::SessionIdentity,
    pub serial: u64,
}
#[derive(Clone, Debug)]
pub struct CaptureRequest {
    pub id: CaptureId,
    pub width: Option<u32>,
    pub height: Option<u32>,
}
struct Pending {
    request: CaptureRequest,
    root: RootToken,
    claimed: bool,
    restore: Option<data::Restore>,
}
#[derive(Default)]
pub(super) struct Captures {
    serial: u64,
    pending: VecDeque<Pending>,
    charge: Option<HeapReservation>,
}
impl Captures {
    pub fn active(&self) -> bool {
        !self.pending.is_empty()
    }
}

impl SessionVm {
    pub fn has_capture_requests(&self) -> bool {
        !self.revoked && self.captures.pending.iter().any(|pending| !pending.claimed)
    }
    pub(super) fn dispatch_capture(&mut self, ticket: HostTicket) -> Result<(), AdapterError> {
        if ticket.context.no_wait || ticket.context.init || ticket.context.derived {
            return Err(AdapterError::Vm(aside_vm::executor::VmError::Effect));
        }
        let args = self.vm.host_arguments(ticket)?;
        let dimension = |value: Value| -> Result<Option<u32>, AdapterError> {
            if value.tag() == ValueTag::Null {
                return Ok(None);
            }
            value
                .as_int()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| *n > 0)
                .map(Some)
                .ok_or(AdapterError::Input(
                    "capture dimensions must be positive integers",
                ))
        };
        let width = dimension(args[0])?;
        let height = dimension(args[1])?;
        let owner = if args[2].tag() == ValueTag::Null {
            self.default_activity_owner(ticket.task)?
        } else {
            args[2]
        };
        if owner.tag() != ValueTag::Null {
            self.stage.id(self.vm.heap(), owner)?;
        }
        results::reserve_queue(
            self.vm.heap(),
            &mut self.captures.pending,
            &mut self.captures.charge,
        )?;
        let value = self.results.create_capture(&mut self.vm, owner)?;
        let root = self
            .vm
            .heap_mut()
            .add_root(self.root_owner, RootKind::Host, value)?;
        self.captures.serial += 1;
        self.captures.pending.push_back(Pending {
            request: CaptureRequest {
                id: CaptureId {
                    session: self.identity,
                    serial: self.captures.serial,
                },
                width,
                height,
            },
            root,
            claimed: false,
            restore: None,
        });
        self.vm.respond(ticket, HostResponse::Ready(value))?;
        self.issued = None;
        Ok(())
    }
    pub fn take_capture_request(&mut self) -> Result<Option<CaptureRequest>, AdapterError> {
        self.maintain_capture_results(0)?;
        let Some(pending) = self.captures.pending.iter_mut().find(|p| !p.claimed) else {
            return Ok(None);
        };
        pending.claimed = true;
        Ok(Some(pending.request.clone()))
    }
    pub fn capture_pending(&mut self, id: CaptureId) -> Result<bool, AdapterError> {
        if self.revoked || id.session != self.identity {
            return Ok(false);
        }
        let Some(pending) = self.captures.pending.iter().find(|p| p.request.id == id) else {
            return Ok(false);
        };
        let object = self.vm.heap().root_value(pending.root)?;
        results::pending(self.vm.heap_mut(), object)
    }
    pub fn complete_capture_request(
        &mut self,
        id: CaptureId,
        result: Result<Vec<u8>, String>,
    ) -> Result<bool, AdapterError> {
        if !self.capture_pending(id)? {
            return Ok(false);
        }
        let index = self
            .captures
            .pending
            .iter()
            .position(|p| p.request.id == id)
            .unwrap();
        let result = result.and_then(|bytes| {
            data::Restore::new(
                self.vm.heap_mut(),
                self.root_owner,
                Arc::new(data::DataDocument::from_bytes(bytes)),
            )
            .map_err(|e| e.to_string())
        });
        match result {
            Ok(restore) => self.captures.pending[index].restore = Some(restore),
            Err(message) => {
                let object = self
                    .vm
                    .heap()
                    .root_value(self.captures.pending[index].root)?;
                let message = text::alloc_string(self.vm.heap_mut(), &message, self.read_limit)?;
                results::finish_task(
                    self.vm.heap_mut(),
                    object,
                    results::TaskResult::Rejected(message),
                )?;
                self.results.changed();
            }
        }
        Ok(true)
    }
    pub fn capture_results_pending(&self) -> bool {
        self.captures.pending.iter().any(|p| p.restore.is_some())
    }
    pub fn maintain_capture_results(&mut self, work: u32) -> Result<(), AdapterError> {
        let mut remaining = work;
        let count = self.captures.pending.len();
        for _ in 0..count {
            let mut pending = self.captures.pending.pop_front().unwrap();
            let object = self.vm.heap().root_value(pending.root)?;
            let mut keep = results::pending(self.vm.heap_mut(), object)?;
            if keep
                && remaining != 0
                && let Some(restore) = &mut pending.restore
            {
                let result = restore.step(self.vm.heap_mut(), remaining, self.read_limit);
                remaining = 0;
                if !matches!(result, Ok(false)) {
                    let result = match result {
                        Ok(true) => {
                            let root = restore.value_root(self.vm.heap_mut(), self.root_owner)?;
                            let value = self.vm.heap().root_value(root)?;
                            results::finish_task(
                                self.vm.heap_mut(),
                                object,
                                results::TaskResult::Returned(value),
                            )?;
                            self.vm.heap_mut().release_root(root)?;
                            Ok(())
                        }
                        Err(error) => {
                            let value = text::alloc_string(
                                self.vm.heap_mut(),
                                &error.to_string(),
                                self.read_limit,
                            )?;
                            results::finish_task(
                                self.vm.heap_mut(),
                                object,
                                results::TaskResult::Rejected(value),
                            )
                        }
                        _ => unreachable!(),
                    };
                    result?;
                    keep = false;
                    self.results.changed();
                }
            }
            if keep {
                self.captures.pending.push_back(pending);
            } else {
                if let Some(restore) = pending.restore {
                    restore.release(self.vm.heap_mut())?;
                }
                self.vm.heap_mut().release_root(pending.root)?;
                self.results.changed();
            }
        }
        if self.captures.pending.is_empty() {
            self.captures.pending.shrink_to_fit();
            self.captures.charge = None;
        }
        Ok(())
    }
}
