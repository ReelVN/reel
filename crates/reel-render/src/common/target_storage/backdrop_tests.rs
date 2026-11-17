//! Animated blend pools exercise ordinary-budget lifetime selection and reuse.
use super::*;
use crate::common::{glyph_plan, offscreen, target_cache, target_plan};
use crate::test_support::r01_fixture as fixture;
use reel_core::{
    budget::{BudgetCategory, BudgetPool, BudgetTicket, QueueLimits},
    identity::FrameId,
};
use reel_ui::{
    frame::{FrameBuild, FrameInfo, RenderFrame},
    imaging::{
        Composite,
        record::{Draw, Geometry, Group, Scene},
    },
    kurbo::{Affine, Rect},
    peniko::{Brush, Color, Compose, Mix},
};
use std::sync::Arc;

const ROOT: [u32; 2] = [960, 544];
const LIMIT: u64 = 64 << 20;

fn cost(size: [u32; 2]) -> Result<u64, GpuError> {
    Ok(u64::from(size[0]) * u64::from(size[1]) * 4)
}

fn frame(width: u32, nested: bool) -> RenderFrame {
    frame_groups(&[[728, 408], [width, 312], [248, 216]], nested)
}

fn frame_groups(domains: &[[u32; 2]], nested: bool) -> RenderFrame {
    let mut scene = Scene::new();
    for (index, [width, height]) in domains.iter().copied().enumerate() {
        scene.push_group(Group {
            composite: Composite {
                alpha: 0.5,
                blend: if domains.len() == 1 {
                    reel_ui::peniko::BlendMode {
                        mix: Mix::Normal,
                        compose: Compose::PlusLighter,
                    }
                } else {
                    [Mix::Screen, Mix::ColorDodge, Mix::Multiply][index % 3].into()
                },
            },
            ..Default::default()
        });
        for left in [8., 20.] {
            scene.draw(Draw::Fill {
                transform: Affine::IDENTITY,
                fill_rule: Default::default(),
                brush: Brush::Solid(Color::from_rgba8(80, 120, 160, 128)),
                brush_transform: None,
                shape: Geometry::Rect(Rect::new(left, 8., f64::from(width), f64::from(height))),
                composite: Default::default(),
            });
        }
        if !nested {
            scene.pop_group();
        }
    }
    if nested {
        for _ in domains {
            scene.pop_group();
        }
    }
    let mut widget = fixture::RecordWidget::new(scene);
    widget.clip = false;
    let mut bridge = fixture::widget_bridge(widget, Affine::IDENTITY);
    let pool = fixture::pool();
    let mut build = FrameBuild::new(
        bridge.record(FrameId(1)).unwrap(),
        fixture::empty_snapshot(&pool),
        FrameInfo {
            logical_size: ROOT.map(f64::from),
            physical_size: ROOT,
        },
        fixture::limits(),
        &pool,
    )
    .unwrap();
    while !build.step(fixture::step_budget()).unwrap().complete {}
    build.finish().unwrap()
}

fn choose(frame: &RenderFrame) -> (offscreen::Plan, u64) {
    choose_limit(frame, LIMIT)
}

