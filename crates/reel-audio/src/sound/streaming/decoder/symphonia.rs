use crate::sound::AudioRead;
use crate::{
	frame::Frame,
	sound::{FromFileError, symphonia::load_frames_from_buffer_ref_into},
};
use symphonia::core::{
	audio::{Audio, GenericAudioBufferRef},
	codecs::{
		CodecParameters,
		audio::{AudioCodecParameters, AudioDecoder, well_known::CODEC_ID_OPUS},
	},
	formats::{FormatReader, SeekMode, SeekTo, TrackType, probe::Hint},
	io::{MediaSource, MediaSourceStream},
	units::Timestamp,
};

/// The existing packet decoder, shared by static and windowed preparation.
pub struct SymphoniaDecoder {
	format_reader: Box<dyn FormatReader>,
	decoder: Box<dyn AudioDecoder>,
	sample_rate: u32,
	num_frames: usize,
	track_id: u32,
	seek_preroll: usize,
	timeline_offset: u64,
	channel_count: usize,
}

impl SymphoniaDecoder {
	/// Probe a native Symphonia media source.
	pub fn new(media_source: Box<dyn MediaSource>) -> Result<Self, FromFileError> {
		Self::new_with_decoder(media_source, |params, _| {
			Ok(symphonia::default::get_codecs().make_audio_decoder(params, &Default::default())?)
		})
	}

	fn new_with_decoder(
		media_source: Box<dyn MediaSource>,
		make_decoder: impl FnOnce(
			&AudioCodecParameters,
			Timestamp,
		) -> Result<Box<dyn AudioDecoder>, FromFileError>,
	) -> Result<Self, FromFileError> {
		let probe = symphonia::default::get_probe();
		let mss = MediaSourceStream::new(media_source, Default::default());
		let format_reader = probe.probe(
			&Hint::default(),
			mss,
			Default::default(),
			Default::default(),
		)?;
		let default_track = format_reader
			.default_track(TrackType::Audio)
			.ok_or(FromFileError::NoDefaultTrack)?;
		let audio_params = match default_track.codec_params.as_ref() {
			Some(CodecParameters::Audio(p)) => p,
			_ => return Err(FromFileError::NoDefaultTrack),
		};
		let sample_rate = audio_params
			.sample_rate
			.ok_or(FromFileError::UnknownSampleRate)?;
		let channel_count = audio_params
			.channels
			.as_ref()
			.ok_or(FromFileError::UnsupportedChannelConfiguration)?
			.count();
		// Symphonia 0.6 Ogg/Opus retains the raw granule clock. Convert the
		// first audible position and total into the shared source timeline.
		let timeline_offset = if audio_params.codec == CODEC_ID_OPUS {
			(default_track.start_ts.get().max(0) as u64)
				+ u64::from(default_track.delay.unwrap_or(0))
		} else {
			0
		};
		let num_frames = default_track
			.num_frames
			.map(|frames| frames.saturating_sub(timeline_offset))
			.map(usize::try_from)
			.transpose()
			.map_err(|_| std::io::Error::other("audio frame count exceeds address space"))?
			.unwrap_or(usize::MAX);
		let decoder = make_decoder(audio_params, default_track.start_ts)?;
		// RFC 7845 section 4.6 requires at least 80 ms of Opus pre-roll.
		let seek_preroll = if audio_params.codec == CODEC_ID_OPUS {
			3840
		} else {
			0
		};
		let track_id = default_track.id;
		Ok(Self {
			format_reader,
			decoder,
			sample_rate,
			num_frames,
			track_id,
			seek_preroll,
			timeline_offset,
			channel_count,
		})
	}

	/// The encoded source channel count, before the common stereo PCM mapping.
	pub fn channel_count(&self) -> usize {
		self.channel_count
	}

	/// Probe a host reader without requiring the host to depend on Symphonia.
	pub fn from_reader(
		reader: impl AudioRead + 'static,
		length: u64,
	) -> Result<Self, FromFileError> {
		Self::new(Box::new(Reader { reader, length }))
	}

	/// Probe a host reader and construct its codec on the preparation worker.
	/// The callback receives the track's raw start timestamp and may reserve the
	/// codec's owned buffers before creating it.
	pub fn from_reader_with_decoder(
		reader: impl AudioRead + 'static,
		length: u64,
		make_decoder: impl FnOnce(
			&AudioCodecParameters,
			Timestamp,
		) -> Result<Box<dyn AudioDecoder>, FromFileError>,
	) -> Result<Self, FromFileError> {
		Self::new_with_decoder(Box::new(Reader { reader, length }), make_decoder)
	}

	/// Decode the next nonempty PCM chunk, reserving storage for each packet.
	/// Empty codec output (for example encoder priming) is consumed internally;
	/// an empty result means the format reader reached EOF. The reservation
	/// callback remains a cancellation checkpoint for every selected packet.
	pub fn decode_reserved(
		&mut self,
		reserve: impl FnMut(usize) -> Result<(), std::io::Error>,
	) -> Result<Vec<Frame>, FromFileError> {
		let mut frames = Vec::new();
		self.decode_reserved_into(&mut frames, reserve)?;
		Ok(frames)
	}

