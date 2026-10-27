//! Offline MP4/M4A/AAC import through bounded PCM and the common Opus encoder.
//! FFmpeg codec storage and OS pipe capacity are external to Reel's PCM budget.
use crate::prepare_media::{Tools, command, failure, run};
use reel_core::budget::{BudgetCategory, BudgetPool, BudgetTicket, QueueLimits};
use reel_media::decode::{AudioDecoder, AudioInfo, Frame};
use serde::Deserialize;
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
    process::{Child, ChildStdout, Stdio},
    sync::atomic::{AtomicBool, Ordering},
};

const FRAMES: usize = 960;

pub(crate) fn uses_external(source: &Path) -> bool {
    source.extension().is_some_and(|extension| {
        ["mp4", "m4a", "aac"]
            .iter()
            .any(|candidate| extension.eq_ignore_ascii_case(candidate))
    })
}

pub(crate) fn external_tools_identity() -> io::Result<&'static str> {
    Ok(&Tools::discover()?.identity)
}

pub(crate) fn prepare(
    source: &Path,
    output: &Path,
    bitrate: u32,
    memory_bytes: u64,
) -> io::Result<u64> {
    let budget = BudgetPool::new(
        BudgetCategory::Temporary,
        QueueLimits {
            max_requests: 64,
            max_bytes: memory_bytes,
        },
    );
    let cancelled = AtomicBool::new(false);
    let mut output = File::create(output)?;
    if uses_external(source) {
        let decoder = Decoder::open(source, memory_bytes, &budget)?;
        reel_media::encode::opus_decoded(
            Box::new(decoder),
            &mut output,
            bitrate,
            budget.clone(),
            &cancelled,
        )
        .map_err(io::Error::other)?;
    } else {
        let input = File::open(source)?;
        let length = input.metadata()?.len();
        reel_media::encode::opus(
            input,
            length,
            &mut output,
            bitrate,
            budget.clone(),
            &cancelled,
        )
        .map_err(io::Error::other)?;
    }
    output.sync_all()?;
    Ok(budget.usage().peak_bytes)
}

#[derive(Deserialize)]
struct Probe {
    streams: Vec<Stream>,
}
#[derive(Deserialize)]
struct Stream {
    sample_rate: String,
    channels: u8,
}

struct Decoder {
    child: Option<Child>,
    stdout: ChildStdout,
    stderr: File,
    bytes: Vec<u8>,
    pcm: Vec<Frame>,
    info: AudioInfo,
    _ticket: BudgetTicket,
}

impl Decoder {
    fn open(source: &Path, memory_bytes: u64, budget: &BudgetPool) -> io::Result<Self> {
        let tools = Tools::discover()?;
        let probe = run(
            command(&tools.ffprobe)
                .args([
                    "-v",
                    "error",
                    "-max_alloc",
                    &memory_bytes.to_string(),
                    "-threads",
                    "1",
                    "-select_streams",
                    "a:0",
                    "-show_entries",
                    "stream=sample_rate,channels",
                    "-of",
                    "json",
                ])
                .arg(source),
            "FFprobe audio metadata",
        )?;
        let mut probe: Probe = serde_json::from_slice(&probe).map_err(io::Error::other)?;
        if probe.streams.len() != 1 {
            return Err(io::Error::other(
                "audio import requires one selected audio stream",
            ));
        }
        let stream = probe.streams.remove(0);
        let rate = stream
            .sample_rate
            .parse::<u32>()
            .map_err(io::Error::other)?;
        if !(1..=2).contains(&stream.channels) || !(8000..=384000).contains(&rate) {
            return Err(io::Error::other(
                "audio import requires mono/stereo at 8000..=384000 Hz",
            ));
        }
        let byte_count = FRAMES * usize::from(stream.channels) * size_of::<f32>();
        let ticket = budget
            .reserve((byte_count + FRAMES * size_of::<Frame>()) as u64)
            .map_err(|error| io::Error::other(error.message))?;
        let bytes = vec![0; byte_count];
        let pcm = Vec::with_capacity(FRAMES);
        let stderr = tempfile::tempfile()?;
        let mut child = command(&tools.ffmpeg)
            .args([
                "-v",
                "error",
                "-nostdin",
                "-xerror",
                "-max_alloc",
                &memory_bytes.to_string(),
                "-threads",
                "1",
                "-i",
            ])
            .arg(source)
            .args([
                "-map",
                "0:a:0",
                "-vn",
                "-sn",
                "-dn",
                "-threads",
                "1",
                "-c:a",
                "pcm_f32le",
                "-f",
                "f32le",
                "pipe:1",
            ])
            .stdout(Stdio::piped())
            .stderr(stderr.try_clone()?)
            .spawn()?;
        let stdout = child.stdout.take().expect("piped audio stdout");
        Ok(Self {
            child: Some(child),
            stdout,
            stderr,
            bytes,
            pcm,
            info: AudioInfo {
                sample_rate: rate,
                channels: stream.channels,
                frames: None,
            },
            _ticket: ticket,
        })
    }

