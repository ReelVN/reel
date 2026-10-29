use super::*;
use reel_media::video::{
    DecodeDemand, DecodedPacket, MediaRead, VideoDecoder, VideoDecoderFactory, VideoInfo,
};
use std::{
    sync::{
        Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::Poll,
};

#[derive(Debug, Default)]
struct Gate {
    opens: AtomicUsize,
    closing: AtomicBool,
    released: Mutex<bool>,
    wake: Condvar,
    seeks: Mutex<Vec<f64>>,
}
impl Gate {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.wake.notify_all();
    }
}
struct Release(Arc<Gate>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}
#[derive(Debug)]
struct Factory(Arc<Gate>);
impl VideoDecoderFactory for Factory {
    fn open(
        &self,
        _: Box<dyn MediaRead>,
        _: BudgetPool,
        _: Arc<AtomicBool>,
    ) -> Result<Box<dyn VideoDecoder>, String> {
        self.0.opens.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Decoder {
            gate: self.0.clone(),
            info: VideoInfo {
                width: 2,
                height: 2,
                duration: Some(1.),
                audio_sample_rate: None,
            },
        }))
    }
}
struct Decoder {
    gate: Arc<Gate>,
    info: VideoInfo,
}
impl VideoDecoder for Decoder {
    fn info(&self) -> &VideoInfo {
        &self.info
    }
    fn audio_finished(&self) -> bool {
        true
    }
    fn set_end(&mut self, seconds: f64) {
        self.info.duration = Some(self.info.duration.unwrap().min(seconds));
    }
    fn prepare_poster(&mut self, seconds: f64) -> Result<(), String> {
        self.seek(seconds)
    }
    fn poll_packet(&mut self, _: DecodeDemand) -> Result<Poll<Option<DecodedPacket>>, String> {
        Ok(Poll::Pending)
    }
    fn seek(&mut self, seconds: f64) -> Result<(), String> {
        self.gate.seeks.lock().unwrap().push(seconds);
        Ok(())
    }
}
impl Drop for Decoder {
    fn drop(&mut self) {
        self.gate.closing.store(true, Ordering::SeqCst);
        let guard = self.gate.released.lock().unwrap();
        drop(
            self.gate
                .wake
                .wait_while(guard, |released| !*released)
                .unwrap(),
        );
    }
}

#[test]
fn replacement_waits_for_native_close_and_pending_request_keeps_controls() {
    let gate = Arc::new(Gate::default());
    // Unblock worker cleanup even if an assertion fails.
    let _release = Release(gate.clone());
    let (mut player, _) = rig_at(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."),
        MediaOptions {
            video_decoder: Some(Arc::new(Factory(gate.clone()))),
            ..Default::default()
        },
    );
    let first = player.play_video(movie(false), false).unwrap();
    until(|| {
        player.maintain(Duration::ZERO);
        gate.opens.load(Ordering::SeqCst) == 1
    });
    player.command(first, MediaCommand::Stop).unwrap();
    assert_eq!(
        player.take_result(first).unwrap().result,
        MediaResult::Stopped
    );
    until(|| gate.closing.load(Ordering::SeqCst));
    let cancelled = player.play_video(movie(false), false).unwrap();
    for _ in 0..20 {
        player.maintain(Duration::ZERO);
    }
    assert_eq!(gate.opens.load(Ordering::SeqCst), 1);
    player.cancel(cancelled);
    let next = player.play_video(movie(false), true).unwrap();
    player
        .command(next, MediaCommand::Seek(Duration::from_millis(250)))
        .unwrap();
    for _ in 0..20 {
        player.maintain(Duration::ZERO);
    }
    assert_eq!(gate.opens.load(Ordering::SeqCst), 1);
    assert_eq!(player.position(next), Some(Duration::from_millis(250)));
    assert!(snapshot(&player, next).paused);
    gate.release();
    until(|| {
        player.maintain(Duration::ZERO);
        gate.seeks.lock().unwrap().as_slice() == [0.25]
    });
    assert_eq!(gate.opens.load(Ordering::SeqCst), 2);
    player.close();
    until(|| {
        player.maintain(Duration::ZERO);
        !player.cleanup_pending()
    });
}
