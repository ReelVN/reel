// Reel modification (2026-10-08): bounded retirement/control reclamation.
// See PROVENANCE.md for the original source and maintained behavior.
use crate::{
	Decibels, PlaySoundError, Tween, Value,
	backend::resources::ResourceController,
	command::{CommandWriter, ValueChangeCommand},
	sound::{Sound, SoundData},
};

/// Controls the main mixer track.
#[derive(Debug)]
pub struct MainTrackHandle {
	pub(crate) set_volume_command_writer: CommandWriter<ValueChangeCommand<Decibels>>,
	pub(crate) sound_controller: ResourceController<Box<dyn Sound>>,
}

impl MainTrackHandle {
	/// Moves one retired Sound without destroying it. Reserve the caller's
	/// bounded destination before calling; None leaves all remaining ownership.
	pub fn try_take_unused_sound(&mut self) -> Option<Box<dyn Sound>> {
		self.sound_controller.try_pop_unused()
	}

	/// Reclaims at most `limit` retired sounds on the calling control thread.
	/// Returns (reclaimed count, further maintenance required). Reaching the
	/// limit requires another maintenance turn; observing an empty queue does
	/// not rule out a retirement publication still in flight. No new sound or
	/// PCM budget is needed. Playing a main-track sound never reclaims old ones.
	pub fn reclaim_unused(&mut self, limit: usize) -> (usize, bool) {
		self.sound_controller.reclaim_unused(limit)
	}

	/// Plays a sound.
	pub fn play<D: SoundData>(
		&mut self,
		sound_data: D,
	) -> Result<D::Handle, PlaySoundError<D::Error>> {
		let (sound, handle) = sound_data
			.into_sound()
			.map_err(PlaySoundError::IntoSoundError)?;
		self.sound_controller
			.insert_without_reclamation(sound)
			.map_err(|_| PlaySoundError::SoundLimitReached)?;
		Ok(handle)
	}

	/// Sets the (post-effects) volume of the mixer track.
	pub fn set_volume(&mut self, volume: impl Into<Value<Decibels>>, tween: Tween) {
		self.set_volume_command_writer.write(ValueChangeCommand {
			target: volume.into(),
			tween,
		})
	}

	/// Returns the maximum number of sounds that can play simultaneously on this track.
	#[must_use]
	pub fn sound_capacity(&self) -> usize {
		self.sound_controller.capacity()
	}

	/// Returns the number of sounds currently playing on this track.
	#[must_use]
	pub fn num_sounds(&self) -> usize {
		self.sound_controller.len()
	}
}
