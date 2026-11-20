//! Encode artwork as 8-bit sRGB samples with straight alpha in a staged cache object.
use image::{DynamicImage, ExtendedColorType, ImageEncoder, codecs::qoi::QoiEncoder};
use std::{fs::File, io, path::Path};

pub(super) fn prepare(source: &Path, output: &Path, memory_bytes: u64) -> io::Result<u64> {
    let (decoder, codec_workspace) =
        reel_player::artwork::open(source, memory_bytes).map_err(io::Error::other)?;
    let color = decoder.color_type();
    if color.bytes_per_pixel() != color.channel_count() {
        return Err(super::invalid(
            "lossless QOI conversion requires 8-bit channels",
        ));
    }
    let (width, height) = decoder.dimensions();
    let (channels, output_color) = if color.has_alpha() {
        (4, ExtendedColorType::Rgba8)
    } else {
        (3, ExtendedColorType::Rgb8)
    };
    // The source and expanded samples can coexist. QOI 0.4.1 reserves up to
    // channels + 1 bytes per pixel with its 14-byte header and 8-byte marker.
    let pixels = u128::from(width) * u128::from(height);
    let workspace = u64::try_from(
        (pixels * (2 * channels + 1) + (128 << 10))
            .max(u128::from(decoder.total_bytes()) + u128::from(codec_workspace)),
    )
    .map_err(|_| super::invalid("QOI workspace size overflow"))?;
    if workspace > memory_bytes {
        return Err(super::invalid(format!(
            "QOI preparation needs {workspace} workspace bytes; limit is {memory_bytes}"
        )));
    }
    let image = DynamicImage::from_decoder(decoder).map_err(io::Error::other)?;
    let samples = if output_color == ExtendedColorType::Rgba8 {
        image.into_rgba8().into_raw()
    } else {
        image.into_rgb8().into_raw()
    };
    QoiEncoder::new(File::create(output)?)
        .write_image(&samples, width, height, output_color)
        .map_err(io::Error::other)?;
    Ok(workspace)
}
