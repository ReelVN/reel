#![cfg(feature = "video")]
use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    clock::MonoTime,
    wake::WakeSignal,
};
use reel_media::{video::*, *};
use std::{
    thread,
    time::{Duration, Instant},
};
fn pool() -> BudgetPool {
    BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 32,
            max_bytes: 1 << 20,
        },
    )
}
fn wait(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < deadline, "background progress timed out");
        thread::sleep(Duration::from_millis(1));
    }
}
fn source() -> Box<dyn MediaRead> {
    Box::new(std::io::Cursor::new(
        include_bytes!("fixtures/baseline-aac.mp4").as_slice(),
    ))
}
#[test]
fn real_movie_shares_mixer_clock_pause_completion_and_reclaims_pcm() {
    let wake = WakeSignal::new(|| {});
    let budget = pool();
    let mut output = OfflineOutput::new(
        AudioDeviceConfig {
            sample_rate_hz: 48000,
            sample_format: SampleFormat::F32,
            layout: ChannelLayout::Stereo,
        },
        wake.clone(),
    )
    .unwrap();
    let format = output.configure(AudioRequest::default()).unwrap();
    let reclaim = AudioReclaimService::new(ReclaimLimits::default(), wake.clone()).unwrap();
    let mut mixer = KiraMixer::new(&format, MixerLimits::default(), &reclaim).unwrap();
    output.start(mixer.take_fill().unwrap()).ok().unwrap();
    let mut movie = VideoPlayback::start(
        source(),
        || {},
        VideoOptions::default(),
        budget.clone(),
        wake,
    )
    .unwrap();
    let mut sound = None;
    wait(|| {
        sound = movie.take_audio();
        sound.is_some()
    });
    movie.pause(true);
    mixer
        .play_video_audio(AudioGroup::Music, sound.unwrap())
        .unwrap();
    let mut pcm = [0.0_f32; 512];
    for _ in 0..4 {
        output.render(
            &mut pcm,
            CallbackTiming {
                now: MonoTime(0),
                latency_frames: None,
                uncertainty_frames: None,
            },
        );
        movie.tick(Duration::from_millis(20));
    }
    assert_eq!(movie.position(), Duration::ZERO);
    assert!(pcm.iter().all(|&s| s == 0.));
    movie.pause(false);
    let mut energy = 0.;
    let mut images = 0;
    let mut previous = 0.;
    let deadline = Instant::now() + Duration::from_secs(5);
    while movie.terminal().is_none() {
        assert!(
            Instant::now() < deadline,
            "movie stalled at {:?}",
            movie.position()
        );
        output.render(
            &mut pcm,
            CallbackTiming {
                now: MonoTime(0),
                latency_frames: None,
                uncertainty_frames: None,
            },
        );
        energy += pcm.iter().map(|&x| f64::from(x * x)).sum::<f64>();
        movie.tick(Duration::from_secs_f64(256. / 48000.));
        if let Some(frame) = movie.take_frame() {
            assert!(frame.frame.pts >= previous);
            previous = frame.frame.pts;
            images += 1;
        }
        mixer.reclaim_unused(16);
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(movie.terminal(), Some(VideoTerminal::Completed));
    assert_eq!(movie.position(), Duration::from_secs(1));
    assert!(images >= 10);
    assert!(energy > 100.);
    wait(|| movie.worker_finished());
    // Complete the mixer's ownership transfer; teardown never runs on PCM.
    output.stop().unwrap();
    drop(output);
    drop(mixer);
    drop(movie);
    wait(|| budget.usage().bytes == 0);
    drop(reclaim);
}
#[test]
fn cancelling_a_backpressured_movie_releases_queues_without_waiting_on_control() {
    let budget = pool();
    let mut movie = VideoPlayback::start(
        source(),
        || {},
        VideoOptions {
            queued_video_frames: 1,
            queued_audio_frames: 32,
            ..Default::default()
        },
        budget.clone(),
        WakeSignal::new(|| {}),
    )
    .unwrap();
    wait(|| movie.info().is_some());
    movie.cancel();
    assert_eq!(movie.terminal(), Some(VideoTerminal::Cancelled));
    wait(|| movie.worker_finished());
    drop(movie);
    assert_eq!(budget.usage().bytes, 0);
}

#[test]
fn silent_movie_uses_session_time_and_stop_remains_terminal() {
    let budget = pool();
    let mut movie = VideoPlayback::start(
        Box::new(std::io::Cursor::new(
            include_bytes!("fixtures/baseline-srgb.mp4").as_slice(),
        )),
        || {},
        VideoOptions::default(),
        budget.clone(),
        WakeSignal::new(|| {}),
    )
    .unwrap();
    wait(|| movie.worker_finished());
    assert!(movie.take_audio().is_none());
    assert!(movie.take_frame().is_some());
    movie.pause(true);
    movie.tick(Duration::from_secs(2));
    assert_eq!(movie.position(), Duration::ZERO);
    movie.pause(false);
    movie.tick(Duration::from_millis(125));
    assert_eq!(movie.position(), Duration::from_millis(125));
    assert!(movie.take_frame().is_some());
    movie.stop();
    movie.tick(Duration::from_secs(2));
    movie.cancel();
    assert_eq!(movie.terminal(), Some(VideoTerminal::Stopped));
    drop(movie);
    assert_eq!(budget.usage().bytes, 0);
}
