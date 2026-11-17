//! Existing portable transport descriptions carried by the shared typed graph.
#[cfg(test)]
mod tests;
use super::*;
use crate::aside::{
    NativeBudget, NativeDispatch,
    data::{LogicalReference, TypedDataError},
    stage::StageBinding,
};
use aside_vm::{HeapHandle, HeapReservation, RootOwner, bytecode::NativeId, executor::Vm};
use std::{borrow::Cow, cell::RefCell, collections::BTreeMap};

struct Sample {
    playback: Option<MediaPlayback>,
    completion: Option<MediaCompletion>,
    payload: RefCell<Option<Vec<u8>>>,
    position: f64,
    work: u32,
}
/// One fixed transport sample. Encoding is admitted before graph traversal.
pub(in crate::aside) struct MediaCapture {
    samples: BTreeMap<u64, Sample>,
    after: Option<u64>,
    minimum: u32,
    complete: bool,
    charge: HeapReservation,
}
fn playback_bytes(playback: &MediaPlayback) -> usize {
    match playback {
        MediaPlayback::Audio(audio) => {
            size_of::<MediaPlayback>()
                + audio.request.source.capacity()
                + audio.queue.capacity() * size_of::<crate::audio::AudioQueueItem>()
                + audio
                    .queue
                    .iter()
                    .map(|item| item.source.capacity())
                    .sum::<usize>()
                + match &audio.result {
                    Some(MediaResult::Failed(message)) => message.capacity(),
                    _ => 0,
                }
        }
        MediaPlayback::Video(video) => {
            size_of::<MediaPlayback>()
                + video.request.source.capacity()
                + match &video.result {
                    Some(MediaResult::Failed(message)) => message.capacity(),
                    _ => 0,
                }
        }
    }
}
fn transport_position(playback: &MediaPlayback) -> Duration {
    match playback {
        MediaPlayback::Audio(audio) => audio.request.start_position,
        MediaPlayback::Video(video) => video.position,
    }
}
impl Media {
    /// Sample once while business values are frozen; backend output continues normally.
    pub(in crate::aside) fn capture_logical(
        &self,
        heap: &Heap,
    ) -> Result<MediaCapture, AdapterError> {
        let mut samples = BTreeMap::new();
        let mut charge = heap
            .budget_usage()
            .reserve(size_of::<MediaCapture>() as u64)?;
        for live in &self.live {
            let value = heap.root_value(live.root)?;
            let handle = heap.handle(value)?.key;
            let (playback, completion, position, bytes) = if let Some(completion) = &live.result {
                let bytes = size_of::<MediaCompletion>()
                    + match &completion.result {
                        MediaResult::Failed(message) => message.capacity(),
                        _ => 0,
                    };
                if live.has_frame
                    && let Some(request) = &live.video
                {
                    let playback = MediaPlayback::Video(crate::video::VideoPlaybackState {
                        request: request.clone(),
                        position: completion.position,
                        paused: true,
                        result: Some(completion.result.clone()),
                    });
                    let bytes = playback_bytes(&playback);
                    (Some(playback), None, completion.position, bytes)
                } else {
                    (None, Some(completion.clone()), completion.position, bytes)
                }
            } else {
                let playback = self
                    .backend
                    .as_ref()
                    .ok_or(AdapterError::Input("live media backend missing"))?
                    .snapshot(live.id)
                    .map_err(AdapterError::Asset)?;
                let position = transport_position(&playback);
                let bytes = playback_bytes(&playback);
                (Some(playback), None, position, bytes)
            };
            charge.resize(charge.bytes() + (bytes + size_of::<Sample>() + 128) as u64)?;
            // Snapshot cloning, byte encoding and sequence bookkeeping share this admission.
            let work = u32::try_from(bytes.saturating_mul(2).saturating_add(1))
                .map_err(|_| AdapterError::Allocation)?;
            samples.insert(
                handle,
                Sample {
                    playback,
                    completion,
                    payload: RefCell::new(None),
                    position: position.as_secs_f64(),
                    work,
                },
            );
        }
        Ok(MediaCapture {
            samples,
            after: None,
            minimum: 1,
            complete: false,
            charge,
        })
    }
}
impl MediaCapture {
    pub(in crate::aside) fn retained_bytes(&self) -> u64 {
        self.charge.bytes()
    }
    pub(in crate::aside) fn required_work(&self) -> u32 {
        self.minimum
    }
    /// Encode one already owned description after its atomic work bound is admitted.
    pub(in crate::aside) fn prepare(
        &mut self,
        budget: NativeBudget,
    ) -> Result<NativeDispatch, AdapterError> {
        use std::ops::Bound::{Excluded, Unbounded};
        if self.complete {
            return Ok(NativeDispatch::Ready { work: 0 });
        }
        let next = match self.after {
            Some(after) => self.samples.range((Excluded(after), Unbounded)).next(),
            None => self.samples.first_key_value(),
        };
        let Some((&key, sample)) = next else {
            self.complete = true;
            return Ok(NativeDispatch::Ready { work: 0 });
        };
        self.minimum = sample.work;
        if budget.with_minimum(sample.work) < sample.work {
            return Ok(NativeDispatch::BudgetYield {
                work: 0,
                required_work: sample.work,
            });
        }
        let bytes = if let Some(playback) = &sample.playback {
            postcard::to_stdvec(playback)
        } else {
            postcard::to_stdvec(sample.completion.as_ref().unwrap())
        }
        .map_err(|_| AdapterError::Input("portable media description encoding failed"))?;
        self.charge
            .resize(self.charge.bytes() + bytes.capacity() as u64)?;
        *sample.payload.borrow_mut() = Some(bytes);
        self.after = Some(key);
        self.minimum = 1;
        Ok(NativeDispatch::Decoding { work: sample.work })
    }
    pub(in crate::aside) fn resource_header(
        &self,
        heap: &Heap,
        value: Value,
    ) -> Option<Result<LogicalReference, TypedDataError>> {
        if is_cue(heap, value) {
            return Some(Ok(LogicalReference {
                kind: "reel.media.cue".into(),
                version: 1,
                fields: 7,
                payload: Vec::new(),
            }));
        }
        if !is_media(heap, value) {
            return None;
        }
        Some((|| {
            let sample = self.samples.get(&heap.handle(value)?.key);
            let (kind, payload) = match sample {
                Some(sample) => (
                    if sample.playback.is_some() {
                        "reel.media.playback"
                    } else {
                        "reel.media.terminal"
                    },
                    sample
                        .payload
                        .borrow_mut()
                        .take()
                        .ok_or(TypedDataError::Contract(
                            "prepare media descriptions before graph capture",
                        ))?,
                ),
                None if heap.host_payload(value, Slot::Result as u32)?.tag() != ValueTag::Null => {
                    ("reel.media.terminal", Vec::new())
                }
                None => {
                    return Err(TypedDataError::Contract(
                        "live media transport is missing from its capture",
                    ));
                }
            };
            Ok(LogicalReference {
                kind: kind.into(),
                version: 1,
                fields: 6,
                payload,
            })
        })())
    }
    pub(in crate::aside) fn resource_field(
        &self,
        heap: &Heap,
        stage: &StageBinding,
        value: Value,
        index: u32,
        default_owner: Value,
    ) -> Result<(Cow<'static, str>, Value), TypedDataError> {
        let sample = self.samples.get(&heap.handle(value)?.key);
        let (owner, root_scope) = stage::resources::logical_owner(
            heap,
            stage,
            heap.host_payload(value, Slot::Owner as u32)?,
            default_owner,
        )?;
        Ok(match index {
            0 => (
                "result".into(),
                heap.host_payload(value, Slot::Result as u32)?,
            ),
            1 => (
                "error".into(),
                heap.host_payload(value, Slot::Error as u32)?,
            ),
            2 => (
                "position".into(),
                if is_cue(heap, value) {
                    heap.host_payload(value, Slot::Position as u32)?
                } else if let Some(sample) = sample {
                    Value::float(sample.position)
                } else {
                    position(heap, value)?
                },
            ),
            3 => (
                "target".into(),
                heap.host_payload(value, Slot::Target as u32)?,
            ),
            4 => ("owner".into(), owner),
            5 => ("root_scope".into(), Value::bool(root_scope)),
            6 if is_cue(heap, value) => {
                ("source".into(), heap.host_payload(value, Slot::Id as u32)?)
            }
            _ => return Err(TypedDataError::Contract("unknown media resource field")),
        })
    }
}

