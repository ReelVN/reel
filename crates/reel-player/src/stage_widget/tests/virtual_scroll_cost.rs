use super::*;
use crate::aside_session::{AsideSessionUi, StoryProgress};
use reel_engine::aside::{RunOutcome, SliceBudget, VmLimits, source};
fn settle(driver: &mut AsideSessionUi) {
    for _ in 0..1_000_000 {
        match driver
            .tick(
                SliceBudget {
                    instructions: 1000,
                    ..Default::default()
                },
                64,
                || 0,
            )
            .unwrap()
        {
            StoryProgress::Idle | StoryProgress::Vm(RunOutcome::Waiting(_))
                if !driver.has_runnable_tasks() && !driver.activities_pending() =>
            {
                return;
            }
            StoryProgress::HostFailed(_) => panic!("{:?}", driver.host_failure()),
            StoryProgress::Faulted(_) => panic!("{:?}", driver.fault()),
            _ => {}
        }
    }
    panic!("positioning did not settle");
}
#[test]
#[ignore = "requires REEL_SESSION_FONT; release far-target layout measurement"]
fn virtual_scroll_to_far_target_has_bounded_actual_text_layout() {
    let font: reel_ui::peniko::Blob<u8> =
        std::fs::read(std::env::var_os("REEL_SESSION_FONT").unwrap())
            .unwrap()
            .into();
    let mut counts = Vec::new();
    for total in [1000, 10_000, 100_000] {
        let code = format!(
            r#"fn main(){{
            let values=[];let mut i=0;while i<{total}{{values.append(i);i+=1;}}
            let items=list(||values,key:|x|x,virtual:true,build:|row|column{{
                text(rich("[b]A measured target[/b] has a longer description that wraps naturally across several lines. The row is measured at the real viewport width before positioning completes."),width:300,font_size:20);
            }});
            stage.replace(scroll(width:320,height:120){{items}});
            let row=await items.scroll_to({total}-20,align:"center");row.opacity=0.75;
            narrator("positioned");
        }}"#
        );
        let compiled = source::compile_story(&code, "scroll_cost", 1, "main").unwrap();
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            compiled,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
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
                default_font: Some(font.clone()),
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
        let started = std::time::Instant::now();
        for frame in 1..10 {
            driver.sync(&mut bridge, root_id).unwrap();
            let Some((recording, stamp)) =
                driver.record(&mut bridge, root_id, FrameId(frame)).unwrap()
            else {
                settle(&mut driver);
                continue;
            };

            assert_eq!(
                bridge.resolve(PresentationNotice::new(
                    recording.key(),
                    SurfaceEpoch(1),
                    PresentationOutcome::Accepted(FrameId(frame))
                )),
                NoticeResult::Accepted
            );
            driver.presented(&bridge, &stamp).unwrap();
            settle(&mut driver);
        }
        let elapsed = started.elapsed();
        assert!(
            driver
                .adapter()
                .stage()
                .nodes()
                .any(|n| n.props.opacity == 0.75)
        );
        fn walk(widget: WidgetRef<'_, dyn Widget>, count: &mut (usize, usize)) {
            if let Some(scene) = widget.downcast::<SceneWidget>() {
                count.0 += 1;
                if matches!(scene.node.kind, NodeKind::Text(_)) {
                    count.1 += scene.layouts;
                }
            }
            for child in widget.children() {
                walk(child, count);
            }
        }
        let mut count = (0, 0);
        walk(bridge.root().get_widget(root_id).unwrap(), &mut count);
        assert!(count.0 < 40 && count.1 < 40, "{total}: {count:?}");
        counts.push(count);
        eprintln!(
            "scroll-to real-font total={total} widgets={} text_layouts={} accepted_cpu={elapsed:?}",
            count.0, count.1
        );
        driver.cancel().unwrap();
    }
    assert_eq!(counts[0], counts[1]);
    assert_eq!(counts[1], counts[2]);
}
