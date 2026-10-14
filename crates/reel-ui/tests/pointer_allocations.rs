//! Whole real dispatch allocation probe; production code remains safe Rust.
#[path = "support/fixture.rs"]
#[allow(
    dead_code,
    reason = "The shared real-control fixture also supports other contracts"
)]
mod fixture;
use fixture::*;
use reel_core::identity::{FrameId, SurfaceEpoch};
use reel_ui::bridge::*;
use reel_ui::{
    app::{GeometryLimits, PresentedGeometryView},
    core::*,
    kurbo::{Point, Rect, Size},
};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::{alloc::System, cell::RefCell, rc::Rc, time::Instant};

#[global_allocator]
static ALLOCATOR: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
fn real_uncaptured_pointer_dispatch_allocations() {
    for (count, modal) in [32, 256, 1024]
        .into_iter()
        .flat_map(|count| [false, true].map(|modal| (count, modal)))
    {
        let trace = Rc::new(RefCell::new(vec![]));
        let mut group = StageGroup::new(trace.clone(), 1., Rect::new(0., 0., 1920., 1080.));
        for i in 0..count {
            group.add(
                Observed {
                    inner: reel_ui::widgets::Slider::new(0., 1., 0.),
                    trace: trace.clone(),
                    isolated: false,
                    queries: None,
                }
                .prepare(),
                Point::new((i % 32) as f64 * 60., (i / 32) as f64 * 30.),
                Size::new(60., 30.),
            );
        }
        let group = group.prepare();
        let group_id = group.id();
        let (mut root, signals) = root(group);
        root.edit_widget(group_id, |mut w| w.ctx.set_modal(modal));
        root.handle_window_event(WindowEvent::Resize(reel_ui::dpi::PhysicalSize::new(
            1920, 1080,
        )));
        let nodes = count + 2;
        let bytes = PresentedGeometryView::bytes_for_nodes(nodes).unwrap();
        let mut bridge = PresentationBridge::new(
            root,
            SurfaceEpoch(1),
            BridgeLimits {
                geometry: GeometryLimits {
                    max_bytes: 2 * bytes,
                },
                max_candidates: 1,
                max_bridge_bytes: 64 * 1024,
                max_snapshot_bytes: (bytes * 3 + 1024) as u64,
            },
        )
        .unwrap();
        let candidate = bridge.record(FrameId(1)).unwrap();
        bridge.resolve(PresentationNotice::new(
            candidate.key(),
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(1)),
        ));
        assert_eq!(candidate.geometry().len(), nodes);
        for (kind, point) in [
            ("first-child-hit", Point::new(15., 10.)),
            ("wide-miss", Point::new(1900., 1070.)),
        ] {
            for _ in 0..20 {
                bridge.handle_pointer_event(mouse("move", point)).unwrap();
            }
            trace.borrow_mut().clear();
            signals.borrow_mut().clear();
            assert_eq!(bridge.root().pointer_capture_target(), None);
            let mut elapsed = Vec::with_capacity(100);
            let region = Region::new(ALLOCATOR);
            for _ in 0..100 {
                trace.borrow_mut().clear();
                signals.borrow_mut().clear();
                let start = Instant::now();
                bridge.handle_pointer_event(mouse("move", point)).unwrap();
                elapsed.push(start.elapsed().as_nanos());
            }
            let stats = region.change();
            assert_eq!(
                stats.allocations, 0,
                "warmed real {kind} dispatch: {nodes} nodes"
            );
            assert_eq!(
                stats.reallocations, 0,
                "warmed dispatch route/trace capacity is stable"
            );
            elapsed.sort_unstable();
            println!(
                "real dispatch modal={modal} kind={kind} controls={count} actual_nodes={nodes} events=100 allocations={} reallocations={} allocated_bytes={} p95={}ns max={}ns",
                stats.allocations,
                stats.reallocations,
                stats.bytes_allocated,
                elapsed[94],
                elapsed[99]
            );
        }

        for id in 2..=3 {
            bridge
                .handle_pointer_event(touch(
                    "down",
                    id,
                    Point::new(15. + (id - 2) as f64 * 60., 10.),
                ))
                .unwrap();
        }
        for _ in 0..20 {
            for id in 2..=3 {
                bridge
                    .handle_pointer_event(touch(
                        "move",
                        id,
                        Point::new(20. + (id - 2) as f64 * 60., 10.),
                    ))
                    .unwrap();
            }
        }
        trace.borrow_mut().clear();
        signals.borrow_mut().clear();
        let region = Region::new(ALLOCATOR);
        for _ in 0..100 {
            for id in 2..=3 {
                trace.borrow_mut().clear();
                signals.borrow_mut().clear();
                bridge
                    .handle_pointer_event(touch(
                        "move",
                        id,
                        Point::new(20. + (id - 2) as f64 * 60., 10.),
                    ))
                    .unwrap();
            }
        }
        let stats = region.change();
        assert_eq!(
            (stats.allocations, stats.reallocations),
            (0, 0),
            "stable simultaneous touch drags"
        );
        println!(
            "touch dispatch modal={modal} controls={count} contacts=2 events=200 allocations={} reallocations={}",
            stats.allocations, stats.reallocations
        );
    }
}
