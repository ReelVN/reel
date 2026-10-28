//! Shared presentation and input acceptance for every host/backend combination.
use crate::{
    demand::{Demand, Draw},
    hal::PlatformHost,
};
use reel_core::identity::FrameId;
use reel_render::{
    BackendError, FailureKind, PresentOutcome, RenderBackend, RenderEvent, SurfaceInfo,
    presentation_notice,
};
use reel_ui::{
    bridge::{NoticeResult, PresentationBridge},
    core::WindowEvent,
    dpi::PhysicalSize,
    frame::RenderFrame,
};

type Error = Box<dyn std::error::Error>;

#[derive(Default)]
pub struct PresentationResult {
    /// Only a new, actually presented candidate can install new input geometry.
    pub accepted: bool,
    pub refused: Option<Error>,
    /// Missing pixels for the exact retained candidate.
    pub(crate) pixels: Option<(reel_ui::bridge::CandidateKey, reel_render::PixelDemand)>,
}

pub struct Presentation {
    pub bridge: PresentationBridge,
    pub(crate) demand: Demand,
    pub(crate) accepted_frame: Option<RenderFrame>,
    pub(crate) frame: u64,
    info: SurfaceInfo,
    suspended: bool,
    preparing: Option<PreparingFrame>,
}

struct PreparingFrame {
    frame: RenderFrame,
    draw: Draw,
    invalidated: bool,
    ready: bool,
}

