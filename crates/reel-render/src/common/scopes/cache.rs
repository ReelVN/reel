//! One budget-owned structural proof, reused while numeric frame values change.
use super::*;
use reel_core::budget::{BudgetPool, BudgetTicket};
use reel_ui::frame::FrameDrawing;
use std::sync::Arc;

pub(crate) enum Proof {
    Owned(GroupPlan),
    Shared(Arc<Cached>),
}
impl std::ops::Deref for Proof {
    type Target = GroupPlan;
    fn deref(&self) -> &GroupPlan {
        match self {
            Self::Owned(plan) => plan,
            Self::Shared(plan) => &plan.plan,
        }
    }
}
pub(crate) struct Cached {
    key: Vec<u8>,
    plan: GroupPlan,
    _storage: BudgetTicket,
}
/// Retain one structural proof. The key contains every predicate consumed by
/// GroupPlan, including retained/capture boundaries; numeric coordinates and
/// shader parameters stay in the current drawing and are evaluated every frame.
#[derive(Default)]
pub(crate) struct Cache {
    entry: Option<Arc<Cached>>,
    disabled: bool,
}
impl Cache {
    #[cfg(all(test, feature = "wgpu"))]
    pub(crate) fn metadata_bytes(&self) -> u64 {
        self.entry
            .as_ref()
            .map_or(0, |entry| entry._storage.bytes())
    }
    pub(crate) fn get(
        &mut self,
        drawing: &FrameDrawing,
        capture: bool,
        budget: &BudgetPool,
    ) -> Option<Arc<Cached>> {
        if self.disabled {
            return None;
        }
        let commands = drawing.commands();
        if !commands
            .iter()
            .any(|command| matches!(command.op, FrameOp::PushGroup(_)))
        {
            self.clear();
            return None;
        }
        if let Some(entry) = &self.entry
            && entry.key.len() == commands.len()
            && entry.key.iter().copied().eq(keys(drawing, capture))
        {
            return Some(entry.clone());
        }
        self.entry = None;
        // Optional caching never changes frame admission. All allocations are
        // charged before construction; temporary traversal storage ends here.
        let mut temporary = Vec::new();
        let mut storage = budget
            .reserve((commands.len() + size_of::<Cached>() + 2 * size_of::<usize>()) as u64)
            .ok()?;
        let key: Vec<_> = keys(drawing, capture).collect();
        let plan = GroupPlan::new(
            commands,
            Some(0),
            drawing.transitions().iter().map(|t| t.commands.clone()),
            drawing
                .capture_exclusions()
                .iter()
                .filter(|_| capture)
                .cloned(),
            |bytes| {
                temporary.push(budget.reserve(bytes).map_err(|_| GpuError::InvalidSize)?);
                Ok(())
            },
        )
        .ok()?;
        // Transfer the proof's actual vector capacities from construction tickets.
        // Reserve the overlap before dropping those tickets.
        storage
            .resize(
                (key.capacity()
                    + plan.inline.capacity()
                    + plan.source_child.capacity()
                    + size_of::<Cached>()
                    + 2 * size_of::<usize>()) as u64,
            )
            .ok()?;
        let result = Arc::new(Cached {
            key,
            plan,
            _storage: storage,
        });
        self.entry = Some(result.clone());
        Some(result)
    }
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
    /// A transient-pressure retry must not immediately recreate the proof that
    /// caused pressure. Cache reset enables it again.
    pub(crate) fn disable(&mut self) {
        self.entry = None;
        self.disabled = true;
    }
}
fn keys(drawing: &FrameDrawing, capture: bool) -> impl Iterator<Item = u8> + '_ {
    let commands = drawing.commands();
    let mut transitions = drawing
        .transitions()
        .iter()
        .map(|t| t.commands.clone())
        .peekable();
    let mut exclusions = drawing
        .capture_exclusions()
        .iter()
        .filter(move |_| capture)
        .cloned()
        .peekable();
    commands.iter().enumerate().map(move |(index, command)| {
        let boundary =
            touches_boundary(&mut transitions, index) | touches_boundary(&mut exclusions, index);
        let kind = match &command.op {
            FrameOp::PushGroup(group) => {
                8 | u8::from(source_over(
                    group,
                    matches!(
                        commands.get(index + 1).map(|c| &c.op),
                        Some(FrameOp::PopGroup)
                    ),
                )) | (u8::from(group.composite.blend == Default::default()) << 1)
            }
            FrameOp::PopGroup => 4,
            FrameOp::PushContext(_) | FrameOp::PopContext => 5,
            FrameOp::Draw(
                FrameDraw::Fill { composite, .. } | FrameDraw::Stroke { composite, .. },
            ) => u8::from(composite.blend == Default::default()),
            FrameOp::Draw(FrameDraw::Sprites(s)) => {
                u8::from(s.composite.blend == Default::default())
            }
            FrameOp::Draw(FrameDraw::GlyphRun(run)) => {
                u8::from(run.composite.blend == Default::default())
            }
            _ => 0,
        };
        kind | (u8::from(boundary) << 7)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::r01_fixture;
    use reel_core::identity::FrameId;
    use reel_ui::{
        frame::{FrameBuild, FrameInfo, RenderFrame},
        imaging::{
            Composite,
            record::{Draw, Geometry, Group, Scene},
        },
        kurbo::{Affine, Rect},
        peniko::{Brush, Color},
    };

    fn frame(alpha: f32, translate: f64, capture_included: bool) -> RenderFrame {
        let mut scene = Scene::new();
        for index in 0..128 {
            scene.push_group(Group {
                composite: Composite {
                    alpha,
                    ..Default::default()
                },
                ..Default::default()
            });
            for offset in [0., 4.] {
                scene.draw(Draw::Fill {
                    transform: Affine::translate((translate + offset, 0.)),
                    fill_rule: Default::default(),
                    brush: Brush::Solid(Color::WHITE),
                    brush_transform: None,
                    shape: Geometry::Rect(Rect::new(index as f64, 0., index as f64 + 8., 16.)),
                    composite: Default::default(),
                });
            }
            scene.pop_group();
        }
        let pool = r01_fixture::pool();
        let mut widget = r01_fixture::RecordWidget::new(scene);
        widget.clip = false;
        widget.capture_included = capture_included;
        let mut bridge = r01_fixture::widget_bridge(widget, Affine::IDENTITY);
        let mut build = FrameBuild::new(
            bridge.record(FrameId(1)).unwrap(),
            r01_fixture::empty_snapshot(&pool),
            FrameInfo {
                logical_size: [960., 544.],
                physical_size: [960, 544],
            },
            r01_fixture::limits(),
            &pool,
        )
        .unwrap();
        while !build.step(r01_fixture::step_budget()).unwrap().complete {}
        build.finish().unwrap()
    }
    #[test]
    fn numeric_changes_reuse_proofs_and_boundaries_invalidate_them() {
        let budget = r01_fixture::pool();
        let mut cache = Cache::default();
        let a = frame(0.5, 0., true);
        let b = frame(0.7, 9., true);
        let first = cache.get(a.drawing(), false, &budget).unwrap();
        let used = budget.usage().bytes;
        let second = cache.get(b.drawing(), false, &budget).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(used, budget.usage().bytes);
        for (drawing, capture) in [(a.drawing(), false), (b.drawing(), false)] {
            let exact = GroupPlan::new(
                drawing.commands(),
                Some(0),
                drawing.transitions().iter().map(|t| t.commands.clone()),
                drawing
                    .capture_exclusions()
                    .iter()
                    .filter(|_| capture)
                    .cloned(),
                |_| Ok(()),
            )
            .unwrap();
            let cached = cache.get(drawing, capture, &budget).unwrap();
            assert_eq!(exact.inline, cached.plan.inline);
            assert_eq!(exact.source_child, cached.plan.source_child);
        }
        let inline = frame(1., 9., true);
        let changed = cache.get(inline.drawing(), false, &budget).unwrap();
        assert!(!Arc::ptr_eq(&first, &changed));
        let excluded = frame(1., 9., false);
        let normal = cache.get(excluded.drawing(), false, &budget).unwrap();
        let capture = cache.get(excluded.drawing(), true, &budget).unwrap();
        assert!(!Arc::ptr_eq(&normal, &capture));
        cache.disable();
        assert!(cache.get(a.drawing(), false, &budget).is_none());
        drop((first, second, changed, normal, capture));
        assert_eq!(budget.usage().bytes, 0);
    }
    #[test]
    #[ignore = "CPU planning measurement; run explicitly with --nocapture"]
    fn measure_complete_planning_with_stable_numeric_animation() {
        use std::{hint::black_box, time::Instant};
        let frames = [frame(0.5, 0., true), frame(0.7, 9., true)];
        let budget = r01_fixture::pool();
        let glyphs = crate::common::glyph_plan::Plan::default();
        let selection = crate::common::retained::Plan::default();
        let mut cache = Cache::default();
        let iterations = 1000;
        for cached in [false, true] {
            let started = Instant::now();
            let mut allocations = 0u64;
            let mut total = 0;
            for index in 0..iterations {
                let frame = &frames[index % frames.len()];
                let proof = cached.then(|| cache.get(frame.drawing(), false, &budget).unwrap());
                let (plan, bytes, _) = crate::common::offscreen::Plan::choose_with_groups(
                    frame,
                    None,
                    [960, 544],
                    false,
                    1,
                    4096,
                    &glyphs,
                    u64::MAX,
                    &|size| Ok(u64::from(size[0]) * u64::from(size[1]) * 4),
                    crate::common::target_plan::MaskExecution::Inline,
                    &selection,
                    proof,
                    |_| {
                        allocations += 1;
                        Ok(())
                    },
                )
                .unwrap();
                // This is the proof lookup/build performed by replay::start_stream.
                let replay = plan.groups(frame.commands(), true).unwrap_or_else(|| {
                    Proof::Owned(
                        GroupPlan::new(
                            frame.commands(),
                            Some(0),
                            std::iter::empty(),
                            std::iter::empty(),
                            |_| {
                                allocations += 1;
                                Ok(())
                            },
                        )
                        .unwrap(),
                    )
                });
                total += bytes;
                black_box((plan, replay));
            }
            eprintln!(
                "group-proof cached={cached} iterations={iterations} elapsed_us={} reserve_calls={allocations} retained_bytes={} target_bytes={total}",
                started.elapsed().as_micros(),
                budget.usage().bytes
            );
        }
    }
}
