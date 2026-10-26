use super::{AdapterError, HostTicket, SessionVm, observable, stage, text};
use crate::audio::{AudioGroup, AudioLoopRegion, AudioRequest};
use crate::media::{
    MediaBackend, MediaCommand, MediaCompletion, MediaId, MediaPlayback, MediaResult,
};
use aside_vm::executor::{Completion, HostResponse, TaskId, WaitId};
use aside_vm::{Heap, HeapError, RootKind, RootToken, Value, ValueTag};
use reel_ui::stage::NodeId;
use std::{collections::VecDeque, time::Duration};

const TAG: i64 = -1003;
const CUE_TAG: i64 = -1009;
// Cues share the result/error slots with their producer. Id retains the source,
// and Position stores the requested threshold until settlement.
#[repr(u32)]
enum Slot {
    Tag,
    Id,
    Result,
    Error,
    Position,
    Target,
    Owner,
}
struct Live {
    id: MediaId,
    root: RootToken,
    target: Option<NodeId>,
    result: Option<MediaCompletion>,
    video: Option<crate::video::VideoRequest>,
    has_frame: bool,
    pending_start: Option<TaskId>,
    latest_position: Duration,
}
#[derive(Default)]
pub(super) struct Media {
    backend: Option<Box<dyn MediaBackend>>,
    live: Vec<Live>,
    waits: VecDeque<(WaitId, Value)>,
    wait_charge: Option<aside_vm::HeapReservation>,
    cursor: usize,
    last_time: f64,
    posters: Vec<Poster>,
    poster_cursor: usize,
    position_cursor: usize,
    cues: VecDeque<RootToken>,
    cue_charge: Option<aside_vm::HeapReservation>,
    cue_remaining: usize,
    pending_terminals: usize,
    keys: observable::SchemaKeys,
}

