//! Whole real dispatch allocation probe; production code remains safe Rust.
#[path = "support/fixture.rs"]
#[allow(
    dead_code,
    reason = "The shared real-control fixture also supports other contracts"
)]
mod fixture;
use fixture::*;
use masonry_core::{
    app::{GeometryLimits, PresentedGeometryView},
    core::*,
    kurbo::{Point, Rect, Size},
};
use reel_core::identity::{FrameId, SurfaceEpoch};
use reel_stage::*;
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::{alloc::System, cell::RefCell, rc::Rc, time::Instant};

#[global_allocator]
static ALLOCATOR: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
fn real_uncaptured_pointer_dispatch_allocations() {
    for count in [32, 256, 1024] {
        let trace = Rc::new(RefCell::new(vec![]));
        let mut group = StageGroup::new(trace.clone(), 1., Rect::new(0., 0., 1920., 1080.));
        for i in 0..count {
            group.add(
                Observed {
                    inner: masonry::widgets::Slider::new(0., 1., 0.),
                    trace: trace.clone(),
                    isolated: false,
                    queries: None,
                }
                .prepare(),
                Point::new((i % 32) as f64 * 60., (i / 32) as f64 * 30.),
                Size::new(60., 30.),
            );
        }
        let (mut root, signals) = root(group.prepare());
        root.handle_window_event(WindowEvent::Resize(masonry_core::dpi::PhysicalSize::new(
            1920, 1080,
        )));
        let nodes = count + 2;
        let bytes = PresentedGeometryView::bytes_for_nodes(nodes).unwrap();
        let mut bridge = PresentationBridge::new(
            root,
            SurfaceEpoch(1),
            BridgeLimits {
                geometry: GeometryLimits {
                    max_nodes: nodes,
                    max_bytes: bytes + 2 * nodes * size_of::<WidgetId>(),
                    max_path: 128,
                },
                max_candidates: 1,
                max_bridge_bytes: 64 * 1024,
                max_snapshot_bytes: (bytes * 3 + 1024) as u64,
            },
        )
        .unwrap();
        let candidate = bridge.record(FrameId(1)).unwrap();
        bridge.resolve(PresentationNotice {
            key: candidate.key(),
            surface_epoch: SurfaceEpoch(1),
            outcome: PresentationOutcome::Accepted(FrameId(1)),
        });
        assert_eq!(candidate.geometry().len(), nodes);
        for (kind, point) in [
            ("first-child-hit", Point::new(15., 10.)),
            ("wide-miss", Point::new(1900., 1070.)),
        ] {
            for _ in 0..20 {
                bridge.handle_pointer_event(mouse("move", point));
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
                bridge.handle_pointer_event(mouse("move", point));
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
                "real dispatch kind={kind} controls={count} actual_nodes={nodes} events=100 allocations={} reallocations={} allocated_bytes={} p95={}ns max={}ns",
                stats.allocations,
                stats.reallocations,
                stats.bytes_allocated,
                elapsed[94],
                elapsed[99]
            );
        }
    }
}
