//! Real source/ASB editing changes published geometry without replacing its root.
use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    identity::{FrameId, SurfaceEpoch},
};
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{RunOutcome, SliceBudget, VmLimits, source::CompiledStory},
};
use reel_player::aside_session::{AsideSessionUi, StoryProgress};
use reel_ui::{
    app::{GeometryLimits, RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::{
        BridgeLimits, NoticeResult, PresentationBridge, PresentationNotice, PresentationOutcome,
    },
    core::{DefaultProperties, Widget},
    dpi::PhysicalSize,
    frame::{BuildLimits, FrameBuild, FrameDraw, FrameInfo, FrameOp, StepBudget},
    peniko::Brush,
    resources::ResourceSnapshot,
};
use std::sync::Arc;

const SOURCE: &str = r##"fn main() {
    let red = stack(width: 20, height: 20, background: "#ff0000");
    let blue = stack(width: 20, height: 20, background: "#0000ff");
    let root = row(x: 10, y: 10) { red; blue; };
    stage.replace(root);
    await run(tween(root, duration: 1));
    root.add(stack(width: 20, height: 20, background: "#00ff00"), index: 1);
    await run(tween(root, duration: 1));
    red.remove();
    await run(tween(root, duration: 1));
    root.clear();
    await run(tween(root, duration: 1));
    root.add(stack(width: 20, height: 20, background: "#ffff00"));
    await run(tween(root, duration: 1));
}"##;

fn wait(driver: &mut AsideSessionUi) {
    for _ in 0..10000 {
        match driver.tick(SliceBudget::default(), 256, || 0).unwrap() {
            StoryProgress::Vm(RunOutcome::Waiting(_)) => return,
            StoryProgress::Faulted(_) => panic!("{:?}", driver.fault().unwrap()),
            StoryProgress::HostFailed(_) => panic!("{:?}", driver.host_failure()),
            StoryProgress::Finished => panic!("ended before expected wait"),
            _ => {}
        }
    }
    panic!("expected animation wait");
}
type Fill = ([u8; 4], [f64; 6]);
fn frames(story: CompiledStory) -> Vec<Vec<Fill>> {
    let mut driver = AsideSessionUi::from_compiled(
        Session::new(SessionId(1), Epoch(1)).unwrap(),
        story,
        "en",
        VmLimits::default(),
    )
    .unwrap();
    wait(&mut driver);
    let node_id = driver.adapter().stage().root();
    let widget = driver.widget().prepare();
    let root_id = widget.id();
    let root = RenderRoot::new(
        widget,
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(640, 420),
            scale_factor: 1.,
            default_font: None,
        },
    );
    let mut bridge = PresentationBridge::new(
        root,
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
            max_requests: 32,
            max_bytes: 32 << 20,
        },
    );
    let mut frames = Vec::new();
    for second in 0..5 {
        if second > 0 {
            wait(&mut driver);
        }
        assert_eq!(driver.adapter().stage().root(), node_id);
        driver.sync(&mut bridge, root_id).unwrap();
        let recording = bridge.record(FrameId(second + 1)).unwrap();
        let key = recording.key();
        let mut build = FrameBuild::new(
            recording,
            ResourceSnapshot::capture(std::iter::empty(), &pool).unwrap(),
            FrameInfo {
                logical_size: [640., 420.],
                physical_size: [640, 420],
            },
            BuildLimits { max_bytes: 8 << 20 },
            &pool,
        )
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
        let frame = build.finish().unwrap();
        let fills = frame
            .commands()
            .iter()
            .filter_map(|command| match &command.op {
                FrameOp::Draw(FrameDraw::Fill {
                    transform,
                    brush: Brush::Solid(color),
                    ..
                }) if [
                    [255, 0, 0, 255],
                    [0, 0, 255, 255],
                    [0, 255, 0, 255],
                    [255, 255, 0, 255],
                ]
                .contains(&color.to_rgba8().to_u8_array()) =>
                {
                    Some((color.to_rgba8().to_u8_array(), transform.as_coeffs()))
                }
                _ => None,
            })
            .collect();
        frames.push(fills);
        assert_eq!(
            bridge.resolve(PresentationNotice::new(
                key,
                SurfaceEpoch(1),
                PresentationOutcome::Accepted(FrameId(second + 1))
            )),
            NoticeResult::Accepted
        );
        driver.presented(driver.projection_stamp());
        assert!(driver.advance_time((second + 1) as f64, 64).unwrap().woke);
    }
    for _ in 0..10000 {
        match driver.tick(SliceBudget::default(), 256, || 0).unwrap() {
            StoryProgress::Finished => return frames,
            StoryProgress::Faulted(_) | StoryProgress::HostFailed(_) => panic!("ending failed"),
            _ => {}
        }
    }
    panic!("story did not finish");
}

#[test]
fn source_and_asb_publish_the_same_dynamic_child_order_and_removal() {
    let checked = reel_cli::check(SOURCE, "dynamic-stage.aside").unwrap();
    let built = reel_cli::build(&checked, Some("main::main")).unwrap();
    let direct = frames(reel_cli::into_story(checked, "main::main").unwrap());
    assert_eq!(
        direct,
        frames(reel_cli::decode_story(&built.bytes).unwrap())
    );
    let red = [255, 0, 0, 255];
    let green = [0, 255, 0, 255];
    let blue = [0, 0, 255, 255];
    let yellow = [255, 255, 0, 255];
    for (actual, expected) in direct.iter().zip([
        vec![red, blue],
        vec![red, green, blue],
        vec![green, blue],
        vec![],
        vec![yellow],
    ]) {
        assert_eq!(actual.iter().map(|v| v.0).collect::<Vec<_>>(), expected);
        for (i, (_, transform)) in actual.iter().enumerate() {
            assert_eq!(*transform, [1., 0., 0., 1., 10. + 20. * i as f64, 10.]);
        }
    }
}
