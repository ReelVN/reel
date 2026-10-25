//! Reference libopus packet decoding under the shared Symphonia timeline.
use std::sync::Mutex;

use symphonia::core::{
    audio::{AudioBuffer, AudioMut, AudioSpec, Channels, GenericAudioBufferRef, Position},
    codecs::{
        CodecInfo,
        audio::{AudioCodecParameters, AudioDecoder, FinalizeResult},
    },
    errors::{Result, decode_error, unsupported_error},
    packet::PacketRef,
};

// RFC 6716 permits up to 120 ms in one packet at the mandatory 48 kHz clock.
const MAX_FRAMES: usize = 5760;
const INFO: CodecInfo = CodecInfo {
    short_name: "opus",
    long_name: "Opus (libopus)",
    profiles: &[],
};

pub(super) fn buffer_bytes(params: &AudioCodecParameters) -> Result<u64> {
    let Some(channels) = params.channels.as_ref() else {
        return decode_error("opus: missing channels");
    };
    match channels.count() {
        count @ 1..=2 => Ok((MAX_FRAMES * count * size_of::<f32>()) as u64),
        _ => unsupported_error("opus: playback requires mono or stereo"),
    }
}

pub(super) struct OpusDecoder {
    // Symphonia requires Sync. The worker exclusively borrows the codec through
    // get_mut(), so this wrapper never locks or transfers native handles.
    codec: Mutex<opus::Decoder>,
    params: AudioCodecParameters,
    interleaved: Vec<f32>,
    buffer: AudioBuffer<f32>,
    channels: usize,
    audible_start: i128,
}

impl OpusDecoder {
    pub(super) fn new(params: &AudioCodecParameters, start_ts: i64) -> Result<Self> {
        let Some(head) = params.extra_data.as_deref() else {
            return decode_error("opus: missing OpusHead");
        };
        if head.len() < 19 || &head[..8] != b"OpusHead" || head[8] > 15 {
            return decode_error("opus: invalid OpusHead");
        }
        let (channels, layout, opus_channels) = match head[9] {
            1 => (1, Position::FRONT_LEFT, opus::Channels::Mono),
            2 => (
                2,
                Position::FRONT_LEFT | Position::FRONT_RIGHT,
                opus::Channels::Stereo,
            ),
            _ => return unsupported_error("opus: playback requires mono or stereo"),
        };
        if params.sample_rate != Some(48_000)
            || params
                .channels
                .as_ref()
                .is_none_or(|value| value.count() != channels)
        {
            return decode_error("opus: inconsistent output format");
        }
        match head[18] {
            0 => (),
            1 if head.len() >= 21 + channels
                && head[19] == 1
                && usize::from(head[20]) == channels - 1
                && head[21..21 + channels]
                    .iter()
                    .copied()
                    .eq(0..channels as u8) => {}
            _ => return unsupported_error("opus: unsupported channel mapping"),
        }
        let mut codec = opus::Decoder::new(48_000, opus_channels).map_err(codec_error)?;
        codec
            .set_gain(i32::from(i16::from_le_bytes([head[16], head[17]])))
            .map_err(codec_error)?;
        Ok(Self {
            codec: Mutex::new(codec),
            params: params.clone(),
            interleaved: vec![0.; MAX_FRAMES * channels],
            buffer: AudioBuffer::new(
                AudioSpec::new(48_000, Channels::Positioned(layout)),
                MAX_FRAMES,
            ),
            channels,
            audible_start: i128::from(start_ts)
                + i128::from(u16::from_le_bytes([head[10], head[11]])),
        })
    }

    fn decode_packet(&mut self, packet: &PacketRef<'_>) -> Result<()> {
        if packet.data.is_empty() {
            return decode_error("opus: empty packet");
        }
        let count = self
            .codec
            .get_mut()
            .expect("worker owns codec")
            .decode_float(packet.data, &mut self.interleaved, false)
            .map_err(codec_error)?;
        let start = usize::try_from(packet.trim_start.get())
            .map_err(|_| std::io::Error::other("opus: start trim exceeds address space"))?;
        let start = start.max(
            (self.audible_start - i128::from(packet.pts.get())).clamp(0, count as i128) as usize,
        );
        let end = usize::try_from(packet.trim_end.get())
            .map_err(|_| std::io::Error::other("opus: end trim exceeds address space"))?;
        if start.checked_add(end).is_none_or(|trim| trim > count) {
            return decode_error("opus: trim exceeds packet duration");
        }
        let audible = count - start - end;
        self.buffer.render_uninit(Some(audible));
        for channel in 0..self.channels {
            let plane = self.buffer.plane_mut(channel).expect("declared channel");
            for (index, sample) in plane.iter_mut().enumerate() {
                *sample = self.interleaved[(start + index) * self.channels + channel];
            }
        }
        Ok(())
    }
}

fn codec_error(error: opus::Error) -> symphonia::core::errors::Error {
    std::io::Error::other(error.to_string()).into()
}

impl AudioDecoder for OpusDecoder {
    fn reset(&mut self) {
        self.codec
            .get_mut()
            .expect("worker owns codec")
            .reset_state()
            .expect("valid Opus reset");
        self.buffer.clear();
    }
    fn codec_info(&self) -> &CodecInfo {
        &INFO
    }
    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }
    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        self.buffer.clear();
        if let Err(error) = self.decode_packet(packet) {
            self.buffer.clear();
            return Err(error);
        }
        Ok(GenericAudioBufferRef::F32(&self.buffer))
    }
    fn finalize(&mut self) -> FinalizeResult {
        FinalizeResult::default()
    }
    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        GenericAudioBufferRef::F32(&self.buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symphonia::core::{
        audio::Audio,
        codecs::audio::well_known::CODEC_ID_OPUS,
        packet::Packet,
        units::{Duration, Timestamp},
    };

    #[test]
    fn header_gain_scales_pcm_and_survives_seek_reset() {
        let mut encoder =
            opus::Encoder::new(48_000, opus::Channels::Mono, opus::Application::Audio).unwrap();
        let pcm: Vec<f32> = (0..960).map(|i| (i as f32 * 0.1).sin() * 0.3).collect();
        let mut data = [0; 4000];
        let count = encoder.encode_float(&pcm, &mut data).unwrap();
        let packet = Packet::new(
            0,
            Timestamp::ZERO,
            Duration::new(960),
            Vec::from(&data[..count]),
        );
        let decode = |gain: i16, reset: bool| {
            let mut head = Vec::from(&b"OpusHead\x01\x01\0\0\x80\xbb\0\0"[..]);
            head.extend_from_slice(&gain.to_le_bytes());
            head.push(0);
            let mut params = AudioCodecParameters::new();
            params
                .for_codec(CODEC_ID_OPUS)
                .with_sample_rate(48_000)
                .with_channels(Channels::Positioned(Position::FRONT_LEFT))
                .with_extra_data(head.into_boxed_slice());
            let mut decoder = OpusDecoder::new(&params, 0).unwrap();
            if reset {
                decoder.reset();
            }
            decoder.decode(&packet).unwrap();
            decoder.buffer.plane(0).unwrap().to_vec()
        };
        let reference = decode(0, false);
        let attenuated = decode(-1536, true);
        let gain = 10_f32.powf(-6. / 20.);
        for (reference, attenuated) in reference.into_iter().zip(attenuated) {
            assert!((reference * gain - attenuated).abs() < 2e-6);
        }
    }
}
