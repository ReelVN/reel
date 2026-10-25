//! Shared H.264 picture and colour metadata, independent of the decoder backend.
use super::{YuvColor, YuvMatrix, YuvTransfer};
use h264_reader::nal::{
    Nal, RefNal,
    sps::{ChromaFormat, FrameMbsFlags, SeqParameterSet},
};

pub(super) fn sequence_color(nal: &[u8]) -> Result<YuvColor, String> {
    let sps = SeqParameterSet::from_bits(RefNal::new(nal, &[], true).rbsp_bits())
        .map_err(|e| format!("H.264 SPS: {e:?}"))?;
    if sps.frame_mbs_flags != FrameMbsFlags::Frames
        || sps.chroma_info.chroma_format != ChromaFormat::YUV420
        || sps.chroma_info.bit_depth_luma_minus8 != 0
        || sps.chroma_info.bit_depth_chroma_minus8 != 0
    {
        return Err("this video decoder requires progressive 8-bit YUV420".into());
    }
    let (_, height) = sps
        .pixel_dimensions()
        .map_err(|e| format!("H.264 dimensions: {e:?}"))?;
    let signal = sps
        .vui_parameters
        .as_ref()
        .and_then(|v| v.video_signal_type.as_ref());
    let matrix = match signal
        .and_then(|s| s.colour_description.as_ref())
        .map(|c| c.matrix_coefficients)
    {
        Some(1) => YuvMatrix::Bt709,
        Some(5 | 6) => YuvMatrix::Bt601,
        None | Some(2) => {
            if height >= 720 {
                YuvMatrix::Bt709
            } else {
                YuvMatrix::Bt601
            }
        }
        Some(matrix) => return Err(format!("unsupported H.264 colour matrix {matrix}")),
    };
    let transfer = match signal
        .and_then(|s| s.colour_description.as_ref())
        .map(|c| c.transfer_characteristics)
    {
        Some(13) => YuvTransfer::Srgb,
        None | Some(1 | 2 | 6) => YuvTransfer::Bt709,
        Some(transfer) => {
            return Err(format!(
                "unsupported H.264 transfer characteristic {transfer}"
            ));
        }
    };
    Ok(YuvColor {
        matrix,
        transfer,
        full_range: signal.is_some_and(|s| s.video_full_range_flag),
    })
}
