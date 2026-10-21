//! Real Masonry recording widget with controlled pre/own/post records and scrolling.
use super::fixture::{StageGroup, root};
use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    identity::SurfaceEpoch,
};
use reel_ui::{
    accesskit::{Node, Role},
    app::RenderRoot,
    core::*,
    imaging::{Painter, record::Scene},
    kurbo::{Affine, Axis, Rect, Size, Vec2},
    layout::{LenReq, Length},
};
use reel_ui::{bridge::*, frame::*, resources::*};
use std::{cell::RefCell, rc::Rc};
pub struct RecordWidget {
    pub inner: StageGroup,
    pub pre: Scene,
    pub own: Scene,
    pub post: Scene,
    pub mode: PaintLayerMode,
    pub token: u64,
    pub scroll: Vec2,
    pub clip: bool,
    pub opacity: f32,
    pub invert: f32,
    pub blend: reel_ui::peniko::BlendMode,
    pub mask: Option<PaintImageMask>,
    pub capture_included: bool,
}
impl RecordWidget {
    pub fn new(own: Scene) -> Self {
        Self {
            inner: StageGroup::new(
                Rc::new(RefCell::new(vec![])),
                1.,
                Rect::new(0., 0., 400., 300.),
            ),
            pre: Scene::new(),
            own,
            post: Scene::new(),
            mode: PaintLayerMode::Inline,
            token: 0,
            scroll: Vec2::ZERO,
            clip: true,
            opacity: 1.,
            invert: 0.,
            blend: Default::default(),
            mask: None,
            capture_included: true,
        }
    }
}
impl Widget for RecordWidget {
    type Action = ();
    fn capture_included(&self) -> bool {
        self.capture_included
    }
    fn register_children(&mut self, c: &mut RegisterCtx<'_>) {
        self.inner.register_children(c)
    }
    fn measure(
        &mut self,
        c: &mut MeasureCtx<'_>,
        p: &PropertiesRef<'_>,
        a: Axis,
        l: LenReq,
        n: Option<Length>,
    ) -> Length {
        self.inner.measure(c, p, a, l, n)
    }
    fn layout(&mut self, c: &mut LayoutCtx<'_>, p: &PropertiesRef<'_>, s: Size) {
        self.inner.layout(c, p, s);
        if !self.clip {
            c.clear_clip_path();
        }
    }
    fn compose(&mut self, c: &mut ComposeCtx<'_>) {
        for (child, _, _) in &mut self.inner.children {
            c.set_animated_child_scroll_translation(child, self.scroll);
        }
    }
    fn pre_paint(&mut self, c: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, p: &mut Painter<'_>) {
        c.set_subtree_composite(self.opacity, self.invert, self.blend, self.mask.clone());
        p.replay(&self.pre)
    }
    fn paint(&mut self, c: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, p: &mut Painter<'_>) {
        if self.mode == PaintLayerMode::External {
            c.set_external_paint(self.token);
        } else {
            c.set_paint_layer_mode(self.mode);
        }
        p.replay(&self.own)
    }
    fn post_paint(&mut self, _: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, p: &mut Painter<'_>) {
        p.replay(&self.post)
    }
    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }
    fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, _: &mut Node) {}
    fn children_ids(&self) -> ChildrenIds {
        self.inner.children_ids()
    }
    fn accepts_pointer_interaction(&self) -> bool {
        true
    }
}
pub fn limits() -> BuildLimits {
    BuildLimits {
        max_bytes: 16 * 1024 * 1024,
    }
}
pub fn step_budget() -> StepBudget {
    StepBudget {
        max_commands: 2,
        max_units: 128,
        max_bytes: 4096,
    }
}
pub fn pool() -> BudgetPool {
    BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 16,
            max_bytes: 256 * 1024 * 1024,
        },
    )
}
pub fn bridge(root: RenderRoot) -> PresentationBridge {
    PresentationBridge::new(
        root,
        SurfaceEpoch(1),
        BridgeLimits {
            geometry: reel_ui::app::GeometryLimits {
                max_bytes: 4 * 1024 * 1024,
            },
            max_candidates: 4,
            max_bridge_bytes: 64 * 1024,
            max_snapshot_bytes: 24 * 1024 * 1024,
        },
    )
    .unwrap()
}
pub fn widget_bridge(widget: RecordWidget, transform: Affine) -> PresentationBridge {
    bridge(root(widget.prepare().with_transform(transform)).0)
}
pub fn build(source: RecordedSource, snapshot: ResourceSnapshot, pool: &BudgetPool) -> RenderFrame {
    let mut build = FrameBuild::new(
        source,
        snapshot,
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        limits(),
        pool,
    )
    .unwrap();
    let mut slice = step_budget();
    for _ in 0..100_000 {
        let p = build.step(slice).unwrap();
        assert!(
            p.units <= slice.max_units
                && p.bytes <= slice.max_bytes
                && p.commands <= slice.max_commands
        );
        if p.complete {
            return build.finish().unwrap();
        }
        slice.max_units = slice.max_units.max(p.minimum_units);
        slice.max_bytes = slice.max_bytes.max(p.minimum_bytes);
    }
    panic!("bounded build made no final progress")
}
pub fn empty_snapshot(pool: &BudgetPool) -> ResourceSnapshot {
    ResourceRegistry::new(0, 0)
        .unwrap()
        .freeze(&[], pool)
        .unwrap()
}
