use std::{sync::Arc, time::Duration};

use crate::{
	Decibels, Panning, StartTime, Tween,
	clock::ClockTime,
	frame::Frame,
	info::MockInfoBuilder,
	sound::{
		PlaybackState, Sound,
		static_sound::{StaticSoundData, StaticSoundSettings},
	},
	test_helpers::expect_frame_soon,
};

fn queue_data(rate: u32, samples: &[f32]) -> StaticSoundData {
	StaticSoundData {
		sample_rate: rate,
		frames: samples
			.iter()
			.map(|sample| Frame::from_mono(*sample))
			.collect(),
		settings: StaticSoundSettings::new(),
		slice: None,
	}
}

#[test]
fn prepared_queue_has_no_silent_frame_at_segment_boundaries() {
	use crate::sound::static_sound::QueuedStaticSound;
	let (mut sound, handle, mut queue) =
		queue_data(4, &[1., 2.]).into_sound_with_queue(4, Arc::new(()));
	queue.announce(2).unwrap();
	for (index, samples) in [(1, &[3., 4.][..]), (2, &[5., 6.][..])] {
		assert!(
			queue
				.push(QueuedStaticSound {
					index,
					data: queue_data(4, samples),
					retention: None
				})
				.is_ok()
		);
	}
	let info = MockInfoBuilder::new().build();
	assert_eq!(sound.process_one(0.25, &info), Frame::ZERO);
	queue.start();
	for sample in 1..=6 {
		sound.on_start_processing();
		assert_eq!(
			sound.process_one(0.25, &info),
			Frame::from_mono(sample as f32).panned(Panning::CENTER)
		);
	}
	sound.on_start_processing();
	assert_eq!(handle.queue_cursor().0, 2);
	for _ in 0..8 {
		sound.process_one(0.25, &info);
	}
	assert!(sound.finished());
	assert_eq!(queue.take_retired().unwrap().index, 0);
	assert_eq!(queue.take_retired().unwrap().index, 1);
}

#[test]
fn announced_late_queue_holds_position_and_resumes() {
	use crate::sound::static_sound::QueuedStaticSound;
	let (mut sound, handle, mut queue) =
		queue_data(4, &[1., 2., 3.]).into_sound_with_queue(2, Arc::new(()));
	queue.announce(1).unwrap();
	queue.start();
	let info = MockInfoBuilder::new().build();
	for _ in 0..12 {
		sound.process_one(0.25, &info);
	}
	sound.on_start_processing();
	let waiting = handle.queue_cursor();
	assert_eq!(waiting, (0, 0.75));
	for _ in 0..12 {
		assert_eq!(sound.process_one(0.25, &info), Frame::ZERO);
	}
	sound.on_start_processing();
	assert_eq!(handle.queue_cursor(), waiting);
	assert!(!sound.finished());
	assert!(
		queue
			.push(QueuedStaticSound {
				index: 1,
				data: queue_data(4, &[4., 5., 6.]),
				retention: None
			})
			.is_ok()
	);
	for sample in 4..=6 {
		assert_eq!(
			sound.process_one(0.25, &info),
			Frame::from_mono(sample as f32).panned(Panning::CENTER)
		);
	}
}

#[test]
fn queued_loop_exits_at_current_pass_end() {
	use crate::sound::static_sound::QueuedStaticSound;
	let data = queue_data(4, &[1., 2., 3., 4.]).loop_region(0.25..0.75);
	let (mut sound, _, mut queue) = data.into_sound_with_queue(2, Arc::new(()));
	queue.start();
	let info = MockInfoBuilder::new().build();
	for sample in [1., 2., 3., 2., 3.] {
		assert_eq!(
			sound.process_one(0.25, &info),
			Frame::from_mono(sample).panned(Panning::CENTER)
		);
	}
	queue.announce(1).unwrap();
	assert!(
		queue
			.push(QueuedStaticSound {
				index: 1,
				data: queue_data(4, &[5., 6., 7.]),
				retention: None
			})
			.is_ok()
	);
	let mut output = Vec::new();
	for _ in 0..8 {
		output.push(sound.process_one(0.25, &info).left);
	}
	let expected =
		[2., 3., 5., 6., 7.].map(|sample| Frame::from_mono(sample).panned(Panning::CENTER).left);
	assert_eq!(&output[..5], &expected);
}

