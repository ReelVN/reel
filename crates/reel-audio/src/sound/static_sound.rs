/*!
Playable chunks of audio that are loaded into memory all at once.

To play a static sound, pass a [`StaticSoundData`] to
[`AudioManager::play`](crate::AudioManager::play).

```no_run
# #[cfg(feature = "symphonia")]
# fn main() -> Result<(), Box<dyn std::error::Error>> {
use reel_audio::{
	AudioManager, AudioManagerSettings, DefaultBackend,
	sound::static_sound::{StaticSoundData, StaticSoundSettings},
};

let mut manager = AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())?;
let sound_data = StaticSoundData::from_file("sound.ogg")?;
manager.play(sound_data)?;
# Result::<(), Box<dyn std::error::Error>>::Ok(())
# }
# #[cfg(not(feature = "symphonia"))]
# fn main() {}
```

Compared to streaming sounds, static sounds have lower CPU usage and shorter delays
when starting and seeking, but they use a lot more memory.
*/

mod data;
mod frames;
mod handle;
mod queue;
mod settings;
mod sound;

pub use data::*;
pub use frames::{FrameSource, FrameStorage, StreamStatus};
pub use handle::*;
pub use queue::{QueueClosed, QueuedStaticSound, StaticSoundQueue};
pub use settings::*;

/// One coherent source cursor and fixed-rate transition observation.
#[derive(Clone, Copy, Debug)]
pub struct StaticSoundPlayback {
	/// Current queued source identity, or zero for the initial source.
	pub item: u64,
	/// Position in seconds within the current source.
	pub position: f64,
	/// Current content speed.
	pub rate: PlaybackRate,
	/// Fixed tween target; equals the current value for an idle/modulated rate.
	pub rate_target: PlaybackRate,
	/// Remaining active playback time for the fixed rate tween.
	pub rate_remaining: std::time::Duration,
}

use crate::{
	Decibels, Panning, PlaybackRate, StartTime, command::ValueChangeCommand,
	command_writers_and_readers, tween::Tween,
};

use super::Region;

// A coalesced ramp retains the last accepted immediate fixed value. Its
// sequence prevents a later ramp from resetting a baseline already applied by PCM.
#[derive(Clone, Copy, Debug)]
struct RateChangeCommand {
	sequence: u64,
	baseline: Option<(u64, PlaybackRate)>,
	change: ValueChangeCommand<PlaybackRate>,
}

impl RateChangeCommand {
	fn baseline_after(self, applied: u64) -> Option<PlaybackRate> {
		self.baseline.and_then(|(sequence, rate)| {
			// Serial-number order also handles wrapping; fewer than half the u64
			// sequence space can be outstanding between callback observations.
			let distance = sequence.wrapping_sub(applied);
			(distance != 0 && distance < (1 << 63)).then_some(rate)
		})
	}
}

command_writers_and_readers! {
	set_volume: ValueChangeCommand<Decibels>,
	set_playback_rate: RateChangeCommand,
	set_panning: ValueChangeCommand<Panning>,
	set_loop_region: Option<Region>,
	pause: Tween,
	resume: (StartTime, Tween),
	stop: Tween,
	seek_by: f64,
	seek_to: (u64, f64),
}
