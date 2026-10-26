use super::*;
use crate::PlaybackRate;

fn advance(sound: &mut dyn Sound, frames: usize) -> f32 {
	let info = MockInfoBuilder::new().build();
	let mut peak = 0_f32;
	for _ in 0..frames {
		sound.on_start_processing();
		peak = peak.max(sound.process_one(0.001, &info).left.abs());
	}
	sound.on_start_processing();
	peak
}

#[test]
fn rate_consumes_source_time_and_retains_audible_pcm() {
	for rate in [0.25, 0.5, 1., 2., 4.] {
		let data = queue_data(1000, &vec![0.5; 10_000]).playback_rate(rate);
		let (mut sound, handle) = data.split();
		assert!(advance(&mut sound, 1000) > 0.1);
		let state = handle.playback();
		assert!(
			(state.position - rate).abs() <= 0.002,
			"rate={rate} position={}",
			state.position
		);
		assert_eq!(state.rate, PlaybackRate(rate));
		assert_eq!(state.rate_remaining, Duration::ZERO);
	}
}

#[test]
fn rate_ramp_snapshot_tracks_pending_commands_pause_and_resume() {
	let (mut sound, mut handle) = queue_data(1000, &vec![0.5; 10_000]).split();
	handle.set_playback_rate(
		2.,
		Tween {
			duration: Duration::from_secs(1),
			..Default::default()
		},
	);
	let pending = handle.playback();
	assert_eq!(
		(
			pending.rate.0,
			pending.rate_target.0,
			pending.rate_remaining
		),
		(1., 2., Duration::from_secs(1))
	);
	advance(&mut sound, 400);
	let state = handle.playback();
	assert!((state.rate.0 - 1.4).abs() < 1e-9);
	assert!((state.rate_remaining.as_secs_f64() - 0.6).abs() < 1e-9);
	handle.pause(Tween {
		duration: Duration::ZERO,
		..Default::default()
	});
	assert_eq!(advance(&mut sound, 500), 0.);
	let paused = handle.playback();
	assert_eq!(
		(paused.position, paused.rate, paused.rate_remaining),
		(state.position, state.rate, state.rate_remaining)
	);
	handle.set_playback_rate(3., Tween::default());
	handle.set_playback_rate(
		0.5,
		Tween {
			duration: Duration::ZERO,
			..Default::default()
		},
	);
	assert_eq!(
		handle.playback().rate.0,
		0.5,
		"latest immediate command is observable before PCM"
	);
	advance(&mut sound, 1);
	assert_eq!(handle.playback().rate.0, 0.5);
	handle.set_playback_rate(
		1.5,
		Tween {
			duration: Duration::from_millis(500),
			..Default::default()
		},
	);
	handle.resume(Tween {
		duration: Duration::ZERO,
		..Default::default()
	});
	advance(&mut sound, 500);
	assert_eq!(handle.playback().rate.0, 1.5);
	assert_eq!(handle.playback().rate_remaining, Duration::ZERO);
}

#[test]
fn held_queue_freezes_rate_ramp_and_segments_share_the_transport_rate() {
	use crate::sound::static_sound::QueuedStaticSound;
	let (mut sound, mut handle, mut queue) = queue_data(1000, &vec![0.5; 200])
		.playback_rate(2.)
		.into_sound_with_queue(2, Arc::new(()));
	queue.announce(1).unwrap();
	assert!(
		queue
			.push(QueuedStaticSound {
				index: 1,
				data: queue_data(1000, &vec![0.5; 1000]),
				retention: None
			})
			.is_ok()
	);
	handle.set_playback_rate(
		3.,
		Tween {
			duration: Duration::from_secs(1),
			..Default::default()
		},
	);
	assert_eq!(advance(sound.as_mut(), 100), 0.);
	let held = handle.playback();
	assert_eq!(
		(held.position, held.rate.0, held.rate_remaining),
		(0., 2., Duration::from_secs(1))
	);
	queue.start();
	advance(sound.as_mut(), 200);
	let state = handle.playback();
	assert_eq!(state.item, 1);
	assert!((state.rate.0 - 2.2).abs() < 1e-9);
	assert!((state.position - 0.22).abs() < 0.003, "{state:?}");
}