#[test]
fn queue_seek_targets_current_source_even_when_successor_is_prefetched() {
	use crate::sound::static_sound::QueuedStaticSound;
	let (mut sound, mut handle, mut queue) =
		queue_data(4, &[1., 2., 3., 4.]).into_sound_with_queue(3, Arc::new(()));
	queue.announce(1).unwrap();
	assert!(
		queue
			.push(QueuedStaticSound {
				index: 1,
				data: queue_data(8, &[5., 6., 7., 8.]),
				retention: None
			})
			.is_ok()
	);
	queue.start();
	let info = MockInfoBuilder::new().build();
	for _ in 0..2 {
		sound.process_one(0.25, &info);
	}
	sound.on_start_processing();
	assert_eq!(handle.queue_cursor(), (0, 0.5));
	handle.seek_to(0.0);
	sound.on_start_processing();
	assert_eq!(handle.queue_cursor(), (0, 0.0));
	let output: Vec<_> = (0..6)
		.map(|_| sound.process_one(0.25, &info).left)
		.collect();
	let expected = [1., 2., 3., 4., 5., 7.]
		.map(|sample| Frame::from_mono(sample).panned(Panning::CENTER).left);
	assert_eq!(output, expected);
}

#[test]
fn queue_skips_exhausted_start_positions_without_inserting_silence() {
	use crate::sound::static_sound::QueuedStaticSound;
	for exhausted_first in [false, true] {
		let data = queue_data(4, &[1., 2.]).start_position(if exhausted_first { 0.5 } else { 0.0 });
		let (mut sound, _, mut queue) = data.into_sound_with_queue(8, Arc::new(()));
		queue.announce(7).unwrap();
		for index in 1..=7 {
			let data = if index == 7 {
				queue_data(4, &[3., 4.])
			} else {
				queue_data(4, &[9.]).start_position(0.25)
			};
			assert!(
				queue
					.push(QueuedStaticSound {
						index,
						data,
						retention: None
					})
					.is_ok()
			);
		}
		queue.start();
		let info = MockInfoBuilder::new().build();
		for sample in (if exhausted_first { 3 } else { 1 })..=4 {
			assert_eq!(
				sound.process_one(0.25, &info),
				Frame::from_mono(sample as f32).panned(Panning::CENTER)
			);
		}
	}
}

/// Tests that a `StaticSound` will play all of its samples before finishing.
#[test]
fn plays_all_samples() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: Arc::new([
			Frame::from_mono(1.0),
			Frame::from_mono(2.0),
			Frame::from_mono(3.0),
		]),
		settings: StaticSoundSettings::new(),
		slice: None,
	};
	let (mut sound, _) = data.split();

	assert!(!sound.finished());

	for i in 1..=3 {
		assert_eq!(
			sound.process_one(1.0, &MockInfoBuilder::new().build()),
			Frame::from_mono(i as f32).panned(Panning::CENTER)
		);
		assert!(!sound.finished());
	}

	// give some time for the resample buffer to empty. in the meantime we should
	// get silent output.
	for _ in 0..10 {
		assert_eq!(
			sound.process_one(1.0, &MockInfoBuilder::new().build()),
			Frame::from_mono(0.0).panned(Panning::CENTER)
		);
	}

	// the sound should be finished and stopped by now
	assert!(sound.finished());
	assert_eq!(
		sound.playback_state_manager.playback_state(),
		PlaybackState::Stopped
	);
}

/// Tests that a `StaticSound` correctly reports its playback state
/// to be queried by StaticSoundHandle::state.
#[test]
fn reports_playback_state() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: Arc::new([Frame::from_mono(0.0); 10]),
		settings: StaticSoundSettings::new(),
		slice: None,
	};
	let (mut sound, handle) = data.split();

	for _ in 0..20 {
		assert_eq!(
			handle.state(),
			sound.playback_state_manager.playback_state()
		);
		sound.process_one(1.0, &MockInfoBuilder::new().build());
	}
}

