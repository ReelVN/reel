//! Video transport on the session's existing IO and audio devices.
use super::*;
use crate::assets::StreamControl;
use reel_core::{identity::ContentVersion, image::Yuv420Image};
use reel_engine::video::{VideoPlaybackState, VideoRequest};
use reel_media::video::{VideoOptions, VideoPlayback, VideoTerminal};
use reel_ui::resources::{ResourceLease, ResourceRegistry};

#[derive(Clone, Debug)]
pub struct VideoLimits {
    pub decoded_bytes: u64,
    pub queued_pictures: usize,
    pub queued_audio_frames: usize,
}
impl Default for VideoLimits {
    fn default() -> Self {
        Self {
            decoded_bytes: 64 << 20,
            queued_pictures: 3,
            queued_audio_frames: 8192,
        }
    }
}
pub(super) struct Movies {
    pub entries: BTreeMap<MediaId, Movie>,
    pub retiring: Vec<VideoPlayback>,
    budget: BudgetPool,
}
impl Movies {
    #[cfg(feature = "profiling")]
    pub(super) fn profile(&self, phase: &str) {
        eprintln!(
            "[reel-media] {phase} video={} retiring={} decoded={:?}",
            self.entries.len(),
            self.retiring.len(),
            self.budget.usage()
        );
    }
    pub fn new(limits: &VideoLimits) -> Self {
        Self {
            entries: BTreeMap::new(),
            retiring: Vec::new(),
            budget: BudgetPool::new(
                BudgetCategory::CpuOutput,
                QueueLimits {
                    // Every picture and PCM ring has nonzero bytes; the byte budget
                    // bounds residency independently of variable picture sizes.
                    max_requests: u32::MAX,
                    max_bytes: limits.decoded_bytes,
                },
            ),
        }
    }
    fn retire(&mut self, mut playback: VideoPlayback) {
        playback.cancel();
        if !playback.worker_finished() {
            self.retiring.push(playback);
        }
    }
    pub fn cancel(&mut self, id: MediaId) {
        if let Some(mut movie) = self.entries.remove(&id) {
            if let Some(playback) = movie.playback.take() {
                self.retire(playback);
            }
            if let Some(playback) = movie.retiring.take() {
                self.retire(playback);
            }
        }
    }
    pub fn close(&mut self) {
        while let Some(&id) = self.entries.keys().next() {
            self.cancel(id);
        }
    }
    pub fn take_result(&mut self, id: MediaId) -> Option<MediaCompletion> {
        let movie = self.entries.get(&id)?;
        let result = movie.result.clone()?;
        let position = movie.position();
        self.cancel(id);
        Some(MediaCompletion { result, position })
    }
}
pub(super) struct Movie {
    request: VideoRequest,
    pub paused: bool,
    playback: Option<VideoPlayback>,
    retiring: Option<VideoPlayback>,
    pending_seek: bool,
    source: StreamControl,
    result: Option<MediaResult>,
    pub frame: Option<ResourceLease>,
    position: Duration,
    poster_result: Option<MediaResult>,
}
impl Movie {
    pub fn position(&self) -> Duration {
        self.playback
            .as_ref()
            .map_or(self.position, VideoPlayback::position)
    }
    pub fn snapshot(&self) -> VideoPlaybackState {
        let request = self.request.clone();
        VideoPlaybackState {
            request,
            position: self.position(),
            paused: self.paused,
            result: self.result.clone(),
        }
    }
    pub fn pause(&self, value: bool) {
        if let Some(playback) = &self.playback {
            playback.pause(value);
        }
    }
    fn settle(&mut self, result: MediaResult) -> Option<VideoPlayback> {
        if self.result.is_some() {
            return None;
        }
        self.position = self.position();
        self.result = Some(result);
        self.playback.take()
    }
    fn failed(&mut self, error: String) -> Option<VideoPlayback> {
        self.settle(MediaResult::Failed(format!(
            "{}: {error}",
            self.request.source
        )))
    }
}
impl MediaPlayer {
    #[cfg(test)]
    pub(super) fn decoder_generations(&self) -> usize {
        self.movies.retiring.len()
            + self
                .movies
                .entries
                .values()
                .map(|movie| {
                    usize::from(movie.playback.is_some()) + usize::from(movie.retiring.is_some())
                })
                .sum::<usize>()
    }
    fn open_movie(
        &self,
        request: &VideoRequest,
        position: Duration,
        poster: bool,
        paused: bool,
    ) -> Result<(StreamControl, VideoPlayback), String> {
        let decoder = self
            .options
            .video_decoder
            .clone()
            .ok_or("video decoder is unavailable on this platform")?;
        let (control, stream) = self.assets.stream(&request.source)?;
        let cancel = control.clone();
        let playback = VideoPlayback::start_with_decoder(
            Box::new(stream),
            move || cancel.cancel(),
            VideoOptions {
                io_deadline: Some(control.read_deadline()),
                queued_video_frames: self.options.video.queued_pictures,
                queued_audio_frames: self.options.video.queued_audio_frames,
                start: position,
                poster,
                end: request.end_position,
                paused: paused || self.suspended,
            },
            self.movies.budget.clone(),
            self.wake.clone(),
            decoder,
        )?;
        playback.set_volume(request.volume)?;
        Ok((control, playback))
    }
    pub(super) fn start_video(
        &mut self,
        request: VideoRequest,
        paused: bool,
    ) -> Result<MediaId, String> {
        if self.closed {
            return Err("media session closed".into());
        }
        if self.active_count() >= self.options.mixer.max_sounds {
            return Err("media activity capacity exhausted".into());
        }
        if !request.volume.is_finite() || request.volume < 0. || request.volume > f32::MAX as f64 {
            return Err("video volume must be finite and nonnegative".into());
        }
        // A silent movie remains usable without a device. Audio admission below
        // reports an actual output failure only when the container has a track.
        let (source, playback) = self
            .open_movie(&request, request.start_position, false, paused)
            .map_err(|e| format!("{}: {e}", request.source))?;
        self.next = self.next.checked_add(1).ok_or("media identity exhausted")?;
        let id = MediaId(self.next);
        self.movies.entries.insert(
            id,
            Movie {
                position: request.start_position,
                request,
                paused,
                playback: Some(playback),
                retiring: None,
                pending_seek: false,
                source,
                result: None,
                frame: None,
                poster_result: None,
            },
        );
        Ok(id)
    }
    pub(super) fn restore_movie(&mut self, state: VideoPlaybackState) -> Result<MediaId, String> {
        if self.closed {
            return Err("media session closed".into());
        }
        if self.active_count() >= self.options.mixer.max_sounds {
            return Err("media activity capacity exhausted".into());
        }
        let (source, playback) = self
            .open_movie(
                &state.request,
                state.position,
                state.result.is_some(),
                state.paused,
            )
            .map_err(|e| format!("{}: {e}", state.request.source))?;
        self.next = self.next.checked_add(1).ok_or("media identity exhausted")?;
        let id = MediaId(self.next);
        self.movies.entries.insert(
            id,
            Movie {
                request: state.request,
                position: state.position,
                paused: state.paused,
                playback: Some(playback),
                retiring: None,
                pending_seek: false,
                source,
                result: None,
                frame: None,
                poster_result: state.result,
            },
        );
        Ok(id)
    }
    pub(super) fn video_command(
        &mut self,
        id: MediaId,
        command: MediaCommand,
    ) -> Result<(), String> {
        let movie = self.movies.entries.get_mut(&id).unwrap();
        if movie.result.is_some() {
            return Ok(());
        }
        match command {
            MediaCommand::Pause | MediaCommand::Resume => {
                movie.paused = matches!(command, MediaCommand::Pause);
                movie.pause(movie.paused || self.suspended);
            }
            MediaCommand::Stop | MediaCommand::Finish => {
                if matches!(command, MediaCommand::Finish) && movie.request.looping {
                    return Err("finish is available for finite video playback".into());
                }
                let result = if matches!(command, MediaCommand::Finish) {
                    MediaResult::Completed
                } else {
                    MediaResult::Stopped
                };
                if let Some(playback) = movie.settle(result) {
                    self.movies.retire(playback);
                }
            }
            MediaCommand::Seek(position) => {
                if movie
                    .request
                    .end_position
                    .is_some_and(|end| position >= end)
                {
                    return Err("video seek must precede the segment end".into());
                }
                // Pointer-driven scrubbing can issue many seeks before the next
                // host tick. Retain only the newest target, and never overlap
                // decoder generations for one transport.
                if let Some(mut playback) = movie.playback.take() {
                    playback.cancel();
                    movie.retiring = Some(playback);
                }
                movie.position = position;
                movie.pending_seek = true;
                // The previous displayed frame remains in the stage until the
                // seek target arrives, without retaining it in this producer.
                movie.frame = None;
            }
            MediaCommand::Volume { value, duration } => {
                if !duration.is_zero() {
                    return Err("video volume envelopes are not installed".into());
                }
                if !value.is_finite() || value < 0. || value > f32::MAX as f64 {
                    return Err("video volume must be finite and nonnegative".into());
                }
                if let Some(playback) = &movie.playback {
                    playback.set_volume(value)?;
                }
                movie.request.volume = value;
            }
            MediaCommand::Pan { .. } | MediaCommand::FadeOut(_) => {
                return Err("video pan and fade-out envelopes are not installed".into());
            }
            MediaCommand::Rate { .. } => return Err("audio.rate requires an audio activity".into()),
        }
        Ok(())
    }
    pub(super) fn maintain_video(&mut self, elapsed: Duration) {
        self.movies
            .retiring
            .retain_mut(|playback| !playback.worker_finished());
        // Temporarily move the map to allow the same coordinator to admit its
        // audio track and restart a completed loop through ordinary AssetIo.
        let mut entries = std::mem::take(&mut self.movies.entries);
        for movie in entries.values_mut() {
            if movie
                .retiring
                .as_mut()
                .is_some_and(VideoPlayback::worker_finished)
            {
                movie.retiring = None;
            }
            if movie.pending_seek && movie.retiring.is_none() && movie.result.is_none() {
                movie.pending_seek = false;
                match self.open_movie(&movie.request, movie.position, false, movie.paused) {
                    Ok((source, playback)) => {
                        movie.source = source;
                        movie.playback = Some(playback);
                    }
                    Err(error) => {
                        movie.failed(error);
                    }
                }
            }
            let Some(playback) = &mut movie.playback else {
                continue;
            };
            if self
                .service
                .as_ref()
                .is_some_and(AudioActivityService::can_play_video_audio)
                && let Some(sound) = playback.take_audio()
                && let Err(error) = self.play_video_audio(sound)
            {
                if let Some(playback) = movie.failed(error) {
                    self.movies.retire(playback);
                }
                continue;
            }
            if self.failure.is_some()
                && playback
                    .info()
                    .is_some_and(|info| info.audio_sample_rate.is_some())
            {
                if let Some(playback) = movie.failed(self.failure.clone().unwrap()) {
                    self.movies.retire(playback);
                }
                continue;
            }
            playback.tick(if self.suspended {
                Duration::ZERO
            } else {
                elapsed
            });
            if let Some(frame) = playback.take_frame() {
                let version = movie
                    .source
                    .version()
                    .expect("decoded picture has described source");
                match register_frame(version, frame) {
                    Ok(frame) => movie.frame = Some(frame),
                    Err(error) => {
                        if let Some(playback) = movie.failed(error) {
                            self.movies.retire(playback);
                        }
                        continue;
                    }
                }
            }
            let Some(terminal) = playback.terminal() else {
                continue;
            };
            if terminal == VideoTerminal::Completed
                && movie.request.looping
                && movie.poster_result.is_none()
            {
                let request = &movie.request;
                match self.open_movie(request, request.start_position, false, movie.paused) {
                    Ok((source, playback)) => {
                        let previous = movie.playback.replace(playback).unwrap();
                        movie.source = source;
                        self.movies.retire(previous);
                    }
                    Err(error) => {
                        if let Some(playback) = movie.failed(error) {
                            self.movies.retire(playback);
                        }
                    }
                }
                continue;
            }
            let result = match terminal {
                VideoTerminal::Completed => {
                    movie.poster_result.take().unwrap_or(MediaResult::Completed)
                }
                VideoTerminal::Stopped => MediaResult::Stopped,
                VideoTerminal::Cancelled => MediaResult::Cancelled,
                VideoTerminal::Failed(error) => {
                    MediaResult::Failed(format!("{}: {error}", movie.request.source))
                }
            };
            if let Some(playback) = movie.settle(result) {
                self.movies.retire(playback);
            }
        }
        self.movies.entries = entries;
    }
    pub fn video_usage(&self) -> reel_core::budget::BudgetUsage {
        self.movies.budget.usage()
    }
}

fn register_frame(
    mut version: ResourceVersion,
    frame: reel_media::video::VideoFrameLease,
) -> Result<ResourceLease, String> {
    // Immutable file digest plus presentation timestamp identifies the decoded
    // picture. No full-picture hashing or RGBA conversion on the control thread.
    let picture = &frame.frame;
    let mut digest = blake3::Hasher::new();
    digest.update(b"reel-yuv420-v1");
    digest.update(&version.version.0);
    digest.update(&picture.pts.to_bits().to_le_bytes());
    version.version = ContentVersion(*digest.finalize().as_bytes());
    version.key.0.push_str("#video-frame");
    let image = Yuv420Image::with_layout(
        [picture.width, picture.height],
        picture.color,
        picture.layout,
        picture.row_strides,
        frame,
    )
    .map_err(|e| format!("video picture: {e:?}"))?;
    let mut registry = ResourceRegistry::new(1, 1024 + version.key.0.capacity() as u64)
        .map_err(|e| format!("video descriptor: {e:?}"))?;
    let handle = registry
        .register_yuv420(version, image)
        .map_err(|e| format!("video resource: {e:?}"))?;
    registry
        .acquire(handle)
        .map_err(|e| format!("video lease: {e:?}"))
}
