use super::*;
use crate::video::{
    AudioFrames, DecodeDemand, DecodedPacket, VideoDecoder, Yuv420Layout, YuvColor,
};
use reel_core::budget::{BudgetCategory, QueueLimits};
use std::{io::Cursor, task::Poll, time::Instant};

#[derive(Debug)]
struct InterleavedFactory {
    audio_first: bool,
}
struct Interleaved {
    info: VideoInfo,
    audio_first: bool,
    pictures: usize,
    audio_sent: bool,
}
impl VideoDecoderFactory for InterleavedFactory {
    fn open(
        &self,
        _: Box<dyn MediaRead>,
        _: BudgetPool,
        _: Arc<AtomicBool>,
    ) -> Result<Box<dyn VideoDecoder>, String> {
        Ok(Box::new(Interleaved {
            info: VideoInfo {
                width: 2,
                height: 2,
                duration: Some(2.),
                audio_sample_rate: Some(4),
            },
            audio_first: self.audio_first,
            pictures: 0,
            audio_sent: false,
        }))
    }
}
impl VideoDecoder for Interleaved {
    fn info(&self) -> &VideoInfo {
        &self.info
    }
    fn audio_finished(&self) -> bool {
        self.audio_sent
    }
    fn set_end(&mut self, _: f64) {}
    fn seek(&mut self, _: f64) -> Result<(), String> {
        Ok(())
    }
    fn prepare_poster(&mut self, _: f64) -> Result<(), String> {
        self.info.audio_sample_rate = None;
        self.audio_sent = true;
        Ok(())
    }
    fn poll_packet(&mut self, demand: DecodeDemand) -> Result<Poll<Option<DecodedPacket>>, String> {
        let audio = demand.audio && !self.audio_sent;
        let video = demand.video && self.pictures < 3;
        if audio && (self.audio_first || !video) {
            self.audio_sent = true;
            return Ok(Poll::Ready(Some(DecodedPacket::Audio(AudioFrames {
                pts: 0.,
                sample_rate: 4,
                frames: (1..=8).map(|i| [i as f32, -(i as f32)]).collect(),
            }))));
        }
        if video {
            self.pictures += 1;
            return Ok(Poll::Ready(Some(DecodedPacket::Video(VideoFrame {
                storage: None,
                pts: self.pictures as f64 / 2.,
                duration: 0.5,
                width: 2,
                height: 2,
                layout: Yuv420Layout::Planar,
                row_strides: [2, 1, 1],
                planes: [vec![0; 4], vec![128], vec![128]],
                color: YuvColor {
                    matrix: crate::video::YuvMatrix::Bt709,
                    full_range: false,
                    transfer: crate::video::YuvTransfer::Bt709,
                },
            }))));
        }
        if self.pictures == 3 && self.audio_sent {
            Ok(Poll::Ready(None))
        } else {
            Ok(Poll::Pending)
        }
    }
}
fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "decoder made no bounded progress"
        );
        thread::sleep(Duration::from_millis(1));
    }
}
fn movie(audio_first: bool) -> (VideoPlayback, BudgetPool) {
    let budget = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 32,
            max_bytes: 32768,
        },
    );
    let movie = VideoPlayback::start_with_decoder(
        Box::new(Cursor::new(Vec::<u8>::new())),
        || {},
        VideoOptions {
            queued_video_frames: 2,
            queued_audio_frames: 2,
            ..Default::default()
        },
        budget.clone(),
        WakeSignal::new(|| {}),
        Arc::new(InterleavedFactory { audio_first }),
    )
    .unwrap();
    (movie, budget)
}