/// Tests that a `StaticSound` correctly reports its playback state
/// to be queried by StaticSoundHandle::state.
#[test]
#[allow(clippy::float_cmp)]
fn reports_playback_position() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: Arc::new([Frame::from_mono(0.0); 10]),
		settings: StaticSoundSettings::new(),
		slice: None,
	};
	let (mut sound, handle) = data.split();

	for i in 0..20 {
		assert_eq!(
			handle.position(),
			i.clamp(0, 10) as f64 / sound.sample_rate as f64
		);
		sound.process_one(1.0, &MockInfoBuilder::new().build());
		sound.on_start_processing();
	}
}

/// Tests that a `StaticSound` fades out fully before pausing
/// and fades back in when resuming.
#[test]
#[allow(clippy::float_cmp)]
fn pauses_and_resumes_with_fades() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: Arc::new([Frame::from_mono(1.0); 100]),
		settings: StaticSoundSettings::new(),
		slice: None,
	};
	let (mut sound, mut handle) = data.split();

	sound.process_one(1.0, &MockInfoBuilder::new().build());
	assert_eq!(
		sound.playback_state_manager.playback_state(),
		PlaybackState::Playing
	);

	handle.pause(Tween {
		duration: Duration::from_secs(4),
		..Default::default()
	});
	sound.on_start_processing();
	assert_eq!(
		sound.playback_state_manager.playback_state(),
		PlaybackState::Pausing
	);

	// allow for a few samples of delay because of the resampling, but the
	// sound should fade out soon.
	expect_frame_soon(
		Frame::from_mono(Decibels(-15.0).as_amplitude()).panned(Panning::CENTER),
		&mut sound,
	);
	assert_eq!(
		sound.process_one(1.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(Decibels(-30.0).as_amplitude()).panned(Panning::CENTER)
	);
	assert_eq!(
		sound.process_one(1.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(Decibels(-45.0).as_amplitude()).panned(Panning::CENTER)
	);

	sound.on_start_processing();
	let position = handle.position();
	for _ in 0..10 {
		assert_eq!(
			sound.process_one(1.0, &MockInfoBuilder::new().build()),
			Frame::from_mono(0.0).panned(Panning::CENTER)
		);
		sound.on_start_processing();
		assert_eq!(handle.position(), position);
		assert_eq!(
			sound.playback_state_manager.playback_state(),
			PlaybackState::Paused
		);
	}

	handle.resume(Tween {
		duration: Duration::from_secs(4),
		..Default::default()
	});
	sound.on_start_processing();

	// allow for a few samples of delay because of the resampling, but the
	// sound should fade back in soon.
	expect_frame_soon(
		Frame::from_mono(Decibels(-45.0).as_amplitude()).panned(Panning::CENTER),
		&mut sound,
	);
	assert_eq!(
		sound.playback_state_manager.playback_state(),
		PlaybackState::Resuming
	);
	assert_eq!(
		sound.process_one(1.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(Decibels(-30.0).as_amplitude()).panned(Panning::CENTER)
	);
	assert_eq!(
		sound.playback_state_manager.playback_state(),
		PlaybackState::Resuming
	);
	assert_eq!(
		sound.process_one(1.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(Decibels(-15.0).as_amplitude()).panned(Panning::CENTER)
	);
	assert_eq!(
		sound.playback_state_manager.playback_state(),
		PlaybackState::Resuming
	);

	for _ in 0..3 {
		assert_eq!(
			sound.process_one(1.0, &MockInfoBuilder::new().build()),
			Frame::from_mono(1.0).panned(Panning::CENTER)
		);
		assert_eq!(
			sound.playback_state_manager.playback_state(),
			PlaybackState::Playing
		);
	}

	let mut frames = vec![Frame::ZERO; 3];
	sound.process(&mut frames, 1.0, &MockInfoBuilder::new().build());
	assert_eq!(
		frames,
		vec![Frame::from_mono(1.0).panned(Panning::CENTER); 3]
	);
}

/// Tests that a `StaticSound` stops and finishes after a fade-out.
#[test]
#[allow(clippy::float_cmp)]
fn stops_with_fade_out() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: Arc::new([Frame::from_mono(1.0); 100]),
		settings: StaticSoundSettings::new(),
		slice: None,
	};
	let (mut sound, mut handle) = data.split();

	sound.process_one(1.0, &MockInfoBuilder::new().build());
	assert_eq!(
		sound.playback_state_manager.playback_state(),
		PlaybackState::Playing
	);

	handle.stop(Tween {
		duration: Duration::from_secs(4),
		..Default::default()
	});
	sound.on_start_processing();
	assert_eq!(
		sound.playback_state_manager.playback_state(),
		PlaybackState::Stopping
	);

	// allow for a few samples of delay because of the resampling, but the
	// sound should fade out soon.
	expect_frame_soon(
		Frame::from_mono(Decibels(-15.0).as_amplitude()).panned(Panning::CENTER),
		&mut sound,
	);
	assert_eq!(
		sound.process_one(1.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(Decibels(-30.0).as_amplitude()).panned(Panning::CENTER)
	);
	assert_eq!(
		sound.process_one(1.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(Decibels(-45.0).as_amplitude()).panned(Panning::CENTER)
	);

	sound.on_start_processing();
	let position = handle.position();
	for _ in 0..3 {
		assert_eq!(
			sound.process_one(1.0, &MockInfoBuilder::new().build()),
			Frame::from_mono(0.0).panned(Panning::CENTER)
		);
		sound.on_start_processing();
		assert_eq!(handle.position(), position);
		assert_eq!(
			sound.playback_state_manager.playback_state(),
			PlaybackState::Stopped
		);
		assert!(sound.finished());
	}
}

/// Tests that a `StaticSound` will wait for its start clock time
/// when appropriate.
#[test]
#[allow(clippy::float_cmp)]
fn waits_for_start_time() {
	let mut info_builder = MockInfoBuilder::new();
	let clock_id_1 = info_builder.add_clock(true, 0, 0.0);
	let info = info_builder.build();

	let data = StaticSoundData {
		sample_rate: 1,
		frames: (1..100).map(|i| Frame::from_mono(i as f32)).collect(),
		settings: StaticSoundSettings::new().start_time(ClockTime {
			clock: clock_id_1,
			ticks: 2,
			fraction: 0.0,
		}),
		slice: None,
	};
	let (mut sound, _) = data.split();

	// the sound should not be playing yet
	for _ in 0..3 {
		assert_eq!(sound.process_one(1.0, &info), Frame::from_mono(0.0));
	}

	let info = {
		let mut builder = MockInfoBuilder::new();
		builder.add_clock(true, 1, 0.0);
		builder.add_clock(true, 0, 0.0);
		builder.build()
	};

	// the sound is set to start at tick 2, so it should not
	// play yet
	for _ in 0..3 {
		assert_eq!(sound.process_one(1.0, &info), Frame::from_mono(0.0));
	}

	let info = {
		let mut builder = MockInfoBuilder::new();
		builder.add_clock(true, 1, 0.0);
		builder.add_clock(true, 2, 0.0);
		builder.build()
	};

	// a different clock reached tick 2, so the sound should
	// not play yet
	for _ in 0..3 {
		assert_eq!(sound.process_one(1.0, &info), Frame::from_mono(0.0));
	}

	let info = {
		let mut builder = MockInfoBuilder::new();
		builder.add_clock(true, 2, 0.0);
		builder.add_clock(true, 2, 0.0);
		builder.build()
	};

	// the sound should start playing now
	for i in 1..10 {
		assert_eq!(
			sound.process_one(1.0, &info),
			Frame::from_mono(i as f32).panned(Panning::CENTER)
		);
	}
}

/// Tests that a `StaticSound` will stop (allowing it to be removed)
/// if it's waiting on a clock that no longer exists before it can
/// start.
///
/// Without this behavior, a sound in this situation would be in limbo
/// forever with no way to free up its resources.
#[test]
fn stops_if_waiting_on_missing_clock() {
	let mut info_builder = MockInfoBuilder::new();
	let clock_id = info_builder.add_clock(true, 0, 0.0);
	let info = info_builder.build();

	let data = StaticSoundData {
		sample_rate: 1,
		frames: (1..100).map(|i| Frame::from_mono(i as f32)).collect(),
		settings: StaticSoundSettings::new().start_time(ClockTime {
			clock: clock_id,
			ticks: 2,
			fraction: 0.0,
		}),
		slice: None,
	};
	let (mut sound, handle) = data.split();

	sound.process_one(1.0, &info);
	sound.on_start_processing();
	assert_eq!(handle.state(), PlaybackState::Playing);

	// the clock is removed
	let info = MockInfoBuilder::new().build();

	sound.process_one(1.0, &info);
	sound.on_start_processing();
	assert_eq!(handle.state(), PlaybackState::Stopped);
	assert!(sound.finished());
}

/// Tests that a `StaticSound` that had its start time set to a clock time and already
/// started will not stop if the clock stops.
#[test]
fn continues_when_clock_stops() {
	let mut info_builder = MockInfoBuilder::new();
	let clock_id = info_builder.add_clock(true, 0, 0.0);
	let info = info_builder.build();

	let data = StaticSoundData {
		sample_rate: 1,
		frames: (1..100).map(|_| Frame::from_mono(1.0)).collect(),
		settings: StaticSoundSettings::new().start_time(ClockTime {
			clock: clock_id,
			ticks: 0,
			fraction: 0.0,
		}),
		slice: None,
	};
	let (mut sound, _) = data.split();

	assert_eq!(sound.process_one(1.0, &info), Frame::from_mono(1.0),);

	let info = {
		let mut builder = MockInfoBuilder::new();
		builder.add_clock(false, 0, 0.0);
		builder.build()
	};

	assert_eq!(sound.process_one(1.0, &info), Frame::from_mono(1.0),);
}

/// Tests that a `StaticSound` can be paused, resumed, and stopped immediately
/// even if it's waiting for its start time.
#[test]
fn immediate_playback_state_change_with_start_time() {
	let mut info_builder = MockInfoBuilder::new();
	let clock_id = info_builder.add_clock(true, 0, 0.0);
	let info = info_builder.build();

	let data = StaticSoundData {
		sample_rate: 1,
		frames: (1..100).map(|_| Frame::from_mono(1.0)).collect(),
		settings: StaticSoundSettings::new().start_time(ClockTime {
			clock: clock_id,
			ticks: 1,
			fraction: 0.0,
		}),
		slice: None,
	};
	let (mut sound, mut handle) = data.split();

	handle.pause(Tween {
		duration: Duration::ZERO,
		..Default::default()
	});
	sound.on_start_processing();
	sound.process_one(1.0, &info);
	sound.on_start_processing();
	assert_eq!(handle.state(), PlaybackState::Paused);

	handle.resume(Tween {
		duration: Duration::ZERO,
		..Default::default()
	});
	sound.on_start_processing();
	sound.process_one(1.0, &info);
	sound.on_start_processing();
	assert_eq!(handle.state(), PlaybackState::Playing);

	handle.stop(Tween {
		duration: Duration::ZERO,
		..Default::default()
	});
	sound.on_start_processing();
	sound.process_one(1.0, &info);
	sound.on_start_processing();
	assert_eq!(handle.state(), PlaybackState::Stopped);
}

/// Tests that a `StaticSound` can be set to resume at a certain start time.
#[test]
fn resume_at() {
	let mut info_builder = MockInfoBuilder::new();
	let clock_id = info_builder.add_clock(true, 0, 0.0);
	let info = info_builder.build();

	let data = StaticSoundData {
		sample_rate: 1,
		frames: (1..100).map(|_| Frame::from_mono(1.0)).collect(),
		settings: StaticSoundSettings::new(),
		slice: None,
	};
	let (mut sound, mut handle) = data.split();

	handle.pause(Tween {
		duration: Duration::ZERO,
		..Default::default()
	});
	sound.on_start_processing();
	sound.process_one(1.0, &info);
	sound.on_start_processing();
	assert_eq!(handle.state(), PlaybackState::Paused);

	handle.resume_at(
		StartTime::ClockTime(ClockTime {
			clock: clock_id,
			ticks: 1,
			fraction: 0.0,
		}),
		Tween {
			duration: Duration::from_secs(1),
			..Default::default()
		},
	);
	sound.on_start_processing();
	sound.process_one(1.0, &info);
	sound.on_start_processing();
	assert_eq!(handle.state(), PlaybackState::WaitingToResume);

	let info = {
		let mut builder = MockInfoBuilder::new();
		builder.add_clock(true, 1, 0.0);
		builder.build()
	};
	sound.process_one(1.0, &info);
	sound.on_start_processing();
	assert_eq!(handle.state(), PlaybackState::Resuming);
	sound.process_one(1.0, &info);
	sound.on_start_processing();
	assert_eq!(handle.state(), PlaybackState::Playing);
}

/// Tests that a `StaticSound` can be played partially.
#[test]
#[allow(clippy::float_cmp)]
fn start_position() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: (0..10).map(|i| Frame::from_mono(i as f32)).collect(),
		settings: StaticSoundSettings::new().start_position(3.0),
		slice: None,
	};
	let (mut sound, handle) = data.split();

	for i in 3..=6 {
		assert_eq!(handle.position(), i as f64);
		assert_eq!(
			sound.process_one(1.0, &MockInfoBuilder::new().build()),
			Frame::from_mono(i as f32).panned(Panning::CENTER)
		);
		sound.on_start_processing();
	}
}

/// Tests that starting a `StaticSound` past the end of the sound
/// will not cause a panic.
#[test]
#[allow(clippy::float_cmp)]
fn out_of_bounds_start_position() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: (0..10).map(|i| Frame::from_mono(i as f32)).collect(),
		settings: StaticSoundSettings::new().start_position(15.0),
		slice: None,
	};
	let (mut sound, _) = data.split();
	sound.process_one(1.0, &MockInfoBuilder::new().build());
}

