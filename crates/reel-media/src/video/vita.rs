//! AvPlayer adapter for the common worker, picture queues and PCM clock.
use super::{
    AudioFrames, DecodeDemand, DecodedPacket, MediaRead, VideoDecoder, VideoDecoderFactory,
    VideoFrame, VideoInfo, Yuv420Layout, YuvColor, h264::sequence_color,
};
use reel_core::budget::{BudgetPool, BudgetTicket};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::task::Poll;
use vita_native::{
    app::memory::{MemoryBudget, MemoryReservation},
    video::{Nv12Pool, Player, Poll as NativePoll, SharedNv12},
};

#[derive(Debug)]
struct NativeBudget(BudgetPool);
impl MemoryBudget for NativeBudget {
    fn reserve(
        &self,
        bytes: u64,
    ) -> Result<Box<dyn MemoryReservation>, vita_native::memory::MemoryBudgetError> {
        self.0
            .reserve(bytes)
            .map(|ticket| Box::new(ticket) as Box<dyn MemoryReservation>)
            .map_err(|error| match error.code {
                reel_core::result::RejectCode::Busy => {
                    vita_native::memory::MemoryBudgetError::Capacity(error.message)
                }
                _ => vita_native::memory::MemoryBudgetError::Other(error.message),
            })
    }
}

/// Opens the bounded Vita H.264/AAC hardware profile on one preparation worker.
/// The decoder itself is thread-bound; only owned NV12 and PCM cross workers.
#[derive(Debug)]
pub struct VitaVideoDecoderFactory {
    /// Maximum rounded physical capacity for owned sampled NV12 blocks.
    pub frame_pool_bytes: u64,
}
impl Default for VitaVideoDecoderFactory {
    fn default() -> Self {
        Self {
            frame_pool_bytes: 8 << 20,
        }
    }
}
#[derive(Debug)]
struct NativeFrame(SharedNv12);
impl reel_core::image::Yuv420Storage for NativeFrame {
    fn plane(&self, index: usize) -> &[u8] {
        self.0.plane(index)
    }
    fn native_storage(&self) -> Option<&(dyn std::any::Any + Send + Sync)> {
        Some(&self.0)
    }
}
impl VideoDecoderFactory for VitaVideoDecoderFactory {
    fn open(
        &self,
        source: Box<dyn MediaRead>,
        budget: BudgetPool,
        stopped: Arc<AtomicBool>,
    ) -> Result<Box<dyn VideoDecoder>, String> {
        let player = Player::open(
            Box::new(source),
            Arc::new(NativeBudget(budget.clone())),
            stopped.clone(),
        )?;
        let color = sequence_color(player.sequence_parameter_set())?;
        let video_end = player.video_duration();
        let native = player.info();
        let info = VideoInfo {
            width: native.width,
            height: native.height,
            duration: Some(native.duration),
            audio_sample_rate: native.audio_sample_rate,
        };
        let audio_done = info.audio_sample_rate.is_none();
        let end = native.duration;
        let frames = Nv12Pool::new(
            [info.width, info.height],
            self.frame_pool_bytes,
            Arc::new(NativeBudget(budget.clone())),
        )?;
        Ok(Box::new(VitaDecoder {
            frames,
            player,
            info,
            color,
            budget,
            stopped,
            pending: None,
            audio_done,
            video_done: false,
            end,
            video_end,
            seek_position: 0.,
            poster: false,
            prefer_video: false,
            pcm: super::pcm_pool::PcmPool::default(),
        }))
    }
}

struct VitaDecoder {
    player: Player,
    info: VideoInfo,
    color: YuvColor,
    budget: BudgetPool,
    stopped: Arc<AtomicBool>,
    // One picture of lookahead determines duration from adjacent timestamps.
    // Every picture retains its pool reservation through all CPU/GPU readers.
    pending: Option<VideoFrame>,
    frames: Nv12Pool,
    audio_done: bool,
    video_done: bool,
    end: f64,
    video_end: f64,
    seek_position: f64,
    poster: bool,
    prefer_video: bool,
    pcm: super::pcm_pool::PcmPool,
}

