use std::sync::{
	Arc,
	atomic::{AtomicU32, Ordering},
};

use reel_audio::{
	AudioManager, AudioManagerSettings, Frame,
	backend::mock::{MockBackend, MockBackendSettings},
	effect::{Effect, EffectBuilder},
	info::Info,
	track::TrackBuilder,
};
use rtrb::{Consumer, Producer, RingBuffer};

struct TestEffect {
	sample_rate: Arc<AtomicU32>,
	dt_producer: Producer<f64>,
}

impl Effect for TestEffect {
	fn init(&mut self, sample_rate: u32, _internal_buffer_size: usize) {
		self.sample_rate.store(sample_rate, Ordering::SeqCst);
	}

	fn on_change_sample_rate(&mut self, sample_rate: u32) {
		self.sample_rate.store(sample_rate, Ordering::SeqCst);
	}

	fn process(&mut self, _input: &mut [Frame], dt: f64, _info: &Info) {
		self.dt_producer.push(dt).unwrap();
	}
}

struct TestEffectHandle {
	sample_rate: Arc<AtomicU32>,
	dt_consumer: Consumer<f64>,
}

struct TestEffectBuilder;

impl EffectBuilder for TestEffectBuilder {
	type Handle = TestEffectHandle;

	fn build(self) -> (Box<dyn Effect>, Self::Handle) {
		let (dt_producer, dt_consumer) = RingBuffer::new(100);
		let sample_rate = Arc::new(AtomicU32::new(0));
		(
			Box::new(TestEffect {
				sample_rate: sample_rate.clone(),
				dt_producer,
			}),
			TestEffectHandle {
				sample_rate,
				dt_consumer,
			},
		)
	}
}

#[test]
fn change_sample_rate() {
	let mut manager = AudioManager::<MockBackend>::new(AudioManagerSettings {
		backend_settings: MockBackendSettings { sample_rate: 100 },
		..Default::default()
	})
	.unwrap();
	let mut effect_handle;
	let _track = manager
		.add_sub_track({
			let mut builder = TrackBuilder::new();
			effect_handle = builder.add_effect(TestEffectBuilder);
			builder
		})
		.unwrap();
	let backend = manager.backend_mut();
	backend.on_start_processing();
	assert_eq!(effect_handle.sample_rate.load(Ordering::SeqCst), 100);
	backend.process();
	assert_eq!(effect_handle.dt_consumer.pop(), Ok(1.0 / 100.0));
	backend.set_sample_rate(200);
	assert_eq!(effect_handle.sample_rate.load(Ordering::SeqCst), 200);
	backend.process();
	assert_eq!(effect_handle.dt_consumer.pop(), Ok(1.0 / 200.0));
}

mod sync_send {
	use reel_audio::{
		AudioManager,
		backend::mock::MockBackend,
		clock::ClockHandle,
		effect::{
			compressor::CompressorHandle, delay::DelayHandle, distortion::DistortionHandle,
			eq_filter::EqFilterHandle, filter::FilterHandle, panning_control::PanningControlHandle,
			reverb::ReverbHandle, volume_control::VolumeControlHandle,
		},
		listener::ListenerHandle,
		modulator::{lfo::LfoHandle, tweener::TweenerHandle},
		sound::{static_sound::StaticSoundHandle, streaming::StreamingSoundHandle},
		track::{MainTrackHandle, SendTrackHandle, SpatialTrackHandle, TrackHandle},
	};

	#[test]
	fn handles_are_send_and_sync() {
		sync_send::<AudioManager<MockBackend>>();
		sync_send::<LfoHandle>();
		sync_send::<ClockHandle>();
		sync_send::<DelayHandle>();
		sync_send::<TrackHandle>();
		sync_send::<FilterHandle>();
		sync_send::<ReverbHandle>();
		sync_send::<TweenerHandle>();
		sync_send::<EqFilterHandle>();
		sync_send::<ListenerHandle>();
		sync_send::<MainTrackHandle>();
		sync_send::<SendTrackHandle>();
		sync_send::<CompressorHandle>();
		sync_send::<DistortionHandle>();
		sync_send::<StaticSoundHandle>();
		sync_send::<SpatialTrackHandle>();
		sync_send::<VolumeControlHandle>();
		sync_send::<PanningControlHandle>();
		sync_send::<StreamingSoundHandle<std::io::Error>>();
	}

	fn sync_send<T: Sync + Send>() {}
}

mod streaming_sound_stops_on_error {
	use std::time::Duration;

	use reel_audio::{
		AudioManager, AudioManagerSettings, Frame,
		backend::mock::MockBackend,
		sound::{
			PlaybackState,
			streaming::{Decoder, StreamingSoundData},
		},
	};

	struct MockDecoder;

	#[derive(Debug, PartialEq, Eq)]
	struct MockDecoderError;

	impl Decoder for MockDecoder {
		type Error = MockDecoderError;

		fn sample_rate(&self) -> u32 {
			1
		}

		fn num_frames(&self) -> usize {
			1
		}

		fn decode(&mut self) -> Result<Vec<Frame>, Self::Error> {
			Err(MockDecoderError)
		}

		fn seek(&mut self, _index: usize) -> Result<usize, Self::Error> {
			Ok(0)
		}
	}

	#[test]
	fn streaming_sound_stops_on_error() {
		let mut manager =
			AudioManager::<MockBackend>::new(AudioManagerSettings::default()).unwrap();
		let data = StreamingSoundData::from_decoder(MockDecoder);
		let mut sound = manager.play(data).unwrap();
		manager.backend_mut().on_start_processing();
		std::thread::sleep(Duration::from_secs(1));
		manager.backend_mut().process();
		manager.backend_mut().on_start_processing();
		assert_eq!(sound.state(), PlaybackState::Stopped);
		assert_eq!(sound.pop_error(), Some(MockDecoderError));
		assert_eq!(manager.main_track().num_sounds(), 0);
	}
}