#[test]
fn current_pixel_destination_retires_the_backdrop_without_eliding_the_source() {
    use crate::common::blend::DestinationAccess;
    let frame = frame_groups(&[[160, 96]], false);
    let choose_access = |destination| {
        offscreen::Plan::choose_with_groups(
            &frame,
            None,
            ROOT,
            false,
            1,
            4096,
            &glyph_plan::Plan::default(),
            LIMIT,
            &cost,
            None,
            target_plan::MaskExecution::Sampled,
            destination,
            &Default::default(),
            None,
            None,
            |_, bytes| Ok((bytes, ())),
            |_| Ok(()),
        )
        .unwrap()
    };
    let (sampled, sampled_bytes, _, ()) = choose_access(DestinationAccess::Sampled);
    let (current, current_bytes, _, ()) = choose_access(DestinationAccess::CurrentPixelUnorm8);
    assert!(sampled.sampled_backdrop());
    assert!(!current.sampled_backdrop());
    let source = sampled
        .requirements()
        .iter()
        .find(|(size, _)| *size != ROOT)
        .unwrap()
        .0;
    assert_eq!(sampled.requirements().count(ROOT), 1);
    assert_eq!(current.requirements().count(ROOT), 1);
    assert_eq!(sampled.requirements().count(source), 2);
    assert_eq!(current.requirements().count(source), 1);
    assert_eq!(
        sampled_bytes,
        cost(ROOT).unwrap() + 2 * cost(source).unwrap()
    );
    assert_eq!(current_bytes, cost(ROOT).unwrap() + cost(source).unwrap());
    assert!(
        current.is_empty(),
        "source group keeps its original painter position"
    );
}

fn choose_limit(frame: &RenderFrame, limit: u64) -> (offscreen::Plan, u64) {
    choose_cost(frame, limit, &cost)
}

fn choose_cost(
    frame: &RenderFrame,
    limit: u64,
    cost: &dyn Fn([u32; 2]) -> Result<u64, GpuError>,
) -> (offscreen::Plan, u64) {
    let (plan, bytes, _) = offscreen::Plan::choose(
        frame,
        None,
        ROOT,
        false,
        1,
        4096,
        &glyph_plan::Plan::default(),
        limit,
        cost,
        target_plan::MaskExecution::Sampled,
        &Default::default(),
        |_| Ok(()),
    )
    .unwrap();
    (plan, bytes)
}

// Reproduce the previously selected ordinary-budget pool, including padding.
fn separate(frame: &RenderFrame) -> (target_plan::Requirements, u64) {
    let measured = target_plan::Input {
        frame,
        second: None,
        size: ROOT,
        capture: false,
        samples: 1,
        max_dimension: 4096,
        glyphs: &Default::default(),
        schedule: &Default::default(),
        masks: target_plan::MaskExecution::Sampled,
        selection: &Default::default(),
        cost: &cost,
        coverage_cost: None,
    }
    .measure(|_| Ok(()))
    .unwrap();
    assert!(measured.bytes < LIMIT && measured.local_backdrop);
    let requirements = measured.requirements(|_| Ok(())).unwrap();
    match Plan::capacities(
        &requirements,
        ROOT,
        4096,
        LIMIT,
        &cost,
        &target_plan::Requirements::default(),
        |_| Ok(()),
    )
    .unwrap()
    {
        Some((plan, _)) => (plan.requirements, plan.bytes),
        None => (requirements, measured.bytes),
    }
}

struct Residency {
    data: BudgetPool,
    metadata: BudgetPool,
    cache: target_cache::Cache<Arc<BudgetTicket>>,
    previous: Vec<([u32; 2], Arc<BudgetTicket>)>,
    created: usize,
}

impl Residency {
    fn new() -> Self {
        let pool = || {
            BudgetPool::new(
                BudgetCategory::Offscreen,
                QueueLimits {
                    max_requests: u32::MAX,
                    max_bytes: LIMIT,
                },
            )
        };
        Self {
            data: pool(),
            metadata: pool(),
            cache: Default::default(),
            previous: Vec::new(),
            created: 0,
        }
    }

    fn submit(&mut self, requirements: &target_plan::Requirements) {
        let mut flight = Vec::new();
        for (size, count) in requirements.iter() {
            for index in 0..count as usize {
                let owner = if let Some(owner) = self.cache.get(size, index) {
                    owner.clone()
                } else {
                    let owner = Arc::new(self.data.reserve(cost(size).unwrap()).unwrap());
                    self.cache.reserve(1, &self.metadata, LIMIT).unwrap();
                    assert!(self.cache.insert(size, owner.clone()));
                    self.created += 1;
                    owner
                };
                flight.push((size, owner));
            }
        }
        self.cache.submitted();
        for (size, owner) in &flight {
            self.cache
                .mark_used(*size, |cached| Arc::ptr_eq(cached, owner));
        }
        // Two preparations overlap until the previous frame is retired.
        self.previous = flight;
    }

