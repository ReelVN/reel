//! CPU-only coverage regression using the selected real font and shaped runs.
use super::*;
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    core::*,
    dpi::PhysicalSize,
    peniko::Blob,
    resources::{ResourceData, ResourceRegistry},
};

#[test]
#[ignore = "requires REEL_R01_TEST_FONT set to SimSun.ttf; CPU only, no graphics adapter"]
fn cjk_synthetic_bold_expands_every_glyph_and_reuses_raster_scratch() {
    let bytes = std::fs::read(std::env::var("REEL_R01_TEST_FONT").unwrap()).unwrap();
    let font = Blob::from(bytes);
    let pool = pool();
    let mut registry = ResourceRegistry::new(1, 65264).unwrap();
    let handle = registry
        .register(
            reel_core::identity::ResourceVersion {
                key: reel_core::identity::AssetKey("synthesis-font".into()),
                version: reel_core::identity::ContentVersion([1; 32]),
            },
            ResourceData::Font(font.clone()),
            pool.reserve(font.data().len() as u64).unwrap(),
        )
        .unwrap();
    let mut raster = crate::gpu::glyph::Rasterizer::default();
    for text in [
        "通过令人目眩的街道，突然产生了这样的想法。",
        "街边的展览板到处安置着3D全息投影。",
        "自称IT强国，韩国。",
    ] {
        let root = RenderRoot::new(
            NewWidget::new(
                reel_ui::widgets::Label::new(text)
                    .with_style(StyleProperty::FontFamily("SimSun".into()))
                    .with_style(StyleProperty::FontSize(30.))
                    .with_style(StyleProperty::FontWeight(
                        reel_ui::reel_text::FontWeight::new(700.),
                    )),
            ),
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
        let mut bridge = r01_fixture::bridge(root);
        let frame = build(
            bridge.record(FrameId(1)).unwrap(),
            registry.freeze(&[handle], &pool).unwrap(),
            &pool,
        );
        let mut count = 0;
        for command in frame.commands() {
            let reel_ui::frame::FrameOp::Draw(reel_ui::frame::FrameDraw::GlyphRun(run)) =
                &command.op
            else {
                continue;
            };
            assert_eq!(run.font, handle, "all characters use the configured font");
            assert_eq!(run.font_index, 0);
            assert_eq!(run.font_size, 30.);
            assert_eq!(run.font_embolden, reel_ui::kurbo::Vec2::new(1.25, 1.25));
            let mut regular = run.clone();
            regular.font_embolden = reel_ui::kurbo::Vec2::ZERO;
            assert!(
                crate::gpu::glyph_cache::RunKey::new(run, 1.)
                    != crate::gpu::glyph_cache::RunKey::new(&regular, 1.)
            );
            for density in [1., 2.] {
                for (index, glyph) in run.glyphs.iter().enumerate() {
                    assert_ne!(glyph.id, 0);
                    let render = |raster: &mut crate::gpu::glyph::Rasterizer, run| {
                        crate::gpu::glyph::rasterize(
                            raster,
                            frame.drawing(),
                            run,
                            density,
                            index,
                            command.origin.at,
                            crate::gpu::glyph::RasterLimits {
                                available: u64::MAX,
                                max_dimension: 8192,
                            },
                        )
                        .unwrap()
                        .unwrap()
                    };
                    let raw = render(&mut raster, &regular);
                    let bold = render(&mut raster, run);
                    let cold = render(&mut crate::gpu::glyph::Rasterizer::default(), run);
                    assert_eq!((bold.x, bold.y, bold.size), (cold.x, cold.y, cold.size));
                    assert_eq!(bold.alpha, cold.alpha, "scratch reuse cannot change weight");
                    let coverage =
                        |alpha: &[u8]| alpha.iter().map(|value| u64::from(*value)).sum::<u64>();
                    let raw_coverage = coverage(&raw.alpha);
                    let bold_coverage = coverage(&bold.alpha);
                    assert!(
                        bold_coverage > raw_coverage,
                        "glyph {} became thinner: {raw_coverage} -> {bold_coverage}",
                        glyph.id
                    );
                    println!(
                        "{text} glyph={} density={density} raw={raw_coverage} bold={bold_coverage}",
                        glyph.id
                    );
                    count += 1;
                }
            }
        }
        assert_eq!(count, text.chars().count() * 2);
    }
}
