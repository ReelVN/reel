//! Delayed first audio output and paused producer control use real worker paths.
use super::*;
use crate::video::{AudioFrames, DecodeDemand, DecodedPacket, VideoDecoder};
use reel_core::budget::{BudgetCategory, QueueLimits};
use std::{io::Cursor, sync::atomic::AtomicUsize, task::Poll, time::Instant};
#[derive(Debug)]
struct Factory {
    release: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    empty: bool,
}
struct Decoder {
    factory: Factory,
    info: VideoInfo,
    sent: bool,
    video: bool,
}
impl VideoDecoderFactory for Factory {
    fn open(
        &self,
        _: Box<dyn MediaRead>,
        _: BudgetPool,
        _: Arc<AtomicBool>,
    ) -> Result<Box<dyn VideoDecoder>, String> {
        Ok(Box::new(Decoder {
            factory: Factory {
                release: self.release.clone(),
                calls: self.calls.clone(),
                empty: self.empty,
            },
            info: VideoInfo {
                width: 2,
                height: 2,
                duration: Some(2.),
                audio_sample_rate: Some(4),
            },
            sent: false,
            video: false,
        }))
    }
}
impl VideoDecoder for Decoder {
    fn info(&self) -> &VideoInfo {
        &self.info
    }
    fn audio_finished(&self) -> bool {
        self.sent
    }
    fn set_end(&mut self, _: f64) {}
    fn seek(&mut self, _: f64) -> Result<(), String> {
        Ok(())
    }
    fn prepare_poster(&mut self, _: f64) -> Result<(), String> {
        Ok(())
    }
    fn poll_packet(&mut self, demand: DecodeDemand) -> Result<Poll<Option<DecodedPacket>>, String> {
        self.factory.calls.fetch_add(1, Ordering::AcqRel);
        if !self.factory.release.load(Ordering::Acquire) {
            return Ok(Poll::Pending);
        }
        if self.factory.empty {
            self.sent = true;
            return Ok(Poll::Ready(None));
        }
        if demand.audio && !self.sent {
            self.sent = true;
            return Ok(Poll::Ready(Some(DecodedPacket::Audio(AudioFrames {
                pts: 0.,
                sample_rate: 4,
                frames: vec![[0.5, -0.5]; 8],
            }))));
        }
        if demand.video && !self.video {
            self.video = true;
            return Ok(Poll::Ready(Some(DecodedPacket::Video(VideoFrame {
                storage: None,
                pts: 0.,
                duration: 2.,
                width: 2,
                height: 2,
                layout: crate::video::Yuv420Layout::Planar,
                row_strides: [2, 1, 1],
                planes: [vec![0; 4], vec![128], vec![128]],
                color: crate::video::YuvColor {
                    matrix: crate::video::YuvMatrix::Bt709,
                    full_range: false,
                    transfer: crate::video::YuvTransfer::Bt709,
                },
            }))));
        }
        Ok(Poll::Pending)
    }
}
fn wait(mut ready: impl FnMut() -> bool) {
    let limit = Instant::now() + Duration::from_secs(3);
    while !ready() {
        assert!(Instant::now() < limit);
        thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn metadata_does_not_admit_empty_ring_and_first_pcm_wakes_control_then_paused_worker_parks() {
    let release = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut movie = VideoPlayback::start_with_decoder(
        Box::new(Cursor::new(Vec::<u8>::new())),
        || {},
        VideoOptions {
            paused: true,
            queued_audio_frames: 2,
            queued_video_frames: 1,
            ..Default::default()
        },
        BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 32,
                max_bytes: 32768,
            },
        ),
        WakeSignal::new(|| {}),
        Arc::new(Factory {
            release: release.clone(),
            calls: calls.clone(),
            empty: false,
        }),
    )
    .unwrap();
    wait(|| movie.info().is_some());
    assert!(movie.take_audio().is_none());
    movie.shared.wake.acknowledge();
    release.store(true, Ordering::Release);
    wait(|| movie.shared.transport.audio_ready.load(Ordering::Acquire));
    assert!(
        movie.shared.wake.is_pending(),
        "first PCM must wake admission without a callback"
    );
    let data = movie.take_audio().unwrap();
    assert!(movie.audio_pending());
    wait(|| movie.shared.state.lock().unwrap().frames.len() == 1);
    thread::sleep(Duration::from_millis(10));
    let before = calls.load(Ordering::Acquire);
    thread::sleep(Duration::from_millis(40));
    assert_eq!(
        calls.load(Ordering::Acquire),
        before,
        "paused full queues polled the decoder"
    );
    movie.pause(false);
    wait(|| calls.load(Ordering::Acquire) > before);
    movie.cancel();
    assert!(!movie.audio_pending());
    drop(data);
    wait(|| movie.worker_finished());
}
#[test]
fn empty_declared_audio_track_can_be_admitted_and_drained() {
    let mut movie = VideoPlayback::start_with_decoder(
        Box::new(Cursor::new(Vec::<u8>::new())),
        || {},
        VideoOptions::default(),
        BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 32,
                max_bytes: 1 << 20,
            },
        ),
        WakeSignal::new(|| {}),
        Arc::new(Factory {
            release: Arc::new(AtomicBool::new(true)),
            calls: Arc::new(AtomicUsize::new(0)),
            empty: true,
        }),
    )
    .unwrap();
    let mut data = None;
    wait(|| {
        data = movie.take_audio();
        data.is_some()
    });
    let (mut sound, _) = data.unwrap().into_sound().unwrap();
    let info = reel_audio::info::MockInfoBuilder::new().build();
    sound.process(&mut [Frame::ZERO; 4], 0.25, &info);
    assert!(sound.finished());
    assert!(!movie.audio_pending());
    movie.cancel();
    wait(|| movie.worker_finished());
}
#[test]
fn full_unadmitted_audio_window_waits_for_its_control_side_admission() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut movie = VideoPlayback::start_with_decoder(
        Box::new(Cursor::new(Vec::<u8>::new())),
        || {},
        VideoOptions {
            queued_audio_frames: 2,
            queued_video_frames: 1,
            ..Default::default()
        },
        BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 32,
                max_bytes: 32768,
            },
        ),
        WakeSignal::new(|| {}),
        Arc::new(Factory {
            release: Arc::new(AtomicBool::new(true)),
            calls: calls.clone(),
            empty: false,
        }),
    )
    .unwrap();
    wait(|| {
        movie.shared.transport.audio_ready.load(Ordering::Acquire)
            && movie.shared.state.lock().unwrap().frames.len() == 1
    });
    thread::sleep(Duration::from_millis(10));
    let before = calls.load(Ordering::Acquire);
    thread::sleep(Duration::from_millis(40));
    assert_eq!(
        calls.load(Ordering::Acquire),
        before,
        "unadmitted PCM window polled consumption"
    );
    let data = movie.take_audio().unwrap();
    wait(|| calls.load(Ordering::Acquire) > before);
    movie.cancel();
    drop(data);
    wait(|| movie.worker_finished());
}