    fn retire(mut self) {
        self.cache
            .release_idle(|owner| Arc::strong_count(owner) == 1);
        assert!(self.data.usage().bytes != 0);
        self.previous.clear();
        self.cache
            .release_idle(|owner| Arc::strong_count(owner) == 1);
        assert_eq!(self.data.usage().bytes, 0);
        assert_eq!(self.metadata.usage().bytes, 0);
    }
}

#[test]
fn animated_blends_share_ordinary_budget_pool_and_preserve_overlapping_sources() {
    for nested in [false, true] {
        let mut old = Residency::new();
        let mut shared = Residency::new();
        for phase in 0..384 {
            let width = 264 + phase.min(383 - phase) as u32;
            let frame = frame(width, nested);
            let (separate, old_bytes) = separate(&frame);
            let (plan, bytes) = choose(&frame);
            assert_eq!(plan.storage_size(ROOT), ROOT);
            assert_eq!(plan.requirements().count(ROOT), 1);
            assert!(
                bytes <= old_bytes,
                "nested={nested}: {bytes} <= {old_bytes}"
            );
            if !nested {
                assert!(bytes < old_bytes);
            } else {
                // Three nested live sources plus the innermost backdrop cannot
                // collapse to the sequential case's two scratch allocations.
                assert!(
                    plan.requirements()
                        .iter()
                        .map(|(_, count)| count)
                        .sum::<u64>()
                        >= 5
                );
            }
            for (logical, _) in separate.iter() {
                let storage = plan.storage_size(logical);
                assert!(storage[0] >= logical[0] && storage[1] >= logical[1]);
            }
            old.submit(&separate);
            shared.submit(plan.requirements());
        }
        println!(
            "animated_blends nested={nested} frames=384 created={} -> {} peak_bytes={} -> {}",
            old.created,
            shared.created,
            old.data.usage().peak_bytes,
            shared.data.usage().peak_bytes,
        );
        assert!(shared.created <= old.created);
        assert!(shared.data.usage().peak_bytes <= old.data.usage().peak_bytes);
        if !nested {
            assert_eq!(shared.created, 3);
            assert!(shared.data.usage().peak_bytes < old.data.usage().peak_bytes);
        }
        old.retire();
        shared.retire();
    }
}

