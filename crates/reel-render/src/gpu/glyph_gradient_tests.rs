//! Actual Label shaping, device submission and numerical glyph-paint readback.
use super::*;
use reel_ui::{
    WritingMode,
    app::{RenderRoot, RenderRootOptions, VisualLayerKind, WindowSizePolicy},
    core::{
        DefaultProperties, NewWidget, RevealFade, StyleProperty, TextGradient, TextGradientSpace,
    },
    dpi::PhysicalSize,
    frame::{FrameDraw, FrameGlyphRun, FrameOp},
    imaging::record::Command,
    kurbo::{Point, Vec2},
    peniko::{Blob, GradientKind, color::ColorSpaceTag},
    resources::{ResourceData, ResourceHandle, ResourceRegistry, ResourceSnapshot},
    widgets::Label,
};

fn font_fixture() -> (BudgetPool, Blob<u8>, ResourceRegistry, ResourceHandle) {
    let pool = pool();
    let bytes = std::fs::read(
        std::env::var_os("REEL_R01_TEST_FONT").expect("REEL_R01_TEST_FONT with CJK and Latin"),
    )
    .unwrap();
    let ticket = pool.reserve(bytes.len() as u64).unwrap();
    let font = Blob::from(bytes);
    let mut registry = ResourceRegistry::new(1, 65264).unwrap();
    let handle = registry
        .register(
            reel_core::identity::ResourceVersion {
                key: reel_core::identity::AssetKey("gradient-font".into()),
                version: reel_core::identity::ContentVersion([1; 32]),
            },
            ResourceData::Font(font.clone()),
            ticket,
        )
        .unwrap();
    (pool, font, registry, handle)
}

fn label_frame(
    label: Label,
    font: &Blob<u8>,
    snapshot: ResourceSnapshot,
    pool: &BudgetPool,
    brush_transform: Option<Affine>,
    color_space: ColorSpaceTag,
) -> RenderFrame {
    let mut root = RenderRoot::new(
        NewWidget::new(label),
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(400, 300),
            scale_factor: 1.,
            default_font: Some(font.clone()),
        },
    );
    let (plan, _) = root.redraw();
    let mut recording = Scene::new();
    for layer in &plan.layers {
        let VisualLayerKind::Scene(scene) = &layer.kind else {
            continue;
        };
        for command in scene.commands() {
            let Command::Draw(id) = command else {
                continue;
            };
            let Draw::GlyphRun(run) = scene.draw_op(*id) else {
                continue;
            };
            let mut run = run.clone();
            assert!(run.glyphs.iter().all(|glyph| glyph.id != 0));
            let Brush::Gradient(gradient) = &mut run.brush else {
                panic!("Label must produce the real gradient glyph path")
            };
            // The source API has no brush-transform or interpolation-space setter.
            // Exercise these existing drawing-record fields without changing coverage.
            run.brush_transform = brush_transform;
            gradient.interpolation_cs = color_space;
            recording.draw(Draw::GlyphRun(run));
        }
    }
    let mut widget = RecordWidget::new(recording);
    widget.clip = false;
    let mut bridge = widget_bridge(widget, Affine::IDENTITY);
    build(bridge.record(FrameId(1)).unwrap(), snapshot, pool)
}

fn runs(frame: &RenderFrame) -> impl Iterator<Item = &FrameGlyphRun> {
    frame
        .commands()
        .iter()
        .filter_map(|command| match &command.op {
            FrameOp::Draw(FrameDraw::GlyphRun(run)) => Some(run),
            _ => None,
        })
}

fn gradient_axis(run: &FrameGlyphRun) -> (Affine, Point, Vec2) {
    let Brush::Gradient(gradient) = &run.brush else {
        unreachable!()
    };
    let GradientKind::Linear(line) = gradient.kind else {
        unreachable!()
    };
    (
        run.transform * run.brush_transform.unwrap_or(Affine::IDENTITY),
        line.start,
        line.end - line.start,
    )
}

fn color_centroid(pixels: &[[f32; 4]], channel: usize) -> Point {
    let mut mass = 0.;
    let mut sum = Vec2::ZERO;
    for (index, pixel) in pixels.iter().enumerate() {
        let weight = f64::from(pixel[channel]);
        mass += weight;
        sum += Vec2::new((index % 400) as f64 + 0.5, (index / 400) as f64 + 0.5) * weight;
    }
    assert!(
        mass > 100.,
        "both gradient colors must cover visible glyph ink"
    );
    (sum / mass).to_point()
}

