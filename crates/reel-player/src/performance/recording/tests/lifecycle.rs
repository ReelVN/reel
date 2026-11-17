use super::*;
use crate::performance::{CachePolicy, FrameCaptureOptions, Request};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Output(PathBuf);
impl Output {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "reel-performance-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
    fn request(&self) -> Request {
        Request {
            version: 1,
            package: "game.asb".into(),
            saves: "saves".into(),
            output: self.0.clone(),
            vm_timing: false,
            source_atlas: None,
            frame: None,
        }
    }
    fn status(&self) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(self.0.join("status.json")).unwrap()).unwrap()
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        if self.0.exists() {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
}

#[test]
fn request_recording_publishes_running_frames_and_one_terminal_result() {
    let output = Output::new();
    let mut recording = Recording::start(output.request()).unwrap();
    assert_eq!(output.status()["state"], "running");
    assert!(reel_core::profile::WallSnapshot::is_active());
    // Isolate report publication from the process-global phase collector used
    // concurrently by other tests; boundary accumulation has its owning tests.
    let mut row = Row {
        loops: 1,
        ..Default::default()
    };
    row.phases.calls[reel_core::profile::Phase::Present as usize] = 1;
    recording.frames.record("workload", &row);
    recording.finish("workload", "closed").unwrap();
    let status = output.status();
    assert_eq!(status["schema"], 1);
    assert_eq!(status["state"], "complete");
    assert!(status.get("error").is_none());
    assert_eq!(status["request"]["source_atlas"], serde_json::Value::Null);
    assert_eq!(status["capabilities"]["gpu_timestamp"], false);
    assert!(!reel_core::profile::WallSnapshot::is_active());
    let frames = std::fs::read_to_string(output.0.join("frames.tsv")).unwrap();
    assert!(frames.contains("performance_scope\tordinary_session"));
    assert!(frames.contains("[reel-frame-report] begin"));
    assert!(
        frames
            .lines()
            .any(|line| line.starts_with("frame_sample\t"))
    );
    assert!(!output.0.join("frozen.tsv").exists());
    recording.finish("workload", "late failure").unwrap();
    assert_eq!(output.status()["state"], "complete");
}

#[test]
fn errors_and_unfinished_recordings_retain_terminal_failure() {
    let output = Output::new();
    let mut recording = Recording::start(output.request()).unwrap();
    recording
        .finish("workload", "original script failure")
        .unwrap();
    assert_eq!(output.status()["state"], "error");
    assert_eq!(output.status()["error"], "original script failure");
    let abandoned = Output::new();
    drop(Recording::start(abandoned.request()).unwrap());
    assert_eq!(abandoned.status()["state"], "error");
    assert!(
        abandoned.status()["error"]
            .as_str()
            .unwrap()
            .contains("without a terminal result")
    );
}

#[test]
fn stopped_collection_stays_running_until_native_shutdown_result_is_published() {
    let output = Output::new();
    let mut recording = Recording::start(output.request()).unwrap();
    recording.stop();
    assert!(!reel_core::profile::WallSnapshot::is_active());
    assert_eq!(output.status()["state"], "running");
    assert!(!output.0.join("frames.tsv").exists());
    recording
        .finish("workload", "native graphics shutdown failed")
        .unwrap();
    assert_eq!(output.status()["state"], "error");
    assert_eq!(output.status()["error"], "native graphics shutdown failed");
}

#[test]
fn fixed_capture_scope_keeps_setup_separate_from_frozen_report() {
    let output = Output::new();
    let mut request = output.request();
    request.frame = Some(FrameCaptureOptions {
        label: "workload".into(),
        accepted_frame: 1,
        repetitions: 1,
        cache: CachePolicy::Replay,
        measure_passes: true,
        save_frame: false,
    });
    let mut recording = Recording::start(request).unwrap();
    recording.finish("workload", "closed").unwrap();
    let frames = std::fs::read_to_string(output.0.join("frames.tsv")).unwrap();
    assert!(frames.contains("performance_scope\tfixed_frame_setup"));
    assert!(!frames.contains("frozen_sample\t"));
    assert!(!output.0.join("frozen.tsv").exists());
}
