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
