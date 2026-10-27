//! Offline H.264/AAC preparation through FFmpeg and FFprobe on `PATH`.
//!
//! Each conversion runs synchronously with one decoder thread and two encoder
//! threads. Frame buffers are estimated before conversion; codec subprocess RSS
//! is allocated by the external tools and is not a Reel budget-pool measurement.
//! Tool selection and identity are cached for the current CLI process.
use crate::prepare_media::{Tools as MediaTools, command, run, run_to_file};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// Arrangement of color and coverage in the encoded video frame.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AlphaLayout {
    /// Use the complete video frame as opaque color.
    #[default]
    Native,
    /// Preserve color above a coverage image; scale each half independently.
    TopBottom,
}

/// Portable H.264 baseline/yuv420p and optional stereo AAC-LC output settings.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Maximum encoded width and height; the source is never enlarged.
    pub size: [u32; 2],
    /// Target video bitrate in bits per second; audio uses 128000 bits per second.
    pub bitrate: u32,
    pub alpha_layout: AlphaLayout,
}

/// Identify the resolved executables, their contents, linked library versions,
/// build configuration and selected H.264 encoder for resource-cache identity.
pub fn external_tools_identity() -> io::Result<String> {
    Ok(Tools::discover()?.identity.clone())
}

/// Convert one video to MP4 and atomically publish a previously absent output.
///
/// Dimensions fit `size` while preserving aspect ratio to integer alignment:
/// both axes are even, and packed color/coverage frames have a height divisible
/// by four. Frames retain their timestamps without frame-rate conversion. The
/// completed file is decoded by FFprobe to check every frame timestamp, count,
/// duration, profile, pixel layout and optional audio before publication.
///
/// Returns a working-set estimate (32 MiB plus source/output frame storage and
/// reference buffers). `memory_bytes` rejects estimates above that limit and
/// limits any single FFmpeg allocation. External codec RSS is not a hard-capped
/// or measured quantity. Missing tools or H.264/AAC encoders produce an error.
pub fn prepare(
    source: &Path,
    output: &Path,
    options: Options,
    memory_bytes: u64,
) -> io::Result<u64> {
    if output.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("video output already exists: {}", output.display()),
        ));
    }
    if options.size.contains(&0) || !(16_000..=100_000_000).contains(&options.bitrate) {
        return Err(invalid(
            "video size must be positive and bitrate must be 16000..100000000",
        ));
    }
    let tools = Tools::discover()?;
    let mut original = probe(&tools, source, false, memory_bytes)?;
    let dimensions = fitted_size(original.size, options.size, options.alpha_layout)?;
    let workspace = workspace_bytes(original.size, dimensions)?;
    if workspace > memory_bytes {
        return Err(invalid(format!(
            "video preparation estimates {workspace} workspace bytes; limit is {memory_bytes}"
        )));
    }
    original = probe(&tools, source, true, memory_bytes)?;
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = tempfile::Builder::new()
        .prefix(".reel-video-")
        .suffix(".mp4")
        .tempfile_in(parent)?
        .into_temp_path();
    let mut command = command(&tools.ffmpeg);
    command.args(["-v", "error", "-nostdin", "-y", "-max_alloc"]);
    command.arg(memory_bytes.to_string());
    command.args(["-threads", "1", "-copyts", "-start_at_zero", "-i"]);
    command.arg(source);
    match options.alpha_layout {
        AlphaLayout::Native => {
            command.args(["-map", "0:v:0", "-vf"]);
            command.arg(format!(
                "scale={}:{}:flags=lanczos,setsar=1",
                dimensions[0], dimensions[1]
            ));
        }
        AlphaLayout::TopBottom => {
            let width = dimensions[0];
            let height = dimensions[1] / 2;
            command.args(["-filter_complex", &format!(
                "[0:v:0]split=2[c][a];[c]crop=iw:ih/2:0:0:exact=1,scale={width}:{height}:flags=lanczos[c1];\
                 [a]crop=iw:ih/2:0:ih/2:exact=1,scale={width}:{height}:flags=lanczos[a1];\
                 [c1][a1]vstack=inputs=2,setsar=1[v]"
            ), "-map", "[v]"]);
        }
    }
    command.args([
        "-map",
        "0:a:0?",
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
        "-c:v",
        tools.encoder,
        "-pix_fmt",
        "yuv420p",
        "-bf",
        "0",
        "-g",
        "60",
        "-threads:v",
        "2",
        "-filter_threads",
        "1",
        "-filter_complex_threads",
        "1",
        "-fps_mode:v",
        "passthrough",
        "-enc_time_base:v",
        &original.stream.time_base,
        "-b:v",
    ]);
    command.arg(options.bitrate.to_string());
    if tools.encoder == "libx264" {
        command.args(["-profile:v", "baseline", "-preset", "medium", "-refs", "1"]);
    } else {
        command.args([
            "-profile:v",
            "constrained_baseline",
            "-allow_skip_frames",
            "0",
        ]);
    }
    command.args([
        "-c:a",
        "aac",
        "-profile:a",
        "aac_low",
        "-b:a",
        "128000",
        "-ar",
        "48000",
        "-ac",
        "2",
        "-threads:a",
        "1",
        "-movflags",
        "+faststart",
        "-f",
        "mp4",
    ]);
    command.arg(temporary.as_os_str());
    run(&mut command, "FFmpeg video conversion")?;
    let converted = probe(&tools, &temporary, true, memory_bytes)?;
    validate(&original, &converted, dimensions)?;
    OpenOptions::new()
        .write(true)
        .open(&temporary)?
        .sync_all()?;
    temporary
        .persist_noclobber(output)
        .map_err(|error| error.error)?;
    Ok(workspace)
}

fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}

struct Tools {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    encoder: &'static str,
    identity: String,
}

impl Tools {
    fn discover() -> io::Result<&'static Self> {
        static TOOLS: OnceLock<Tools> = OnceLock::new();
        if let Some(tools) = TOOLS.get() {
            return Ok(tools);
        }
        let common = MediaTools::discover()?;
        let ffmpeg = common.ffmpeg.clone();
        let ffprobe = common.ffprobe.clone();
        let mut identity = common.identity.clone();
        let encoders = run(
            command(&ffmpeg).args(["-hide_banner", "-encoders"]),
            "FFmpeg encoder discovery",
        )?;
        let encoders = String::from_utf8_lossy(&encoders);
        let has = |name: &str| {
            encoders
                .lines()
                .any(|line| line.split_whitespace().nth(1) == Some(name))
        };
        let encoder = if has("libx264") {
            "libx264"
        } else if has("libopenh264") {
            "libopenh264"
        } else {
            return Err(invalid(
                "video preparation needs FFmpeg with libx264 or libopenh264",
            ));
        };
        if !has("aac") {
            return Err(invalid(
                "video preparation needs FFmpeg with the AAC encoder",
            ));
        }
        identity.push_str(encoder);
        Ok(TOOLS.get_or_init(|| Self {
            ffmpeg,
            ffprobe,
            encoder,
            identity,
        }))
    }
}

fn fitted_size(source: [u32; 2], bounds: [u32; 2], alpha: AlphaLayout) -> io::Result<[u32; 2]> {
    if source.contains(&0) || bounds.contains(&0) {
        return Err(invalid("video dimensions must be positive"));
    }
    let height_alignment = match alpha {
        AlphaLayout::Native => 2,
        AlphaLayout::TopBottom if source[1] % 2 == 0 => 4,
        AlphaLayout::TopBottom => {
            return Err(invalid("top_bottom video must have an even source height"));
        }
    };
    let scale = (f64::from(bounds[0]) / f64::from(source[0]))
        .min(f64::from(bounds[1]) / f64::from(source[1]))
        .min(1.);
    let width = (f64::from(source[0]) * scale).floor() as u32 / 2 * 2;
    let height =
        (f64::from(source[1]) * scale).floor() as u32 / height_alignment * height_alignment;
    if width == 0 || height == 0 {
        return Err(invalid(
            "video is too small for the requested chroma/alpha alignment",
        ));
    }
    Ok([width, height])
}

