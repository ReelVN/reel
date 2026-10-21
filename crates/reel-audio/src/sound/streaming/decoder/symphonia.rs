use crate::{
	frame::Frame,
	sound::{FromFileError, symphonia::load_frames_from_buffer_ref},
};
use symphonia::core::{
	audio::{Audio, GenericAudioBufferRef},
	codecs::{CodecParameters, audio::AudioDecoder},
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
}

impl SymphoniaDecoder {
	/// Probe a native Symphonia media source.
	pub fn new(media_source: Box<dyn MediaSource>) -> Result<Self, FromFileError> {
		let codecs = symphonia::default::get_codecs();
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
		let num_frames = default_track
			.num_frames
			.map(usize::try_from)
			.transpose()
			.map_err(|_| std::io::Error::other("audio frame count exceeds address space"))?
			.unwrap_or(usize::MAX);
		let decoder = codecs.make_audio_decoder(audio_params, &Default::default())?;
		let track_id = default_track.id;
		Ok(Self {
			format_reader,
			decoder,
			sample_rate,
			num_frames,
			track_id,
		})
	}

	/// Probe a host reader without requiring the host to depend on Symphonia.
	pub fn from_reader(
		reader: impl AudioRead + 'static,
		length: u64,
	) -> Result<Self, FromFileError> {
		Self::new(Box::new(Reader { reader, length }))
	}

	/// Decode one packet, reserving conversion storage before allocation.
	pub fn decode_reserved(
		&mut self,
		mut reserve: impl FnMut(usize) -> Result<(), std::io::Error>,
	) -> Result<Vec<Frame>, FromFileError> {
		let packet = loop {
			match self.format_reader.next_packet()? {
				Some(packet) if packet.track_id == self.track_id => break packet,
				Some(_) => continue,
				None => return Ok(vec![]),
			}
		};
		let packet_bytes = packet.data.len();
		reserve(packet_bytes)?;
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
		reserve(
			packet_bytes
				+ native + buffer
				.frames()
				.checked_mul(std::mem::size_of::<Frame>())
				.ok_or_else(|| std::io::Error::other("audio packet size overflow"))?,
		)?;
		load_frames_from_buffer_ref(&buffer)
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

/// A seekable host resource. Reads and seeks run on decoder workers only.
pub trait AudioRead: std::io::Read + std::io::Seek + Send + Sync {}
impl<T: std::io::Read + std::io::Seek + Send + Sync> AudioRead for T {}
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
		let seeked_to = self.format_reader.seek(
			SeekMode::Accurate,
			SeekTo::Timestamp {
				ts: Timestamp::new(index as i64),
				track_id: self.track_id,
			},
		)?;
		let actual = seeked_to.actual_ts.get();
		self.decoder.reset();
		Ok(if actual < 0 {
			0
		} else {
			(actual as usize).min(self.num_frames)
		})
	}
}