#[test]
#[ignore = "requires actual graphics adapter and REEL_R01_TEST_FONT with CJK and Latin"]
fn label_gradients_render_transformed_color_opacity_and_reuse_glyph_atlas() {
    let (pool, font, registry, handle) = font_fixture();
    let mut renderer = renderer(Default::default());
    eprintln!("adapter={:?}", renderer.gpu.adapter.get_info());
    for mode in [
        WritingMode::HorizontalTb,
        WritingMode::VerticalRl,
        WritingMode::VerticalLr,
    ] {
        for space in [TextGradientSpace::Bounds, TextGradientSpace::Glyph] {
            let make_frame = |opacity: f64, reverse: bool| {
                let colors = [[255, 0, 0, 255], [0, 0, 255, 255]];
                label_frame(
                    Label::new("田H")
                        .with_style(StyleProperty::FontSize(120.))
                        .with_writing_mode(mode)
                        .with_gradient(Some(TextGradient {
                            from: colors[usize::from(reverse)],
                            to: colors[usize::from(!reverse)],
                            angle: 180.,
                            space,
                        }))
                        .with_reveal_fade(Some(RevealFade {
                            from: 0,
                            progress: opacity,
                            step: 0.,
                        })),
                    &font,
                    registry.freeze(&[handle], &pool).unwrap(),
                    &pool,
                    None,
                    ColorSpaceTag::Srgb,
                )
            };
            let full = make_frame(1., false);
            let (transform, start, axis) = gradient_axis(runs(&full).next().unwrap());
            let physical_axis = transform * (start + axis) - transform * start;
            if mode != WritingMode::HorizontalTb {
                let first = runs(&full).next().unwrap().glyph_transform;
                assert!(first.is_some());
                assert!(runs(&full).any(|run| run.glyph_transform != first));
            }
            let prepared = renderer.prepare(&full).unwrap();
            let cached = prepared.glyphs.entries.clone();
            assert!(!cached.is_empty());
            let opaque = renderer.readback(prepared).unwrap();
            let residency = renderer.usage().resources;
            let direction = color_centroid(&opaque, 2) - color_centroid(&opaque, 0);
            assert!(
                direction.dot(physical_axis / physical_axis.hypot()) > 3.,
                "{mode:?}/{space:?}: gradient direction must follow the paragraph transform"
            );
            let mut faded = Vec::new();
            for reverse in [false, true] {
                let frame = make_frame(0.5, reverse);
                let prepared = renderer.prepare(&frame).unwrap();
                assert_eq!(prepared.glyphs.entries.len(), cached.len());
                for (key, glyph) in &cached {
                    assert!(Arc::ptr_eq(
                        glyph,
                        prepared.glyphs.entries.get(key).unwrap()
                    ));
                }
                let pixels = renderer.readback(prepared).unwrap();
                assert_eq!(renderer.usage().resources, residency);
                for (actual, original) in pixels.iter().zip(&opaque) {
                    let expected = if reverse {
                        [original[2], original[1], original[0], original[3]]
                    } else {
                        *original
                    };
                    assert!(
                        actual
                            .iter()
                            .zip(expected)
                            .all(|(a, b)| (*a - b * 0.5).abs() < 0.008),
                        "{mode:?}/{space:?}: reveal opacity and reversed colors retain coverage"
                    );
                }
                faded = pixels;
            }
            assert!(faded.iter().any(|pixel| pixel[3] > 0.49));
            eprintln!(
                "{mode:?}/{space:?}: atlas_entries={}, direction={direction:?}",
                cached.len()
            );
        }
    }
}

#[test]
#[ignore = "requires actual graphics adapter and REEL_R01_TEST_FONT with CJK"]
fn glyph_gradient_brush_transform_preserves_srgb_and_linear_midpoints() {
    let (pool, font, registry, handle) = font_fixture();
    let mut renderer = renderer(Default::default());
    let brush_transform = Affine::translate((0., -20.)) * Affine::scale_non_uniform(1., 1.25);
    for mode in [WritingMode::HorizontalTb, WritingMode::VerticalRl] {
        for color_space in [ColorSpaceTag::Srgb, ColorSpaceTag::LinearSrgb] {
            let frame = label_frame(
                Label::new("田")
                    .with_style(StyleProperty::FontSize(160.))
                    .with_writing_mode(mode)
                    .with_gradient(Some(TextGradient {
                        from: [0, 0, 0, 255],
                        to: [255; 4],
                        angle: 180.,
                        space: TextGradientSpace::Bounds,
                    })),
                &font,
                registry.freeze(&[handle], &pool).unwrap(),
                &pool,
                Some(brush_transform),
                color_space,
            );
            let (transform, start, axis) = gradient_axis(runs(&frame).next().unwrap());
            let pixels = renderer
                .readback(renderer.prepare(&frame).unwrap())
                .unwrap();
            let mut interior = 0;
            let mut midpoint = 0;
            for (index, pixel) in pixels.iter().enumerate().filter(|(_, p)| p[3] > 0.99) {
                let point = transform.inverse()
                    * Point::new((index % 400) as f64 + 0.5, (index / 400) as f64 + 0.5);
                let t = ((point - start).dot(axis) / axis.hypot2()).clamp(0., 1.);
                let expected = if color_space == ColorSpaceTag::Srgb {
                    if t <= 0.04045 {
                        t / 12.92
                    } else {
                        ((t + 0.055) / 1.055).powf(2.4)
                    }
                } else {
                    t
                } as f32;
                assert!(
                    pixel[..3]
                        .iter()
                        .all(|channel| (*channel - expected).abs() < 0.012),
                    "{mode:?}/{color_space:?}: t={t}, pixel={pixel:?}, expected={expected}"
                );
                interior += 1;
                if (t - 0.5).abs() < 0.01 {
                    midpoint += 1;
                    let expected_midpoint = if color_space == ColorSpaceTag::Srgb {
                        0.214
                    } else {
                        0.5
                    };
                    assert!((pixel[0] - expected_midpoint).abs() < 0.015);
                }
            }
            assert!(
                interior > 100 && midpoint > 10,
                "actual glyph ink must cover the midpoint"
            );
            eprintln!("{mode:?}/{color_space:?}: interior={interior}, midpoint={midpoint}");
        }
    }
}
