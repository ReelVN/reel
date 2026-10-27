//! Transition timing through compiled, accepted CPU frames, without a native surface.
use super::*;
use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    identity::{FrameId, SurfaceEpoch},
};
use reel_engine::{Epoch, Session, SessionId, aside::transitions::TransitionPhase};
use reel_ui::{
    app::{GeometryLimits, RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::{
        BridgeLimits, NoticeResult, PresentationBridge, PresentationNotice, PresentationOutcome,
    },
    core::{Widget, WidgetId},
    dpi::PhysicalSize,
    frame::{BuildLimits, FrameBuild, FrameInfo, RenderFrame, StepBudget},
    resources::ResourceSnapshot,
};

fn candidate(
    story: &mut StoryRuntime,
    bridge: &mut PresentationBridge,
    root: WidgetId,
    id: u64,
    pool: &BudgetPool,
) -> (crate::session::RecordedProjection, RenderFrame) {
    let driver = story.driver_mut().unwrap();
    driver.sync(bridge, root).unwrap();
    let (source, receipt) = driver.record(bridge, root, FrameId(id)).unwrap().unwrap();
    let mut build = FrameBuild::new(
        source,
        ResourceSnapshot::capture(std::iter::empty(), pool).unwrap(),
        FrameInfo {
            logical_size: [640., 420.],
            physical_size: [640, 420],
        },
        BuildLimits { max_bytes: 8 << 20 },
        pool,
    )
    .unwrap();
    build
        .set_transitions(receipt.frame_transitions().to_vec())
        .unwrap();
    while !build
        .step(StepBudget {
            max_commands: 64,
            max_units: 8192,
            max_bytes: 64 << 10,
        })
        .unwrap()
        .complete
    {}
    (receipt, build.finish().unwrap())
}

#[test]
fn transition_starts_at_acceptance_after_preparation_and_suspension() {
    let code = r##"fn main() {
        let node=stack(width:64,height:64,background:"#ff0000");
        stage.replace(node);
        stage.transition(target:node,duration:0.3,change:|| { node.background="#0000ff"; });
        await run(delay(1));
    }"##;
    for encoded in [false, true] {
        for suspended in [false, true] {
            let mut compiled =
                reel_engine::aside::source::compile_story(code, "transition_timing", 1, "main")
                    .unwrap();
            if encoded {
                use aside_vm::bytecode as bc;
                compiled.program = bc::verify(
                    bc::decode(
                        &bc::encode(compiled.program.package(), bc::Limits::default()).unwrap(),
                        bc::Limits::default(),
                    )
                    .unwrap(),
                    bc::Limits::default(),
                )
                .unwrap();
            }
            let driver = AsideSessionUi::from_compiled(
                Session::new(SessionId(74), Epoch(1)).unwrap(),
                compiled,
                "en",
                Default::default(),
            )
            .unwrap();
            let widget = driver.widget().prepare();
            let root = widget.id();
            let tree = RenderRoot::new(
                widget,
                |_| {},
                RenderRootOptions {
                    default_properties: std::sync::Arc::new(Default::default()),
                    use_system_fonts: false,
                    size_policy: WindowSizePolicy::User,
                    size: PhysicalSize::new(640, 420),
                    scale_factor: 1.,
                    default_font: None,
                },
            );
            let mut bridge = PresentationBridge::new(
                tree,
                SurfaceEpoch(1),
                BridgeLimits {
                    geometry: GeometryLimits { max_bytes: 1 << 20 },
                    max_candidates: 4,
                    max_bridge_bytes: 64 << 10,
                    max_snapshot_bytes: 8 << 20,
                },
            )
            .unwrap();
            let pool = BudgetPool::new(
                BudgetCategory::CpuOutput,
                QueueLimits {
                    max_requests: 128,
                    max_bytes: 64 << 20,
                },
            );
            let mut story = StoryRuntime::new(driver);
            // Restored sessions already have this logical offset before host time starts.
            if encoded {
                story.time_offset_ns = 3_000_000_000;
            }
            let base = if suspended {
                story.suspend(0).unwrap();
                story.resume(10_000_000_000);
                10_000_000_000
            } else {
                0
            };
            let mut id = 0;
            for _ in 0..10_000 {
                story.tick(SliceBudget::default(), || base).unwrap();
                story.driver_mut().unwrap().sync(&mut bridge, root).unwrap();
                let phase = story
                    .driver()
                    .adapter()
                    .transition_views()
                    .next()
                    .map(|view| view.phase);
                if !story.driver().adapter().stage_ready() {
                    continue;
                }
                if phase == Some(TransitionPhase::NewPending) {
                    break;
                }
                if phase == Some(TransitionPhase::OldPending) {
                    id += 1;
                    let (receipt, frame) = candidate(&mut story, &mut bridge, root, id, &pool);
                    assert_eq!(
                        bridge.resolve(PresentationNotice::new(
                            frame.key(),
                            SurfaceEpoch(1),
                            PresentationOutcome::Accepted(frame.frame_id())
                        )),
                        NoticeResult::Accepted
                    );
                    story
                        .presented_frame(&bridge, &receipt, &frame, base)
                        .unwrap();
                    story.wake_input();
                }
            }
            assert_eq!(
                story
                    .driver()
                    .adapter()
                    .transition_views()
                    .next()
                    .unwrap()
                    .phase,
                TransitionPhase::NewPending
            );
            id += 1;
            let (receipt, frame) = candidate(&mut story, &mut bridge, root, id, &pool);
            assert_eq!(receipt.frame_transitions()[0].progress, 0.);
            assert_ne!(
                bridge.resolve(PresentationNotice::new(
                    frame.key(),
                    SurfaceEpoch(1),
                    PresentationOutcome::Skipped
                )),
                NoticeResult::Accepted
            );
            story
                .presented_frame(&bridge, &receipt, &frame, base + 100_000_000)
                .unwrap();
            assert_eq!(
                story
                    .driver()
                    .adapter()
                    .transition_views()
                    .next()
                    .unwrap()
                    .phase,
                TransitionPhase::NewPending,
                "an unaccepted candidate must not start playback"
            );
            id += 1;
            let (receipt, frame) = candidate(&mut story, &mut bridge, root, id, &pool);
            // The renderer spends 180ms preparing/submitting this zero-progress candidate.
            let accepted = base + 180_000_000;
            assert_eq!(
                bridge.resolve(PresentationNotice::new(
                    frame.key(),
                    SurfaceEpoch(1),
                    PresentationOutcome::Accepted(frame.frame_id())
                )),
                NoticeResult::Accepted
            );
            story
                .presented_frame(&bridge, &receipt, &frame, accepted)
                .unwrap();
            story.wake_input();
            assert_eq!(
                story.driver().adapter().logical_time(),
                story.time_offset_ns as f64 / 1_000_000_000.,
                "acceptance must not advance unrelated activities"
            );
            story.tick(SliceBudget::default(), || base).unwrap();
            assert_eq!(
                story
                    .driver()
                    .adapter()
                    .transition_views()
                    .next()
                    .unwrap()
                    .progress,
                0.,
                "maintenance at an earlier sample must leave the acceptance anchor intact"
            );
            story
                .presented_frame(&bridge, &receipt, &frame, accepted + 10_000_000)
                .unwrap();
            // Drain the prior clock turn before sampling the acceptance offset.
            // Each deadline-free host call grants only one bounded work slice.
            let sample = accepted + 20_000_000;
            let logical_sample = story.logical_time_at(sample) as f64 / 1_000_000_000.;
            for _ in 0..1000 {
                story.tick(SliceBudget::default(), || sample).unwrap();
                if story.driver().adapter().logical_time() == logical_sample
                    && story.driver().adapter().stage_ready()
                {
                    break;
                }
            }
            assert_eq!(story.driver().adapter().logical_time(), logical_sample);
            let progress = story
                .driver()
                .adapter()
                .transition_views()
                .next()
                .unwrap()
                .progress;
            assert!(
                (progress - 0.02 / 0.3).abs() < 1e-9,
                "source={encoded}, suspended={suspended}: first progress={progress}, expected 20ms of 300ms"
            );
        }
    }
}
