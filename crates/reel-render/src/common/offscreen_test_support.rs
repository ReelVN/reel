//! Real nested source recordings and scoped reference execution for schedule tests.
use crate::test_support::r01_fixture as fixture;
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
};
use reel_ui::{
    core::{PaintEffect, Widget},
    frame::{FrameBuild, FrameInfo, RenderFrame},
    imaging::{
        Composite,
        record::{Clip, Draw, Geometry, Scene},
    },
    kurbo::{Affine, Point, Rect, Shape, Size},
    peniko::{Brush, Color},
    resources::{EffectShaderDescriptor, EffectShaderFormat, ResourceRegistry},
    stage::effect::*,
};
use std::cell::Cell;
thread_local! { static DISABLED: Cell<bool> = const { Cell::new(false) }; }
thread_local! { static ATLAS_DISABLED: Cell<bool> = const { Cell::new(false) }; }
pub(super) fn atlas_disabled() -> bool {
    ATLAS_DISABLED.get()
}
pub(crate) fn without_atlas<T>(run: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            ATLAS_DISABLED.set(self.0);
        }
    }
    let _restore = Restore(ATLAS_DISABLED.replace(true));
    run()
}
pub(super) fn disabled() -> bool {
    DISABLED.with(Cell::get)
}
pub(crate) fn without_windows<T>(work: impl FnOnce() -> T) -> T {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            DISABLED.with(|flag| flag.set(self.0));
        }
    }
    let _reset = Reset(DISABLED.with(|flag| flag.replace(true)));
    work()
}
pub(crate) fn nested_sources() -> RenderFrame {
    nested_sources_transformed(Affine::IDENTITY)
}
fn nested_sources_transformed(transform: Affine) -> RenderFrame {
    source_recording(transform, false, None)
}
pub(crate) fn terminal_sources(transform: Affine) -> RenderFrame {
    source_recording(transform, true, None)
}
pub(crate) fn terminal_sources_with_source_free(transform: Affine, nested: bool) -> RenderFrame {
    source_recording(transform, true, Some(nested))
}
fn source_recording(transform: Affine, terminal: bool, source_free: Option<bool>) -> RenderFrame {
    let pool = fixture::pool();
    let program = EffectProgram::new(
        vec![],
        vec![
            EffectNode {
                ty: EffectType::Vec2,
                op: EffectOp::Input(EffectInput::Uv),
            },
            EffectNode {
                ty: EffectType::Vec4,
                op: EffectOp::SourceSample {
                    uv: EffectNodeId(0),
                },
            },
        ],
        EffectNodeId(1),
    )
    .unwrap();
    let shader =
        crate::effect::compile_fragment(&program, crate::effect::Language::Wgsl, 16384).unwrap();
    let descriptor = EffectShaderDescriptor::with_textures(
        [47; 32],
        EffectShaderFormat::Wgsl,
        vec![],
        0,
        true,
        EffectType::Vec4,
        shader.source.as_bytes(),
    )
    .unwrap();
    let mut registry = ResourceRegistry::new(2, 1 << 20).unwrap();
    let source = shader.source.into_bytes();
    let ticket = pool.reserve(source.capacity() as u64).unwrap();
    let handle = registry
        .register_effect_shader(
            ResourceVersion {
                key: AssetKey("nested-source".into()),
                version: ContentVersion(descriptor.shader_key),
            },
            descriptor,
            OwnedBytes::new(source, ticket).unwrap(),
        )
        .unwrap();
    let bounds = Rect::new(0., 0., 64., 48.);
    let effect = PaintEffect {
        instance: EffectInstance::new([47; 32], program, vec![]).unwrap(),
        shader: registry.acquire(handle).unwrap(),
        images: vec![],
        bounds,
        coverage_invert: false,
        resolution_scale: 1.,
    };
    let free_effect = source_free.map(|_| {
        let program = EffectProgram::new(
            vec![],
            vec![EffectNode {
                ty: EffectType::Vec4,
                op: EffectOp::Constant(EffectValue::Vec4(
                    [0.1f32, 0.4, 0.1, 0.6].map(f32::to_bits),
                )),
            }],
            EffectNodeId(0),
        )
        .unwrap();
        let shader =
            crate::effect::compile_fragment(&program, crate::effect::Language::Wgsl, 16384)
                .unwrap();
        let descriptor = EffectShaderDescriptor::with_textures(
            [48; 32],
            EffectShaderFormat::Wgsl,
            vec![],
            0,
            false,
            EffectType::Vec4,
            shader.source.as_bytes(),
        )
        .unwrap();
        let bytes = shader.source.into_bytes();
        let ticket = pool.reserve(bytes.capacity() as u64).unwrap();
        let handle = registry
            .register_effect_shader(
                ResourceVersion {
                    key: AssetKey("source-free-window-boundary".into()),
                    version: ContentVersion(descriptor.shader_key),
                },
                descriptor,
                OwnedBytes::new(bytes, ticket).unwrap(),
            )
            .unwrap();
        PaintEffect {
            instance: EffectInstance::new([48; 32], program, vec![]).unwrap(),
            shader: registry.acquire(handle).unwrap(),
            images: vec![],
            bounds,
            coverage_invert: false,
            resolution_scale: 1.,
        }
    });
    let fill = |scene: &mut Scene, bounds, color| {
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Solid(color),
            brush_transform: None,
            shape: Geometry::Rect(bounds),
            composite: Composite::default(),
        })
    };
    let mut root = fixture::RecordWidget::new(Scene::new());
    root.clip = false;
    let free_widget = || {
        let mut scene = Scene::new();
        fill(&mut scene, bounds, Color::from_rgba8(255, 0, 0, 255));
        let mut widget = fixture::RecordWidget::new(scene);
        widget.clip = false;
        widget.effect = free_effect.clone();
        widget.opacity = 0.65;
        widget
    };
    for index in 0..2 {
        let mut scene = Scene::new();
        scene.push_clip(Clip::Fill {
            transform: Affine::IDENTITY,
            shape: Geometry::Path(bounds.to_path(0.1)),
            fill_rule: Default::default(),
        });
        fill(
            &mut scene,
            bounds,
            Color::from_rgba8(80 + index * 70, 120, 160, 128),
        );
        scene.pop_clip();
        let mut child = fixture::RecordWidget::new(scene);
        child.clip = false;
        child.effect = Some(effect.clone());
        let mut source = fixture::RecordWidget::new(Scene::new());
        source.clip = false;
        source.opacity = 0.5;
        if terminal {
            source.effect = Some(effect.clone());
        }
        // Keep a real ordinary draw in addition to the nested effect input.
        fill(
            &mut source.own,
            Rect::new(3., 3., 20., 20.),
            Color::from_rgba8(20, 40, 60, 100),
        );
        source
            .inner
            .add(child.prepare(), Point::ZERO, Size::new(64., 48.));
        if index == 0 && source_free == Some(true) {
            source.inner.add(
                free_widget().prepare(),
                Point::new(5., 5.),
                Size::new(64., 48.),
            );
        }
        root.inner.add(
            source.prepare(),
            Point::new(20. + index as f64 * if terminal { 32. } else { 110. }, 30.),
            Size::new(64., 48.),
        );
        if index == 0 && source_free == Some(false) {
            root.inner.add(
                free_widget().prepare(),
                Point::new(35., 35.),
                Size::new(64., 48.),
            );
        }
    }
    let mut bridge = fixture::widget_bridge(root, transform);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        registry
            .freeze(
                &free_effect.as_ref().map_or_else(
                    || vec![handle],
                    |effect| vec![handle, effect.shader.handle()],
                ),
                &pool,
            )
            .unwrap(),
        FrameInfo {
            logical_size: [400., 300.],
            physical_size: [400, 300],
        },
        fixture::limits(),
        &pool,
    )
    .unwrap();
    while !build.step(fixture::step_budget()).unwrap().complete {}
    build.finish().unwrap()
}

