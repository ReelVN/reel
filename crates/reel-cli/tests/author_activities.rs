//! Source and ASB use the same real activity clock, stage and frame pipeline.
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
    let card = stack(width: 20, height: 20, x: 10, y: 10, background: "#ff0000");
    stage.replace(card);
    await run(parallel(
        sequence(tween(card, x: 50, y: 30, duration: 1), tween(card, x: 90, y: 50, duration: 1)),
        sequence(tween(card, rotation: 45, duration: 1), repeat(tween(card, rotation: 90, duration: 0.5), times: 2)),
        tween(card, opacity: 0.5, duration: 2)
    ));
}"##;

fn frames(story: CompiledStory, canvas: Option<[u32; 2]>) -> Vec<[f64; 6]> {
    let mut driver = AsideSessionUi::from_compiled(
        Session::new(SessionId(1), Epoch(1)).unwrap(),
        story,
        "en",
        VmLimits::default(),
    )
    .unwrap();
    for _ in 0..10000 {
        match driver.tick(SliceBudget::default(), 256, || 0).unwrap() {
            StoryProgress::Vm(RunOutcome::Waiting(_)) => break,
            StoryProgress::Faulted(_) => panic!("{:?}", driver.fault().unwrap()),
            StoryProgress::HostFailed(_) => panic!("{:?}", driver.host_failure()),
            _ => {}
        }
    }
    assert!(driver.activities_need_time());
    let widget = driver.widget().prepare();
    let root_id = widget.id();
    let widget = if let Some(size) = canvas {
        reel_ui::widgets::Canvas::new(widget, size)
            .prepare()
            .erased()
    } else {
        widget.erased()
    };
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
    let mut transforms = Vec::new();
    for second in 0..=2 {
        let mut woke = false;
        loop {
            let progress = driver.advance_time(second as f64, 8).unwrap();
            assert!(progress.work <= 8);
            woke |= progress.woke;
            if !progress.pending {
                break;
            }
        }
        assert_eq!(woke, second == 2);
        driver.sync(&mut bridge, root_id).unwrap();
        let (recording, receipt) = driver
            .record(&mut bridge, root_id, FrameId(second as u64 + 1))
            .unwrap()
            .expect("settled frame");
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
        let red = frame
            .commands()
            .iter()
            .filter_map(|command| match &command.op {
                FrameOp::Draw(FrameDraw::Fill {
                    transform,
                    brush: Brush::Solid(color),
                    ..
                }) if color.to_rgba8().to_u8_array() == [255, 0, 0, 255] => {
                    Some(transform.as_coeffs())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(red.len(), 1);
        transforms.push(red[0]);
        assert_eq!(
            bridge.resolve(PresentationNotice::new(
                key,
                SurfaceEpoch(1),
                PresentationOutcome::Accepted(FrameId(second as u64 + 1))
            )),
            NoticeResult::Accepted
        );
        driver.presented(&bridge, &receipt).unwrap();
    }
    for _ in 0..10000 {
        if matches!(
            driver.tick(SliceBudget::default(), 256, || 0).unwrap(),
            StoryProgress::Finished
        ) {
            return transforms;
        }
    }
    panic!("await did not resume after final frame");
}

#[test]
fn source_and_asb_animate_the_same_presented_geometry() {
    let checked = reel_cli::check(SOURCE, "activity.aside").unwrap();
    let built = reel_cli::build(&checked, Some("main::main")).unwrap();
    let direct = frames(reel_cli::into_story(checked, "main::main").unwrap(), None);
    let packaged = frames(reel_cli::decode_story(&built.bytes).unwrap(), None);
    assert_eq!(direct, packaged);
    let c = std::f64::consts::FRAC_1_SQRT_2;
    for (actual, expected) in direct.iter().zip([
        [1., 0., 0., 1., 10., 10.],
        [c, c, -c, c, 50., 30.],
        [0., 1., -1., 0., 90., 50.],
    ]) {
        for (a, b) in actual.iter().zip(expected) {
            assert!((a - b).abs() < 1e-9, "{actual:?}");
        }
    }
}

#[test]
fn fixed_canvas_uses_the_same_composed_frame_with_uniform_scale_and_letterboxing() {
    let checked = reel_cli::check(SOURCE, "activity.aside").unwrap();
    let built = reel_cli::build(&checked, Some("main::main")).unwrap();
    let transforms = frames(
        reel_cli::decode_story(&built.bytes).unwrap(),
        Some([1280, 420]),
    );
    assert_eq!(transforms[0], [0.5, 0., 0., 0.5, 5., 110.]);
    let end = transforms.last().unwrap();
    for (actual, expected) in end.iter().zip([0., 0.5, -0.5, 0., 45., 130.]) {
        assert!((actual - expected).abs() < 1e-9, "{end:?}");
    }
}
