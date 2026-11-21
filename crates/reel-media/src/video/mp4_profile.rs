//! MP4 container and track admission for preparation and desktop playback.
use super::h264::sequence_color;
use mp4::{Mp4Reader, Mp4Track, TrackType};
use std::io::{Read, Seek};

/// Maximum visible dimensions of the Vita hardware decoder profile.
pub const VITA_VIDEO_DIMENSIONS: [u32; 2] = [960, 544];

/// Inspect the MP4 index and the selected AVC and optional AAC configurations.
pub fn validate_desktop_mp4(reader: impl Read + Seek, size: u64) -> Result<(), String> {
    let movie = Mp4Reader::read_header(reader, size).map_err(|error| error.to_string())?;
    check_sequence(baseline_track(&movie)?)?;
    aac_lc_track(&movie)?;
    Ok(())
}

/// Check the container and the track properties available on the host for Vita.
/// The MP4 parser exposes AVC1 but not AVC3 sample descriptions. Unknown video
/// descriptions and playback timeline details remain subject to native admission.
pub fn validate_vita_mp4(reader: impl Read + Seek, size: u64) -> Result<(), String> {
    let movie = Mp4Reader::read_header(reader, size).map_err(|error| error.to_string())?;
    if movie.is_fragmented() || movie.moov.mvex.is_some() {
        return Err("the Vita player requires a non-fragmented MP4".into());
    }
    let mut video = None;
    let mut has_audio = false;
    for track in movie.tracks().values() {
        match track.track_type().ok() {
            Some(TrackType::Video) if video.is_none() => video = Some(track),
            Some(TrackType::Video) => {
                return Err("the Vita player requires exactly one video track".into());
            }
            Some(TrackType::Audio) if !has_audio => has_audio = true,
            Some(TrackType::Audio) => {
                return Err("the Vita player supports at most one AAC audio track".into());
            }
            _ => return Err("the Vita player accepts only video and optional audio tracks".into()),
        }
    }
    let video = video.ok_or("media has no video track")?;
    if let Some(avc1) = &video.trak.mdia.minf.stbl.stsd.avc1 {
        let size = [u32::from(avc1.width), u32::from(avc1.height)];
        let [max_width, max_height] = VITA_VIDEO_DIMENSIONS;
        if size
            .into_iter()
            .zip(VITA_VIDEO_DIMENSIONS)
            .any(|(value, maximum)| value == 0 || value > maximum || !value.is_multiple_of(2))
        {
            let [width, height] = size;
            let profile = avc1.avcc.avc_profile_indication;
            let name = profile_name(profile);
            return Err(format!(
                "H.264 profile {profile} ({name}) is {width}x{height}; the Vita player requires positive even dimensions up to {max_width}x{max_height}"
            ));
        }
        check_sequence(video)?;
    } else if let Ok(codec) = video.media_type() {
        return Err(format!(
            "MP4 video codec {codec}; the Vita player requires H.264 video"
        ));
    }
    aac_lc_track(&movie)?;
    Ok(())
}

fn check_sequence(track: &Mp4Track) -> Result<(), String> {
    let sps = track
        .sequence_parameter_set()
        .ok()
        .filter(|bytes| !bytes.is_empty())
        .ok_or("AVC sequence parameter set missing")?;
    sequence_color(sps).map(|_| ())
}

pub(super) fn baseline_track<R: Read + Seek>(movie: &Mp4Reader<R>) -> Result<&Mp4Track, String> {
    if movie.is_fragmented() {
        return Err("the desktop player requires a non-fragmented MP4".into());
    }
    let track = movie
        .tracks()
        .values()
        .filter(|track| track.track_type().ok() == Some(TrackType::Video))
        .min_by_key(|track| track.track_id())
        .ok_or("media has no video track")?;
    let avc = &track
        .trak
        .mdia
        .minf
        .stbl
        .stsd
        .avc1
        .as_ref()
        .ok_or("the desktop player requires an H.264 video track")?
        .avcc;
    check_profile(avc.avc_profile_indication)?;
    let (first, rest) = avc
        .sequence_parameter_sets
        .split_first()
        .ok_or("AVC sequence parameter set missing")?;
    for nal in std::iter::once(first).chain(rest) {
        let profile = nal
            .bytes
            .get(1)
            .filter(|_| nal.bytes[0] & 31 == 7)
            .ok_or("invalid AVC sequence parameter set")?;
        check_profile(*profile)?;
    }
    Ok(track)
}