fn workspace_bytes(source: [u32; 2], output: [u32; 2]) -> io::Result<u64> {
    (u64::from(source[0]) * u64::from(source[1]))
        .checked_add(u64::from(output[0]) * u64::from(output[1]))
        .and_then(|pixels| pixels.checked_mul(24))
        .and_then(|bytes| bytes.checked_add(32 << 20))
        .ok_or_else(|| invalid("video workspace estimate overflows"))
}

#[derive(Deserialize, Default)]
struct ProbeResult {
    #[serde(default)]
    streams: Vec<Stream>,
    #[serde(default)]
    format: Container,
}

#[derive(Deserialize, Default)]
struct Container {
    duration: Option<String>,
}

#[derive(Deserialize, Default)]
struct Stream {
    #[serde(default)]
    codec_name: String,
    #[serde(default)]
    profile: String,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    #[serde(default)]
    has_b_frames: u32,
    #[serde(default)]
    pix_fmt: String,
    #[serde(default)]
    avg_frame_rate: String,
    #[serde(default)]
    r_frame_rate: String,
    #[serde(default)]
    time_base: String,
    duration: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u32>,
}

struct Video {
    stream: Stream,
    audio: Option<Stream>,
    size: [u32; 2],
    frames: u64,
    rate: f64,
    duration: f64,
    timeline: Option<Timeline>,
}

struct Timeline {
    file: File,
    frames: u64,
}

// Frame timestamps stay on disk, with only one bounded line in memory. This is
// also the decoded-frame count: validation does not need a second decode pass.
fn timeline(tools: &Tools, source: &Path, memory_bytes: u64) -> io::Result<Timeline> {
    let mut command = command(&tools.ffprobe);
    command
        .args([
            "-v",
            "error",
            "-max_alloc",
            &memory_bytes.to_string(),
            "-threads",
            "1",
            "-select_streams",
            "v:0",
            "-show_frames",
            "-show_entries",
            "frame=best_effort_timestamp:frame_side_data=",
            "-of",
            "csv=p=0",
        ])
        .arg(source);
    let mut file = run_to_file(&mut command, "FFprobe video frame timestamps")?;
    let mut frames = 0;
    {
        let mut reader = BufReader::new(&mut file);
        let mut line = String::new();
        while next_timestamp(&mut reader, &mut line)?.is_some() {
            frames += 1;
        }
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(Timeline { file, frames })
}

fn next_timestamp(reader: &mut impl BufRead, line: &mut String) -> io::Result<Option<i64>> {
    loop {
        line.clear();
        let length = (&mut *reader).take(128).read_line(line)?;
        if length == 0 {
            return Ok(None);
        }
        if length == 128 {
            return Err(invalid("video frame timestamp exceeds 127 bytes"));
        }
        let value = line.trim().split(',').next().unwrap_or("");
        if value.is_empty() {
            continue;
        }
        return value
            .parse()
            .map(Some)
            .map_err(|_| invalid("video frame has no valid presentation timestamp"));
    }
}

fn probe(tools: &Tools, source: &Path, count_frames: bool, memory_bytes: u64) -> io::Result<Video> {
    let probe_stream = |selection: &str| -> io::Result<(Option<Stream>, Container)> {
        let mut command = command(&tools.ffprobe);
        command.args([
            "-v",
            "error",
            "-max_alloc",
            &memory_bytes.to_string(),
            "-threads",
            "1",
        ]);
        command.args([
            "-select_streams", selection, "-show_entries",
            "stream=codec_name,profile,width,height,pix_fmt,has_b_frames,avg_frame_rate,r_frame_rate,time_base,duration,sample_rate,channels:format=duration",
            "-of", "json",
        ]);
        command.arg(source);
        let bytes = run(&mut command, "FFprobe video metadata")?;
        let result: ProbeResult = serde_json::from_slice(&bytes)
            .map_err(|error| invalid(format!("FFprobe metadata: {error}")))?;
        Ok((result.streams.into_iter().next(), result.format))
    };
    let (stream, container) = probe_stream("v:0")?;
    let stream = stream
        .ok_or_else(|| invalid(format!("video has no video stream: {}", source.display())))?;
    let size = [stream.width, stream.height];
    let duration = number(stream.duration.as_deref())
        .or_else(|| number(container.duration.as_deref()))
        .unwrap_or(0.);
    let timeline = count_frames
        .then(|| timeline(tools, source, memory_bytes))
        .transpose()?;
    let frames = timeline.as_ref().map_or(0, |timeline| timeline.frames);
    let rate = rational(&stream.avg_frame_rate)
        .filter(|rate| *rate > 0.)
        .or_else(|| rational(&stream.r_frame_rate))
        .unwrap_or(0.);
    if size.contains(&0)
        || (count_frames
            && (duration <= 0.
                || frames == 0
                || rate <= 0.
                || rational(&stream.time_base).is_none_or(|value| value <= 0.)))
    {
        return Err(invalid(format!(
            "video needs finite dimensions, duration, frame count and rate: {}",
            source.display()
        )));
    }
    Ok(Video {
        stream,
        audio: probe_stream("a:0")?.0,
        size,
        frames,
        rate,
        duration,
        timeline,
    })
}

fn number(value: Option<&str>) -> Option<f64> {
    value?
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite())
}

