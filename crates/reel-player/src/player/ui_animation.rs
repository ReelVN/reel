//! Native widget animation shares the player's clock, cadence and frame admission.
use super::*;
use std::time::Duration;

#[derive(Default)]
pub(crate) struct UiAnimation {
    last: Option<MonoTime>,
}
impl UiAnimation {
    pub(super) fn reset(&mut self) {
        self.last = None;
    }
    fn deadline(&self, now: MonoTime, period: u64) -> MonoTime {
        self.last
            .map_or(now, |last| MonoTime(last.0.saturating_add(period)))
    }
    fn sample(&mut self, now: MonoTime) -> Duration {
        let previous = self.last.replace(now).unwrap_or(now);
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
    fn ui_animation_period(&self) -> u64 {
        let rate = self
            .game
            .as_ref()
            .and_then(|game| game.frame_rate_millihertz)
            .or(self.window_state.refresh_rate_millihertz)
            .unwrap_or(60_000);
        1_000_000_000_000u64.div_ceil(u64::from(rate))
    }
    fn ui_animation_ready(&self) -> bool {
        self.frame_production() == crate::story_runtime::FrameProduction::Ready
    }
    pub(super) fn ui_animation_deadline(&self, now: MonoTime) -> Option<MonoTime> {
        (self.ui_animation_enabled() && self.ui_animation_ready())
            .then(|| self.ui_animation.deadline(now, self.ui_animation_period()))
    }
    pub(super) fn request_ui_frame(&mut self, host: &impl Host) {
        if !self.ui_animation_enabled() {
            self.ui_animation.reset();
            return;
        }
        if !self.ui_animation_ready() {
            return;
        }
        let now = host.monotonic_now();
        if now >= self.ui_animation.deadline(now, self.ui_animation_period()) {
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
    fn native_animation_samples_follow_cadence_and_restart_without_idle_time() {
        let mut clock = UiAnimation::default();
        let period = 16_666_666;
        let start = MonoTime(1_000_000_000);
        assert_eq!(clock.sample(start), Duration::ZERO);
        assert_eq!(clock.deadline(start, period), MonoTime(start.0 + period));
        assert_eq!(
            clock.sample(MonoTime(start.0 + period / 2)),
            Duration::from_nanos(period / 2)
        );
        assert_eq!(
            clock.deadline(start, period),
            MonoTime(start.0 + period / 2 + period)
        );
        assert_eq!(
            clock.sample(MonoTime(start.0 + period * 5)),
            Duration::from_nanos(period * 5 - period / 2)
        );
        clock.reset();
        assert_eq!(clock.sample(MonoTime(start.0 * 10)), Duration::ZERO);
    }
}
