use reel_render::gpu::GpuError;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Demand {
    #[default]
    Pending,
    Waiting,
    Idle,
}
impl Demand {
    pub(crate) fn invalidate(&mut self) {
        if *self == Self::Idle {
            *self = Self::Pending;
        }
    }
    pub(crate) fn can_prepare(self) -> bool {
        self == Self::Pending
    }
    pub(crate) fn refused(&mut self, error: &GpuError) {
        *self = if error.is_retryable() {
            Self::Waiting
        } else {
            Self::Idle
        };
    }
    pub(crate) fn completed(&mut self, any: bool) {
        if any && *self == Self::Waiting {
            *self = Self::Pending;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use masonry_core::core::Widget;
    use reel_stage::RecordLocation;
    #[test]
    fn pressure_and_busy_wait_for_completion_then_retry_latest_state() {
        for error in [
            GpuError::Busy,
            GpuError::Pressure {
                domain: "targets",
                requested: 10,
                available: 0,
            },
        ] {
            let mut demand = Demand::Pending;
            let mut latest = 1;
            demand.refused(&error);
            latest += 1;
            demand.invalidate();
            for _ in 0..100 {
                demand.completed(false);
                assert!(!demand.can_prepare());
            }
            demand.completed(true);
            assert!(demand.can_prepare());
            assert_eq!(latest, 2);
        }
    }
    #[test]
    fn permanent_errors_do_not_repeat_preparation() {
        let at = RecordLocation {
            fragment: 0,
            command: 3,
            mask_depth: 1,
            owner: masonry::widgets::Label::new("").prepare().id(),
        };
        for error in [
            GpuError::Budget {
                domain: "targets",
                requested: 10,
                available: 0,
            },
            GpuError::DepthLimit { at, limit: 2 },
            GpuError::InvalidRecord {
                at,
                record: "unclosed scope",
            },
        ] {
            let mut demand = Demand::Pending;
            demand.refused(&error);
            for _ in 0..100 {
                demand.completed(true);
                assert!(!demand.can_prepare());
            }
            demand.invalidate();
            assert!(demand.can_prepare());
        }
    }
}