	/// Decode into reusable stereo PCM, reserving growth and old/new overlap
	/// before allocation. False means EOF; cancellation leaves no published PCM.
	pub fn decode_reserved_into(
		&mut self,
		frames: &mut Vec<Frame>,
		mut reserve: impl FnMut(usize) -> Result<(), std::io::Error>,
	) -> Result<bool, FromFileError> {
		frames.clear();
		loop {
			let packet = loop {
				match self.format_reader.next_packet()? {
					Some(packet) if packet.track_id == self.track_id => break packet,
					Some(_) => continue,
					None => return Ok(false),
				}
			};
			let packet_bytes = packet.data.len();
			reserve(packet_bytes + frames.capacity() * std::mem::size_of::<Frame>())?;
			let buffer = self.decoder.decode(&packet)?;
			macro_rules! bytes {
				($b:expr, $t:ty) => {
					$b.capacity() * $b.num_planes() * std::mem::size_of::<$t>()
				};
			}
			let native = match &buffer {
				GenericAudioBufferRef::U8(b) => bytes!(b, u8),
				GenericAudioBufferRef::U16(b) => bytes!(b, u16),
				GenericAudioBufferRef::U24(b) => bytes!(b, u32),
				GenericAudioBufferRef::U32(b) => bytes!(b, u32),
				GenericAudioBufferRef::S8(b) => bytes!(b, i8),
				GenericAudioBufferRef::S16(b) => bytes!(b, i16),
				GenericAudioBufferRef::S24(b) => bytes!(b, i32),
				GenericAudioBufferRef::S32(b) => bytes!(b, i32),
				GenericAudioBufferRef::F32(b) => bytes!(b, f32),
				GenericAudioBufferRef::F64(b) => bytes!(b, f64),
			};
			// A codec may consume priming or padding packets without producing PCM.
			// Only the format reader's end-of-stream result terminates this decoder.
			if buffer.frames() == 0 {
				continue;
			}
			let capacity = frames.capacity().max(buffer.frames());
			let overlap = if capacity > frames.capacity() {
				frames.capacity()
			} else {
				0
			};
			let pcm_bytes = capacity
				.checked_add(overlap)
				.and_then(|n| n.checked_mul(std::mem::size_of::<Frame>()))
				.ok_or_else(|| std::io::Error::other("audio packet size overflow"))?;
			reserve(packet_bytes + native + pcm_bytes)?;
			if capacity > frames.capacity() {
				frames
					.try_reserve_exact(capacity)
					.map_err(std::io::Error::other)?;
			}
			reserve(packet_bytes + native + frames.capacity() * std::mem::size_of::<Frame>())?;
			load_frames_from_buffer_ref_into(&buffer, frames)?;
			return Ok(true);
		}
	}

	/// Finish static preparation using this already-probed packet decoder.
	pub fn into_static(
		mut self,
		mut reserve: impl FnMut(usize) -> Result<(), std::io::Error>,
	) -> Result<crate::sound::static_sound::StaticSoundData, FromFileError> {
		let mut frames = Vec::<Frame>::new();
		loop {
			let retained = frames.capacity() * std::mem::size_of::<Frame>();
			let mut workspace = 0;
			let packet = self.decode_reserved(|scratch| {
				workspace = scratch;
				reserve(retained + scratch)
			})?;
			if packet.is_empty() {
				break;
			}
			let required = frames
				.len()
				.checked_add(packet.len())
				.ok_or_else(|| std::io::Error::other("PCM size overflow"))?;
			let capacity = if required > frames.capacity() {
				required.max(frames.capacity().saturating_mul(2))
			} else {
				frames.capacity()
			};
			let old = if capacity > frames.capacity() {
				frames.capacity()
			} else {
				0
			};
			reserve(
				(capacity + old + packet.capacity()) * std::mem::size_of::<Frame>() + workspace,
			)?;
			if capacity > frames.capacity() {
				frames
					.try_reserve_exact(capacity - frames.len())
					.map_err(std::io::Error::other)?;
			}
			frames.extend(packet);
		}
		reserve((frames.capacity() + frames.len()) * std::mem::size_of::<Frame>())?;
		Ok(crate::sound::static_sound::StaticSoundData {
			sample_rate: self.sample_rate,
			frames: frames.into(),
			settings: Default::default(),
			slice: None,
		})
	}
}

struct Reader<R> {
	reader: R,
	length: u64,
}
impl<R: AudioRead> std::io::Read for Reader<R> {
	fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
		self.reader.read(output)
	}
}
impl<R: AudioRead> std::io::Seek for Reader<R> {
	fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
		self.reader.seek(to)
	}
}
impl<R: AudioRead> MediaSource for Reader<R> {
	fn is_seekable(&self) -> bool {
		true
	}
	fn byte_len(&self) -> Option<u64> {
		Some(self.length)
	}
}