#[test]
fn future_picture_backpressure_keeps_pcm_advancing_and_retires_partial_packet() {
    let (mut movie, budget) = movie(false);
    let mut audio = None;
    wait(|| {
        audio = movie.take_audio();
        audio.is_some()
    });
    let mut audio = audio.unwrap();
    // No picture is due at clock zero. Filling that queue must still reach PCM.
    wait(|| audio.consumer.slots() == 2);
    assert_eq!(movie.shared.state.lock().unwrap().frames.len(), 2);
    assert!(movie.take_frame().is_none());
    assert!(!movie.shared.transport.audio_end.load(Ordering::Acquire));
    let mut pictures = Vec::new();
    for expected in 1..=8 {
        let mut sample = None;
        wait(|| {
            sample = audio.consumer.pop().ok();
            sample.is_some()
        });
        let sample = sample.unwrap();
        assert_eq!(
            sample.frame,
            Frame::new(expected as f32, -(expected as f32))
        );
        movie.shared.transport.clock.store(sample.end);
        movie.tick(Duration::ZERO);
        if let Some(frame) = movie.take_frame() {
            pictures.push(frame.frame.pts);
        }
    }
    wait(|| movie.worker_finished());
    if let Some(frame) = movie.take_frame() {
        pictures.push(frame.frame.pts);
    }
    assert_eq!(pictures, [0.5, 1., 1.5]);
    assert!(movie.shared.transport.audio_end.load(Ordering::Acquire));
    movie
        .shared
        .transport
        .audio_drained
        .store(true, Ordering::Release);
    movie.tick(Duration::ZERO);
    assert_eq!(movie.terminal(), Some(VideoTerminal::Completed));
    drop(audio);
    drop(movie);
    assert_eq!(budget.usage().bytes, 0);
}

#[test]
fn partial_pcm_backpressure_keeps_picture_queue_filled_and_cancel_reclaims_storage() {
    let (mut movie, budget) = movie(true);
    // The first PCM packet exceeds the entire ring. Its remainder cannot stop
    // picture publication, including while the mixer has not admitted the stream.
    wait(|| movie.shared.state.lock().unwrap().frames.len() == 2);
    assert!(!movie.shared.transport.audio_end.load(Ordering::Acquire));
    assert!(
        movie
            .shared
            .state
            .lock()
            .unwrap()
            .audio
            .as_ref()
            .unwrap()
            .consumer
            .slots()
            == 2
    );
    movie.cancel();
    wait(|| movie.worker_finished());
    assert_eq!(movie.terminal(), Some(VideoTerminal::Cancelled));
    drop(movie);
    assert_eq!(budget.usage().bytes, 0);
}

#[test]
fn split_clock_reads_complete_publications() {
    let clock = Arc::new(Clock::new(f64::from_bits(0x3ff0_0000_ffff_ffff)));
    let writer = clock.clone();
    let running = Arc::new(AtomicBool::new(true));
    let finished = running.clone();
    let values = [0x3ff0_0000_ffff_ffff, 0x4000_0000_1111_1111];
    let worker = thread::spawn(move || {
        for i in 0..100_000 {
            writer.store(f64::from_bits(values[i & 1]));
        }
        finished.store(false, Ordering::Release);
    });
    while running.load(Ordering::Acquire) {
        assert!(values.contains(&clock.load().to_bits()));
    }
    worker.join().unwrap();
}

#[test]
fn owned_picture_storage_retains_its_budget_through_shared_cpu_views() {
    #[derive(Debug)]
    struct Owned {
        y: [u8; 4],
        uv: [u8; 2],
        _ticket: BudgetTicket,
    }
    impl reel_core::image::Yuv420Storage for Owned {
        fn plane(&self, index: usize) -> &[u8] {
            match index {
                0 => &self.y,
                1 => &self.uv,
                _ => &[],
            }
        }
        fn native_storage(&self) -> Option<&(dyn std::any::Any + Send + Sync)> {
            Some(self)
        }
    }
    let budget = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 4,
            max_bytes: 16,
        },
    );
    let source = Arc::new(Owned {
        y: [3; 4],
        uv: [128; 2],
        _ticket: budget.reserve(6).unwrap(),
    });
    let frame = Arc::new(ResidentFrame {
        frame: VideoFrame {
            storage: Some(source),
            pts: 0.,
            duration: 1.,
            width: 2,
            height: 2,
            layout: Yuv420Layout::Nv12,
            row_strides: [2, 2, 0],
            planes: std::array::from_fn(|_| Vec::new()),
            color: YuvColor {
                matrix: crate::video::YuvMatrix::Bt709,
                full_range: false,
                transfer: crate::video::YuvTransfer::Bt709,
            },
        },
        _ticket: budget.reserve(0).unwrap(),
    });
    let image = reel_core::image::Yuv420Image::with_layout(
        [2, 2],
        frame.frame.color,
        Yuv420Layout::Nv12,
        [2, 2, 0],
        frame.clone(),
    )
    .unwrap();
    drop(frame);
    assert_eq!(image.plane(0), [3; 4]);
    assert!(image.native_storage().unwrap().is::<Owned>());
    assert_eq!(budget.usage().bytes, 6);
    drop(image);
    assert_eq!(budget.usage().bytes, 0);
}
