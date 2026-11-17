//! Encode 8-bit artwork samples as lossless WebP inside a staged cache object.
use image::{DynamicImage, ImageEncoder, codecs::webp::WebPEncoder};
use std::{fs::File, io, path::Path};

pub(super) fn prepare(source: &Path, output: &Path, memory_bytes: u64) -> io::Result<u64> {
    let (mut decoder, codec_workspace) =
        reel_player::artwork::open(source, memory_bytes).map_err(io::Error::other)?;
    let color = decoder.color_type();
    if color.bytes_per_pixel() != color.channel_count() {
        return Err(super::invalid(
            "lossless WebP conversion requires 8-bit channels",
        ));
    }
    let (width, height) = decoder.dimensions();
    let pixels = u128::from(width) * u128::from(height);
    // The source samples, the encoder's RGBA transform and its growing VP8L buffer
    // can coexist. Keep room for the decoder's profile and codec tables as well.
    let workspace = u64::try_from(
        (pixels * 16 + (1 << 20))
            .max(u128::from(decoder.total_bytes()) + u128::from(codec_workspace)),
    )
    .map_err(|_| super::invalid("WebP workspace size overflow"))?;
    if workspace > memory_bytes {
        return Err(super::invalid(format!(
            "WebP preparation needs {workspace} workspace bytes; limit is {memory_bytes}"
        )));
    }
    let profile = decoder.icc_profile().map_err(io::Error::other)?;
    let pixels = DynamicImage::from_decoder(decoder).map_err(io::Error::other)?;
    let mut encoder = WebPEncoder::new_lossless(File::create(output)?);
    if let Some(profile) = profile {
        encoder.set_icc_profile(profile).map_err(io::Error::other)?;
    }
    encoder
        .encode(pixels.as_bytes(), width, height, color.into())
        .map_err(io::Error::other)?;
    Ok(workspace)
}