fn rational(value: &str) -> Option<f64> {
    let (numerator, denominator) = value.split_once('/')?;
    let numerator = number(Some(numerator))?;
    let denominator = number(Some(denominator))?;
    (denominator > 0.).then_some(numerator / denominator)
}

fn validate(source: &Video, output: &Video, size: [u32; 2]) -> io::Result<()> {
    let stream = &output.stream;
    if output.size != size
        || stream.codec_name != "h264"
        || !matches!(stream.profile.as_str(), "Baseline" | "Constrained Baseline")
        || stream.pix_fmt != "yuv420p"
        || stream.has_b_frames != 0
    {
        return Err(invalid(
            "prepared video does not satisfy H.264 baseline/yuv420p dimensions",
        ));
    }
    if source.frames != output.frames
        || (source.duration - output.duration).abs() > 1. / source.rate + 0.001
    {
        return Err(invalid(format!(
            "video timing changed: source {}/{}fps/{}s; output {}/{}fps/{}s",
            source.frames,
            source.rate,
            source.duration,
            output.frames,
            output.rate,
            output.duration
        )));
    }
    compare_timestamps(source, output)?;
    match (&source.audio, &output.audio) {
        (None, None) => {}
        (Some(original), Some(converted))
            if converted.codec_name == "aac"
                && converted.profile == "LC"
                && converted.sample_rate.as_deref() == Some("48000")
                && converted.channels == Some(2) =>
        {
            if let (Some(before), Some(after)) = (
                number(original.duration.as_deref()),
                number(converted.duration.as_deref()),
            ) && (before - after).abs() > 0.1
            {
                return Err(invalid("video audio duration changed by more than 100 ms"));
            }
        }
        _ => {
            return Err(invalid(
                "prepared video must preserve optional audio as stereo AAC-LC/48000 Hz",
            ));
        }
    }
    Ok(())
}

