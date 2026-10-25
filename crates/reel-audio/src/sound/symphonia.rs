use symphonia::core::audio::{
	Audio, AudioBuffer, GenericAudioBufferRef,
	conv::{FromSample, IntoSample},
	sample::Sample,
};

use crate::frame::Frame;

use super::FromFileError;

pub fn load_frames_from_buffer_ref(
	buffer: &GenericAudioBufferRef,
) -> Result<Vec<Frame>, FromFileError> {
	let mut frames = Vec::new();
	load_frames_from_buffer_ref_into(buffer, &mut frames)?;
	Ok(frames)
}

/// Convert planar codec output into reusable common stereo PCM storage.
pub fn load_frames_from_buffer_ref_into(
	buffer: &GenericAudioBufferRef,
	frames: &mut Vec<Frame>,
) -> Result<(), FromFileError> {
	match buffer {
		GenericAudioBufferRef::U8(buffer) => load_frames_from_buffer_into(buffer, frames),
		GenericAudioBufferRef::U16(buffer) => load_frames_from_buffer_into(buffer, frames),
		GenericAudioBufferRef::U24(buffer) => load_frames_from_buffer_into(buffer, frames),
		GenericAudioBufferRef::U32(buffer) => load_frames_from_buffer_into(buffer, frames),
		GenericAudioBufferRef::S8(buffer) => load_frames_from_buffer_into(buffer, frames),
		GenericAudioBufferRef::S16(buffer) => load_frames_from_buffer_into(buffer, frames),
		GenericAudioBufferRef::S24(buffer) => load_frames_from_buffer_into(buffer, frames),
		GenericAudioBufferRef::S32(buffer) => load_frames_from_buffer_into(buffer, frames),
		GenericAudioBufferRef::F32(buffer) => load_frames_from_buffer_into(buffer, frames),
		GenericAudioBufferRef::F64(buffer) => load_frames_from_buffer_into(buffer, frames),
	}
}

fn load_frames_from_buffer_into<S: Sample>(
	buffer: &AudioBuffer<S>,
	frames: &mut Vec<Frame>,
) -> Result<(), FromFileError>
where
	f32: FromSample<S>,
{
	frames.clear();
	match buffer.num_planes() {
		1 => frames.extend(
			buffer
				.plane(0)
				.unwrap()
				.iter()
				.map(|sample| Frame::from_mono((*sample).into_sample())),
		),
		2 => frames.extend(
			buffer
				.plane(0)
				.unwrap()
				.iter()
				.zip(buffer.plane(1).unwrap().iter())
				.map(|(left, right)| Frame::new((*left).into_sample(), (*right).into_sample())),
		),
		_ => return Err(FromFileError::UnsupportedChannelConfiguration),
	}
	Ok(())
}
