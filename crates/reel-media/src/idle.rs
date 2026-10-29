//! Non-RT maintenance polls retain a bounded native-error/PCM-flag fallback.
use std::time::Duration;
const INITIAL: Duration = Duration::from_millis(2);
const MAXIMUM: Duration = Duration::from_millis(16);

pub(crate) struct QuietWait(Duration);
impl QuietWait {
    pub fn new() -> Self {
        Self(INITIAL)
    }
    pub fn timeout(&mut self, productive: bool, no_producer: bool) -> Option<Duration> {
        if no_producer {
            self.0 = INITIAL;
            return None;
        }
        if productive {
            self.0 = INITIAL;
        }
        let timeout = self.0;
        self.0 = (timeout * 2).min(MAXIMUM);
        Some(timeout)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dormant_wait_and_active_fallback_remain_bounded() {
        let mut wait = QuietWait::new();
        assert_eq!(wait.timeout(false, true), None);
        for millis in [2, 4, 8, 16, 16, 16] {
            assert_eq!(
                wait.timeout(false, false),
                Some(Duration::from_millis(millis))
            );
        }
        assert_eq!(wait.timeout(true, false), Some(INITIAL));
        assert_eq!(wait.timeout(false, true), None);
    }
}
