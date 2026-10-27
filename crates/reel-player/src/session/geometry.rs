//! A projection receipt binds script barriers to one unforgeable candidate identity.
use super::*;
use reel_engine::aside::{GeometryQuery, GeometryReplies, NodeGeometry};
use reel_ui::{
    app::PresentedGeometryView,
    bridge::{CandidateKey, RecordedSource},
    kurbo::Affine,
    stage::StageIdentity,
};

#[derive(Debug)]
pub struct RecordedProjection {
    pub(crate) current: bool,
    pub(crate) key: CandidateKey,
    pub(crate) stamp: ProjectionStamp,
    pub(crate) stage_widget: WidgetId,
    pub(crate) identity: Option<StageIdentity>,
    pub(crate) geometry: Option<GeometryReplies>,
    pub(crate) transitions: crate::transitions::TransitionReceipt,
    pub(crate) frame_transitions: Vec<reel_ui::frame::FrameTransition>,
}
impl RecordedProjection {
    pub fn key(&self) -> CandidateKey {
        self.key.clone()
    }
    pub fn needs_transition_drawing(&self) -> bool {
        self.transitions.needs_drawing()
    }
    /// Feed the same recorded transition projection to every frame compiler.
    pub fn frame_transitions(&self) -> &[reel_ui::frame::FrameTransition] {
        &self.frame_transitions
    }
}
fn visible(view: &PresentedGeometryView, id: WidgetId) -> Option<&reel_ui::app::GeometryRecord> {
    let record = view.get(id)?;
    let mut current = record;
    loop {
        if current.is_stashed {
            return None;
        }
        match current.parent {
            Some(parent) => current = &view.records()[parent],
            None => return Some(record),
        }
    }
}
fn geometry(
    view: &PresentedGeometryView,
    node: WidgetId,
    relative: WidgetId,
) -> Option<NodeGeometry> {
    let target = visible(view, node)?;
    let reference = visible(view, relative)?;
    let determinant = reference.window_transform.determinant();
    if !determinant.is_finite() || determinant == 0. {
        return None;
    }
    let transform: Affine = reference.window_transform.inverse() * target.window_transform;
    let coefficients = transform.as_coeffs();
    if !coefficients.iter().all(|value| value.is_finite()) {
        return None;
    }
    let bounds = transform.transform_rect_bbox(target.size.to_rect());
    Some(NodeGeometry {
        width: target.size.width,
        height: target.size.height,
        bounds: [bounds.x0, bounds.y0, bounds.width(), bounds.height()],
        transform: coefficients,
        text_end: None,
        text_caret: None,
    })
}
impl SessionUi {
    pub(crate) fn stage_widget(bridge: &PresentationBridge, root: WidgetId) -> Option<WidgetId> {
        Some(
            bridge
                .root()
                .get_widget(root)?
                .downcast::<SessionWidget>()?
                .stage,
        )
    }
    pub(crate) fn transition_target(
        bridge: &PresentationBridge,
        root: WidgetId,
        target: Option<reel_ui::stage::NodeId>,
    ) -> Option<WidgetId> {
        match target {
            Some(node) => Self::projected_node(bridge, root, node),
            None => bridge
                .root()
                .get_widget(root)?
                .downcast::<SessionWidget>()
                .map(|widget| widget.content.id()),
        }
    }
    fn accepted_stage(
        &self,
        bridge: &PresentationBridge,
        root: WidgetId,
    ) -> Option<(WidgetId, Affine)> {
        self.binding.borrow().control_accepted?;
        let widget = bridge
            .root()
            .get_widget(root)?
            .downcast::<SessionWidget>()?;
        let stage_id = widget.stage;
        let stage = bridge
            .root()
            .get_widget(stage_id)?
            .downcast::<StageWidget>()?;
        if stage.identity != self.stage.as_ref()?.identity {
            return None;
        }
        let reel_ui::app::InputGeometrySource::Presented(Some(view)) =
            bridge.root().input_geometry_source()
        else {
            return None;
        };
        let transform = visible(view, stage_id)?.window_transform;
        (transform.determinant().is_finite() && transform.determinant() != 0.)
            .then_some((stage_id, transform))
    }
    pub(crate) fn pointer_position(
        &self,
        bridge: &PresentationBridge,
        root: WidgetId,
    ) -> Option<[f64; 2]> {
        let (_, transform) = self.accepted_stage(bridge, root)?;
        let point = transform.inverse() * bridge.root().mouse_position()?;
        Some([point.x, point.y])
    }
    pub(crate) fn pointer_destination(
        &self,
        bridge: &PresentationBridge,
        root: WidgetId,
        owner: reel_ui::stage::NodeId,
        position: [f64; 2],
    ) -> Option<[f64; 2]> {
        bridge.root().mouse_position()?;
        let (_, transform) = self.accepted_stage(bridge, root)?;
        if !bridge
            .root()
            .permits_input(Self::projected_node(bridge, root, owner)?)
        {
            return None;
        }
        let point = transform * reel_ui::kurbo::Point::new(position[0], position[1]);
        Some([point.x, point.y])
    }
    /// Apply author focus through the same accepted stage and modal input geometry.
    pub(crate) fn request_focus(
        &self,
        bridge: &mut PresentationBridge,
        root: WidgetId,
        target: reel_ui::stage::NodeId,
        visibility: reel_ui::input::FocusVisibility,
    ) -> bool {
        if self.accepted_stage(bridge, root).is_none() {
            return false;
        }
        let Some(target) = Self::projected_node(bridge, root, target) else {
            return false;
        };
        bridge.request_focus(target, visibility)
    }
    /// Record the actual widget tree and its projection identity in one operation.
    /// Pending model edits cannot advance the watermark until sync publishes them.
    pub fn record(
        &self,
        bridge: &mut PresentationBridge,
        root: WidgetId,
        id: reel_core::identity::FrameId,
    ) -> Result<(RecordedSource, RecordedProjection), Error> {
        let source = bridge
            .record(id)
            .map_err(|error| format!("record: {error:?}"))?;
        let widget = bridge
            .root()
            .get_widget(root)
            .and_then(|widget| widget.downcast::<SessionWidget>())
            .ok_or("recorded session widget is unavailable")?;
        let stage_widget = widget.stage;
        let stage = bridge
            .root()
            .get_widget(stage_widget)
            .and_then(|widget| widget.downcast::<StageWidget>())
            .ok_or("recorded stage widget is unavailable")?;
        let receipt = RecordedProjection {
            current: !self.images_pending(),
            key: source.key(),
            stamp: widget.current_stamp(),
            stage_widget,
            identity: stage.identity.clone(),
            geometry: None,
            transitions: Default::default(),
            frame_transitions: Vec::new(),
        };
        Ok((source, receipt))
    }
    pub(crate) fn read_geometry(
        &self,
        bridge: &PresentationBridge,
        source: &RecordedSource,
        root: WidgetId,
        stage_widget: WidgetId,
        query: GeometryQuery,
    ) -> Option<NodeGeometry> {
        let stage = bridge
            .root()
            .get_widget(Self::stage_for_node(bridge, root, query.node)?)?
            .downcast::<StageWidget>()?;
        let node = Self::projected_node(bridge, root, query.node)?;
        let relative = match query.relative_to {
            Some(node) => Self::projected_node(bridge, root, node)?,
            None => stage_widget,
        };
        let view = source.geometry();
        let mut result = geometry(view, node, relative)?;
        if let Some((label, caret)) = stage.text_caret(bridge.root(), query.node) {
            let label = visible(view, label)?;
            let reference = visible(view, relative)?;
            let caret = reference.window_transform.inverse() * label.window_transform * caret;
            let bounds = reel_ui::kurbo::Rect::from_points(caret.p0, caret.p1);
            result.text_end = Some([caret.p1.x, caret.p1.y]);
            result.text_caret = Some([bounds.x0, bounds.y0, bounds.width(), bounds.height()]);
        }
        Some(result)
    }
    /// Accept only the exact frame currently accepted by the bridge. Old epochs and
    /// stage instances cannot supply input confirmation or presentation results.
    pub fn presented(&mut self, bridge: &PresentationBridge, frame: &RecordedProjection) -> bool {
        if bridge.accepted_geometry(&frame.key).is_none()
            || frame.identity != self.stage.as_ref().and_then(|view| view.identity.clone())
        {
            return false;
        }
        let stamp = frame.stamp;
        let current_stamp = self.projection_stamp();
        let mut binding = self.binding.borrow_mut();
        binding.control_accepted = Some(stamp);
        binding.accepted_buttons = Some(stamp.button_revision);
        binding.accepted =
            (frame.current && !self.images_pending() && stamp == current_stamp).then_some(stamp);
        if frame.current && !self.images_pending() && stamp == current_stamp {
            self.images_waiting_presentation = false;
        }
        true
    }
}