impl Presentation {
    pub fn new(bridge: PresentationBridge, info: SurfaceInfo) -> Self {
        Self {
            bridge,
            info,
            demand: Demand::default(),
            accepted_frame: None,
            frame: 0,
            suspended: false,
            preparing: None,
        }
    }
    pub fn surface_info(&self) -> SurfaceInfo {
        self.info
    }
    /// Invalidate input before native reconfiguration, including failed configuration.
    pub fn replace_surface(&mut self, info: SurfaceInfo) -> bool {
        if !self.bridge.replace_surface(info.surface_epoch) {
            return false;
        }
        self.info = info;
        self.bridge
            .handle_window_event(WindowEvent::Rescale(info.scale_factor));
        self.bridge
            .handle_window_event(WindowEvent::Resize(PhysicalSize::new(
                info.physical_size[0],
                info.physical_size[1],
            )));
        self.accepted_frame = None;
        self.preparing = None;
        self.suspended = false;
        self.demand = if info.physical_size.contains(&0) {
            Demand::idle()
        } else {
            Demand::default()
        };
        true
    }
    pub fn suspend(&mut self) {
        self.suspended = true;
        self.bridge.clear();
        self.accepted_frame = None;
        self.preparing = None;
        self.demand = Demand::idle();
    }
    pub fn is_suspended(&self) -> bool {
        self.suspended
    }
    pub fn invalidate(&mut self) {
        if let Some(preparing) = &mut self.preparing {
            preparing.invalidated = true;
        }
        self.demand.invalidate();
    }
    /// Whether the backend is continuing preparation of one recorded candidate.
    pub fn is_preparing(&self) -> bool {
        self.preparing.is_some()
    }
    /// Cancel a candidate's preparation while submitted GPU work retires normally.
    pub fn cancel_preparation(&mut self, backend: &mut impl RenderBackend) {
        if let Some(preparing) = self.preparing.take() {
            backend.cancel_preparation(&preparing.frame);
            if preparing.draw == Draw::Current {
                self.bridge
                    .resolve(reel_ui::bridge::PresentationNotice::new(
                        preparing.frame.key(),
                        self.info.surface_epoch,
                        reel_ui::bridge::PresentationOutcome::Skipped,
                    ));
            }
            self.demand = Demand::default();
        }
    }
    pub fn exposed(&mut self) {
        self.demand.exposed();
    }
    pub fn needs_redraw(&self) -> bool {
        !self.suspended
            && !self.info.physical_size.contains(&0)
            && self
                .preparing
                .as_ref()
                .map_or_else(|| self.demand.has_work(), |frame| frame.ready)
    }
    /// Continue only the candidate whose missing pixels have been restored.
    pub(crate) fn pixels_ready(&mut self, key: &reel_ui::bridge::CandidateKey) -> bool {
        if let Some(preparing) = &mut self.preparing
            && &preparing.frame.key() == key
        {
            preparing.ready = true;
            self.demand.completed(true);
            return true;
        }
        false
    }
    pub(crate) fn pixels_failed(
        &mut self,
        key: &reel_ui::bridge::CandidateKey,
        backend: &mut impl RenderBackend,
    ) {
        if self
            .preparing
            .as_ref()
            .is_some_and(|pending| &pending.frame.key() == key)
        {
            let pending = self.preparing.take().unwrap();
            backend.cancel_preparation(&pending.frame);
            if pending.draw == Draw::Current {
                self.bridge
                    .resolve(reel_ui::bridge::PresentationNotice::new(
                        pending.frame.key(),
                        self.info.surface_epoch,
                        reel_ui::bridge::PresentationOutcome::Error,
                    ));
            }
            self.demand.refused(pending.draw, FailureKind::Refused);
        }
    }
    pub(crate) fn preparing_key(&self) -> Option<reel_ui::bridge::CandidateKey> {
        self.preparing.as_ref().map(|pending| pending.frame.key())
    }
    /// The caller owns a reusable result buffer and routes resource completion events.
    pub fn poll<B: RenderBackend>(
        &mut self,
        backend: &mut B,
        out: &mut Vec<RenderEvent<B::Error>>,
        limit: usize,
    ) -> Result<(), B::Error> {
        if !backend.has_pending() {
            return Ok(());
        }
        let start = out.len();
        backend.poll(out, limit)?;
        let completed = out[start..].iter().any(|event| {
            matches!(
                event,
                RenderEvent::RenderFinished { .. } | RenderEvent::ResourcesAvailable
            )
        });
        if completed && let Some(preparing) = &mut self.preparing {
            preparing.ready = true;
        }
        self.demand.completed(completed);
        Ok(())
    }
    /// Build only when current model work is eligible. Frozen repaint never runs build.
    /// `None` retains the accepted frame until the caller invalidates after preparation.
    pub fn render<B: RenderBackend, H: PlatformHost>(
        &mut self,
        backend: &mut B,
        host: &mut H,
        build: impl FnOnce(
            &mut PresentationBridge,
            FrameId,
            SurfaceInfo,
        ) -> Result<Option<RenderFrame>, Error>,
    ) -> Result<PresentationResult, Error> {
        if !self.needs_redraw() {
            return Ok(PresentationResult::default());
        }
        let continuation = self.preparing.take();
        let continuing = continuation.is_some();
        let invalidated = continuation.as_ref().is_some_and(|frame| frame.invalidated);
        let draw = continuation.as_ref().map_or_else(
            || self.demand.draw().expect("eligible draw"),
            |frame| frame.draw,
        );
        let frame = if let Some(preparing) = continuation {
            preparing.frame
        } else {
            match draw {
                Draw::Current => {
                    self.frame = self
                        .frame
                        .checked_add(1)
                        .ok_or("frame identity exhausted")?;
                    let Some(frame) = build(&mut self.bridge, FrameId(self.frame), self.info)?
                    else {
                        self.demand.preparing();
                        return Ok(PresentationResult::default());
                    };
                    frame
                }
                Draw::Accepted => {
                    let Some(frame) = &self.accepted_frame else {
                        self.demand.presented(draw);
                        return Ok(PresentationResult::default());
                    };
                    if !self.bridge.accepted_key().is_some_and(|key| {
                        key.frame_id() == frame.frame_id()
                            && key.surface_epoch() == frame.surface_epoch()
                    }) {
                        self.accepted_frame = None;
                        self.demand.presented(draw);
                        return Ok(PresentationResult::default());
                    }
                    frame.clone()
                }
            }
        };
        let result = match backend.render_present(&frame, || host.before_present()) {
            Err(error) => match error.into_pixel_demand() {
                Ok(demand) => {
                    let key = frame.key();
                    self.preparing = Some(PreparingFrame {
                        frame,
                        draw,
                        invalidated,
                        ready: false,
                    });
                    self.demand.waiting_for_pixels(draw);
                    return Ok(PresentationResult {
                        pixels: Some((key, demand)),
                        ..Default::default()
                    });
                }
                Err(error) => Err(error),
            },
            ok => ok,
        };
        if matches!(result, Ok(PresentOutcome::Preparing))
            || result
                .as_ref()
                .is_err_and(|error| error.kind() == FailureKind::Pressure)
                && backend.has_pending()
        {
            self.preparing = Some(PreparingFrame {
                frame,
                draw,
                invalidated,
                ready: false,
            });
            self.demand.refused(draw, FailureKind::Pressure);
            return Ok(PresentationResult::default());
        }
        if continuing && !matches!(result, Ok(PresentOutcome::PresentAccepted(_))) {
            backend.cancel_preparation(&frame);
        }
        let accepted = if draw == Draw::Current {
            presentation_notice(
                &frame,
                self.info.surface_epoch,
                result.as_ref().map(|value| *value),
            )
            .is_some_and(|notice| self.bridge.resolve(notice) == NoticeResult::Accepted)
        } else {
            false
        };
        match result {
            Ok(PresentOutcome::PresentAccepted(_)) => {
                if accepted {
                    self.accepted_frame = Some(frame);
                }
                self.demand.presented(draw);
                if invalidated {
                    self.demand.invalidate();
                }
                Ok(PresentationResult {
                    accepted,
                    refused: None,
                    pixels: None,
                })
            }
            Ok(PresentOutcome::SurfaceUnavailable) => Ok(PresentationResult::default()),
            Ok(PresentOutcome::Preparing) => unreachable!("continuation retained above"),
            Ok(PresentOutcome::Skipped(_)) => {
                self.demand.presented(draw);
                if invalidated {
                    self.demand.invalidate();
                }
                Ok(PresentationResult::default())
            }
            Err(error) => {
                let kind = match error.kind() {
                    // Without an outstanding completion, waiting cannot make
                    // this pressure retryable. Retire the candidate and report it.
                    FailureKind::Pressure if !backend.has_pending() => FailureKind::Refused,
                    kind => kind,
                };
                if !matches!(kind, FailureKind::Pressure | FailureKind::Refused) {
                    return Err(Box::new(error));
                }
                self.demand.refused(draw, kind);
                if invalidated {
                    self.demand.invalidate();
                }
                if kind == FailureKind::Refused
                    && draw == Draw::Current
                    && self.accepted_frame.is_some()
                {
                    self.demand.exposed();
                }
                Ok(PresentationResult {
                    accepted: false,
                    refused: Some(Box::new(error)),
                    pixels: None,
                })
            }
        }
    }
}