/// Tests that a `StaticSound` properly obeys looping behavior when
/// playing forward.
#[test]
#[allow(clippy::float_cmp)]
fn loops_forward() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: (0..10).map(|i| Frame::from_mono(i as f32)).collect(),
		settings: StaticSoundSettings::new().loop_region(Some((3.0..6.0).into())),
		slice: None,
	};
	let (mut sound, _) = data.split();

	for i in 0..6 {
		assert_eq!(
			sound.process_one(1.0, &MockInfoBuilder::new().build()),
			Frame::from_mono(i as f32).panned(Panning::CENTER)
		);
	}

	assert_eq!(
		sound.process_one(2.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(3.0).panned(Panning::CENTER)
	);
	assert_eq!(
		sound.process_one(2.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(5.0).panned(Panning::CENTER)
	);
	assert_eq!(
		sound.process_one(2.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(4.0).panned(Panning::CENTER)
	);
	assert_eq!(
		sound.process_one(2.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(3.0).panned(Panning::CENTER)
	);
}

/// Tests that the volume of a `StaticSound` can be adjusted.
#[test]
#[allow(clippy::float_cmp)]
fn volume() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: Arc::new([Frame::from_mono(1.0); 10]),
		settings: StaticSoundSettings::new().volume(-6.0),
		slice: None,
	};
	let (mut sound, _) = data.split();

	assert_eq!(
		sound.process_one(1.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(Decibels(-6.0).as_amplitude()).panned(Panning::CENTER)
	);
}

/// Tests that the volume of a `StaticSound` can be changed
/// after the sound is started.
#[test]
fn set_volume() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: Arc::new([Frame::from_mono(1.0); 10]),
		settings: StaticSoundSettings::new(),
		slice: None,
	};
	let (mut sound, mut handle) = data.split();

	assert_eq!(
		sound.process_one(1.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(1.0).panned(Panning::CENTER)
	);
	handle.set_volume(
		-6.0,
		Tween {
			duration: Duration::ZERO,
			..Default::default()
		},
	);
	sound.on_start_processing();
	expect_frame_soon(
		Frame::from_mono(Decibels(-6.0).as_amplitude()).panned(Panning::CENTER),
		&mut sound,
	);
}

/// Tests that the panning of a `StaticSound` can be adjusted.
#[test]
#[allow(clippy::float_cmp)]
fn panning() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: Arc::new([Frame::from_mono(1.0); 10]),
		settings: StaticSoundSettings::new().panning(Panning::LEFT),
		slice: None,
	};
	let (mut sound, _) = data.split();

	assert_eq!(
		sound.process_one(1.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(1.0).panned(Panning::LEFT)
	);
}