fn compare_timestamps(source: &Video, output: &Video) -> io::Result<()> {
    let source_time_base = rational(&source.stream.time_base)
        .filter(|value| *value > 0.)
        .ok_or_else(|| invalid("source video has no valid timestamp time base"))?;
    let output_time_base = rational(&output.stream.time_base)
        .filter(|value| *value > 0.)
        .ok_or_else(|| invalid("prepared video has no valid timestamp time base"))?;
    let reader = |timeline: &Option<Timeline>| -> io::Result<_> {
        let mut file = timeline
            .as_ref()
            .ok_or_else(|| invalid("video timestamps were not decoded"))?
            .file
            .try_clone()?;
        file.seek(SeekFrom::Start(0))?;
        Ok(BufReader::new(file))
    };
    let mut source_reader = reader(&source.timeline)?;
    let mut output_reader = reader(&output.timeline)?;
    let (mut source_line, mut output_line) = (String::new(), String::new());
    let mut origin = None;
    let mut index = 0;
    // Rescaling may round a presentation time by half an output clock tick.
    // avg_frame_rate is container bookkeeping and can differ for identical PTS.
    let tolerance = output_time_base / 2. + 1e-9;
    loop {
        match (
            next_timestamp(&mut source_reader, &mut source_line)?,
            next_timestamp(&mut output_reader, &mut output_line)?,
        ) {
            (None, None) => return Ok(()),
            (Some(before), Some(after)) => {
                let before = before as f64 * source_time_base;
                let after = after as f64 * output_time_base;
                let (first_before, first_after) = *origin.get_or_insert((before, after));
                if ((before - first_before) - (after - first_after)).abs() > tolerance {
                    return Err(invalid(format!(
                        "video frame {index} presentation time changed: {}s to {}s",
                        before - first_before,
                        after - first_after
                    )));
                }
                index += 1;
            }
            _ => return Err(invalid("video decoded frame count changed")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn fitting_preserves_density_and_chroma_alignment() {
        assert_eq!(
            fitted_size([1024, 768], [960, 544], AlphaLayout::Native).unwrap(),
            [724, 544]
        );
        assert_eq!(
            fitted_size([1920, 2160], [960, 544], AlphaLayout::TopBottom).unwrap(),
            [482, 544]
        );
        assert_eq!(
            fitted_size([320, 242], [960, 544], AlphaLayout::Native).unwrap(),
            [320, 242]
        );
        assert_eq!(
            fitted_size([320, 242], [960, 544], AlphaLayout::TopBottom).unwrap(),
            [320, 240]
        );
        assert!(fitted_size([320, 241], [960, 544], AlphaLayout::TopBottom).is_err());
    }

    #[test]
    #[ignore = "requires FFmpeg/FFprobe with libx264 or libopenh264 and AAC on PATH"]
    fn external_conversion_preserves_timing_and_packed_layout() {
        let tools = Tools::discover().unwrap();
        assert!(external_tools_identity().unwrap().contains(tools.encoder));
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/reel-artifacts/video-tests");
        fs::create_dir_all(&root).unwrap();
        let directory = tempfile::tempdir_in(root).unwrap();
        let source = directory.path().join("source.mkv");
        run(
            command(&tools.ffmpeg)
                .args([
                    "-v",
                    "error",
                    "-nostdin",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=322x242:rate=30000/1001",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:sample_rate=44100",
                    "-t",
                    "0.5",
                    "-c:v",
                    "mpeg4",
                    "-q:v",
                    "2",
                    "-c:a",
                    "pcm_s16le",
                ])
                .arg(&source),
            "test video generation",
        )
        .unwrap();
        for (alpha_layout, size, expected) in [
            (AlphaLayout::Native, [160, 120], [158, 120]),
            (AlphaLayout::TopBottom, [160, 120], [158, 120]),
        ] {
            let output = directory.path().join(format!("{alpha_layout:?}.mp4"));
            let options = Options {
                size,
                bitrate: 400_000,
                alpha_layout,
            };
            assert!(prepare(&source, &output, options, 1 << 20).is_err());
            assert!(!output.exists());
            assert!(prepare(&source, &output, options, 128 << 20).unwrap() < 128 << 20);
            assert_eq!(
                probe(&tools, &output, true, 128 << 20).unwrap().size,
                expected
            );
            assert_eq!(
                prepare(&source, &output, options, 128 << 20)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::AlreadyExists
            );
        }
    }
}