#[test]
fn nested_sources_keep_effect_grids_and_all_live_targets_in_the_shared_budget() {
    use super::*;
    for transform in [
        Affine::IDENTITY,
        Affine::translate((20.3, 5.7))
            * Affine::scale_non_uniform(0.83, 0.71)
            * Affine::rotate(0.07),
    ] {
        let frame = nested_sources_transformed(transform);
        let choose = |limit| {
            Plan::choose(
                &frame,
                None,
                [400, 300],
                false,
                1,
                4096,
                &glyph_plan::Plan::default(),
                limit,
                &|s| Ok(u64::from(s[0]) * u64::from(s[1]) * 4),
                super::super::target_plan::MaskExecution::Sampled,
                &Default::default(),
                |_| Ok(()),
            )
            .map(|(plan, bytes, _)| (plan, bytes))
            .unwrap()
        };
        let (baseline, baseline_bytes) = without_windows(|| choose(u64::MAX));
        assert!(baseline.is_empty());
        let (plan, bytes) = choose(u64::MAX);
        let entries: Vec<_> = (0..frame.commands().len())
            .filter_map(|i| plan.entry(i))
            .collect();
        assert_eq!(
            entries.len(),
            2,
            "complete nested tasks must enter the source window"
        );
        assert!(entries.iter().all(|e| e.shared));
        for entry in entries {
            let view = entry.view.unwrap();
            let ordinary_view = view.placed(entry.source_size, [0; 2]);
            let mut effects = 0;
            for command in &frame.commands()[entry.push + 1..entry.end - 1] {
                if let reel_ui::frame::FrameOp::PushGroup(group) = &command.op
                    && let Some(effect) = &group.effect
                {
                    let grid = |view: SourceView| {
                        super::super::effect_plan::source_layout(
                            effect,
                            group.effect_to_target,
                            view.frame_to_source(Affine::IDENTITY),
                            4096,
                            command.origin.at,
                        )
                        .unwrap()
                    };
                    let before = grid(ordinary_view);
                    let after = grid(view);
                    assert_eq!(before.size, after.size);
                    assert_eq!(before.frame_to_source, after.frame_to_source);
                    effects += 1;
                }
            }
            assert_eq!(effects, 1);
        }
        let (tight, tight_bytes) = choose(baseline_bytes);
        assert!(tight_bytes <= baseline_bytes);
        if bytes > baseline_bytes {
            assert!(tight.is_empty());
        }
        eprintln!(
            "nested_effect_sources=2 target_bytes={baseline_bytes}->{bytes} tight_bytes={tight_bytes} tight_scheduled={}",
            !tight.is_empty()
        );
    }
}

