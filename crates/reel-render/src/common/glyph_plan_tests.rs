use super::*;
use crate::test_support::r01_fixture;
use reel_core::identity::{AssetKey, ContentVersion, FrameId, ResourceVersion};
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, VisualLayerKind, WindowSizePolicy},
    core::{DefaultProperties, NewWidget, StyleProperty},
    dpi::PhysicalSize,
    imaging::{
        Composite,
        record::{Command, Draw, Group, Scene},
    },
    peniko::{Blob, Brush, Color},
    resources::{ResourceData, ResourceRegistry},
    widgets::Label,
};
use std::sync::Arc;

#[derive(Default)]
struct Probe {
    glyphs: usize,
    densities: Vec<f64>,
    reserved: u64,
}
impl Prepare for Probe {
    fn reserve(&mut self, bytes: u64) -> Result<(), GpuError> {
        self.reserved += bytes;
        Ok(())
    }
    fn run(
        &mut self,
        _: &FrameDrawing,
        run: &FrameGlyphRun,
        density: f64,
        _: RecordLocation,
    ) -> Result<InkBounds, GpuError> {
        self.glyphs += run.glyphs.len();
        self.densities.push(density);
        Ok(run.glyphs.iter().fold(InkBounds::Empty, |ink, position| {
            union(
                ink,
                InkBounds::Rect(bitmap_bounds(
                    [position.x, position.y],
                    [-3., -17.],
                    [15, 21],
                    density,
                )),
            )
        }))
    }
}

#[test]
fn recorded_reveal_prepares_only_visible_glyphs_and_bounds_the_group() {
    let pool = r01_fixture::pool();
    let font = Blob::from(
        include_bytes!(
            "../../../../examples/author_arcade/resources/fonts/AtkinsonHyperlegible-Regular.ttf"
        )
        .to_vec(),
    );
    let mut registry = ResourceRegistry::new(1, 65536).unwrap();
    let handle = registry
        .register(
            ResourceVersion {
                key: AssetKey("glyph-plan-font".into()),
                version: ContentVersion([1; 32]),
            },
            ResourceData::Font(font.clone()),
            pool.reserve(font.len() as u64).unwrap(),
        )
        .unwrap();
    let mut root = RenderRoot::new(
        NewWidget::new(
            Label::new("jAV hidden letters")
                .with_style(StyleProperty::FontSize(24.))
                .with_visible_bytes(0..1),
        ),
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(400, 300),
            scale_factor: 1.,
            default_font: Some(font),
        },
    );
    let (layers, _) = root.redraw();
    let mut scene = Scene::new();
    scene.push_group(Group {
        composite: Composite {
            alpha: 0.6,
            ..Default::default()
        },
        ..Default::default()
    });
    let mut recorded = 0;
    for layer in &layers.layers {
        let VisualLayerKind::Scene(source) = &layer.kind else {
            continue;
        };
        for command in source.commands() {
            let Command::Draw(id) = command else {
                continue;
            };
            if let Draw::GlyphRun(run) = source.draw_op(*id) {
                recorded += run.glyphs.len();
                scene.draw(Draw::GlyphRun(run.clone()));
            }
        }
    }
    assert_eq!(
        recorded, 1,
        "Label must omit the hidden suffix before rendering preparation"
    );
    scene.draw(Draw::Fill {
        transform: Affine::translate((80., 10.)),
        fill_rule: Default::default(),
        brush: Brush::Solid(Color::from_rgb8(255, 255, 255)),
        brush_transform: None,
        shape: Geometry::Rect(Rect::new(0., 0., 20., 30.)),
        composite: Default::default(),
    });
    scene.pop_group();
    let mut excluded = r01_fixture::RecordWidget::new(scene.clone());
    excluded.capture_included = false;
    let mut widget = r01_fixture::RecordWidget::new(scene);
    widget.clip = false;
    let mut bridge = r01_fixture::widget_bridge(widget, Affine::IDENTITY);
    let snapshot = registry.freeze(&[handle], &pool).unwrap();
    let frame = r01_fixture::build(bridge.record(FrameId(1)).unwrap(), snapshot, &pool);
    let group = frame
        .commands()
        .iter()
        .find_map(|command| match &command.op {
            FrameOp::PushGroup(group) if group.composite.alpha == 0.6 => Some(group),
            _ => None,
        })
        .unwrap();
    assert_eq!(group.ink_bounds, InkBounds::Unbounded);
    let mut probe = Probe::default();
    let plan = Plan::collect(
        std::iter::once(&frame),
        Affine::scale(1.5),
        4096,
        false,
        &mut probe,
    )
    .unwrap();
    assert_eq!(probe.glyphs, recorded);
    assert!(probe.densities.iter().all(|density| *density == 2.));
    let InkBounds::Rect(ink) = plan.group_ink(group) else {
        panic!("prepared glyphs must give finite group ink");
    };
    assert!(
        ink.x0 <= 80. && ink.x1 >= 100. && ink.y0 <= 10. && ink.y1 >= 40.,
        "non-text drawing remains in the bound"
    );
    assert!(ink.width() < 150. && ink.height() < 100.);
    assert!(probe.reserved > 0);

    let mut bridge = r01_fixture::widget_bridge(excluded, Affine::IDENTITY);
    let frame = r01_fixture::build(
        bridge.record(FrameId(2)).unwrap(),
        registry.freeze(&[handle], &pool).unwrap(),
        &pool,
    );
    let mut probe = Probe::default();
    Plan::collect(
        std::iter::once(&frame),
        Affine::IDENTITY,
        4096,
        true,
        &mut probe,
    )
    .unwrap();
    assert_eq!(
        probe.glyphs, 0,
        "capture exclusions omit the real glyph run"
    );
}