type AacTrack<'a> = (&'a Mp4Track, u32, [u8; 2]);

pub(super) fn aac_lc_track<R: Read + Seek>(
    movie: &Mp4Reader<R>,
) -> Result<Option<AacTrack<'_>>, String> {
    let Some(track) = movie
        .tracks()
        .values()
        .filter(|track| track.track_type().ok() == Some(TrackType::Audio))
        .min_by_key(|track| track.track_id())
    else {
        return Ok(None);
    };
    let descriptor = &track
        .trak
        .mdia
        .minf
        .stbl
        .stsd
        .mp4a
        .as_ref()
        .and_then(|audio| audio.esds.as_ref())
        .ok_or("this MP4 decoder supports AAC-LC audio")?
        .es_desc
        .dec_config
        .dec_specific;
    if descriptor.profile != 2 || !(1..=2).contains(&descriptor.chan_conf) {
        return Err("this MP4 decoder supports mono/stereo AAC-LC audio".into());
    }
    let rate = track
        .sample_freq_index()
        .map_err(|_| "this MP4 decoder requires a valid AAC sample rate")?
        .freq();
    let config = [
        (descriptor.profile << 3) | (descriptor.freq_index >> 1),
        (descriptor.freq_index << 7) | (descriptor.chan_conf << 3),
    ];
    Ok(Some((track, rate, config)))
}

fn check_profile(profile: u8) -> Result<(), String> {
    if profile == 66 {
        return Ok(());
    }
    let name = profile_name(profile);
    Err(format!(
        "H.264 profile {profile} ({name}); the desktop player requires Baseline (profile 66)"
    ))
}

fn profile_name(profile: u8) -> &'static str {
    match profile {
        66 => "Baseline",
        77 => "Main",
        88 => "Extended",
        100 => "High",
        110 => "High 10",
        122 => "High 4:2:2",
        244 => "High 4:4:4",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mp4::{AacConfig, AudioObjectType, AvcConfig, ChannelConfig, Mp4Config, Mp4Writer};
    use std::io::Cursor;

    #[test]
    fn desktop_mp4_checks_the_same_optional_audio_configuration_as_playback() {
        fn video_with_audio(audio: impl IntoIterator<Item = AacConfig>) -> Vec<u8> {
            let bytes = include_bytes!("../../tests/fixtures/baseline-aac.mp4");
            let source = Mp4Reader::read_header(Cursor::new(bytes), bytes.len() as u64).unwrap();
            let track = baseline_track(&source).unwrap();
            let mut movie = Mp4Writer::write_start(
                Cursor::new(Vec::new()),
                &Mp4Config {
                    major_brand: "isom".parse().unwrap(),
                    minor_version: 0,
                    compatible_brands: vec!["isom".parse().unwrap()],
                    timescale: 1000,
                },
            )
            .unwrap();
            movie
                .add_track(
                    &AvcConfig {
                        width: track.width(),
                        height: track.height(),
                        seq_param_set: track.sequence_parameter_set().unwrap().to_vec(),
                        pic_param_set: track.picture_parameter_set().unwrap().to_vec(),
                    }
                    .into(),
                )
                .unwrap();
            for audio in audio {
                movie.add_track(&audio.into()).unwrap();
            }
            movie.write_end().unwrap();
            movie.into_writer().into_inner()
        }

        let validate =
            |bytes: Vec<u8>| validate_desktop_mp4(Cursor::new(&bytes), bytes.len() as u64);
        let stereo = AacConfig::default();
        let mono = AacConfig {
            chan_conf: ChannelConfig::Mono,
            ..stereo.clone()
        };
        let surround = AacConfig {
            chan_conf: ChannelConfig::FiveOne,
            ..stereo.clone()
        };
        let aac_main = AacConfig {
            profile: AudioObjectType::AacMain,
            ..stereo.clone()
        };
        assert_eq!(validate(video_with_audio([])), Ok(()));
        assert_eq!(validate(video_with_audio([mono])), Ok(()));
        assert_eq!(
            validate(video_with_audio([stereo.clone(), surround.clone()])),
            Ok(())
        );
        for tracks in [vec![surround, stereo], vec![aac_main]] {
            assert_eq!(
                validate(video_with_audio(tracks)).unwrap_err(),
                "this MP4 decoder supports mono/stereo AAC-LC audio"
            );
        }
    }
}