    fn finish(&mut self) -> io::Result<()> {
        if let Some(child) = self.child.as_mut() {
            let status = child.wait()?;
            self.child = None;
            if !status.success() {
                return Err(failure(&mut self.stderr, status, "FFmpeg audio decode")?);
            }
        }
        Ok(())
    }
}

impl AudioDecoder for Decoder {
    fn info(&self) -> AudioInfo {
        self.info
    }
    fn seek(&mut self, _: u64) -> Result<u64, String> {
        Err("offline PCM import is a forward-only stream".into())
    }
    fn decode(&mut self, cancelled: &AtomicBool) -> Result<bool, String> {
        let mut length = 0;
        while length < self.bytes.len() {
            if cancelled.load(Ordering::Acquire) {
                return Err("audio import cancelled".into());
            }
            match self.stdout.read(&mut self.bytes[length..]) {
                Ok(0) => {
                    self.finish().map_err(|error| error.to_string())?;
                    break;
                }
                Ok(read) => length += read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(format!("FFmpeg audio PCM read: {error}")),
            }
        }
        let frame_bytes = usize::from(self.info.channels) * size_of::<f32>();
        if length % frame_bytes != 0 {
            return Err("FFmpeg returned an incomplete PCM frame".into());
        }
        self.pcm.clear();
        for frame in self.bytes[..length].chunks_exact(frame_bytes) {
            let left = f32::from_le_bytes(frame[..4].try_into().unwrap());
            let right = if self.info.channels == 1 {
                left
            } else {
                f32::from_le_bytes(frame[4..8].try_into().unwrap())
            };
            self.pcm.push(Frame::new(left, right));
        }
        Ok(!self.pcm.is_empty())
    }
    fn pcm(&self) -> &[Frame] {
        &self.pcm
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_media::decode::{AudioDecoderFactory, SoftwareAudioDecoderFactory};
    use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

    fn pool() -> BudgetPool {
        BudgetPool::new(
            BudgetCategory::Temporary,
            QueueLimits {
                max_requests: 64,
                max_bytes: 4 << 20,
            },
        )
    }

    fn decoded_frames(path: &Path, budget: &BudgetPool) -> (AudioInfo, u64) {
        let file = File::open(path).unwrap();
        let length = file.metadata().unwrap().len();
        let mut decoder = SoftwareAudioDecoderFactory
            .open(
                Box::new(file),
                length,
                budget.clone(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        let info = decoder.info();
        let mut frames = 0;
        while decoder.decode(&AtomicBool::new(false)).unwrap() {
            frames += decoder.pcm().len() as u64;
        }
        (info, frames)
    }

    fn source_frames(path: &Path) -> u64 {
        let tools = Tools::discover().unwrap();
        let frames = run(
            command(&tools.ffprobe)
                .args([
                    "-v",
                    "error",
                    "-threads",
                    "1",
                    "-select_streams",
                    "a:0",
                    "-show_frames",
                    "-show_entries",
                    "frame=nb_samples",
                    "-of",
                    "csv=p=0",
                ])
                .arg(path),
            "FFprobe decoded audio frames",
        )
        .unwrap();
        String::from_utf8(frames)
            .unwrap()
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| line.trim_end_matches(',').parse::<u64>().unwrap())
            .sum()
    }

    #[test]
    #[ignore = "requires FFmpeg/FFprobe on PATH"]
    fn mp4_import_uses_bounded_pcm_and_reaps_failed_or_cancelled_children() {
        let tools = Tools::discover().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("input.m4a");
        run(
            command(&tools.ffmpeg)
                .args([
                    "-v",
                    "error",
                    "-nostdin",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=731:sample_rate=48000",
                    "-t",
                    "0.137",
                    "-ac",
                    "2",
                    "-c:a",
                    "aac",
                ])
                .arg(&source),
            "FFmpeg AAC fixture",
        )
        .unwrap();
        let output = directory.path().join("output.opus");
        let peak = prepare(&source, &output, 96_000, 4 << 20).unwrap();
        assert!(peak <= 40_000, "unbounded PCM workspace: {peak}");
        let budget = pool();
        let (info, frames) = decoded_frames(&output, &budget);
        assert_eq!(info.sample_rate, 48_000);
        assert_eq!(info.channels, 2);
        assert_eq!(info.frames, Some(frames));
        assert_eq!(frames, source_frames(&source));
        assert_eq!(budget.usage().bytes, 0);

        let mut decoder = Decoder::open(&source, 4 << 20, &budget).unwrap();
        assert_eq!(budget.usage().bytes, 960 * (8 + 8));
        assert!(
            decoder
                .decode(&AtomicBool::new(true))
                .unwrap_err()
                .contains("cancelled")
        );
        drop(decoder); // Drop kills and waits for an unconsumed, possibly blocked pipe.
        assert_eq!(budget.usage().bytes, 0);

        let mut decoder = Decoder::open(&source, 4 << 20, &budget).unwrap();
        let mut original = decoder.child.take().unwrap();
        original.kill().unwrap();
        original.wait().unwrap();
        let stderr = tempfile::tempfile().unwrap();
        let mut failing = command(&tools.ffmpeg)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "reel_invalid_filter",
                "-f",
                "f32le",
                "pipe:1",
            ])
            .stdout(Stdio::piped())
            .stderr(stderr.try_clone().unwrap())
            .spawn()
            .unwrap();
        decoder.stdout = failing.stdout.take().unwrap();
        decoder.stderr = stderr;
        decoder.child = Some(failing);
        let error = decoder.decode(&AtomicBool::new(false)).unwrap_err();
        assert!(error.contains("FFmpeg audio decode failed"), "{error}");
        assert!(error.contains("reel_invalid_filter"), "{error}");
        assert!(
            decoder.child.is_none(),
            "failed child must already be reaped"
        );
        drop(decoder);
        assert_eq!(budget.usage().bytes, 0);
        eprintln!(
            "AAC import: {frames} audible frames, {peak} peak Reel PCM/encoder bytes; cancellation and failure reaped"
        );
    }

