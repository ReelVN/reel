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

use crate::{
	Decibels, Panning, PlaybackRate, StartTime, command::ValueChangeCommand,
	command_writers_and_readers, tween::Tween,
};

use super::Region;

command_writers_and_readers! {
	set_volume: ValueChangeCommand<Decibels>,
	set_playback_rate: ValueChangeCommand<PlaybackRate>,
	set_panning: ValueChangeCommand<Panning>,
	set_loop_region: Option<Region>,
	pause: Tween,
	resume: (StartTime, Tween),
	stop: Tween,
	seek_by: f64,
	seek_to: (u64, f64),
}