impl VitaDecoder {
    fn video(&mut self) -> Result<Option<VideoFrame>, String> {
        if self.video_done {
            return Ok(None);
        }
        let width = self.info.width;
        let height = self.info.height;
        let Some(mut output) = self.frames.acquire()? else {
            return Ok(None);
        };
        let color = self.color;
        match self.player.poll_video(|source| {
            let picture = source.copy_to_native(&mut output)?;
            let shared = output.seal();
            let [y_stride, uv_stride] = shared.strides();
            Ok(VideoFrame {
                storage: Some(Arc::new(NativeFrame(shared))),
                pts: picture.pts,
                duration: 0.,
                width,
                height,
                layout: Yuv420Layout::Nv12,
                row_strides: [y_stride, uv_stride, 0],
                planes: std::array::from_fn(|_| Vec::new()),
                color,
            })
        })? {
            NativePoll::Ready(next) => {
                let previous = self.pending.replace(next);
                let Some(mut previous) = previous else {
                    return Ok(None);
                };
                previous.duration = (self.pending.as_ref().unwrap().pts - previous.pts).max(0.);
                if self.pending.as_ref().unwrap().pts >= self.video_end {
                    self.video_done = true;
                    self.pending = None;
                }
                previous.duration = previous
                    .duration
                    .min((self.video_end - previous.pts).max(0.));
                Ok((previous.pts < self.video_end
                    && previous.pts + previous.duration > self.seek_position)
                    .then_some(previous))
            }
            NativePoll::Pending => Ok(None),
            NativePoll::End => {
                self.video_done = true;
                let Some(mut last) = self.pending.take() else {
                    return Ok(None);
                };
                last.duration = (self.video_end - last.pts).max(0.);
                Ok(
                    (last.pts < self.video_end && last.pts + last.duration > self.seek_position)
                        .then_some(last),
                )
            }
        }
    }
    fn audio(&mut self) -> Result<Option<AudioFrames>, String> {
        if self.audio_done && !self.poster {
            return Ok(None);
        }
        let frames = self.pcm.prepare(&self.budget)?;
        match self.player.poll_audio(frames)? {
            NativePoll::Ready(packet) => {
                if self.poster {
                    return Err("poster decoder admitted unexpected audio".into());
                }
                let rate = f64::from(packet.sample_rate);
                let skip = ((self.seek_position - packet.pts).max(0.) * rate).ceil() as usize;
                let keep = ((self.end - packet.pts).max(0.) * rate).ceil() as usize;
                let last = packet.frames.min(keep);
                if packet.pts + packet.frames as f64 / rate >= self.end {
                    self.audio_done = true;
                }
                if skip >= last {
                    return Ok(None);
                }
                frames.truncate(last);
                if skip != 0 {
                    frames.drain(..skip);
                }
                Ok(Some(AudioFrames {
                    pts: packet.pts + skip as f64 / rate,
                    sample_rate: packet.sample_rate,
                    frames: self.pcm.publish(),
                }))
            }
            NativePoll::Pending => Ok(None),
            NativePoll::End => {
                self.audio_done = true;
                Ok(None)
            }
        }
    }
}
impl VideoDecoder for VitaDecoder {
    fn video_output_bytes(&self) -> u64 {
        0
    }
    fn recycle_audio(&mut self, packet: AudioFrames, storage: BudgetTicket) {
        self.pcm.recycle(packet.frames, storage);
    }
    fn info(&self) -> &VideoInfo {
        &self.info
    }
    fn audio_finished(&self) -> bool {
        self.audio_done || self.player.audio_finished()
    }
    fn set_paused(&mut self, paused: bool) -> Result<(), String> {
        self.player.set_paused(paused)
    }
    fn set_end(&mut self, seconds: f64) {
        self.end = self.end.min(seconds);
        self.video_end = self.video_end.min(seconds);
        self.info.duration = Some(self.end);
    }
    fn prepare_poster(&mut self, seconds: f64) -> Result<(), String> {
        self.poster = true;
        self.player.set_audio_enabled(false);
        self.info.audio_sample_rate = None;
        self.audio_done = true;
        let position = seconds.min((self.video_end - 0.001).max(0.));
        self.seek(position)
    }
    fn poll_packet(&mut self, demand: DecodeDemand) -> Result<Poll<Option<DecodedPacket>>, String> {
        if self.stopped.load(Ordering::Acquire) {
            return Err("AvPlayer decode cancelled".into());
        }
        for video_first in [self.prefer_video, !self.prefer_video] {
            if video_first {
                if demand.video
                    && let Some(video) = self.video()?
                {
                    self.prefer_video = false;
                    return Ok(Poll::Ready(Some(DecodedPacket::Video(video))));
                }
            } else if (demand.audio || self.poster)
                && let Some(audio) = self.audio()?
            {
                // Disabled poster audio still drains native buffers internally.
                self.prefer_video = true;
                return Ok(Poll::Ready(Some(DecodedPacket::Audio(audio))));
            }
        }
        if self.video_done && self.audio_finished() {
            Ok(Poll::Ready(None))
        } else {
            Ok(Poll::Pending)
        }
    }
    fn seek(&mut self, seconds: f64) -> Result<(), String> {
        self.player.seek(seconds)?;
        self.pending = None;
        self.video_done = false;
        self.audio_done = self.info.audio_sample_rate.is_none();
        self.seek_position = seconds;
        Ok(())
    }
}
