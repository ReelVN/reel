//! CPU-only frame planning, residency and retirement for animated effect chains.
use super::*;
use crate::common::{glyph_plan, offscreen, target_cache, target_plan};
use crate::test_support::r01_fixture as fixture;
use reel_core::{
    budget::{BudgetCategory, BudgetPool, BudgetTicket, OwnedBytes, QueueLimits},
    identity::{AssetKey, ContentVersion, FrameId, ResourceVersion},
};
use reel_ui::{
    core::{PaintEffect, Widget},
    frame::{FrameBuild, FrameInfo, RenderFrame},
    imaging::record::{Draw, Geometry, Scene},
    kurbo::{Affine, Point, Rect, Size},
    peniko::{Brush, Color},
    resources::{EffectShaderDescriptor, EffectShaderFormat, ResourceRegistry},
    stage::effect::*,
};
use std::sync::Arc;

fn cost(size: [u32; 2]) -> Result<u64, GpuError> {
    Ok(u64::from(size[0]) * u64::from(size[1]) * 4)
}

fn frame(width: u32) -> RenderFrame {
    frame_domains([[width, 384], [400, 380]])
}

fn frame_domains(domains: [[u32; 2]; 2]) -> RenderFrame {
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
    let fragment =
        crate::effect::compile_fragment(&program, crate::effect::Language::Wgsl, 16384).unwrap();
    let descriptor = EffectShaderDescriptor::with_textures(
        [7; 32],
        EffectShaderFormat::Wgsl,
        vec![],
        0,
        true,
        EffectType::Vec4,
        fragment.source.as_bytes(),
    )
    .unwrap();
    let mut registry = ResourceRegistry::new(1, 16384).unwrap();
    let handle = registry
        .register_effect_shader(
            ResourceVersion {
                key: AssetKey("animated-effect-chain".into()),
                version: ContentVersion([7; 32]),
            },
            descriptor,
            OwnedBytes::new(
                fragment.source.clone().into_bytes(),
                pool.reserve(fragment.source.len() as u64).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let mut root = fixture::RecordWidget::new(Scene::new());
    root.clip = false;
    // Two sequential two-pass sources: the second fits in the first's storage,
    // while each chain needs two sources alive together. These are complete local
    // effect domains, just as in the probe's nested source filters.
    for size in domains {
        let bounds = Rect::new(0., 0., f64::from(size[0]), f64::from(size[1]));
        let effect = PaintEffect {
            instance: EffectInstance::new([7; 32], program.clone(), vec![]).unwrap(),
            shader: registry.acquire(handle).unwrap(),
            images: vec![],
            bounds,
            coverage_invert: false,
            resolution_scale: 1.,
        };
        let mut scene = Scene::new();
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Solid(Color::WHITE),
            brush_transform: None,
            shape: Geometry::Rect(bounds),
            composite: Default::default(),
        });
        let mut child = fixture::RecordWidget::new(scene);
        child.clip = false;
        child.effect = Some(effect.clone());
        let mut outer = fixture::RecordWidget::new(Scene::new());
        outer.clip = false;
        outer.effect = Some(effect);
        outer.inner.add(
            child.prepare(),
            Point::ZERO,
            Size::new(f64::from(size[0]), f64::from(size[1])),
        );
        root.inner.add(
            outer.prepare(),
            Point::ZERO,
            Size::new(f64::from(size[0]), f64::from(size[1])),
        );
    }
    let mut bridge = fixture::widget_bridge(root, Affine::IDENTITY);
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        registry.freeze(&[handle], &pool).unwrap(),
        FrameInfo {
            logical_size: [960., 544.],
            physical_size: [960, 544],
        },
        fixture::limits(),
        &pool,
    )
    .unwrap();
    while !build.step(fixture::step_budget()).unwrap().complete {}
    build.finish().unwrap()
}

fn pool(bytes: u64) -> BudgetPool {
    BudgetPool::new(
        BudgetCategory::Offscreen,
        QueueLimits {
            max_requests: u32::MAX,
            max_bytes: bytes,
        },
    )
}