#[test]
fn single_add_sources_use_stable_parent_capacity_when_affordable() {
    for animated in [false, true] {
        let mut old = Residency::new();
        let mut cropped = Residency::new();
        let mut stable = Residency::new();
        let mut copy_pixels = 0;
        for phase in 0..384 {
            let domain = if animated {
                [544 + phase.min(383 - phase), 472]
            } else {
                [60, 24]
            };
            let frame = frame_groups(&[domain], false);
            let (cropped_requirements, cropped_bytes) = separate(&frame);
            let (plan, stable_bytes) = choose(&frame);
            let (tight, tight_bytes) = choose_limit(&frame, cropped_bytes);
            assert_eq!(tight_bytes, cropped_bytes);
            assert_eq!(tight.backdrop_size([64, 32], ROOT), [64, 32]);
            assert_eq!(
                plan.backdrop_size([64, 32], ROOT),
                if animated { ROOT } else { [64, 32] }
            );
            if animated {
                assert!(stable_bytes > cropped_bytes);
            } else {
                assert_eq!(stable_bytes, cropped_bytes);
            }
            // Copy area comes from the actual source scissor, independently of the
            // physical backdrop capacity selected by either budget tier.
            let view = frame
                .commands()
                .iter()
                .find_map(|command| {
                    let reel_ui::frame::FrameOp::PushGroup(group) = &command.op else {
                        return None;
                    };
                    (group.composite.blend != Default::default()).then(|| {
                        crate::common::source_view::SourceView::group(
                            group,
                            group.ink_bounds,
                            Affine::IDENTITY,
                            ROOT,
                            crate::common::scissor::Scissor::full(ROOT),
                            false,
                            4096,
                        )
                        .unwrap()
                    })
                })
                .unwrap();
            copy_pixels += u64::from(view.source_rect.width) * u64::from(view.source_rect.height);
            let previous: Vec<_> = cropped_requirements
                .iter()
                .map(|(size, count)| {
                    if size == ROOT {
                        (size, count + 1)
                    } else {
                        assert_eq!(count, 2);
                        (size, count - 1)
                    }
                })
                .collect();
            let previous =
                target_plan::Requirements::collect(previous.into_iter(), |_| Ok(())).unwrap();
            assert_eq!(
                if animated {
                    &previous
                } else {
                    &cropped_requirements
                }
                .iter()
                .collect::<Vec<_>>(),
                plan.requirements().iter().collect::<Vec<_>>()
            );
            old.submit(&previous);
            cropped.submit(&cropped_requirements);
            stable.submit(plan.requirements());
        }
        println!(
            "single_add animated={animated} frames=384 full_backdrop_created={} cropped_created={} stable_created={} full_backdrop_peak={} cropped_peak={} stable_peak={} full_copy_pixels={} selected_copy_pixels={copy_pixels}",
            old.created,
            cropped.created,
            stable.created,
            old.data.usage().peak_bytes,
            cropped.data.usage().peak_bytes,
            stable.data.usage().peak_bytes,
            384 * u64::from(ROOT[0]) * u64::from(ROOT[1]),
        );
        assert_eq!(cropped.created, 2 * (old.created - 2) + 1);
        let expected = if animated { &old } else { &cropped };
        assert_eq!(stable.created, expected.created);
        assert_eq!(
            stable.data.usage().peak_bytes,
            expected.data.usage().peak_bytes
        );
        assert!(copy_pixels < 384 * u64::from(ROOT[0]) * u64::from(ROOT[1]));
        old.retire();
        cropped.retire();
        stable.retire();
    }
}

#[test]
fn single_add_capacity_threshold_round_trips_bound_pool_creation() {
    for rapid in [false, true] {
        let mut cropped = Residency::new();
        let mut selected = Residency::new();
        let mut stable_frames = 0;
        for step in 0..1664 {
            let width = if rapid {
                if step % 2 == 0 { 512 } else { 544 }
            } else {
                let phase = step % 832;
                320 + phase.min(831 - phase)
            };
            let frame = frame_groups(&[[width, 472]], false);
            let (requirements, _) = separate(&frame);
            let (plan, _) = choose(&frame);
            let source = requirements
                .iter()
                .find(|(size, _)| *size != ROOT)
                .unwrap()
                .0;
            let stable = cost(ROOT).unwrap() <= 2 * cost(source).unwrap();
            stable_frames += usize::from(stable);
            assert_eq!(
                plan.backdrop_size(source, ROOT),
                if stable { ROOT } else { source }
            );
            cropped.submit(&requirements);
            selected.submit(plan.requirements());
        }
        println!(
            "backdrop_threshold rapid={rapid} frames=1664 stable_frames={stable_frames} created={} -> {} peak_bytes={} -> {}",
            cropped.created,
            selected.created,
            cropped.data.usage().peak_bytes,
            selected.data.usage().peak_bytes,
        );
        assert!(stable_frames > 0 && stable_frames < 1664);
        assert!(selected.created <= cropped.created);
        cropped.retire();
        selected.retire();
    }
}

