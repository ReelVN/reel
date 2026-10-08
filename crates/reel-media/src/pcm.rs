use crate::{AudioFormat, ChannelMap, SampleFormat};

mod sealed {
    pub trait Sealed {}
}
/// Device conversion types; integers clamp, nonfinite samples always become zero.
pub trait DeviceSample: sealed::Sealed + Copy {
    const FORMAT: SampleFormat;
    fn from_pcm(value: f32) -> Self;
}
macro_rules! sample {
    ($t:ty, $format:ident, $convert:expr) => {
        impl sealed::Sealed for $t {}
        impl DeviceSample for $t {
            const FORMAT: SampleFormat = SampleFormat::$format;
            fn from_pcm(value: f32) -> Self {
                ($convert)(value)
            }
        }
    };
}
sample!(f32, F32, |v: f32| v);
sample!(f64, F64, |v: f32| f64::from(v));
sample!(i16, I16, |v: f32| if v <= -1.0 {
    i16::MIN
} else {
    (v.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16
});
sample!(u16, U16, |v: f32| if v < 0.0 {
    ((v.clamp(-1.0, 0.0) + 1.0) * 32768.0).round() as u16
} else {
    (32768.0 + v.clamp(0.0, 1.0) * 32767.0).round() as u16
});
sample!(i32, I32, |v: f32| if v <= -1.0 {
    i32::MIN
} else {
    (f64::from(v.clamp(-1.0, 1.0)) * f64::from(i32::MAX)).round() as i32
});

pub(crate) struct Converter {
    pub format: AudioFormat,
    scratch: Vec<f32>,
}
impl Converter {
    pub fn new(format: AudioFormat) -> Self {
        Self {
            scratch: vec![0.0; format.max_pull_frames as usize * 2],
            format,
        }
    }
    pub fn render<T: DeviceSample>(&mut self, out: &mut [T], fill: &mut crate::PcmFill) {
        let channels = self.format.channel_map.len();
        for device_chunk in out.chunks_mut(self.format.max_pull_frames as usize * channels) {
            let frames = device_chunk.len() / channels;
            let stereo = &mut self.scratch[..frames * 2];
            stereo.fill(0.0);
            fill(stereo);
            for (input, output) in stereo
                .chunks_exact(2)
                .zip(device_chunk.chunks_exact_mut(channels))
            {
                let left = if input[0].is_finite() { input[0] } else { 0.0 };
                let right = if input[1].is_finite() { input[1] } else { 0.0 };
                for (sample, map) in output.iter_mut().zip(&self.format.channel_map) {
                    *sample = T::from_pcm(match map {
                        // Multiply before adding to avoid overflow of finite f32 samples.
                        ChannelMap::MonoAverage => 0.5 * left + 0.5 * right,
                        ChannelMap::Left => left,
                        ChannelMap::Right => right,
                        ChannelMap::Silence => 0.0,
                    });
                }
            }
        }
    }
}
