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
    pub(crate) key: CandidateKey,
    pub(crate) stamp: ProjectionStamp,
    pub(crate) identity: Option<StageIdentity>,
    pub(crate) geometry: Option<GeometryReplies>,
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
    })
}
impl SessionUi {
    /// Freeze the projection watermark beside the source produced by bridge.record().
    pub fn recorded(&self, source: &RecordedSource) -> RecordedProjection {
        RecordedProjection {
            key: source.key(),
            stamp: self.projection_stamp(),
            identity: self.stage.as_ref().and_then(|view| view.identity.clone()),
            geometry: None,
        }
    }
    pub(crate) fn read_geometry(
        &self,
        bridge: &PresentationBridge,
        source: &RecordedSource,
        query: GeometryQuery,
    ) -> Option<NodeGeometry> {
        let stage = bridge
            .root()
            .get_widget(self.stage_widget?)?
            .downcast::<StageWidget>()?;
        let node = stage.node_widget(query.node)?;
        let relative = match query.relative_to {
            Some(node) => stage.node_widget(node)?,
            None => self.stage_widget?,
        };
        geometry(source.geometry(), node, relative)
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
        let mut binding = self.binding.borrow_mut();
        binding.control_accepted = Some(stamp);
        binding.accepted_buttons = Some(stamp.button_revision);
        binding.accepted = (!self.images_pending()).then_some(stamp);
        true
    }
}
