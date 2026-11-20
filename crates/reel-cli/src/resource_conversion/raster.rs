//! Store the same straight RGBA8 samples that the player decodes from artwork.

use image::{ColorType, DynamicImage};
use reel_core::prepared_raster::RasterHeader;
use reel_player::{artwork, assets::AssetOptions};
use std::{
    fs::File,
    io::{self, Write},
    path::Path,
};

pub(super) fn check_input_len(length: u64, vita: bool) -> io::Result<()> {
    let mut options = AssetOptions::default();
    if vita {
        options.input_bytes = artwork::VITA_INPUT_BYTES;
    }
    let limit = options.ordinary_input_bytes();
    if length > limit {
        return Err(super::invalid(format!(
            "prepared raster needs {length} input bytes, exceeding the target player limit of {limit}"
        )));
    }
    Ok(())
}

pub(super) fn prepare(
    source: &Path,
    output: &Path,
    memory_bytes: u64,
    vita: bool,
) -> io::Result<u64> {
    let (decoder, codec_workspace) =
        artwork::open(source, memory_bytes).map_err(io::Error::other)?;
    let color = decoder.color_type();
    if color.bytes_per_pixel() != color.channel_count() {
        return Err(super::invalid("raster conversion requires 8-bit channels"));
    }
    let (width, height) = decoder.dimensions();
    let header = RasterHeader::new([width, height])
        .ok_or_else(|| super::invalid("invalid prepared raster dimensions"))?;
    check_input_len(
        header.byte_len().saturating_add(RasterHeader::LEN as u64),
        vita,
    )?;
    // Decoder workspace is released before expanding channels. RGBA8 sources
    // already own the exact buffer that will be written to disk.
    let additional = if color == ColorType::Rgba8 {
        codec_workspace
    } else {
        codec_workspace.max(header.byte_len())
    };
    let workspace = decoder
        .total_bytes()
        .checked_add(additional)
        .ok_or_else(|| super::invalid("raster workspace size overflow"))?;
    if workspace > memory_bytes {
        return Err(super::invalid(format!(
            "raster preparation needs {workspace} workspace bytes; limit is {memory_bytes}"
        )));
    }
    let pixels = DynamicImage::from_decoder(decoder)
        .map_err(io::Error::other)?
        .into_rgba8();
    let mut file = File::create(output)?;
    file.write_all(&header.encode())?;
    file.write_all(pixels.as_raw())?;
    Ok(workspace)
}