#[test]
fn pressured_effect_frames_keep_capacity_through_lifetime_sharing_and_retire_readers() {
    let root = [960, 544];
    let capacity = [512, 384];
    let limit = cost(root).unwrap() + 2 * cost(capacity).unwrap();
    for masks in [
        target_plan::MaskExecution::Inline,
        target_plan::MaskExecution::Sampled,
    ] {
        let data = pool(limit);
        let metadata = pool(u64::MAX);
        let mut cache = target_cache::Cache::<Arc<BudgetTicket>>::default();
        let mut created = 0;
        let mut exact_classes = std::collections::BTreeSet::new();
        let mut last_flight = Vec::new();
        for width in 481..=512 {
            let frame = frame(width);
            let glyphs = glyph_plan::Plan::default();
            let empty = offscreen::Plan::default();
            let selection = Default::default();
            let input = target_plan::Input {
                frame: &frame,
                second: None,
                size: root,
                capture: false,
                samples: 1,
                max_dimension: 4096,
                glyphs: &glyphs,
                schedule: &empty,
                masks,
                selection: &selection,
                cost: &cost,
                coverage_cost: None,
            };
            assert!(input.measure(|_| Ok(())).unwrap().bytes > limit);
            let (exact, _, _) = input.storage(|_| Ok(())).unwrap();
            exact_classes.insert(exact.storage_size([width, 384]));
            let (plan, bytes, _tickets) = offscreen::Plan::choose(
                &frame,
                None,
                root,
                false,
                1,
                4096,
                &glyphs,
                limit,
                &cost,
                masks,
                &selection,
                |_| Ok(()),
            )
            .unwrap();
            assert_eq!(plan.storage_size([width, 384]), capacity);
            assert_eq!(plan.storage_size([400, 380]), capacity);
            assert_eq!(plan.requirements().count(capacity), 2);
            assert_eq!(plan.requirements().count(root), 1);
            assert_eq!(bytes, limit);
            let mut flight = Vec::new();
            for (size, count) in plan.requirements().iter() {
                for index in 0..count as usize {
                    let owner = if let Some(owner) = cache.get(size, index) {
                        owner.clone()
                    } else {
                        let owner = Arc::new(data.reserve(cost(size).unwrap()).unwrap());
                        cache.reserve(1, &metadata, u64::MAX).unwrap();
                        assert!(cache.insert(size, owner.clone()));
                        created += 1;
                        owner
                    };
                    flight.push((size, owner));
                }
            }
            cache.submitted();
            for (size, owner) in &flight {
                cache.mark_used(*size, |cached| Arc::ptr_eq(cached, owner));
            }
            last_flight = flight;
            assert_eq!(data.usage().bytes, limit);
        }
        assert_eq!(exact_classes.len(), 32);
        assert_eq!(
            created, 3,
            "one output and two shared scratch allocations across 32 frames"
        );
        assert_eq!(data.usage().peak_bytes, limit);
        // Pressure cannot release a target still read by a submitted frame.
        cache.release_idle(|owner| Arc::strong_count(owner) == 1);
        assert!(
            !cache.reclaim(&Default::default(), true, |owner| Arc::strong_count(owner)
                == 1)
        );
        assert_eq!(data.usage().bytes, limit);
        drop(last_flight);
        cache.release_idle(|owner| Arc::strong_count(owner) == 1);
        assert_eq!(data.usage().bytes, 0);
        assert_eq!(metadata.usage().bytes, 0);
    }
}

#[test]
fn thin_animated_effect_sources_keep_bounded_axis_capacities() {
    let root = [960, 544];
    // Thin 775x100 local domains occur in the showcase's projectile layers.
    // Vary another thin domain by one physical pixel per accepted frame.
    for masks in [
        target_plan::MaskExecution::Inline,
        target_plan::MaskExecution::Sampled,
    ] {
        let data = pool(8 << 20);
        let metadata = pool(u64::MAX);
        let mut cache = target_cache::Cache::<Arc<BudgetTicket>>::default();
        let mut created = 0;
        let mut previous_flight = Vec::new();
        for width in 641..=672 {
            let domains = [[width, 100], [775, 100]];
            let frame = frame_domains(domains);
            let (plan, bytes, _) = offscreen::Plan::choose(
                &frame,
                None,
                root,
                false,
                1,
                4096,
                &glyph_plan::Plan::default(),
                8 << 20,
                &cost,
                masks,
                &Default::default(),
                |_| Ok(()),
            )
            .unwrap();
            assert_eq!(plan.storage_size(root), root);
            assert_eq!(plan.storage_size(domains[0]), [672, 100]);
            for domain in domains {
                let storage = plan.storage_size(domain);
                assert!(storage[0] >= domain[0] && storage[1] >= domain[1]);
                assert!(cost(storage).unwrap() <= cost(domain).unwrap() * 9 / 8);
                // The old square bucket exceeds its byte allowance, so it used
                // a fresh exact-width storage class on every frame.
                let square = domain.map(|value| value.div_ceil(32) * 32);
                assert!(cost(square).unwrap() > cost(domain).unwrap() * 9 / 8);
            }
            let mut flight = Vec::new();
            for (size, count) in plan.requirements().iter() {
                for index in 0..count as usize {
                    let owner = if let Some(owner) = cache.get(size, index) {
                        owner.clone()
                    } else {
                        let owner = Arc::new(data.reserve(cost(size).unwrap()).unwrap());
                        cache.reserve(1, &metadata, u64::MAX).unwrap();
                        assert!(cache.insert(size, owner.clone()));
                        created += 1;
                        owner
                    };
                    flight.push((size, owner));
                }
            }
            cache.submitted();
            for (size, owner) in &flight {
                cache.mark_used(*size, |cached| Arc::ptr_eq(cached, owner));
            }
            previous_flight = flight;
            assert_eq!(data.usage().bytes, bytes);
        }
        assert_eq!(
            created, 5,
            "two scratch targets per domain plus one exact output"
        );
        println!(
            "thin_effect frames=32 old_exact_scratch_classes=33 created_targets={created} peak_bytes={}",
            data.usage().peak_bytes
        );
        drop(previous_flight);
        cache.release_idle(|owner| Arc::strong_count(owner) == 1);
        assert_eq!(data.usage().bytes, 0);
    }
}
