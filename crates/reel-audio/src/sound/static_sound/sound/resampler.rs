use crate::frame::{Frame, interpolate_frame};

#[derive(Debug, Clone, Copy, PartialEq)]
struct RecentFrame {
	/// A frame of audio.
	frame: Frame,
	/// The current frame index of the source sound at the
	/// time this frame was pushed to the resampler.
	frame_index: usize,
	source: u64,
	sample_rate: u32,
}

pub(super) struct Resampler {
	frames: [RecentFrame; 4],
	time_until_empty: usize,
}

impl Resampler {
	pub fn for_source(starting_frame_index: usize, source: u64, sample_rate: u32) -> Self {
		Self {
			frames: [RecentFrame {
				frame: Frame::ZERO,
				frame_index: starting_frame_index,
				source,
				sample_rate,
			}; 4],
			time_until_empty: 0,
		}
	}

	pub fn push_source_frame(
		&mut self,
		frame: Option<Frame>,
		sample_index: usize,
		source: u64,
		sample_rate: u32,
	) {
		if frame.is_some() {
			self.time_until_empty = 4;
		} else {
			self.time_until_empty = self.time_until_empty.saturating_sub(1);
		}
		let frame = frame.unwrap_or_default();
		self.frames.copy_within(1.., 0);
		self.frames[self.frames.len() - 1] = RecentFrame {
			frame,
			frame_index: sample_index,
			source,
			sample_rate,
		};
	}

	#[must_use]
	pub fn get(&self, fractional_position: f32) -> Frame {
		if fractional_position == 0. {
			return self.frames[1].frame;
		}
		interpolate_frame(
			self.frames[0].frame,
			self.frames[1].frame,
			self.frames[2].frame,
			self.frames[3].frame,
			fractional_position,
		)
	}

	/// Returns the index of the frame in the source sound
	/// that the user is currently hearing from this resampler.
	///
	/// This is not the same as the most recently pushed frame.
	/// The user mainly hears a frame between `self.frames[1]` and
	/// `self.frames[2]`. `self.frames[0]` and `self.frames[3]`
	/// are used to provide additional information to the interpolation
	/// algorithm to get a smoother result.
	#[must_use]
	pub fn current_frame_index(&self) -> usize {
		self.frames[1].frame_index
	}

	pub fn current_source(&self) -> u64 {
		self.frames[1].source
	}
	pub fn current_sample_rate(&self) -> u32 {
		self.frames[1].sample_rate
	}

	#[must_use]
	pub fn empty(&self) -> bool {
		self.time_until_empty == 0
	}
}