struct Poster {
    saved: SavedPoster,
    pending: Option<MediaId>,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct SavedPoster {
    target: NodeId,
    playback: crate::video::VideoPlaybackState,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedMedia {
    root: RootToken,
    pending_start: Option<TaskId>,
    target: Option<NodeId>,
    playback: Option<MediaPlayback>,
    result: Option<MediaCompletion>,
}
impl Media {
    pub(super) fn finish_batch(&mut self, task: TaskId, commit: bool) {
        for live in &mut self.live {
            if live.pending_start != Some(task) {
                continue;
            }
            live.pending_start = None;
            if live.result.is_some() {
                continue;
            }
            let backend = self.backend.as_mut().expect("live media backend");
            if commit {
                backend.commit_audio(live.id);
            } else {
                let position = backend.position(live.id).expect("live media transport");
                backend.cancel(live.id);
                live.result = Some(MediaCompletion {
                    result: MediaResult::Cancelled,
                    position,
                });
                self.pending_terminals += 1;
            }
        }
    }
    pub(super) fn snapshot_records(&self) -> super::snapshot::RecordStream<'_> {
        use super::snapshot::{Part, Record};
        Box::new(
            self.live
                .iter()
                .map(|entry| {
                    let playback = if entry.result.is_some() {
                        None
                    } else {
                        Some(
                            self.backend
                                .as_ref()
                                .ok_or(AdapterError::Input("media backend missing"))?
                                .snapshot(entry.id)
                                .map_err(aside_vm::snapshot::SnapshotError::Invalid)?,
                        )
                    };
                    Record::new(
                        Part::Media,
                        &SavedMedia {
                            root: entry.root,
                            pending_start: entry.pending_start,
                            target: entry.target,
                            playback,
                            result: entry.result.clone(),
                        },
                    )
                })
                .chain(
                    self.cues
                        .iter()
                        .map(|root| Record::new(Part::MediaCue, root)),
                )
                .chain(self.live.iter().filter_map(|live| {
                    let completion = live.result.as_ref()?;
                    let target = live.target?;
                    let request = live.video.as_ref()?;
                    (live.has_frame
                        && matches!(
                            completion.result,
                            MediaResult::Completed | MediaResult::Stopped
                        ))
                    .then(|| {
                        Record::new(
                            Part::VideoPoster,
                            &SavedPoster {
                                target,
                                playback: crate::video::VideoPlaybackState {
                                    request: request.clone(),
                                    position: completion.position,
                                    paused: true,
                                    result: Some(completion.result.clone()),
                                },
                            },
                        )
                    })
                }))
                .chain(
                    self.posters
                        .iter()
                        .filter(|p| !self.live.iter().any(|l| l.target == Some(p.saved.target)))
                        .map(|p| Record::new(Part::VideoPoster, &p.saved)),
                )
                .chain(
                    self.waits
                        .iter()
                        .map(|wait| Record::new(Part::MediaWait, wait)),
                ),
        )
    }
    pub(super) fn restore_record(
        &mut self,
        record: &super::snapshot::Record,
        map: &aside_vm::snapshot::SnapshotMap,
        heap: &mut Heap,
    ) -> Result<(), AdapterError> {
        if matches!(record.part, super::snapshot::Part::MediaCue) {
            super::results::reserve_queue(heap, &mut self.cues, &mut self.cue_charge)?;
            self.cues.push_back(map.root(record.read()?)?);
            self.cue_remaining = self.cues.len();
            return Ok(());
        }
        if matches!(record.part, super::snapshot::Part::MediaWait) {
            let (wait, value): (WaitId, Value) = record.read()?;
            super::results::reserve_queue(heap, &mut self.waits, &mut self.wait_charge)?;
            self.waits.push_back((map.wait(wait)?, map.value(value)?));
            return Ok(());
        }
        if matches!(record.part, super::snapshot::Part::VideoPoster) {
            let saved: SavedPoster = record.read()?;
            let id = self
                .backend
                .as_mut()
                .ok_or(AdapterError::Input("restoring video requires a backend"))?
                .restore_video(saved.playback.clone())
                .map_err(aside_vm::snapshot::SnapshotError::Invalid)?;
            self.posters.push(Poster {
                saved,
                pending: Some(id),
            });
            return Ok(());
        }
        let mut saved: SavedMedia = record.read()?;
        let root = map.root(saved.root)?;
        let pending_start = saved.pending_start.map(|task| map.task(task)).transpose()?;
        let mut id = MediaId(0);
        let mut video = None;
        if let Some(playback) = saved.playback {
            match playback {
                MediaPlayback::Audio(mut playback) => {
                    saved.result = playback.result.map(|result| MediaCompletion {
                        result,
                        position: playback.request.start_position,
                    });
                    if saved.result.is_none() {
                        playback.request.fade_in = std::time::Duration::ZERO;
                        let backend = self
                            .backend
                            .as_mut()
                            .ok_or(AdapterError::Input("restoring media requires a backend"))?;
                        id = backend
                            .play_audio(playback.request, playback.paused)
                            .map_err(aside_vm::snapshot::SnapshotError::Invalid)?;
                        for item in playback.queue {
                            backend
                                .queue_audio(id, item)
                                .map_err(aside_vm::snapshot::SnapshotError::Invalid)?;
                        }
                        if pending_start.is_none() {
                            backend.commit_audio(id);
                        }
                    }
                }
                MediaPlayback::Video(playback) => {
                    video = Some(playback.request.clone());
                    id = self
                        .backend
                        .as_mut()
                        .ok_or(AdapterError::Input("restoring media requires a backend"))?
                        .restore_video(playback)
                        .map_err(aside_vm::snapshot::SnapshotError::Invalid)?;
                }
            }
        }
        heap.set_host_payload(
            heap.root_value(root)?,
            Slot::Id as u32,
            Value::int(id.0 as i64),
        )?;
        let latest_position = match &saved.result {
            Some(completion) => completion.position,
            None => self
                .backend
                .as_ref()
                .unwrap()
                .position(id)
                .ok_or(AdapterError::Input("restored media transport missing"))?,
        };
        self.pending_terminals += usize::from(saved.result.is_some());
        self.live.push(Live {
            id,
            root,
            target: saved.target,
            result: saved.result,
            video,
            has_frame: false,
            pending_start,
            latest_position,
        });
        Ok(())
    }
}
impl Drop for Media {
    fn drop(&mut self) {
        self.close();
    }
}
pub(super) fn is_media(heap: &Heap, value: Value) -> bool {
    heap.host_payload(value, Slot::Tag as u32)
        .ok()
        .and_then(Value::as_int)
        == Some(TAG)
}
fn is_cue(heap: &Heap, value: Value) -> bool {
    heap.host_payload(value, Slot::Tag as u32)
        .ok()
        .and_then(Value::as_int)
        == Some(CUE_TAG)
}
fn position(heap: &Heap, value: Value) -> Result<Value, AdapterError> {
    let record = heap.host_payload(value, Slot::Position as u32)?;
    Ok(heap.host_property(aside_vm::PropertyId {
        host: record,
        field: 0,
    })?)
}
fn publish_position(heap: &mut Heap, value: Value, position: Duration) -> Result<(), AdapterError> {
    let record = heap.host_payload(value, Slot::Position as u32)?;
    observable::publish(heap, record, 0, Value::float(position.as_secs_f64()))
}
fn settle_cue(heap: &mut Heap, cue: Value) -> Result<bool, AdapterError> {
    if response(heap, cue)?.is_some() {
        return Ok(true);
    }
    let owner = heap.host_payload(cue, Slot::Owner as u32)?;
    if !super::scopes::owner_live(heap, owner) {
        let message = heap.alloc_string("media position owner retired")?;
        heap.set_host_payload(cue, Slot::Result as u32, message)?;
        heap.set_host_payload(cue, Slot::Error as u32, Value::int(2))?;
        heap.set_host_payload(cue, Slot::Id as u32, Value::NULL)?;
        heap.set_host_payload(cue, Slot::Owner as u32, Value::NULL)?;
        return Ok(true);
    }
    let source = heap.host_payload(cue, Slot::Id as u32)?;
    let threshold = heap
        .host_payload(cue, Slot::Position as u32)?
        .as_float()
        .unwrap();
    let terminal = heap.host_payload(source, Slot::Result as u32)?;
    let error = heap.host_payload(source, Slot::Error as u32)?;
    // Error/cancellation retain their category even when the final cursor has
    // reached the threshold. Normal endings first inspect their final position.
    let result = if error.as_int() != Some(0) {
        terminal
    } else if position(heap, source)?.as_float().unwrap() >= threshold {
        Value::bool(true)
    } else if terminal.tag() != ValueTag::Null {
        Value::bool(false)
    } else {
        return Ok(false);
    };
    heap.set_host_payload(cue, Slot::Result as u32, result)?;
    heap.set_host_payload(cue, Slot::Error as u32, error)?;
    heap.set_host_payload(cue, Slot::Id as u32, Value::NULL)?;
    heap.set_host_payload(cue, Slot::Owner as u32, Value::NULL)?;
    Ok(true)
}
pub(super) fn readonly(
    _: &aside_vm::PropertyRead<'_>,
    _: Value,
    _: u32,
    _: Value,
) -> Result<(), HeapError> {
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
fn duration(value: Value) -> Result<std::time::Duration, AdapterError> {
    std::time::Duration::try_from_secs_f64(stage::number(value)?)
        .map_err(|_| AdapterError::Input("audio duration must be finite and nonnegative"))
}
fn pan(value: Value) -> Result<f64, AdapterError> {
    let value = stage::number(value)?;
    if !(-1.0..=1.0).contains(&value) {
        return Err(AdapterError::Input("audio pan must be between -1 and 1"));
    }
    Ok(value)
}

fn rate(value: Value) -> Result<f64, AdapterError> {
    let value = super::stage::number(value)?;
    if !crate::audio::AudioRate::constant(value).is_valid() {
        return Err(AdapterError::Input(
            "audio rate must be finite and between 0.25 and 4",
        ));
    }
    Ok(value)
}
fn group(name: &str) -> Result<AudioGroup, AdapterError> {
    match name {
        "music" => Ok(AudioGroup::Music),
        "voice" => Ok(AudioGroup::Voice),
        "effects" => Ok(AudioGroup::Effects),
        "ui" => Ok(AudioGroup::Ui),
        _ => Err(AdapterError::Input(
            "audio group must be music, voice, effects or ui",
        )),
    }
}
impl SessionVm {
    /// Install once before executing author code. No platform dependency enters the VM.
    pub fn set_media(&mut self, backend: Box<dyn MediaBackend>) -> Result<(), AdapterError> {
        if self.media.backend.is_some() || self.revoked {
            return Err(AdapterError::Input(
                "media backend already installed or session retired",
            ));
        }
        self.media.last_time = self.logical_time();
        self.media.backend = Some(backend);
        Ok(())
    }
    pub fn media_poll_after(&self) -> Option<std::time::Duration> {
        let backend = self.media.backend.as_ref().and_then(|b| b.poll_after());
        if self.media.cue_remaining != 0 || self.media.pending_terminals != 0 {
            Some(Duration::ZERO)
        } else {
            backend.or_else(|| (!self.media.waits.is_empty()).then_some(Duration::from_millis(10)))
        }
    }
    pub fn suspend_media(&mut self) {
        if let Some(backend) = &mut self.media.backend {
            backend.suspend();
        }
    }
    pub fn resume_media(&mut self) {
        self.media.last_time = self.logical_time();
        if let Some(backend) = &mut self.media.backend {
            backend.resume();
        }
    }
    pub fn maintain_media(&mut self) -> Result<bool, AdapterError> {
        let now = self.logical_time();
        let elapsed = std::time::Duration::from_secs_f64((now - self.media.last_time).max(0.));
        self.media.last_time = now;
        if let Some(backend) = &mut self.media.backend {
            backend.maintain(elapsed);
        }
        let mut changed = false;
        for _ in 0..self.media.live.len().min(64) {
            if self.media.cursor >= self.media.live.len() {
                self.media.cursor = 0;
            }
            let live = &mut self.media.live[self.media.cursor];
            if live.result.is_none() {
                let backend = self
                    .media
                    .backend
                    .as_mut()
                    .ok_or(AdapterError::Input("live media backend missing"))?;
                let object = self.vm.heap().root_value(live.root)?;
                let owner = self.vm.heap().host_payload(object, Slot::Owner as u32)?;
                if !super::scopes::owner_live(self.vm.heap(), owner)
                    || live
                        .target
                        .is_some_and(|id| self.stage.scene.node(id).is_none())
                {
                    let position = backend
                        .position(live.id)
                        .ok_or(AdapterError::Input("media transport missing"))?;
                    backend.cancel(live.id);
                    live.result = Some(MediaCompletion {
                        result: MediaResult::Cancelled,
                        position,
                    });
                } else {
                    live.latest_position = backend
                        .position(live.id)
                        .ok_or(AdapterError::Input("media transport missing"))?;
                    if let Some(target) = live.target
                        && let Some(frame) = backend.take_video_frame(live.id)
                    {
                        self.stage.set_video_frame(target, frame)?;
                        live.has_frame = true;
                        changed = true;
                    }
                    live.result = backend.take_result(live.id);
                }
                self.media.pending_terminals += usize::from(live.result.is_some());
            }
            if let Some(completion) = &live.result {
                live.latest_position = completion.position;
            }
            self.media.cursor += 1;
        }
        for _ in 0..self.media.posters.len().min(64) {
            if self.media.poster_cursor >= self.media.posters.len() {
                self.media.poster_cursor = 0;
            }
            let poster = &mut self.media.posters[self.media.poster_cursor];
            if self.stage.scene.node(poster.saved.target).is_none() {
                if let Some(id) = poster.pending {
                    self.media.backend.as_mut().unwrap().cancel(id);
                }
                self.media.posters.swap_remove(self.media.poster_cursor);
                continue;
            }
            if let Some(id) = poster.pending {
                let backend = self.media.backend.as_mut().unwrap();
                if let Some(frame) = backend.take_video_frame(id) {
                    self.stage.set_video_frame(poster.saved.target, frame)?;
                    changed = true;
                }
                if let Some(completion) = backend.take_result(id) {
                    poster.pending = None;
                    if let MediaResult::Failed(error) = completion.result {
                        return Err(aside_vm::snapshot::SnapshotError::Invalid(error).into());
                    }
                }
            }
            self.media.poster_cursor += 1;
        }
        Ok(changed | self.settle_media_cues()? | self.complete_media_waits()?)
    }
    fn complete_media_waits(&mut self) -> Result<bool, AdapterError> {
        let mut woke = false;
        for _ in 0..self.media.waits.len().min(64) {
            let (wait, value) = self.media.waits.pop_front().expect("pending media wait");
            if !self.vm.wait_is_pending(wait) {
                continue;
            } else if let Some(result) = response(self.vm.heap(), value)? {
                self.vm.complete(wait, result)?;
                woke = true;
            } else {
                self.media.waits.push_back((wait, value));
            }
        }
        Ok(woke)
    }
    /// Called with the activity clock at the shared reactive sampling boundary.
    pub(super) fn sample_media_positions(&mut self) -> Result<bool, AdapterError> {
        if self.media.cue_remaining != 0 {
            return Ok(false);
        }
        for _ in 0..self.media.live.len().min(64) {
            if self.media.position_cursor >= self.media.live.len() {
                self.media.position_cursor = 0;
            }
            let live = &mut self.media.live[self.media.position_cursor];
            let value = self.vm.heap().root_value(live.root)?;
            publish_position(self.vm.heap_mut(), value, live.latest_position)?;
            if let Some(completion) = &live.result {
                if live.has_frame
                    && matches!(
                        completion.result,
                        MediaResult::Completed | MediaResult::Stopped
                    )
                    && let (Some(target), Some(request)) = (live.target, live.video.as_ref())
                {
                    let saved = SavedPoster {
                        target,
                        playback: crate::video::VideoPlaybackState {
                            request: request.clone(),
                            position: completion.position,
                            paused: true,
                            result: Some(completion.result.clone()),
                        },
                    };
                    if let Some(poster) = self
                        .media
                        .posters
                        .iter_mut()
                        .find(|p| p.saved.target == target)
                    {
                        poster.saved = saved;
                    } else {
                        self.media.posters.push(Poster {
                            saved,
                            pending: None,
                        });
                    }
                }
                let (message, code) = match &completion.result {
                    MediaResult::Completed => ("Completed", 0),
                    MediaResult::Stopped => ("Stopped", 0),
                    MediaResult::Cancelled => ("media owner retired", 2),
                    MediaResult::Failed(message) => (message.as_str(), 1),
                };
                let heap = self.vm.heap_mut();
                let message = heap.alloc_string(message)?;
                let value = heap.root_value(live.root)?;
                heap.set_host_payload(value, Slot::Result as u32, message)?;
                heap.set_host_payload(value, Slot::Error as u32, Value::int(code))?;
                publish_position(heap, value, completion.position)?;
                // Terminal cursors cannot be overwritten by a later sample.
                heap.set_host_payload(value, Slot::Target as u32, Value::NULL)?;
                heap.set_host_payload(value, Slot::Owner as u32, Value::NULL)?;
                heap.release_root(live.root)?;
                self.media.live.swap_remove(self.media.position_cursor);
                self.media.pending_terminals -= 1;
            } else {
                self.media.position_cursor += 1;
            }
        }
        self.media.cue_remaining = self.media.cues.len();
        Ok(self.settle_media_cues()?
            | self.complete_media_waits()?
            | self.vm.heap().derived_pending())
    }
    fn settle_media_cues(&mut self) -> Result<bool, AdapterError> {
        let batch = self.media.cue_remaining != 0;
        let count = if batch {
            self.media.cue_remaining
        } else {
            self.media.cues.len()
        };
        let mut changed = false;
        for _ in 0..count.min(64) {
            let root = self.media.cues.pop_front().expect("pending media cue");
            let value = self.vm.heap().root_value(root)?;
            if settle_cue(self.vm.heap_mut(), value)? {
                self.vm.heap_mut().release_root(root)?;
                changed = true;
            } else {
                self.media.cues.push_back(root);
            }
            if batch {
                self.media.cue_remaining -= 1;
            }
        }
        Ok(changed)
    }
    fn settle_source_cues(&mut self, source: Value) -> Result<(), AdapterError> {
        for root in &self.media.cues {
            let value = self.vm.heap().root_value(*root)?;
            if self
                .vm
                .heap()
                .host_payload(value, Slot::Id as u32)?
                .same_bits(source)
            {
                settle_cue(self.vm.heap_mut(), value)?;
            }
        }
        Ok(())
    }
    pub(super) fn dispatch_media(
        &mut self,
        ticket: HostTicket,
        symbol: &str,
    ) -> Result<(), AdapterError> {
        if matches!(symbol, "reel::audio::position" | "reel::video::position") {
            let value = self.vm.host_arguments(ticket)?[0];
            if !is_media(self.vm.heap(), value) {
                return Err(AdapterError::Input("expected media activity"));
            }
            let record = self.vm.heap().host_payload(value, Slot::Position as u32)?;
            self.vm.heap_mut().observe_read(record, Some(0))?;
            let position = position(self.vm.heap(), value)?;
            self.vm.respond(ticket, HostResponse::Ready(position))?;
            self.issued = None;
            return Ok(());
        }
        let args = self.vm.host_arguments(ticket)?.to_vec();
        if symbol == "reel::audio::group_volume" {
            let name = text::read_string(self.vm.heap(), args[0], self.read_limit)?;
            let group = if name == "master" {
                None
            } else {
                Some(group(&name)?)
            };
            let value = volume(args[1])?;
            let duration = duration(args[2])?;
            let result = self
                .media
                .backend
                .as_mut()
                .ok_or_else(|| "media output is not installed".to_string())
                .and_then(|backend| backend.group_volume(group, value, duration));
            match result {
                Ok(()) => self.vm.respond(ticket, HostResponse::Ready(Value::NULL))?,
                Err(error) => self.media_error(ticket, &error)?,
            }
        } else if symbol == "reel::audio::play" {
            let source = text::read_string(self.vm.heap(), args[0], self.read_limit)?;
            let volume = volume(args[2])?;
            let owner = if args[3].tag() == ValueTag::Null {
                self.default_activity_owner(ticket.task)?
            } else {
                args[3]
            };
            super::scopes::validate_owner(self.vm.heap(), &self.stage, owner)?;
            let group = group(&text::read_string(
                self.vm.heap(),
                args[4],
                self.read_limit,
            )?)?;
            let pan = pan(args[5])?;
            let fade_in = duration(args[6])?;
            let start_position = duration(args[7])?;
            let end_position = audio_end(start_position, args[8])?;
            let loop_region = audio_loop(args[1], args[9], args[10])?;
            let paused = args[11]
                .as_bool()
                .ok_or(AdapterError::Input("paused must be bool"))?;
            let request = AudioRequest {
                source,
                loop_region,
                volume,
                rate: crate::audio::AudioRate::constant(rate(args[12])?),
                group,
                pan,
                fade_in,
                start_position,
                end_position,
            };
            self.start_media(
                ticket,
                MediaPlayback::Audio(crate::audio::AudioPlayback {
                    request,
                    queue: Vec::new(),
                    paused,
                    result: None,
                }),
                owner,
                None,
                Value::NULL,
            )?;
        } else if symbol == "reel::video::play" {
            let target = self.stage.video_id(self.vm.heap(), args[0])?;
            let fields = self.vm.heap().host_fields(args[0])?;
            let source = text::read_string(
                self.vm.heap(),
                self.vm.heap().table_entry(fields, 0)?.1,
                self.read_limit,
            )?;
            let looping = args[1]
                .as_bool()
                .ok_or(AdapterError::Input("looping must be bool"))?;
            let volume = volume(args[2])?;
            let start_position = duration(args[3])?;
            let end_position = if args[4].tag() == ValueTag::Null {
                None
            } else {
                Some(duration(args[4])?)
            };
            if end_position.is_some_and(|end| end <= start_position) {
                return Err(AdapterError::Input("video end must follow start"));
            }
            let owner = if args[5].tag() == ValueTag::Null {
                self.default_activity_owner(ticket.task)?
            } else {
                args[5]
            };
            super::scopes::validate_owner(self.vm.heap(), &self.stage, owner)?;
            let paused = args[6]
                .as_bool()
                .ok_or(AdapterError::Input("paused must be bool"))?;
            let request = crate::video::VideoRequest {
                source,
                looping,
                volume,
                start_position,
                end_position,
            };
            self.start_media(
                ticket,
                MediaPlayback::Video(crate::video::VideoPlaybackState {
                    position: start_position,
                    request,
                    paused,
                    result: None,
                }),
                owner,
                Some(target),
                args[0],
            )?;
        } else {
            let value = args[0];
            if !(is_media(self.vm.heap(), value)
                || symbol == "reel::audio::await" && is_cue(self.vm.heap(), value))
            {
                return Err(AdapterError::Input("expected media activity"));
            }
            if matches!(symbol, "reel::audio::at" | "reel::video::at") {
                self.start_media_cue(ticket, value, args[1])?;
            } else if symbol == "reel::audio::queue" {
                let source = text::read_string(self.vm.heap(), args[1], self.read_limit)?;
                let start_position = duration(args[2])?;
                let end_position = audio_end(start_position, args[3])?;
                let loop_region = audio_loop(args[4], args[5], args[6])?;
                let id = MediaId(
                    self.vm
                        .heap()
                        .host_payload(value, Slot::Id as u32)?
                        .as_int()
                        .unwrap() as u64,
                );
                let result = if response(self.vm.heap(), value)?.is_some() {
                    Err("audio has ended".to_string())
                } else if !self
                    .media
                    .live
                    .iter()
                    .any(|live| live.id == id && live.video.is_none() && live.result.is_none())
                {
                    Err("queue requires a live audio activity".to_string())
                } else {
                    self.media.backend.as_mut().unwrap().queue_audio(
                        id,
                        crate::audio::AudioQueueItem {
                            source,
                            start_position,
                            end_position,
                            loop_region,
                        },
                    )
                };
                match result {
                    Ok(()) => self.vm.respond(ticket, HostResponse::Ready(Value::NULL))?,
                    Err(error) => self.media_error(ticket, &error)?,
                }
            } else if symbol == "reel::audio::await" {
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
                    super::results::reserve_queue(
                        self.vm.heap(),
                        &mut self.media.waits,
                        &mut self.media.wait_charge,
                    )?;
                    let wait = ticket.wait_id();
                    self.vm.respond(ticket, HostResponse::Pending(wait))?;
                    self.media.waits.push_back((wait, value));
                }
            } else {
                let command = match symbol {
                    "reel::audio::seek" | "reel::video::seek" => {
                        MediaCommand::Seek(duration(args[1])?)
                    }
                    "reel::stop" => MediaCommand::Stop,
                    "reel::pause" => MediaCommand::Pause,
                    "reel::resume" => MediaCommand::Resume,
                    "reel::finish" => MediaCommand::Finish,
                    "reel::audio::volume" => MediaCommand::Volume {
                        value: volume(args[1])?,
                        duration: duration(args[2])?,
                    },
                    "reel::audio::pan" => MediaCommand::Pan {
                        value: pan(args[1])?,
                        duration: duration(args[2])?,
                    },
                    "reel::audio::rate" => MediaCommand::Rate {
                        value: rate(args[1])?,
                        duration: duration(args[2])?,
                    },
                    "reel::audio::stop" => MediaCommand::FadeOut(duration(args[1])?),
                    _ => {
                        return Err(AdapterError::Input(
                            "audio has no forced endpoint; use stop",
                        ));
                    }
                };
                if response(self.vm.heap(), value)?.is_none() {
                    let id = MediaId(
                        self.vm
                            .heap()
                            .host_payload(value, Slot::Id as u32)?
                            .as_int()
                            .unwrap() as u64,
                    );
                    if let Err(error) = self.media.backend.as_mut().unwrap().command(id, command) {
                        self.media_error(ticket, &error)?;
                        self.issued = None;
                        return Ok(());
                    }
                    if matches!(symbol, "reel::audio::seek" | "reel::video::seek") {
                        self.settle_source_cues(value)?;
                        let position = self
                            .media
                            .backend
                            .as_ref()
                            .unwrap()
                            .position(id)
                            .ok_or(AdapterError::Input("media transport missing"))?;
                        publish_position(self.vm.heap_mut(), value, position)?;
                        if let Some(live) = self.media.live.iter_mut().find(|live| live.id == id) {
                            live.latest_position = position;
                        }
                        self.settle_source_cues(value)?;
                    }
                }
                let result = if matches!(symbol, "reel::audio::seek" | "reel::video::seek") {
                    Value::NULL
                } else {
                    value
                };
                self.vm.respond(ticket, HostResponse::Ready(result))?;
            }
        }
        self.issued = None;
        Ok(())
    }
    fn start_media_cue(
        &mut self,
        ticket: HostTicket,
        source: Value,
        seconds: Value,
    ) -> Result<(), AdapterError> {
        let seconds = seconds
            .as_float()
            .or_else(|| seconds.as_int().map(|value| value as f64))
            .filter(|value| value.is_finite() && *value >= 0.)
            .ok_or(AdapterError::Input(
                "media position target must be finite and nonnegative",
            ))?;
        let owner = self.default_activity_owner(ticket.task)?;
        super::scopes::validate_owner(self.vm.heap(), &self.stage, owner)?;
        super::results::reserve_queue(
            self.vm.heap(),
            &mut self.media.cues,
            &mut self.media.cue_charge,
        )?;
        let native = self
            .vm
            .program()
            .package()
            .natives
            .iter()
            .position(|native| {
                self.vm.program().package().symbols[native.symbol.index()] == "reel::audio::await"
            })
            .ok_or(AdapterError::Input("media await binding missing"))?;
        let heap = self.vm.heap_mut();
        let fields = heap.alloc_table()?;
        let value = heap.alloc_host(
            fields,
            &[
                Value::int(CUE_TAG),
                source,
                Value::NULL,
                Value::int(0),
                Value::float(seconds),
                Value::NULL,
                owner,
            ],
            Some(readonly),
        )?;
        self.stage
            .types
            .mark(heap, value, "reel::types::MediaCue")?;
        let root = heap.add_root(self.root_owner, RootKind::Host, value)?;
        if let Err(error) = self
            .vm
            .bind_awaitable(value, aside_vm::bytecode::NativeId(native as u32))
        {
            self.vm.heap_mut().release_root(root)?;
            return Err(error.into());
        }
        if settle_cue(self.vm.heap_mut(), value)? {
            self.vm.heap_mut().release_root(root)?;
        } else {
            self.media.cues.push_back(root);
        }
        self.vm.respond(ticket, HostResponse::Ready(value))?;
        Ok(())
    }
    fn start_media(
        &mut self,
        ticket: HostTicket,
        playback: MediaPlayback,
        owner_value: Value,
        target: Option<NodeId>,
        target_value: Value,
    ) -> Result<(), AdapterError> {
        self.media
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
            .ok_or(AdapterError::Input("media await binding missing"))?;
        let start = match &playback {
            MediaPlayback::Audio(p) => p.request.start_position,
            MediaPlayback::Video(p) => p.request.start_position,
        };
        let heap = self.vm.heap_mut();
        let position = self.media.keys.record(
            heap,
            &[("position", Value::float(start.as_secs_f64()))],
            &[],
        )?;
        let fields = heap.alloc_table()?;
        let value = heap.alloc_host(
            fields,
            &[
                Value::int(TAG),
                Value::NULL,
                Value::NULL,
                Value::int(0),
                position,
                target_value,
                owner_value,
            ],
            Some(readonly),
        )?;
        self.stage
            .types
            .mark(heap, value, "reel::types::Playback")?;
        let root = heap.add_root(self.root_owner, RootKind::Host, value)?;
        if let Err(error) = self
            .vm
            .bind_awaitable(value, aside_vm::bytecode::NativeId(native as u32))
        {
            self.vm.heap_mut().release_root(root)?;
            return Err(error.into());
        }
        let video = match &playback {
            MediaPlayback::Video(p) => Some(p.request.clone()),
            _ => None,
        };
        let admitted = self
            .media
            .backend
            .as_mut()
            .ok_or_else(|| "media output is not installed".to_string())
            .and_then(|backend| {
                if let Some(target) = target {
                    for poster in &mut self.media.posters {
                        if poster.saved.target == target
                            && let Some(id) = poster.pending.take()
                        {
                            backend.cancel(id);
                        }
                    }
                    for live in &mut self.media.live {
                        if live.target == Some(target) && live.result.is_none() {
                            backend.command(live.id, MediaCommand::Stop)?;
                            live.target = None;
                        }
                    }
                }
                match playback {
                    MediaPlayback::Audio(p) => backend.play_audio(p.request, p.paused),
                    MediaPlayback::Video(p) => backend.play_video(p.request, p.paused),
                }
            });
        match admitted {
            Ok(id) => {
                self.vm.heap_mut().set_host_payload(
                    value,
                    Slot::Id as u32,
                    Value::int(id.0 as i64),
                )?;
                self.media.live.push(Live {
                    id,
                    root,
                    target,
                    result: None,
                    pending_start: video.is_none().then_some(ticket.task),
                    video,
                    has_frame: false,
                    latest_position: start,
                });
                self.vm.respond(ticket, HostResponse::Ready(value))?;
            }
            Err(error) => {
                self.vm.heap_mut().release_root(root)?;
                self.media_error(ticket, &error)?;
            }
        }
        Ok(())
    }
    fn media_error(&mut self, ticket: HostTicket, error: &str) -> Result<(), AdapterError> {
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
impl Media {
    pub fn active(&self) -> bool {
        !self.live.is_empty() || self.posters.iter().any(|poster| poster.pending.is_some())
    }
    pub fn cleanup_pending(&self) -> bool {
        self.backend.as_ref().is_some_and(|b| b.cleanup_pending())
    }
    pub fn close(&mut self) {
        self.posters.clear();
        self.waits = VecDeque::new();
        self.wait_charge = None;
        self.live.clear(); // Session owner scan releases the retained heap roots.
        self.cues = VecDeque::new();
        self.cue_charge = None;
        self.cue_remaining = 0;
        self.pending_terminals = 0;
        if let Some(backend) = &mut self.backend {
            backend.close();
        }
    }
}

fn audio_end(start: Duration, end: Value) -> Result<Option<Duration>, AdapterError> {
    if end.tag() == ValueTag::Null {
        return Ok(None);
    }
    let end = duration(end)?;
    if end <= start {
        return Err(AdapterError::Input("audio end must follow start"));
    }
    Ok(Some(end))
}

fn audio_loop(
    looping: Value,
    start: Value,
    end: Value,
) -> Result<Option<AudioLoopRegion>, AdapterError> {
    let looping = looping
        .as_bool()
        .ok_or(AdapterError::Input("looping must be bool"))?;
    let start = duration(start)?;
    let end = if end.tag() == ValueTag::Null {
        None
    } else {
        Some(duration(end)?)
    };
    if looping {
        if end.is_some_and(|end| end <= start) {
            return Err(AdapterError::Input(
                "audio loop_end must be greater than loop_start",
            ));
        }
        Ok(Some(AudioLoopRegion { start, end }))
    } else if !start.is_zero() || end.is_some() {
        Err(AdapterError::Input(
            "audio loop bounds require looping: true",
        ))
    } else {
        Ok(None)
    }
}

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Start audio playback and return an awaitable media activity immediately.
        /// `source` is an asset path; `group` is music, voice, effects or ui.
        /// Volume is finite and nonnegative; pan is in -1..1. All times are seconds.
        /// `rate` defaults to 1 and is in 0.25..4; speed and pitch change together.
        /// Positions and loop bounds use source seconds at every rate.
        /// `start`/`end` select the playback interval; a non-null end must follow start.
        /// With `looping: true`, `loop_start`/`loop_end` select the repeated interval.
        /// `fade_in` ramps the initial volume; `paused: true` prepares a paused transport.
        /// `owner` accepts a stage node or scope handle. When omitted, ownership follows
        /// the innermost active scope, otherwise the task's inherited resource owner.
        /// A callback inherits its event node; the main story uses the session.
        /// Retiring the owner cancels loading and playback.
        /// Awaiting returns "Completed" or "Stopped"; output failures and cancellation throw.
        "reel::audio::play";
        /// Append an audio asset to a live audio activity's playback queue.
        /// Times are seconds. `start`/`end` select the queued interval; looping and
        /// loop bounds apply to that queued item. The existing activity retains its owner.
        /// Returns null after queue admission; a terminal or non-audio activity is rejected.
        "reel::audio::queue";
        /// Set an activity's finite, nonnegative volume, optionally ramping over
        /// `duration` seconds. Returns the same activity without waiting for the ramp.
        "reel::audio::volume";
        /// Set the finite, nonnegative gain of music, voice, effects, ui or master.
        /// `duration` is a nonnegative ramp time in seconds. Returns null after admission.
        /// This changes the shared bus rather than creating an owned playback activity.
        "reel::audio::group_volume";
        /// Set an activity's stereo pan in -1..1, optionally ramping over `duration`
        /// seconds. Returns the same activity without waiting for the ramp.
        "reel::audio::pan";
        /// Set audio content speed in 0.25..4, changing pitch with speed. A rate
        /// of 2 consumes twice as much source content per output second.
        /// `duration` defaults to 0; positive durations interpolate linearly in
        /// active playback seconds. Pausing freezes the ramp, and immediate changes
        /// apply while paused. Queued items share the transport rate. Snapshots
        /// preserve its current value and remaining ramp. Returns the activity.
        "reel::audio::rate";
        /// Stop a media activity, fading out over `fade` seconds when requested.
        /// Returns the same activity immediately; await it to observe the terminal result.
        /// Stopping an already terminal activity preserves its recorded outcome.
        "reel::audio::stop";
        /// Seek a media activity to a finite, nonnegative position in seconds.
        /// Returns null after the transport accepts the command. Position cues are
        /// reevaluated against the updated transport cursor; terminal activities stay terminal.
        "reel::audio::seek" | "reel::video::seek";
        /// Create an awaitable cue for a finite, nonnegative transport position in seconds.
        /// Awaiting returns true once the cursor reaches that position, or false if normal
        /// playback ends first. Source failure or cancellation throws even at the threshold.
        /// The cue belongs to the current scope owner and does not pause or stop playback.
        "reel::audio::at" | "reel::video::at";
        /// Read the media transport's latest published position in seconds.
        /// The read participates in reactive dependency tracking. Terminal activities
        /// retain their final position; this does not wait for playback or a new frame.
        "reel::audio::position" | "reel::video::position";
        /// Await a media activity or position cue. Ordinary `await activity` uses this
        /// same binding. Playback yields "Completed" or "Stopped"; cues yield a bool.
        /// Repeated awaits preserve the terminal result. Failure or owner cancellation throws.
        "reel::audio::await";
        /// Play a video node's source and return an awaitable media activity immediately.
        /// `start` and optional `end` are seconds; end must follow start. Volume is finite
        /// and nonnegative. `looping` repeats playback; `paused` prepares a paused transport.
        /// The activity updates the supplied video node. Its owner defaults to the current
        /// scope owner; owner retirement cancels playback. Awaiting returns "Completed" or
        /// "Stopped", while failure and cancellation throw. Completion keeps the last frame.
        "reel::video::play";
    }
}
pub(super) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{NamedType, named, nullable, number};
    for symbol in ["reel::audio::play", "reel::video::play"] {
        api.refine(symbol, named("reel::types::Playback"), &[]);
    }
    for symbol in ["reel::audio::position", "reel::video::position"] {
        api.refine(
            symbol,
            TypeHint::Float,
            &[("activity", named("reel::types::Playback"))],
        );
    }
    let mut playback = NamedType::new(
        "reel::types::Playback",
        "An awaitable audio or video playback controlled through audio and video functions.",
        Vec::new(),
    );
    playback.await_result = Some(TypeHint::String);
    api.types.push(playback);
    for prefix in ["reel::audio::", "reel::video::"] {
        let symbols = api
            .functions
            .iter()
            .filter(|declaration| {
                aside_host::source::canonical(&declaration.symbol).starts_with(prefix)
            })
            .map(|declaration| aside_host::source::canonical(&declaration.symbol))
            .collect::<Vec<_>>();
        for symbol in symbols {
            let signature = api.native_mut(&symbol).symbol.signature.as_mut().unwrap();
            for parameter in &mut signature.parameters {
                parameter.ty = match parameter.name.as_str() {
                    "activity" => named("reel::types::Playback"),
                    "source" | "group" => TypeHint::String,
                    "looping" | "paused" => TypeHint::Bool,
                    "end" | "loop_end" => nullable(number()),
                    "volume" | "pan" | "fade_in" | "start" | "loop_start" | "value"
                    | "duration" | "fade" | "seconds" | "rate" => number(),
                    _ => parameter.ty.clone(),
                };
            }
            let function = symbol.rsplit("::").next().unwrap();
            signature.result = match function {
                "volume" | "pan" | "stop" | "rate" => named("reel::types::Playback"),
                "queue" | "seek" | "group_volume" => TypeHint::Null,
                "at" => named("reel::types::MediaCue"),
                _ => signature.result.clone(),
            };
        }
    }
    let mut cue = NamedType::new(
        "reel::types::MediaCue",
        "An awaitable playback-position cue. Producer retirement and transport changes settle its wait.",
        Vec::new(),
    );
    cue.await_result = Some(TypeHint::Bool);
    api.types.push(cue);
}
