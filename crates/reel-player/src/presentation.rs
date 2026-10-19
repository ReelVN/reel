//! Shared presentation and input acceptance for every host/backend combination.
use crate::{
    demand::{Demand, Draw},
    platform::PlatformHost,
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
}

pub struct Presentation {
    pub bridge: PresentationBridge,
    pub(crate) demand: Demand,
    pub(crate) accepted_frame: Option<RenderFrame>,
    pub(crate) frame: u64,
    info: SurfaceInfo,
    suspended: bool,
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
        self.demand = Demand::idle();
    }
    pub fn invalidate(&mut self) {
        self.demand.invalidate();
    }
    pub fn exposed(&mut self) {
        self.demand.exposed();
    }
    pub fn needs_redraw(&self) -> bool {
        !self.suspended && !self.info.physical_size.contains(&0) && self.demand.has_work()
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
        self.demand.completed(out[start..].iter().any(|event| {
            matches!(
                event,
                RenderEvent::RenderFinished { .. } | RenderEvent::ResourcesAvailable
            )
        }));
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
        let draw = self.demand.draw().expect("eligible draw");
        let frame = match draw {
            Draw::Current => {
                self.frame = self
                    .frame
                    .checked_add(1)
                    .ok_or("frame identity exhausted")?;
                let Some(frame) = build(&mut self.bridge, FrameId(self.frame), self.info)? else {
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
        };
        let result = backend.render_present(&frame, || host.before_present());
        let accepted = if draw == Draw::Current {
            self.bridge.resolve(presentation_notice(
                &frame,
                self.info.surface_epoch,
                result.as_ref().map(|value| *value),
            )) == NoticeResult::Accepted
        } else {
            false
        };
        match result {
            Ok(PresentOutcome::PresentAccepted(_)) => {
                if accepted {
                    self.accepted_frame = Some(frame);
                }
                self.demand.presented(draw);
                Ok(PresentationResult {
                    accepted,
                    refused: None,
                })
            }
            Ok(PresentOutcome::SurfaceUnavailable) => Ok(PresentationResult::default()),
            Ok(PresentOutcome::Skipped(_)) => {
                self.demand.presented(draw);
                Ok(PresentationResult::default())
            }
            Err(error) => {
                let kind = error.kind();
                if !matches!(kind, FailureKind::Pressure | FailureKind::Refused) {
                    return Err(Box::new(error));
                }
                self.demand.refused(draw, kind);
                if kind == FailureKind::Refused
                    && draw == Draw::Current
                    && self.accepted_frame.is_some()
                {
                    self.demand.exposed();
                }
                Ok(PresentationResult {
                    accepted: false,
                    refused: Some(Box::new(error)),
                })
            }
        }
    }
}
