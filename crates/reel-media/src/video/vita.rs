//! AvPlayer adapter for the common worker, picture queues and PCM clock.
use super::{
    AudioFrames, DecodedPacket, MediaRead, VideoDecoder, VideoDecoderFactory, VideoFrame,
    VideoInfo, Yuv420Layout, YuvColor, h264::sequence_color,
};
use pvr_psp2_sys::{
    platform::{
        memory::{MemoryBudget, MemoryReservation},
        time,
    },
    video::{Player, Poll},
};
use reel_core::budget::{BudgetPool, BudgetTicket};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug)]
struct NativeBudget(BudgetPool);
impl MemoryBudget for NativeBudget {
    fn reserve(&self, bytes: u64) -> Result<Box<dyn MemoryReservation>, String> {
        self.0
            .reserve(bytes)
            .map(|ticket| Box::new(ticket) as Box<dyn MemoryReservation>)
            .map_err(|error| error.message)
    }
}

/// Opens the bounded Vita H.264/AAC hardware profile on one preparation worker.
/// The decoder itself is thread-bound; only owned NV12 and PCM cross workers.
#[derive(Debug, Default)]
pub struct VitaVideoDecoderFactory;
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
        Ok(Box::new(VitaDecoder {
            player,
            info,
            color,
            budget,
            stopped,
            pending: None,
            pending_storage: None,
            audio_done,
            video_done: false,
            end,
            video_end,
            seek_position: 0.,
            poster: false,
        }))
    }
}

struct VitaDecoder {
    player: Player,
    info: VideoInfo,
    color: YuvColor,
    budget: BudgetPool,
    stopped: Arc<AtomicBool>,
    // One picture of lookahead determines source duration from actual adjacent
    // timestamps. It carries its own reservation while returned frames transfer
    // into the common worker's separately reserved publication queue.
    pending: Option<VideoFrame>,
    pending_storage: Option<BudgetTicket>,
    audio_done: bool,
    video_done: bool,
    end: f64,
    video_end: f64,
    seek_position: f64,
    poster: bool,
}

impl VitaDecoder {
    fn video(&mut self) -> Result<Option<VideoFrame>, String> {
        if self.video_done {
            return Ok(None);
        }
        let width = self.info.width;
        let height = self.info.height;
        let pixels = width as usize * height as usize;
        let storage = &mut self.pending_storage;
        let budget = &self.budget;
        let color = self.color;
        match self.player.poll_video(|source| {
            if storage.is_none() {
                *storage = Some(
                    budget
                        .reserve((pixels * 3 / 2) as u64)
                        .map_err(|e| e.message)?,
                );
            }
            let (picture, [y, uv]) = source.copy_owned()?;
            Ok(VideoFrame {
                pts: picture.pts,
                duration: 0.,
                width,
                height,
                layout: Yuv420Layout::Nv12,
                row_strides: [width, width, 0],
                planes: [y, uv, Vec::new()],
                color,
            })
        })? {
            Poll::Ready(next) => {
                let previous = self.pending.replace(next);
                let Some(mut previous) = previous else {
                    return Ok(None);
                };
                previous.duration = (self.pending.as_ref().unwrap().pts - previous.pts).max(0.);
                if self.pending.as_ref().unwrap().pts >= self.video_end {
                    self.video_done = true;
                    self.pending = None;
                    self.pending_storage = None;
                }
                previous.duration = previous
                    .duration
                    .min((self.video_end - previous.pts).max(0.));
                Ok((previous.pts < self.video_end
                    && previous.pts + previous.duration > self.seek_position)
                    .then_some(previous))
            }
            Poll::Pending => Ok(None),
            Poll::End => {
                self.video_done = true;
                self.pending_storage = None;
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
        let mut frames = Vec::new();
        match self.player.poll_audio(&mut frames)? {
            Poll::Ready(packet) => {
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
                    frames,
                }))
            }
            Poll::Pending => Ok(None),
            Poll::End => {
                self.audio_done = true;
                Ok(None)
            }
        }
    }
}
impl VideoDecoder for VitaDecoder {
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
    fn next_packet(&mut self) -> Result<Option<DecodedPacket>, String> {
        loop {
            if self.stopped.load(Ordering::Acquire) {
                return Err("AvPlayer decode cancelled".into());
            }
            if let Some(video) = self.video()? {
                return Ok(Some(DecodedPacket::Video(video)));
            }
            if let Some(audio) = self.audio()? {
                return Ok(Some(DecodedPacket::Audio(audio)));
            }
            if self.video_done && self.audio_done {
                return Ok(None);
            }
            time::sleep(std::time::Duration::from_millis(2)).map_err(|e| e.to_string())?;
        }
    }
    fn seek(&mut self, seconds: f64) -> Result<(), String> {
        self.player.seek(seconds)?;
        self.pending = None;
        self.pending_storage = None;
        self.video_done = false;
        self.audio_done = self.info.audio_sample_rate.is_none();
        self.seek_position = seconds;
        Ok(())
    }
}