    #[test]
    #[ignore = "set REEL_AUDIO_PROJECT to an existing private project; requires FFmpeg/FFprobe"]
    fn real_selected_audio_prepares_once_and_preserves_decoded_lengths() {
        let root =
            PathBuf::from(std::env::var_os("REEL_AUDIO_PROJECT").expect("REEL_AUDIO_PROJECT"));
        let mut selected = BTreeMap::new();
        for file in std::fs::read_dir(root.join("resources/audio")).unwrap() {
            let file = file.unwrap().path();
            if uses_external(&file) {
                selected.insert(
                    format!("audio/{}", file.file_name().unwrap().to_str().unwrap()),
                    file,
                );
            }
        }
        assert!(!selected.is_empty());
        let target = reel_package::resource_target::ResourceTarget::parse("vita").unwrap();
        let prepared =
            crate::resource_conversion::prepare_selected(&root, &target, selected.clone()).unwrap();
        assert_eq!(
            prepared.report.converted + prepared.report.reused,
            selected.len()
        );
        let mut total_frames = 0;
        let mut output_bytes = 0;
        for (logical, source) in &selected {
            let output = &prepared.files[logical];
            let budget = pool();
            let (info, frames) = decoded_frames(output, &budget);
            assert_eq!(info.sample_rate, 48_000);
            assert_eq!(info.frames, Some(frames));
            assert_eq!(frames, source_frames(source), "{logical}");
            assert_eq!(budget.usage().bytes, 0);
            total_frames += frames;
            output_bytes += output.metadata().unwrap().len();
            eprintln!(
                "{logical}: {frames} frames, {} output bytes",
                output.metadata().unwrap().len()
            );
        }
        let cached =
            crate::resource_conversion::prepare_selected(&root, &target, selected).unwrap();
        assert_eq!(cached.report.converted, 0);
        assert_eq!(cached.report.reused, prepared.files.len());
        eprintln!(
            "real audio: {} files, {total_frames} audible frames, {output_bytes} output bytes, {} peak Reel bytes; second pass reused all",
            prepared.files.len(),
            prepared.report.workspace_bytes
        );
    }
}
