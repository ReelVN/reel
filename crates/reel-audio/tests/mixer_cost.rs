//! Explicit full-mixer CPU measurement. No native audio device is opened.
use reel_audio::{
	AudioManager, AudioManagerSettings, Frame, Panning, Tween,
	backend::{Backend, Renderer},
	sound::static_sound::{StaticSoundData, StaticSoundSettings},
	track::{MainTrackBuilder, TrackBuilder},
};
use std::{
	convert::Infallible,
	hint::black_box,
	time::{Duration, Instant},
};

struct Host(Option<Renderer>);
impl Backend for Host {
	type Settings = ();
	type Error = Infallible;
	fn setup(_: (), _: usize) -> Result<(Self, u32), Infallible> {
		Ok((Self(None), 48_000))
	}
	fn start(&mut self, renderer: Renderer) -> Result<(), Infallible> {
		self.0 = Some(renderer);
		Ok(())
	}
}

#[test]
#[ignore = "explicit release CPU measurement; run alone with --nocapture"]
fn mixer_fixed_and_changing_parameter_cost() {
	const BLOCKS: usize = 4_000;
	const FRAMES: usize = 512;
	let pcm = (0..2_048)
		.map(|index| {
			let sample = (index as f32 * 0.013).sin() * 0.025;
			Frame::new(sample, sample * -0.75)
		})
		.collect::<Vec<_>>();
	let data = StaticSoundData {
		sample_rate: 48_000,
		frames: pcm.into(),
		settings: StaticSoundSettings::new().loop_region(..),
		slice: None,
	};
	for voices in [1, 8, 24] {
		for changing in [false, true] {
			let mut manager = AudioManager::<Host>::new(AudioManagerSettings {
				main_track_builder: MainTrackBuilder::new().volume(-9.),
				internal_buffer_size: 128,
				backend_settings: (),
				..Default::default()
			})
			.unwrap();
			let mut track = manager
				.add_sub_track(TrackBuilder::new().volume(-3.))
				.unwrap();
			let mut handles = (0..voices)
				.map(|index| {
					track
						.play(
							data.volume(-6. - index as f32 * 0.125)
								.panning(Panning(if index % 2 == 0 { -0.35 } else { 0.25 })),
						)
						.unwrap()
				})
				.collect::<Vec<_>>();
			let mut output = [0.; FRAMES * 2];
			let mut hash = 0xcbf29ce484222325_u64;
			let mut elapsed = Duration::ZERO;
			for block in 0..BLOCKS + 32 {
				if changing && block % 16 == 0 {
					let tween = Tween {
						duration: Duration::from_millis(100),
						..Default::default()
					};
					for (index, handle) in handles.iter_mut().enumerate() {
						handle.set_volume(
							-6. - index as f32 * 0.125 - (block % 32) as f32 * 0.25,
							tween,
						);
						handle.set_panning(
							Panning(if block % 32 == 0 { -0.35 } else { 0.25 }),
							tween,
						);
					}
				}
				let renderer = manager.backend_mut().0.as_mut().unwrap();
				let start = Instant::now();
				renderer.on_start_processing();
				renderer.process(black_box(&mut output), 2);
				let cost = start.elapsed();
				if block < 32 {
					continue;
				}
				elapsed += cost;
				for sample in &output {
					assert!(sample.is_finite());
					hash = (hash ^ u64::from(sample.to_bits())).wrapping_mul(0x100000001b3);
				}
			}
			println!(
				"COST voices={voices} changing={changing} frames={} elapsed_ns={} hash={hash:016x}",
				BLOCKS * FRAMES,
				elapsed.as_nanos()
			);
		}
	}
}
