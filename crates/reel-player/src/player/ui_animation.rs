//! Native widget animation shares the player's clock, cadence and frame admission.
use super::*;
use reel_core::clock::animation_sample_delay;
use std::time::Duration;

#[derive(Default)]
pub(crate) struct UiAnimation {
    last: Option<MonoTime>,
    origin: MonoTime,
}
impl UiAnimation {
    pub(super) fn reset(&mut self) {
        self.last = None;
    }
    fn deadline(&self, now: MonoTime, rate: Option<u32>) -> MonoTime {
        let (Some(last), Some(rate)) = (self.last, rate) else {
            return now;
        };
        let elapsed = Duration::from_nanos(last.0.saturating_sub(self.origin.0));
        let delay = animation_sample_delay(elapsed, rate).as_nanos() as u64;
        MonoTime(last.0.saturating_add(delay))
    }
    fn sample(&mut self, now: MonoTime) -> Duration {
        let Some(previous) = self.last.replace(now) else {
            self.origin = now;
            return Duration::ZERO;
        };
        Duration::from_nanos(now.0.saturating_sub(previous.0))
    }
}

impl<B> Player<B>
where
    B: RenderBackend + CaptureBackend<Error = <B as RenderBackend>::Error>,
{
    fn ui_animation_enabled(&self) -> bool {
        !self.closing
            && !self.presentation.is_suspended()
            && !self.captures.busy()
            && self.presentation.bridge.root().needs_anim()
    }
    fn ui_animation_rate(&self) -> Option<u32> {
        let display = self.window_state.refresh_rate_millihertz;
        self.game.as_ref().map_or(display.or(Some(60_000)), |game| {
            game.animation_rate(display)
        })
    }
    fn ui_animation_ready(&self) -> bool {
        self.frame_production() == crate::story_runtime::FrameProduction::Ready
    }
    pub(super) fn ui_animation_deadline(&self, now: MonoTime) -> Option<MonoTime> {
        if !self.ui_animation_enabled() || !self.ui_animation_ready() {
            return None;
        }
        if let Some(PlayerSession::Aside { story, .. }) = &self.session
            && let Some(deadline) = story.animation_deadline()
            && self.ui_animation.last.is_none_or(|last| deadline > last)
        {
            return Some(deadline);
        }
        Some(self.ui_animation.deadline(now, self.ui_animation_rate()))
    }
    pub(super) fn request_ui_frame(&mut self, host: &impl Host) {
        if !self.ui_animation_enabled() {
            self.ui_animation.reset();
            return;
        }
        let now = host.monotonic_now();
        if self
            .ui_animation_deadline(now)
            .is_some_and(|deadline| now >= deadline)
        {
            self.invalidate();
        }
    }
    pub(super) fn animate_ui(&mut self, host: &impl Host) {
        if !self.ui_animation_enabled() {
            self.ui_animation.reset();
            return;
        }
        // Sample on any current-frame production, including a story or input
        // redraw, so native animation shares that frame instead of creating one
        // at a second cadence phase.
        let elapsed = self.ui_animation.sample(host.monotonic_now());
        self.presentation
            .bridge
            .handle_window_event(reel_ui::core::WindowEvent::AnimFrame(elapsed));
        if self.presentation.bridge.root().needs_paint() {
            self.invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_animation_keeps_its_phase_after_early_and_late_frames() {
        let mut clock = UiAnimation::default();
        let rate = Some(60_000);
        let start = MonoTime(1_000_000_000);
        assert_eq!(clock.deadline(start, rate), start);
        assert_eq!(clock.sample(start), Duration::ZERO);
        assert_eq!(clock.deadline(start, rate), MonoTime(start.0 + 16_666_667));

        let input_frame = MonoTime(start.0 + 8_000_000);
        assert_eq!(clock.sample(input_frame), Duration::from_millis(8));
        assert_eq!(
            clock.deadline(input_frame, rate),
            MonoTime(start.0 + 16_666_667)
        );

        let late = MonoTime(start.0 + 35_000_000);
        assert!(clock.deadline(late, rate) <= late);
        assert_eq!(clock.sample(late), Duration::from_millis(27));
        assert_eq!(clock.deadline(late, rate), MonoTime(start.0 + 50_000_000));
    }

    #[test]
    fn native_animation_fractional_rate_skips_late_beats_and_restarts_without_idle_time() {
        let mut clock = UiAnimation::default();
        let rate = Some(59_940);
        let start = MonoTime(1_000_000_000);
        assert_eq!(clock.sample(start), Duration::ZERO);
        assert_eq!(clock.deadline(start, rate), MonoTime(start.0 + 16_683_351));

        let late = MonoTime(start.0 + 39_000_000);
        assert_eq!(clock.sample(late), Duration::from_millis(39));
        assert_eq!(clock.deadline(late, rate), MonoTime(start.0 + 50_050_051));
        let next = MonoTime(start.0 + 51_000_000);
        assert_eq!(clock.sample(next), Duration::from_millis(12));
        assert_eq!(clock.deadline(next, rate), MonoTime(start.0 + 66_733_401));
        assert_eq!(clock.deadline(next, None), next);

        clock.reset();
        let restart = MonoTime(start.0 * 10);
        assert_eq!(clock.deadline(restart, rate), restart);
        assert_eq!(clock.sample(restart), Duration::ZERO);
        assert_eq!(
            clock.deadline(restart, rate),
            MonoTime(restart.0 + 16_683_351)
        );
    }
}