#[test]
fn source_free_parent_effect_ends_windows_but_nested_effect_stays_in_producer() {
    use super::*;
    for nested in [false, true] {
        let frame = terminal_sources_with_source_free(Affine::IDENTITY, nested);
        let budgets = super::super::renderer::Budgets::new(Default::default());
        let local = super::super::renderer::Budgets::new(Default::default());
        let selection = super::super::retained::Plan::collect(
            std::iter::once(&frame),
            [400, 300],
            false,
            &budgets,
            &local,
            u64::MAX,
            |_| None,
            |_| Ok(()),
        )
        .unwrap();
        let source_free = frame
            .commands()
            .iter()
            .enumerate()
            .filter_map(|(push, _)| {
                selection
                    .sources
                    .end(frame.drawing(), push)
                    .map(|end| push..end)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            source_free.len(),
            1,
            "fixture must select a real dead source"
        );
        let (plan, _, _) = Plan::choose(
            &frame,
            None,
            [400, 300],
            false,
            1,
            4096,
            &glyph_plan::Plan::default(),
            u64::MAX,
            &|size| Ok(u64::from(size[0]) * u64::from(size[1]) * 4),
            super::super::target_plan::MaskExecution::Sampled,
            &selection,
            |_| Ok(()),
        )
        .unwrap();
        let entries = (0..frame.commands().len())
            .filter_map(|push| plan.entry(push))
            .collect::<Vec<_>>();
        if nested {
            assert_eq!(entries.len(), 2);
            assert!(
                entries[0].push < source_free[0].start && entries[0].end >= source_free[0].end,
                "a nested immediate effect belongs to its independent producer"
            );
        } else {
            assert!(
                entries.is_empty(),
                "the immediate parent effect must separate the two single-source runs"
            );
        }
    }
}
