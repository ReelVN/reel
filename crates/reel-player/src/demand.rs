use reel_render::gpu::GpuError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Model {
    Pending,
    Waiting,
    Idle,
    Blocked,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Exposure {
    #[default]
    None,
    Pending,
    Waiting,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Draw {
    Current,
    Accepted,
}
/// Model changes and OS content invalidation are independent obligations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Demand {
    pub(crate) model: Model,
    exposure: Exposure,
}
impl Default for Demand {
    fn default() -> Self {
        Self {
            model: Model::Pending,
            exposure: Exposure::None,
        }
    }
}
impl Demand {
    pub(crate) fn idle() -> Self {
        Self {
            model: Model::Idle,
            exposure: Exposure::None,
        }
    }
    pub(crate) fn invalidate(&mut self) {
        if matches!(self.model, Model::Idle | Model::Blocked) {
            self.model = Model::Pending;
        }
    }
    pub(crate) fn exposed(&mut self) {
        // Duplicate OS redraws cannot bypass a GPU completion wait.
        if self.exposure != Exposure::Waiting {
            self.exposure = Exposure::Pending;
        }
    }
    pub(crate) fn draw(self) -> Option<Draw> {
        if self.model == Model::Pending
            || (self.model == Model::Idle && self.exposure == Exposure::Pending)
        {
            Some(Draw::Current)
        } else if self.exposure == Exposure::Pending {
            Some(Draw::Accepted)
        } else {
            None
        }
    }
    pub(crate) fn has_work(self) -> bool {
        self.draw().is_some()
    }
    pub(crate) fn refused(&mut self, draw: Draw, error: &GpuError) {
        if draw == Draw::Current {
            self.model = if error.is_retryable() {
                Model::Waiting
            } else {
                Model::Blocked
            };
        }
        self.exposure = if error.is_retryable() {
            Exposure::Waiting
        } else {
            Exposure::None
        };
    }
    pub(crate) fn presented(&mut self, draw: Draw) {
        if draw == Draw::Current {
            self.model = Model::Idle;
        }
        self.exposure = Exposure::None;
    }
    pub(crate) fn completed(&mut self, any: bool) {
        if any {
            if self.model == Model::Waiting {
                self.model = Model::Pending;
            }
            if self.exposure == Exposure::Waiting {
                self.exposure = Exposure::Pending;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use reel_ui::core::Widget;
    use reel_ui::frame::RecordLocation;
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
            let mut demand = Demand::default();
            demand.refused(Draw::Current, &error);
            demand.invalidate();
            for _ in 0..100 {
                demand.exposed();
                demand.completed(false);
                assert!(!demand.has_work());
            }
            demand.completed(true);
            assert_eq!(demand.draw(), Some(Draw::Current));
        }
    }
    #[test]
    fn permanent_errors_never_retry_current_model_on_expose_or_completion() {
        let at = RecordLocation {
            fragment: 0,
            command: 3,
            mask_depth: 1,
            owner: reel_ui::widgets::Label::new("").prepare().id(),
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
            let mut demand = Demand::default();
            demand.refused(Draw::Current, &error);
            for _ in 0..100 {
                demand.completed(true);
                assert!(!demand.has_work());
                demand.exposed();
                assert_eq!(demand.draw(), Some(Draw::Accepted));
                demand.presented(Draw::Accepted);
                assert_eq!(demand.model, Model::Blocked);
            }
            demand.invalidate();
            assert_eq!(demand.draw(), Some(Draw::Current));
        }
    }
    #[test]
    fn successful_idle_expose_redraws_once_without_poll_loop() {
        let mut demand = Demand::default();
        demand.presented(Draw::Current);
        assert!(!demand.has_work());
        demand.exposed();
        assert_eq!(demand.draw(), Some(Draw::Current));
        demand.presented(Draw::Current);
        for _ in 0..100 {
            demand.completed(true);
            assert!(!demand.has_work());
        }
    }
    #[test]
    fn accepted_repaint_pressure_preserves_permanent_block() {
        let mut demand = Demand::default();
        demand.refused(
            Draw::Current,
            &GpuError::Budget {
                domain: "targets",
                requested: 10,
                available: 0,
            },
        );
        demand.exposed();
        demand.refused(Draw::Accepted, &GpuError::Busy);
        demand.exposed();
        assert!(!demand.has_work());
        demand.completed(true);
        assert_eq!(demand.draw(), Some(Draw::Accepted));
        demand.presented(Draw::Accepted);
        assert_eq!(demand.model, Model::Blocked);
        assert!(!demand.has_work());
    }
}
