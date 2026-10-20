use super::{AdapterError, HostTicket, SessionVm, stage, text};
use crate::audio::{AudioGroup, AudioLoopRegion, AudioRequest};
use crate::media::{
    MediaBackend, MediaCommand, MediaCompletion, MediaId, MediaPlayback, MediaResult,
};
use aside_vm::executor::{Completion, HostResponse, TaskId, WaitId};
use aside_vm::{Heap, HeapError, RootKind, RootToken, Value, ValueTag};
use reel_ui::stage::NodeId;
use std::time::Duration;

const TAG: i64 = -1003;
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
}
#[derive(Default)]
pub(super) struct Media {
    backend: Option<Box<dyn MediaBackend>>,
    live: Vec<Live>,
    waits: Vec<(WaitId, Value)>,
    cursor: usize,
    last_time: f64,
    posters: Vec<Poster>,
    poster_cursor: usize,
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
        if matches!(record.part, super::snapshot::Part::MediaWait) {
            let (wait, value): (WaitId, Value) = record.read()?;
            self.waits.push((map.wait(wait)?, map.value(value)?));
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
                            .play_audio(playback.request)
                            .map_err(aside_vm::snapshot::SnapshotError::Invalid)?;
                        for item in playback.queue {
                            backend
                                .queue_audio(id, item)
                                .map_err(aside_vm::snapshot::SnapshotError::Invalid)?;
                        }
                        if pending_start.is_none() {
                            backend.commit_audio(id);
                        }
                        if playback.paused {
                            backend
                                .command(id, MediaCommand::Pause)
                                .map_err(aside_vm::snapshot::SnapshotError::Invalid)?;
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
        self.live.push(Live {
            id,
            root,
            target: saved.target,
            result: saved.result,
            video,
            has_frame: false,
            pending_start,
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
pub(super) fn readonly(_: &Heap, _: Value, _: u32, _: Value) -> Result<(), HeapError> {
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
        self.media.backend.as_ref().and_then(|b| b.poll_after())
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
                    if let Some(target) = live.target
                        && let Some(frame) = backend.take_video_frame(live.id)
                    {
                        self.stage.set_video_frame(target, frame)?;
                        live.has_frame = true;
                        changed = true;
                    }
                    live.result = backend.take_result(live.id);
                }
            }
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
                heap.set_host_payload(
                    value,
                    Slot::Position as u32,
                    Value::float(completion.position.as_secs_f64()),
                )?;
                heap.set_host_payload(value, Slot::Target as u32, Value::NULL)?;
                heap.set_host_payload(value, Slot::Owner as u32, Value::NULL)?;
                heap.release_root(live.root)?;
                self.media.live.swap_remove(self.media.cursor);
            } else {
                self.media.cursor += 1;
            }
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
        let mut woke = changed;
        let mut i = 0;
        while i < self.media.waits.len() {
            let (wait, value) = self.media.waits[i];
            if !self.vm.wait_is_pending(wait) {
                self.media.waits.swap_remove(i);
            } else if let Some(result) = response(self.vm.heap(), value)? {
                self.vm.complete(wait, result)?;
                self.media.waits.swap_remove(i);
                woke = true;
            } else {
                i += 1;
            }
        }
        Ok(woke)
    }
    pub(super) fn dispatch_media(
        &mut self,
        ticket: HostTicket,
        symbol: &str,
    ) -> Result<(), AdapterError> {
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
            let request = AudioRequest {
                source,
                loop_region,
                volume,
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
                    paused: false,
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
                    paused: false,
                    result: None,
                }),
                owner,
                Some(target),
                args[0],
            )?;
        } else {
            let value = args[0];
            if !is_media(self.vm.heap(), value) {
                return Err(AdapterError::Input("expected media activity"));
            }
            if symbol == "reel::audio::queue" {
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
            } else if matches!(symbol, "reel::audio::position" | "reel::video::position") {
                if response(self.vm.heap(), value)?.is_some() {
                    let position = self.vm.heap().host_payload(value, Slot::Position as u32)?;
                    self.vm.respond(ticket, HostResponse::Ready(position))?;
                } else {
                    let id = MediaId(
                        self.vm
                            .heap()
                            .host_payload(value, Slot::Id as u32)?
                            .as_int()
                            .unwrap() as u64,
                    );
                    let position = self
                        .media
                        .backend
                        .as_ref()
                        .and_then(|backend| backend.position(id));
                    if let Some(position) = position {
                        self.vm.respond(
                            ticket,
                            HostResponse::Ready(Value::float(position.as_secs_f64())),
                        )?;
                    } else {
                        self.media_error(ticket, "media transport missing")?;
                    }
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
                    self.media
                        .waits
                        .try_reserve(1)
                        .map_err(|_| AdapterError::Allocation)?;
                    let wait = ticket.wait_id();
                    self.vm.respond(ticket, HostResponse::Pending(wait))?;
                    self.media.waits.push((wait, value));
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
        let fields = heap.alloc_table()?;
        let value = heap.alloc_host(
            fields,
            &[
                Value::int(TAG),
                Value::NULL,
                Value::NULL,
                Value::int(0),
                Value::float(start.as_secs_f64()),
                target_value,
                owner_value,
            ],
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
                    MediaPlayback::Audio(p) => backend.play_audio(p.request),
                    MediaPlayback::Video(p) => backend.play_video(p.request),
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
        self.waits.clear();
        self.live.clear(); // Session owner scan releases the retained heap roots.
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
