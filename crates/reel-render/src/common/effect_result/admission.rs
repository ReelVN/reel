//! Optional lifetime extensions must leave the admitted scratch plan available.
#[derive(Default)]
pub(crate) struct Admission {
    enabled: bool,
    remaining: u64,
}
impl Admission {
    /// `required` is the selected physical plan, including coverage and retained
    /// inputs. A retained result can exclude one more scratch allocation from
    /// reuse, so charge its complete backend cost in addition to that plan.
    pub(crate) fn new(required: u64, limit: u64, enabled: bool) -> Self {
        Self {
            enabled,
            remaining: limit.saturating_sub(required),
        }
    }
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }
    pub(crate) fn can_retain(&self, bytes: u64) -> bool {
        self.enabled && bytes <= self.remaining
    }
    /// Called once after the frame successfully acquires a distinct physical
    /// result. Repeated reads use its existing pin and reservation.
    pub(crate) fn retain(&mut self, bytes: u64) {
        assert!(self.can_retain(bytes), "admitted effect lifetime extension");
        self.remaining -= bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lifetime_extensions_leave_the_full_original_plan_and_quality_available() {
        // A three-target scratch peak fits exactly. Holding early results until
        // submission must not turn this into a four-target allocation failure.
        let exact = Admission::new(3 * 4096, 3 * 4096, true);
        assert!(!exact.can_retain(4096));
        let mut spare = Admission::new(3 * 4096, 4 * 4096, true);
        assert!(spare.can_retain(4096));
        spare.retain(4096);
        assert!(!spare.can_retain(4096));
        let retry = Admission::new(3 * 4096, 4 * 4096, false);
        assert!(!retry.enabled());
        assert!(!retry.can_retain(4096));
    }
}