/// Tests that the panning of a `StaticSound` can be changed
/// after the sound is started.
#[test]
fn set_panning() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: Arc::new([Frame::from_mono(1.0); 10]),
		settings: StaticSoundSettings::new(),
		slice: None,
	};
	let (mut sound, mut handle) = data.split();

	assert_eq!(
		sound.process_one(1.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(1.0).panned(Panning::CENTER)
	);
	handle.set_panning(
		Panning::LEFT,
		Tween {
			duration: Duration::ZERO,
			..Default::default()
		},
	);
	sound.on_start_processing();
	expect_frame_soon(Frame::from_mono(1.0).panned(Panning::LEFT), &mut sound);
}

/// Tests that the playback rate of a `StaticSound` can be adjusted.
#[test]
#[allow(clippy::float_cmp)]
fn playback_rate() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: (0..10).map(|i| Frame::from_mono(i as f32)).collect(),
		settings: StaticSoundSettings::new().playback_rate(2.0),
		slice: None,
	};
	let (mut sound, _) = data.split();

	assert_eq!(
		sound.process_one(1.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(0.0).panned(Panning::CENTER)
	);
	assert_eq!(
		sound.process_one(1.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(2.0).panned(Panning::CENTER)
	);
}

/// Tests that the playback rate of a `StaticSound` can be adjusted after
/// it's started.
#[test]
#[allow(clippy::float_cmp)]
fn set_playback_rate() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: (0..100).map(|i| Frame::from_mono(i as f32)).collect(),
		settings: StaticSoundSettings::new(),
		slice: None,
	};
	let (mut sound, mut handle) = data.split();

	assert_eq!(
		sound.process_one(1.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(0.0).panned(Panning::CENTER)
	);
	assert_eq!(
		sound.process_one(1.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(1.0).panned(Panning::CENTER)
	);

	handle.set_playback_rate(
		2.0,
		Tween {
			duration: Duration::ZERO,
			..Default::default()
		},
	);
	sound.on_start_processing();

	assert_eq!(
		sound.process_one(1.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(2.0).panned(Panning::CENTER)
	);
	assert_eq!(
		sound.process_one(1.0, &MockInfoBuilder::new().build()),
		Frame::from_mono(4.0).panned(Panning::CENTER)
	);
}

/// Tests that a `StaticSound` outputs interpolated samples when
/// its playback position is between samples.
#[test]
fn interpolates_samples() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: Arc::new([
			Frame::from_mono(0.0),
			Frame::from_mono(1.0),
			Frame::from_mono(1.0),
			Frame::from_mono(1.0),
			Frame::from_mono(1.0),
			Frame::from_mono(1.0),
			Frame::from_mono(-10.0),
		]),
		settings: Default::default(),
		slice: None,
	};
	let (mut sound, _) = data.split();

	assert_eq!(
		sound.process_one(0.5, &MockInfoBuilder::new().build()),
		Frame::from_mono(0.0).panned(Panning::CENTER)
	);
	// at sample 0.5, the output should be somewhere between 0 and 1.
	// i don't care what exactly, that's up the to the interpolation algorithm.
	let frame = sound.process_one(5.0, &MockInfoBuilder::new().build());
	assert!(frame.left > 0.0 && frame.left < 1.0);
	// at sample 5.5, the output should be between 1 and -10.
	let frame = sound.process_one(1.0, &MockInfoBuilder::new().build());
	assert!(frame.left < 0.0 && frame.left > -10.0);
}