struct Pending {
    handle: HeapHandle,
    playback: Option<MediaPlayback>,
    completion: Option<MediaCompletion>,
    cue: bool,
    root_scope: bool,
    queue: usize,
    id: Option<MediaId>,
    prepared: bool,
    paused: bool,
}
#[derive(Default)]
pub(super) struct Restoring {
    entries: BTreeMap<u64, Pending>,
    prepare_after: Option<u64>,
    adopt_after: Option<u64>,
    await_scan: usize,
    await_native: Option<NativeId>,
    charge: Option<HeapReservation>,
}
impl Media {
    pub(in crate::aside) fn resource_allocate(
        &mut self,
        heap: &mut Heap,
        header: &LogicalReference,
    ) -> Result<Value, TypedDataError> {
        let fields = if header.kind == "reel.media.cue" {
            7
        } else {
            6
        };
        if header.fields != fields {
            return Err(TypedDataError::Contract(
                "media fields differ from their protocol",
            ));
        }
        let (mut playback, mut completion, cue) = match header.kind.as_str() {
            "reel.media.playback" => (
                Some(
                    postcard::from_bytes::<MediaPlayback>(&header.payload).map_err(|_| {
                        TypedDataError::Contract("invalid media playback description")
                    })?,
                ),
                None,
                false,
            ),
            "reel.media.terminal" => (
                None,
                if header.payload.is_empty() {
                    None
                } else {
                    Some(postcard::from_bytes(&header.payload).map_err(|_| {
                        TypedDataError::Contract("invalid media completion description")
                    })?)
                },
                false,
            ),
            "reel.media.cue" if header.payload.is_empty() => (None, None, true),
            _ => return Err(TypedDataError::Contract("unknown media protocol")),
        };
        if let Some(MediaPlayback::Audio(audio)) = &playback
            && let Some(result) = &audio.result
        {
            completion = Some(MediaCompletion {
                result: result.clone(),
                position: audio.request.start_position,
            });
            playback = None;
        }
        let fields = heap.alloc_table()?;
        let position = if cue {
            Value::float(0.)
        } else {
            self.keys
                .record(heap, &[("position", Value::float(0.))], &[])?
        };
        let value = heap.alloc_host(
            fields,
            &[
                Value::int(if cue { CUE_TAG } else { TAG }),
                Value::NULL,
                Value::NULL,
                Value::int(0),
                position,
                Value::NULL,
                Value::NULL,
            ],
            Some(&READONLY),
        )?;
        let kind = heap.host_type(if cue {
            "reel::types::MediaCue"
        } else {
            "reel::types::Playback"
        })?;
        heap.set_host_type(value, kind)?;
        if self.logical.charge.is_none() {
            self.logical.charge = Some(heap.budget_usage().reserve(0)?);
        }
        let charge = self.logical.charge.as_mut().unwrap();
        charge.resize(
            charge.bytes() + (size_of::<Pending>() + header.payload.len() * 2 + 256) as u64,
        )?;
        let handle = heap.handle(value)?;
        let paused = match &playback {
            Some(MediaPlayback::Audio(audio)) => audio.paused,
            Some(MediaPlayback::Video(video)) => video.paused || video.result.is_some(),
            None => true,
        };
        self.logical.entries.insert(
            handle.key,
            Pending {
                handle,
                playback,
                completion,
                cue,
                root_scope: false,
                queue: 0,
                id: None,
                prepared: false,
                paused,
            },
        );
        Ok(value)
    }
    pub(in crate::aside) fn resource_connect(
        &mut self,
        heap: &mut Heap,
        stage: &StageBinding,
        value: Value,
        name: &str,
        field: Value,
    ) -> Result<(), TypedDataError> {
        let pending = self
            .logical
            .entries
            .get_mut(&heap.handle(value)?.key)
            .ok_or(TypedDataError::Contract("media shell is missing"))?;
        match name {
            "position" => {
                if pending.cue {
                    duration(field)?;
                    heap.set_host_payload(value, Slot::Position as u32, field)?;
                } else {
                    publish_position(heap, value, duration(field)?)?;
                }
            }
            "root_scope" => {
                pending.root_scope = field
                    .as_bool()
                    .ok_or(TypedDataError::Contract("invalid media owner marker"))?
            }
            name => {
                let slot = match name {
                    "result" => Slot::Result,
                    "error" => {
                        field.as_int().ok_or(HeapError::TypeMismatch)?;
                        Slot::Error
                    }
                    "target" => {
                        if field.tag() != ValueTag::Null {
                            stage.video_id(heap, field)?;
                        }
                        Slot::Target
                    }
                    "owner" => {
                        if field.tag() != ValueTag::Null {
                            stage.id(heap, field)?;
                        }
                        Slot::Owner
                    }
                    "source" if pending.cue => Slot::Id,
                    _ => return Err(TypedDataError::Contract("unknown media resource edge")),
                };
                heap.set_host_payload(value, slot as u32, field)?;
            }
        }
        Ok(())
    }
    pub(in crate::aside) fn resource_finish(
        &mut self,
        heap: &Heap,
        value: Value,
    ) -> Result<bool, TypedDataError> {
        let pending = self
            .logical
            .entries
            .get(&heap.handle(value)?.key)
            .ok_or(TypedDataError::Contract("media shell is missing"))?;
        if pending.root_scope
            && heap.host_payload(value, Slot::Owner as u32)?.tag() != ValueTag::Null
        {
            return Err(TypedDataError::Contract(
                "media owner has conflicting Node and Root scope identities",
            ));
        }
        if pending.cue
            && heap.host_payload(value, Slot::Result as u32)?.tag() == ValueTag::Null
            && !is_media(heap, heap.host_payload(value, Slot::Id as u32)?)
        {
            return Err(TypedDataError::Contract(
                "media cue source is not a playback",
            ));
        }
        Ok(true)
    }
    /// Bind the current await adapter, then create paused candidate transports.
    pub(in crate::aside) fn prepare_restored_step(
        &mut self,
        vm: &mut Vm,
        stage: &StageBinding,
        owner: RootOwner,
        work: u32,
    ) -> Result<bool, AdapterError> {
        use std::ops::Bound::{Excluded, Unbounded};
        for _ in 0..work {
            let next = match self.logical.prepare_after {
                Some(after) => self
                    .logical
                    .entries
                    .range((Excluded(after), Unbounded))
                    .next(),
                None => self.logical.entries.first_key_value(),
            };
            let Some((&key, _)) = next else {
                return Ok(true);
            };
            if self.logical.await_native.is_none() {
                let p = vm.program().package();
                let index = self.logical.await_scan;
                let native = p
                    .natives
                    .get(index)
                    .ok_or(AdapterError::Input("media await binding missing"))?;
                self.logical.await_scan += 1;
                if p.symbols[native.symbol.index()] == "reel::audio::await" {
                    self.logical.await_native = Some(NativeId(index as u32));
                }
                continue;
            }
            let pending = self.logical.entries.get_mut(&key).unwrap();
            let value = vm.heap().import_handle(pending.handle)?;
            if !pending.prepared {
                vm.bind_awaitable(value, self.logical.await_native.unwrap())?;
                pending.prepared = true;
                continue;
            }
            if pending.cue {
                if vm.heap().host_payload(value, Slot::Result as u32)?.tag() == ValueTag::Null {
                    super::super::results::reserve_queue(
                        vm.heap(),
                        &mut self.cues,
                        &mut self.cue_charge,
                    )?;
                    self.cues
                        .push_back(vm.heap_mut().add_root(owner, RootKind::Host, value)?);
                    self.cue_remaining = self.cues.len();
                }
                self.logical.prepare_after = Some(key);
                continue;
            }
            if let Some(completion) = &pending.completion {
                complete_value(vm.heap_mut(), value, completion)?;
                self.logical.prepare_after = Some(key);
                continue;
            }
            let Some(playback) = &pending.playback else {
                self.logical.prepare_after = Some(key);
                continue;
            };
            if pending.id.is_none() {
                self.live
                    .try_reserve(1)
                    .map_err(|_| AdapterError::Allocation)?;
                let backend = self
                    .backend
                    .as_mut()
                    .ok_or(AdapterError::Input("restoring media requires a backend"))?;
                let id = match playback {
                    MediaPlayback::Audio(audio) => {
                        let mut request = audio.request.clone();
                        request.fade_in = Duration::ZERO;
                        backend.play_audio(request, true)
                    }
                    MediaPlayback::Video(video) => {
                        let mut frozen = video.clone();
                        frozen.paused = true;
                        backend.restore_video(frozen)
                    }
                }
                .map_err(AdapterError::Asset)?;
                pending.id = Some(id);
                vm.heap_mut()
                    .set_host_payload(value, Slot::Id as u32, Value::int(id.0 as i64))?;
                let target_value = vm.heap().host_payload(value, Slot::Target as u32)?;
                let target = if target_value.tag() == ValueTag::Null {
                    None
                } else {
                    Some(stage.video_id(vm.heap(), target_value)?)
                };
                let video = match playback {
                    MediaPlayback::Video(video) => Some(video.request.clone()),
                    _ => None,
                };
                let root = vm.heap_mut().add_root(owner, RootKind::Host, value)?;
                self.live.push(Live {
                    id,
                    root,
                    target,
                    result: None,
                    video,
                    has_frame: false,
                    pending_start: None,
                    latest_position: transport_position(playback),
                });
                continue;
            }
            if let MediaPlayback::Audio(audio) = playback
                && let Some(item) = audio.queue.get(pending.queue)
            {
                self.backend
                    .as_mut()
                    .unwrap()
                    .queue_audio(pending.id.unwrap(), item.clone())
                    .map_err(AdapterError::Asset)?;
                pending.queue += 1;
                continue;
            }
            self.logical.prepare_after = Some(key);
        }
        Ok(false)
    }
    /// Attach current Root owners and release the restored transport pause on adoption.
    pub(in crate::aside) fn adopt_restored_step(
        &mut self,
        heap: &mut Heap,
        current_default: Value,
        work: u32,
    ) -> Result<bool, AdapterError> {
        use std::ops::Bound::{Excluded, Unbounded};
        for _ in 0..work {
            let next = match self.logical.adopt_after {
                Some(after) => self
                    .logical
                    .entries
                    .range((Excluded(after), Unbounded))
                    .next(),
                None => self.logical.entries.first_key_value(),
            };
            let Some((&key, pending)) = next else {
                self.logical = Restoring::default();
                return Ok(true);
            };
            let value = heap.import_handle(pending.handle)?;
            if pending.root_scope {
                heap.set_host_payload(value, Slot::Owner as u32, current_default)?;
            }
            if let Some(id) = pending.id {
                let backend = self.backend.as_mut().unwrap();
                if matches!(pending.playback, Some(MediaPlayback::Audio(_))) {
                    backend.commit_audio(id);
                }
                if !pending.paused {
                    backend
                        .command(id, MediaCommand::Resume)
                        .map_err(AdapterError::Asset)?;
                }
            }
            self.logical.adopt_after = Some(key);
        }
        Ok(false)
    }
}
fn complete_value(
    heap: &mut Heap,
    value: Value,
    completion: &MediaCompletion,
) -> Result<(), AdapterError> {
    let (message, code) = match &completion.result {
        MediaResult::Completed => ("Completed", 0),
        MediaResult::Stopped => ("Stopped", 0),
        MediaResult::Cancelled => ("media owner retired", 2),
        MediaResult::Failed(message) => (message.as_str(), 1),
    };
    let message = heap.alloc_string(message)?;
    heap.set_host_payload(value, Slot::Result as u32, message)?;
    heap.set_host_payload(value, Slot::Error as u32, Value::int(code))?;
    publish_position(heap, value, completion.position)?;
    Ok(())
}