impl super::Decoder for SymphoniaDecoder {
	type Error = FromFileError;

	fn sample_rate(&self) -> u32 {
		self.sample_rate
	}

	fn num_frames(&self) -> usize {
		self.num_frames
	}

	fn decode(&mut self) -> Result<Vec<Frame>, Self::Error> {
		self.decode_reserved(|_| Ok(()))
	}

	fn seek(&mut self, index: usize) -> Result<usize, Self::Error> {
		let raw = (index as u64)
			.saturating_add(self.timeline_offset)
			.saturating_sub(self.seek_preroll as u64);
		let raw = i64::try_from(raw)
			.map_err(|_| std::io::Error::other("audio seek exceeds timestamp range"))?;
		let seeked_to = self.format_reader.seek(
			SeekMode::Accurate,
			SeekTo::Timestamp {
				ts: Timestamp::new(raw),
				track_id: self.track_id,
			},
		)?;
		let actual = seeked_to.actual_ts.get();
		self.decoder.reset();
		Ok(if actual < 0 {
			0
		} else {
			((actual as u64).saturating_sub(self.timeline_offset) as usize).min(self.num_frames)
		})
	}
}

#[cfg(all(test, feature = "mp3"))]
mod tests {
	use super::*;
	use std::io::Cursor;

	const MP3: &[u8] = include_bytes!("../../../../tests/fixtures/short-tone.mp3");

	fn packet_frames(mut decoder: SymphoniaDecoder) -> Vec<usize> {
		let mut counts = Vec::new();
		while let Some(packet) = decoder.format_reader.next_packet().unwrap() {
			if packet.track_id == decoder.track_id {
				counts.push(decoder.decoder.decode(&packet).unwrap().frames());
			}
		}
		counts
	}

	#[test]
	fn mp3_zero_output_packet_is_followed_by_pcm() {
		let bytes = if let Some(path) = std::env::var_os("REEL_DECODER_PROBE") {
			std::fs::read(path).unwrap()
		} else {
			MP3.to_vec()
		};
		let decoder =
			SymphoniaDecoder::from_reader(Cursor::new(bytes.clone()), bytes.len() as u64).unwrap();
		let counts = packet_frames(decoder);
		eprintln!("decoded packet frames: {counts:?}");
		assert_eq!(counts[0], 0);
		assert!(counts[1..].iter().any(|&count| count > 0));
	}
	#[test]
	fn mp3_empty_packets_do_not_terminate_reserved_or_static_decode() {
		let bytes = std::env::var_os("REEL_DECODER_PROBE")
			.map(|path| std::fs::read(path).unwrap())
			.unwrap_or_else(|| MP3.to_vec());
		let open = || {
			SymphoniaDecoder::from_reader(Cursor::new(bytes.clone()), bytes.len() as u64).unwrap()
		};
		let expected: usize = packet_frames(open()).into_iter().sum();
		assert!(expected > 0);
		let mut decoder = open();
		let mut total = 0;
		let mut checkpoints = 0;
		loop {
			let frames = decoder
				.decode_reserved(|_| {
					checkpoints += 1;
					Ok(())
				})
				.unwrap();
			if frames.is_empty() {
				break;
			}
			total += frames.len();
		}
		assert_eq!(total, expected);
		assert!(
			checkpoints >= 4,
			"priming and PCM packets both reserve storage"
		);
		assert!(decoder.decode_reserved(|_| Ok(())).unwrap().is_empty());
		let data = open().into_static(|_| Ok(())).unwrap();
		assert_eq!(data.frames.len(), expected);
		let reference =
			crate::sound::static_sound::StaticSoundData::from_cursor(Cursor::new(bytes)).unwrap();
		assert_eq!(data.frames.len(), reference.frames.len());
		for index in 0..expected {
			assert_eq!(data.frames.frame(index), reference.frames.frame(index));
		}
	}

	#[test]
	fn mp3_reservation_can_cancel_after_zero_output_packet() {
		let mut decoder =
			SymphoniaDecoder::from_reader(Cursor::new(MP3), MP3.len() as u64).unwrap();
		let mut calls = 0;
		let error = decoder
			.decode_reserved(|_| {
				calls += 1;
				if calls == 3 {
					Err(std::io::Error::other("cancelled after priming"))
				} else {
					Ok(())
				}
			})
			.unwrap_err();
		assert_eq!(calls, 3);
		assert!(error.to_string().contains("cancelled after priming"));
		let mut decoder =
			SymphoniaDecoder::from_reader(Cursor::new(MP3), MP3.len() as u64).unwrap();
		let error = decoder
			.decode_reserved(|_| Err(std::io::Error::other("budget rejected")))
			.unwrap_err();
		assert!(error.to_string().contains("budget rejected"));
	}
}