/// Tests that a `StaticSound` outputs interpolated samples correctly
/// when looping.
#[test]
fn interpolates_samples_when_looping() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: Arc::new([Frame::from_mono(10.0), Frame::from_mono(9.0)]),
		settings: StaticSoundSettings::new().loop_region(Some((..).into())),
		slice: None,
	};
	let (mut sound, _) = data.split();
	sound.process_one(1.5, &MockInfoBuilder::new().build());
	// because we're looping back to the first sample, which is 10.0,
	// the interpolated sample should be be tween 9.0 and 10.0
	let frame = sound.process_one(1.0, &MockInfoBuilder::new().build());
	assert!(frame.left > 9.0 && frame.left < 10.0);
}

/// Tests that a `StaticSound` can seek to a position.
#[test]
fn seek_to() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: (0..100).map(|i| Frame::from_mono(i as f32)).collect(),
		settings: StaticSoundSettings::new(),
		slice: None,
	};
	let (mut sound, mut handle) = data.split();
	let info = MockInfoBuilder::new().build();
	sound.process_one(0.5, &info);
	handle.seek_to(15.0);
	assert_eq!(handle.position(), 15.0);
	// The output has consumed the old command, but has not applied it yet.
	let (sequence, position) = sound.command_readers.seek_to.read().unwrap();
	handle.seek_to(30.0);
	sound.apply_seek(sequence, position);
	assert_eq!(
		handle.position(),
		30.0,
		"old acknowledgement must not hide new seek"
	);
	sound.on_start_processing();
	assert_eq!(handle.position(), 30.0);
	assert_eq!(
		sound.process_one(1.0, &info),
		Frame::from_mono(30.0).panned(Panning::CENTER)
	);

	handle.pause(Tween {
		duration: Duration::ZERO,
		..Default::default()
	});
	handle.seek_to(45.0);
	sound.on_start_processing();
	assert_eq!(sound.process_one(1.0, &info), Frame::ZERO);
	assert_eq!(handle.position(), 45.0);
	assert_eq!(handle.state(), PlaybackState::Paused);
	handle.seek_to(50.0);
	sound.on_start_processing();
	assert_eq!(sound.process_one(1.0, &info), Frame::ZERO);
	handle.resume(Tween {
		duration: Duration::ZERO,
		..Default::default()
	});
	sound.on_start_processing();
	assert_eq!(
		sound.process_one(1.0, &info),
		Frame::from_mono(50.0).panned(Panning::CENTER)
	);

	handle.pause(Tween {
		duration: Duration::ZERO,
		..Default::default()
	});
	handle.seek_to(100.0);
	sound.on_start_processing();
	assert_eq!(sound.process_one(1.0, &info), Frame::ZERO);
	assert_eq!(handle.position(), 100.0);
	assert_eq!(handle.state(), PlaybackState::Paused);
	handle.seek_to(5.0);
	sound.on_start_processing();
	handle.resume(Tween {
		duration: Duration::ZERO,
		..Default::default()
	});
	sound.on_start_processing();
	assert_eq!(
		sound.process_one(1.0, &info),
		Frame::from_mono(5.0).panned(Panning::CENTER)
	);
	handle.seek_to(100.0);
	sound.on_start_processing();
	sound.process_one(1.0, &info);
	assert!(sound.finished());
	handle.seek_to(0.0);
	assert_eq!(handle.position(), 100.0);
	assert!(sound.finished());
}

/// Tests that a `StaticSound` can seek by an amount of time.
#[test]
fn seek_by() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: (0..100).map(|i| Frame::from_mono(i as f32)).collect(),
		settings: StaticSoundSettings::new().start_position(10.0),
		slice: None,
	};
	let (mut sound, mut handle) = data.split();
	handle.seek_by(5.0);
	sound.on_start_processing();
	// we wouldn't actually expect the position to be 10.0 seconds right at
	// this moment - the sound probably ran ahead a few samples to fill
	// the resample buffer. so let's just say it should reach 20.0 soon.
	expect_frame_soon(Frame::from_mono(20.0).panned(Panning::CENTER), &mut sound);
}

/// Tests that a `StaticSound` can play in reverse.
#[test]
fn reverse() {
	let data = StaticSoundData {
		sample_rate: 1,
		frames: (0..10).map(|i| Frame::from_mono(i as f32)).collect(),
		settings: StaticSoundSettings::new().reverse(true),
		slice: None,
	};
	let (mut sound, _) = data.split();

	for i in (4..=9).rev() {
		assert_eq!(
			sound.process_one(1.0, &MockInfoBuilder::new().build()),
			Frame::from_mono(i as f32).panned(Panning::CENTER)
		);
	}
}