#[test]
fn single_backdrop_threshold_uses_backend_storage_cost() {
    let frame = frame_groups(&[[300, 536]], false);
    let source = [320, 544];
    let padded =
        |size: [u32; 2]| Ok(u64::from(size[0].div_ceil(512) * 512) * u64::from(size[1]) * 4);
    assert!(cost(ROOT).unwrap() > 2 * cost(source).unwrap());
    assert_eq!(padded(ROOT).unwrap(), 2 * padded(source).unwrap());
    let (logical_cost, _) = choose(&frame);
    let (physical_cost, _) = choose_cost(&frame, LIMIT, &padded);
    assert_eq!(logical_cost.backdrop_size(source, ROOT), source);
    assert_eq!(physical_cost.backdrop_size(source, ROOT), ROOT);
}

#[test]
#[cfg(feature = "wgpu")]
#[ignore = "actual adapter submission; no capture or pixel readback"]
fn animated_shared_backdrops_submit_without_exceeding_the_selected_pool() {
    let gpu = pollster::block_on(crate::gpu::Gpu::request(
        &wgpu::Instance::default(),
        None,
        crate::gpu::DeviceGeneration(1),
        crate::gpu::GpuLimits {
            target_bytes: LIMIT,
            ..Default::default()
        },
    ))
    .unwrap();
    let mut renderer = crate::gpu::Renderer::new(gpu);
    renderer.prepare_pipelines().unwrap();
    for width in (264..456).step_by(6) {
        let frame = frame(width, false);
        let prepared = renderer.prepare(&frame).unwrap();
        let quality = prepared.quality();
        // The three groups share the largest 736x416 source class, with two
        // simultaneous targets. Four samples retain a separate MSAA attachment.
        let stride = if quality.format == wgpu::TextureFormat::Rgba16Float {
            8
        } else {
            4
        };
        let samples = if quality.samples == 1 {
            1
        } else {
            quality.samples + 1
        };
        let bytes = (960 * 544 + 2 * 736 * 416) * stride * u64::from(samples);
        assert_eq!(renderer.usage().targets, bytes);
        let submission = renderer.submit(prepared).unwrap();
        renderer
            .gpu()
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        assert!(renderer.poll().unwrap().contains(&submission));
    }
    println!(
        "animated_shared_backdrops submissions=32 targets={}",
        renderer.usage().targets
    );
}

#[test]
#[cfg(feature = "wgpu")]
#[ignore = "actual adapter submission; no capture or pixel readback"]
fn single_backdrop_budget_tiers_submit_the_same_finite_copy() {
    for (domain, source) in [([60, 24], [64u32, 32]), ([600, 472], [608, 480])] {
        let frame = frame_groups(&[domain], false);
        let source_pixels = u64::from(source[0]) * u64::from(source[1]);
        for limit in [(960 * 544 + 2 * source_pixels) * 8, LIMIT] {
            let gpu = pollster::block_on(crate::gpu::Gpu::request(
                &wgpu::Instance::default(),
                None,
                crate::gpu::DeviceGeneration(1),
                crate::gpu::GpuLimits {
                    target_bytes: limit,
                    ..Default::default()
                },
            ))
            .unwrap();
            let mut renderer = crate::gpu::Renderer::new(gpu);
            renderer.prepare_pipelines().unwrap();
            for _ in 0..8 {
                let prepared = renderer.prepare(&frame).unwrap();
                let quality = prepared.quality();
                assert_eq!(quality.format, wgpu::TextureFormat::Rgba16Float);
                let samples = if quality.samples == 1 {
                    1
                } else {
                    quality.samples + 1
                };
                let pixels = if limit == LIMIT && 960 * 544 <= 2 * source_pixels {
                    2 * 960 * 544 + source_pixels
                } else {
                    960 * 544 + 2 * source_pixels
                };
                assert_eq!(renderer.usage().targets, pixels * 8 * u64::from(samples));
                let submission = renderer.submit(prepared).unwrap();
                renderer
                    .gpu()
                    .device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .unwrap();
                assert!(renderer.poll().unwrap().contains(&submission));
            }
            println!(
                "single_backdrop source={source:?} limit={limit} submissions=8 targets={}",
                renderer.usage().targets
            );
        }
    }
}
