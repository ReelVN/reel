//! Optional measurement boundaries around the production desktop event loop.
use super::{Error, window::State};
use crate::{
    performance::{Request, StatusState, recording::Recording},
    player::{PlayerOptions, session::PlayerSession},
};
use std::{path::Path, time::Instant};

pub(super) struct Capture {
    recording: Recording,
    title: String,
}

impl Capture {
    pub(super) fn from_environment(options: &mut PlayerOptions) -> Result<Option<Self>, Error> {
        let Some(path) = std::env::var_os("REEL_PERFORMANCE_CONFIG") else {
            crate::performance::set_vm_profiling_enabled(false)?;
            return Ok(None);
        };
        let request = Request::read(Path::new(&path))?;
        Self::from_request(request, options).map(Some)
    }

    fn from_request(request: Request, options: &mut PlayerOptions) -> Result<Self, Error> {
        if request.frame.is_some() {
            let reason = "desktop fixed-frame replay and serial-pass timing are unavailable; capabilities: fixed_frame=false, serial_pass=false, gpu_timestamp=false";
            request.write_status(StatusState::Error, Some(reason))?;
            return Err(reason.into());
        }
        options.save_directory = Some(request.saves.clone());
        Ok(Self {
            recording: Recording::start(request)?,
            title: options.title.clone(),
        })
    }

    fn record(&mut self, state: &State, started: Instant) {
        if let Some(settings) = &state.player.game {
            self.title.clone_from(&settings.title);
        }
        let story = match state.player.session.as_ref() {
            Some(PlayerSession::Aside { story, .. }) => Some(story.as_ref()),
            _ => None,
        };
        let usage = state.player.backend.renderer().usage();
        self.recording
            .record(started, story, usage.total(), &self.title);
    }

    pub(super) fn finish(&mut self, outcome: &str) -> Result<(), Error> {
        self.recording.finish(&self.title, outcome)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_rejects_fixed_frame_before_starting_recording_or_overriding_saves() {
        let output = std::env::temp_dir().join(format!(
            "reel-desktop-performance-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let request = Request {
            version: 1,
            package: "game.asb".into(),
            saves: "measurement-saves".into(),
            output: output.clone(),
            vm_timing: false,
            source_atlas: None,
            frame: Some(crate::performance::capture::FrameCaptureOptions {
                label: "scene".into(),
                accepted_frame: 1,
                repetitions: 1,
                cache: crate::performance::capture::CachePolicy::Replay,
                measure_passes: false,
                save_frame: false,
            }),
        };
        let mut options = PlayerOptions::default();
        options.save_directory = Some("ordinary-saves".into());
        let result = Capture::from_request(request, &mut options);
        assert!(result.is_err());
        assert_eq!(
            options.save_directory.as_deref(),
            Some(Path::new("ordinary-saves"))
        );
        let status: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.join("status.json")).unwrap()).unwrap();
        assert_eq!(status["state"], "error");
        assert_eq!(status["capabilities"]["fixed_frame"], false);
        assert_eq!(status["capabilities"]["serial_pass"], false);
        assert_eq!(status["capabilities"]["gpu_timestamp"], false);
        assert!(!output.join("frames.tsv").exists());
    }
}

/// A render-bearing callback has at most one present. Maintenance callbacks
/// accumulate VM and phase work until the next actual presentation submission.
pub(super) fn record(capture: &mut Option<Capture>, state: &State, started: &mut Option<Instant>) {
    if let Some(capture) = capture
        && let Some(at) = started.take()
    {
        capture.record(state, at);
        *started = Some(Instant::now());
    }
}
