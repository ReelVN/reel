use super::{AdapterError, HostTicket, SessionVm, stage, text};
use crate::audio::{AudioBackend, AudioCommand, AudioId, AudioRequest, AudioResult};
use aside_vm::executor::{Completion, HostResponse, WaitId};
use aside_vm::{Heap, HeapError, RootKind, RootToken, Value, ValueTag};
use reel_ui::stage::NodeId;

const TAG: i64 = -1003;
#[repr(u32)]
enum Slot {
    Tag,
    Id,
    Result,
    Error,
}
struct Live {
    id: AudioId,
    root: RootToken,
    owner: Option<NodeId>,
    result: Option<AudioResult>,
}
#[derive(Default)]
pub(super) struct Audio {
    backend: Option<Box<dyn AudioBackend>>,
    live: Vec<Live>,
    waits: Vec<(WaitId, Value)>,
    cursor: usize,
}
pub(super) fn is_audio(heap: &Heap, value: Value) -> bool {
    heap.host_payload(value, Slot::Tag as u32)
        .ok()
        .and_then(Value::as_int)
        == Some(TAG)
}
fn readonly(_: &Heap, _: Value, _: u32, _: Value) -> Result<(), HeapError> {
    Err(HeapError::ReadOnly)
}
fn response(heap: &Heap, value: Value) -> Result<Option<Completion>, AdapterError> {
    let result = heap.host_payload(value, Slot::Result as u32)?;
    if result.tag() == ValueTag::Null {
        return Ok(None);
    }
    let code = heap
        .host_payload(value, Slot::Error as u32)?
        .as_int()
        .unwrap();
    Ok(Some(if code == 0 {
        Completion::Ready(result)
    } else {
        Completion::Error(aside_vm::NewFault {
            value: result,
            code: aside_vm::FaultCode::core(if code == 2 {
                super::FaultKind::Cancelled
            } else {
                super::FaultKind::InvalidState
            }),
            cause: None,
        })
    }))
}
fn volume(value: Value) -> Result<f64, AdapterError> {
    let value = stage::number(value)?;
    if value < 0. {
        return Err(AdapterError::Input("audio volume must be nonnegative"));
    }
    Ok(value)
}
impl SessionVm {
    /// Install once before executing author code. No platform dependency enters the VM.
    pub fn set_audio(&mut self, backend: Box<dyn AudioBackend>) -> Result<(), AdapterError> {
        if self.audio.backend.is_some() || self.revoked {
            return Err(AdapterError::Input(
                "audio backend already installed or session retired",
            ));
        }
        self.audio.backend = Some(backend);
        Ok(())
    }
    pub fn audio_poll_after(&self) -> Option<std::time::Duration> {
        self.audio.backend.as_ref().and_then(|b| b.poll_after())
    }
    pub fn suspend_audio(&mut self) {
        if let Some(backend) = &mut self.audio.backend {
            backend.suspend();
        }
    }
    pub fn resume_audio(&mut self) {
        if let Some(backend) = &mut self.audio.backend {
            backend.resume();
        }
    }
    pub fn maintain_audio(&mut self) -> Result<bool, AdapterError> {
        let Some(backend) = &mut self.audio.backend else {
            return Ok(false);
        };
        backend.maintain();
        for _ in 0..self.audio.live.len().min(64) {
            if self.audio.cursor >= self.audio.live.len() {
                self.audio.cursor = 0;
            }
            let live = &mut self.audio.live[self.audio.cursor];
            if live.result.is_none() {
                if live
                    .owner
                    .is_some_and(|id| self.stage.scene.node(id).is_none())
                {
                    backend.cancel(live.id);
                    live.result = Some(AudioResult::Cancelled);
                } else {
                    live.result = backend.take_result(live.id);
                }
            }
            if let Some(result) = &live.result {
                let (message, code) = match result {
                    AudioResult::Completed => ("Completed", 0),
                    AudioResult::Stopped => ("Stopped", 0),
                    AudioResult::Cancelled => ("audio owner retired", 2),
                    AudioResult::Failed(message) => (message.as_str(), 1),
                };
                let heap = self.vm.heap_mut();
                let message = heap.alloc_string(message)?;
                let value = heap.root_value(live.root)?;
                heap.set_host_payload(value, Slot::Result as u32, message)?;
                heap.set_host_payload(value, Slot::Error as u32, Value::int(code))?;
                heap.release_root(live.root)?;
                self.audio.live.swap_remove(self.audio.cursor);
            } else {
                self.audio.cursor += 1;
            }
        }
        let mut woke = false;
        let mut i = 0;
        while i < self.audio.waits.len() {
            let (wait, value) = self.audio.waits[i];
            if !self.vm.wait_is_pending(wait) {
                self.audio.waits.swap_remove(i);
            } else if let Some(result) = response(self.vm.heap(), value)? {
                self.vm.complete(wait, result)?;
                self.audio.waits.swap_remove(i);
                woke = true;
            } else {
                i += 1;
            }
        }
        Ok(woke)
    }
    pub(super) fn dispatch_audio(
        &mut self,
        ticket: HostTicket,
        symbol: &str,
    ) -> Result<(), AdapterError> {
        let args = self.vm.host_arguments(ticket)?.to_vec();
        if symbol == "reel::audio::play" {
            let source = text::read_string(self.vm.heap(), args[0], self.read_limit)?;
            let looping = args[1]
                .as_bool()
                .ok_or(AdapterError::Input("looping must be bool"))?;
            let volume = volume(args[2])?;
            let owner = if args[3].tag() == ValueTag::Null {
                self.default_activity_owner(ticket.task)?
            } else {
                args[3]
            };
            let owner = if owner.tag() == ValueTag::Null {
                None
            } else {
                Some(self.stage.id(self.vm.heap(), owner)?)
            };
            self.audio
                .live
                .try_reserve(1)
                .map_err(|_| AdapterError::Allocation)?;
            let native = self
                .vm
                .program()
                .package()
                .natives
                .iter()
                .position(|n| {
                    self.vm.program().package().symbols[n.symbol.index()] == "reel::audio::await"
                })
                .ok_or(AdapterError::Input("audio await binding missing"))?;
            let heap = self.vm.heap_mut();
            let fields = heap.alloc_table()?;
            let value = heap.alloc_host(
                fields,
                &[Value::int(TAG), Value::NULL, Value::NULL, Value::int(0)],
                Some(readonly),
            )?;
            let root = heap.add_root(self.root_owner, RootKind::Host, value)?;
            if let Err(error) = self
                .vm
                .bind_awaitable(value, aside_vm::bytecode::NativeId(native as u32))
            {
                self.vm.heap_mut().release_root(root)?;
                return Err(error.into());
            }
            let admitted = self
                .audio
                .backend
                .as_mut()
                .ok_or_else(|| "audio output is not installed".to_string())
                .and_then(|backend| {
                    backend.play(AudioRequest {
                        source,
                        looping,
                        volume,
                    })
                });
            match admitted {
                Ok(id) => {
                    self.vm.heap_mut().set_host_payload(
                        value,
                        Slot::Id as u32,
                        Value::int(id.0 as i64),
                    )?;
                    self.audio.live.push(Live {
                        id,
                        root,
                        owner,
                        result: None,
                    });
                    self.vm.respond(ticket, HostResponse::Ready(value))?;
                }
                Err(error) => {
                    self.vm.heap_mut().release_root(root)?;
                    self.audio_error(ticket, &error)?;
                }
            }
        } else {
            let value = args[0];
            if !is_audio(self.vm.heap(), value) {
                return Err(AdapterError::Input("expected audio activity"));
            }
            if symbol == "reel::audio::await" {
                if let Some(result) = response(self.vm.heap(), value)? {
                    match result {
                        Completion::Ready(value) => {
                            self.vm.respond(ticket, HostResponse::Ready(value))?
                        }
                        Completion::Error(error) => {
                            self.vm.respond(ticket, HostResponse::Error(error))?
                        }
                        _ => unreachable!(),
                    }
                } else {
                    self.audio
                        .waits
                        .try_reserve(1)
                        .map_err(|_| AdapterError::Allocation)?;
                    let wait = ticket.wait_id();
                    self.vm.respond(ticket, HostResponse::Pending(wait))?;
                    self.audio.waits.push((wait, value));
                }
            } else {
                let command = match symbol {
                    "reel::stop" => AudioCommand::Stop,
                    "reel::pause" => AudioCommand::Pause,
                    "reel::resume" => AudioCommand::Resume,
                    "reel::audio::volume" => AudioCommand::Volume(volume(args[1])?),
                    _ => {
                        return Err(AdapterError::Input(
                            "audio has no forced endpoint; use stop",
                        ));
                    }
                };
                if response(self.vm.heap(), value)?.is_none() {
                    let id = AudioId(
                        self.vm
                            .heap()
                            .host_payload(value, Slot::Id as u32)?
                            .as_int()
                            .unwrap() as u64,
                    );
                    if let Err(error) = self.audio.backend.as_mut().unwrap().command(id, command) {
                        self.audio_error(ticket, &error)?;
                        self.issued = None;
                        return Ok(());
                    }
                }
                self.vm.respond(ticket, HostResponse::Ready(value))?;
            }
        }
        self.issued = None;
        Ok(())
    }
    fn audio_error(&mut self, ticket: HostTicket, error: &str) -> Result<(), AdapterError> {
        let value = self.vm.heap_mut().alloc_string(error)?;
        self.vm.respond(
            ticket,
            HostResponse::Error(aside_vm::NewFault {
                value,
                code: aside_vm::FaultCode::core(super::FaultKind::InvalidState),
                cause: None,
            }),
        )?;
        Ok(())
    }
}
impl Audio {
    pub fn active(&self) -> bool {
        !self.live.is_empty()
    }
    pub fn cleanup_pending(&self) -> bool {
        self.backend.as_ref().is_some_and(|b| b.cleanup_pending())
    }
    pub fn close(&mut self) {
        self.waits.clear();
        self.live.clear(); // Session owner scan releases the retained heap roots.
        if let Some(backend) = &mut self.backend {
            backend.close();
        }
    }
}
