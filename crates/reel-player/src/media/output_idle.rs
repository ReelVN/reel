//! Session output policy. All native start/pause/resume calls stay on control.
use super::*;
use std::time::Instant;

const IDLE_GRACE: Duration = Duration::from_millis(100);
#[derive(Default)]
pub(super) struct OutputIdle {
    pub(super) quiet: Option<(Instant, u64)>,
    pub(super) max_pull_frames: u32,
}
impl MediaPlayer {
    fn dormant_sources(&self) -> usize {
        let audio = self
            .entries
            .values()
            .filter(|entry| {
                (entry.paused || !entry.started)
                    && matches!(entry.phase, Phase::Playing(media) if self.service.as_ref()
                    .and_then(|service| service.snapshot(media))
                    .is_some_and(|snapshot| snapshot.terminal.is_none()))
            })
            .count();
        #[cfg(feature = "video")]
        {
            audio
                + self
                    .movies
                    .entries
                    .values()
                    .filter(|movie| movie.dormant_audio())
                    .count()
        }
        #[cfg(not(feature = "video"))]
        {
            audio
        }
    }
    fn output_needed(&self) -> bool {
        // The ledger is settled before this policy runs. A still-pending stop
        // also keeps processing alive until the next maintenance boundary.
        let active = self.entries.values().any(|entry| {
            entry.started && !entry.paused && matches!(entry.phase, Phase::Playing(_))
        });
        #[cfg(feature = "video")]
        let active = active
            || self
                .movies
                .entries
                .values()
                .any(|movie| movie.active_audio());
        active
            || self.reclaim.as_ref().is_some_and(|reclaim| {
                // Unused/stopped Sounds still need a processing boundary before
                // retirement. SourceOwner decreases only after actual destruction.
                reclaim.status().live_sources > self.dormant_sources()
            })
    }
    pub(super) fn run_output(&mut self) -> Result<(), String> {
        self.output_idle.quiet = None;
        if self.suspended || self.closed {
            return Ok(());
        }
        let Some(output) = &mut self.output else {
            return Ok(());
        };
        let result = match output.status().state {
            AudioState::Stopped => {
                let fill = self
                    .service
                    .as_mut()
                    .and_then(AudioActivityService::take_fill)
                    .ok_or_else(|| "audio renderer unavailable".to_owned())?;
                output.start(fill).map_err(|error| error.reason.message)
                // Rejection drops only the DeferredRenderer shell on control;
                // its reserved credit transfers the real renderer to the worker.
            }
            AudioState::Suspended => output.resume().map_err(|error| error.to_string()),
            AudioState::Running => Ok(()),
            AudioState::Failed => Err("audio output failed".into()),
        };
        if let Err(error) = &result {
            self.failure = Some(error.clone());
        }
        result
    }
    pub(super) fn maintain_output(&mut self) {
        if self.suspended || self.closed || self.failure.is_some() {
            self.output_idle.quiet = None;
            return;
        }
        if self.output_needed() {
            let _ = self.run_output();
            return;
        }
        let Some(output) = &mut self.output else {
            return;
        };
        if output.status().state != AudioState::Running || !output.capabilities().suspend {
            self.output_idle.quiet = None;
            return;
        }
        let frames = output.clock_observation().generated_frames;
        let (since, baseline) = *self
            .output_idle
            .quiet
            .get_or_insert_with(|| (Instant::now(), frames));
        // The mixer installs no delay/reverb effects. A complete subsequent
        // callback applies pending pause/stop commands and finishes resampler
        // tails; live_sources above also prevents stranding stopped payloads.
        // This is software progress, never a device-presentation assertion.
        // More than one maximum pull also covers a callback already in flight
        // when control published its pause command and sampled the baseline.
        if since.elapsed() >= IDLE_GRACE
            && frames.saturating_sub(baseline) > u64::from(self.output_idle.max_pull_frames)
        {
            if let Err(error) = output.suspend() {
                self.failure = Some(error.to_string());
            }
            self.output_idle.quiet = None;
        }
    }
}
