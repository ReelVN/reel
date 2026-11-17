//! Exercise the production tick -> redraw path with a ready derived transaction.
use super::*;
use crate::assets::{AssetOptions, Assets};
use std::time::Duration;

fn ready_player(code: &str) -> (Player<Backend>, Host) {
    let driver = AsideSessionUi::from_source(
        session(),
        code,
        "queued_redraw",
        1,
        "main",
        "en",
        VmLimits::default(),
    )
    .unwrap()
    .with_assets(Assets::directory(AssetOptions {
        root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/font-weight"),
        ..Default::default()
    }));
    let mut host = Host::default();
    let backend = Backend {
        inner: RecordingBackend::new(ConsumerLimits {
            max_uploads: 2,
            max_resources: 4,
            max_frames: 2,
            max_events: 16,
            max_upload_bytes: 1 << 20,
            max_retained_bytes: 1 << 20,
            max_metadata_bytes: 8192,
            max_fixed_bytes: 16384,
        })
        .unwrap(),
        refusal: None,
        polls: 0,
        resources_available: false,
        configurations: Vec::new(),
        preparing: false,
        frames: Vec::new(),
        cancelled: Vec::new(),
    };
    let mut player = Player::new(
        driver,
        backend,
        &mut host,
        &PlayerOptions {
            logical_size: Some([640, 420]),
            canvas_size: Some([640, 420]),
            ..Default::default()
        },
    )
    .unwrap();
    for _ in 0..1000 {
        player.tick(&mut host).unwrap();
        if !player.settings_pending() && player.session.as_ref().unwrap().frame_ready() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!player.settings_pending());
    let font = player.resources[0].clone();
    let reel_ui::resources::ResourceView::Font(data) = font.view() else {
        panic!("font resource");
    };
    let pool = BudgetPool::new(
        BudgetCategory::CpuInput,
        QueueLimits {
            max_requests: 8,
            max_bytes: 1 << 20,
        },
    );
    let bytes = OwnedBytes::new(
        data.as_ref().to_vec(),
        pool.reserve(data.len() as u64).unwrap(),
    )
    .unwrap();
    let items = vec![UploadItem {
        resource: font,
        payload: UploadPayload::Font { bytes },
    }];
    let storage = pool
        .reserve((items.capacity() * size_of::<UploadItem>()) as u64)
        .unwrap();
    let upload = player
        .backend
        .upload(UploadBatch {
            items,
            scope: reel_core::identity::Scope::Process,
            storage,
        })
        .unwrap();
    assert!(
        player
            .backend
            .inner
            .complete_upload(upload, reel_core::result::Terminal::Completed(()))
    );
    (player, host)
}

#[test]
fn queued_settled_projection_is_recorded_before_redraw_can_run_the_next_transaction() {
    let (mut player, mut host) = ready_player(
        r##"use std::math::sin;
        fn sample(value) {
            let mut sum=0.0;
            for i in 0..32 {sum+=sin(value+i);}
            value+sum*0
        }
        fn main() {
            game.configure(fonts:["400.ttf"]);
            let state=model({x:0,y:0});
            stage.replace(stack(width:64,height:64,background:"#ff0000",
                x:derive(||sample(state.x)),y:derive(||sample(state.y))));
            await stage.presented();
            let gate=completion(); gate.resolve(null);
            for i in 1..=12 {state.x=i; state.y=i; await gate;}
            await stage.presented();
        }"##,
    );
    player.render(&mut host).unwrap();
    let first = player.presentation.bridge.accepted_key().unwrap().clone();
    player
        .backend
        .inner
        .complete_render(player.backend.inner.last_submission().unwrap());
    player.poll_completions().unwrap();
    player.tick(&mut host).unwrap();
    let story = player.story_mut().unwrap();
    let node = story.driver().adapter().stage().nodes().next().unwrap();
    assert_eq!([node.props.x, node.props.y], [1., 1.]);
    let before = story.driver().adapter().vm().stats();
    assert!(player.needs_redraw());
    host.request_redraw();
    player.redraw_requested(&mut host).unwrap();
    assert_ne!(player.presentation.bridge.accepted_key(), Some(&first));
    let story = player.story_mut().unwrap();
    let node = story.driver().adapter().stage().nodes().next().unwrap();
    assert_eq!([node.props.x, node.props.y], [1., 1.]);
    assert_eq!(
        story.driver().adapter().vm().stats().native_entries,
        before.native_entries
    );
    assert_eq!(story.driver().adapter().vm().stats().slices, before.slices);
}

#[test]
fn accepted_animation_frames_keep_the_sampling_deadline_while_script_waits() {
    let (mut player, mut host) = ready_player(
        r##"
        use std::math::sin;
        fn main() {
            game.configure(fonts:["400.ttf"]);
            let carrier=stack(mask_progress:0);
            stage.replace(stack(width:64,height:64,background:"#ff0000",
                x:derive(||100*carrier.mask_progress+sin(carrier.mask_progress))));
            await stage.presented();
            await tween(carrier,mask_progress:1,duration:1).run();
        }
    "##,
    );
    player.render(&mut host).unwrap();
    player
        .backend
        .inner
        .complete_render(player.backend.inner.last_submission().unwrap());
    player.poll_completions().unwrap();
    for _ in 0..100 {
        player.tick(&mut host).unwrap();
        let story = player.story_mut().unwrap();
        if !story.driver().has_runnable_tasks() && !story.driver().activities_pending() {
            break;
        }
    }
    assert!(!player.story_mut().unwrap().driver().has_runnable_tasks());
    host.now = MonoTime(16_666_667);
    player.tick(&mut host).unwrap();
    let x = |player: &mut Player<Backend>| {
        player
            .story_mut()
            .unwrap()
            .driver()
            .adapter()
            .stage()
            .nodes()
            .find(|node| node.props.width == Some(64.))
            .unwrap()
            .props
            .x
    };
    let sampled = x(&mut player);
    assert!(sampled > 0.);
    let native_entries = player
        .story_mut()
        .unwrap()
        .driver()
        .adapter()
        .vm()
        .stats()
        .native_entries;
    player.render(&mut host).unwrap();
    player
        .backend
        .inner
        .complete_render(player.backend.inner.last_submission().unwrap());
    player.poll_completions().unwrap();
    for ms in 17..34 {
        host.now = MonoTime(ms * 1_000_000);
        player.tick(&mut host).unwrap();
        assert_eq!(
            x(&mut player),
            sampled,
            "sampling before deadline at {ms}ms"
        );
        assert_eq!(
            player
                .story_mut()
                .unwrap()
                .driver()
                .adapter()
                .vm()
                .stats()
                .native_entries,
            native_entries
        );
    }
    host.now = MonoTime(33_333_334);
    player.tick(&mut host).unwrap();
    assert!(x(&mut player) > sampled);
}

fn accept_frame(player: &mut Player<Backend>, host: &mut Host) {
    player.render(host).unwrap();
    player
        .backend
        .inner
        .complete_render(player.backend.inner.last_submission().unwrap());
    player.poll_completions().unwrap();
}

fn settle_player(player: &mut Player<Backend>, host: &mut Host) {
    for _ in 0..1000 {
        player.tick(host).unwrap();
        let story = player.story_mut().unwrap();
        if !story.driver().has_runnable_tasks()
            && !story.driver().activities_pending()
            && story.driver().adapter().stage_ready()
            && player
                .story_deadline(host.now)
                .is_none_or(|deadline| deadline > host.now)
        {
            return;
        }
    }
    panic!("player work did not settle");
}

#[test]
fn native_scroll_bounce_uses_player_deadlines_and_sleeps_without_vm_animation_work() {
    check_native_scroll_bounce(false);
}

#[test]
fn native_scroll_bounce_shares_frames_with_authored_animation() {
    check_native_scroll_bounce(true);
}

fn check_native_scroll_bounce(animated: bool) {
    use reel_ui::core::{PointerEvent, PointerType};
    let code = r##"fn main() {
            game.configure(fonts:["400.ttf"],frame_rate:60);
            let background=stack(width:10,height:10);
            let root=stack(width:640,height:420) {
                background;
                scroll(width:200,height:100,overscroll:"bounce") {
                    stack(width:200,height:1000,background:"#ff0000");
                }
            };
            stage.replace(root);
            await stage.presented();
            /* motion */
            await completion();
        }"##
    .replace(
        "/* motion */",
        if animated {
            "run(tween(background,x:100,duration:1),owner:root);"
        } else {
            ""
        },
    );
    let (mut player, mut host) = ready_player(&code);
    accept_frame(&mut player, &mut host);
    settle_player(&mut player, &mut host);
    host.now = MonoTime(5_000_000);
    let touch = |kind, point| {
        let mut event = mouse(kind, point);
        match &mut event {
            PointerEvent::Down(event) | PointerEvent::Up(event) => {
                event.pointer.pointer_type = PointerType::Touch;
                event.button = None;
            }
            PointerEvent::Move(event) => event.pointer.pointer_type = PointerType::Touch,
            _ => unreachable!(),
        }
        event
    };
    player.dispatch_pointer(touch("down", (20., 20.))).unwrap();
    player.dispatch_pointer(touch("move", (20., 60.))).unwrap();
    accept_frame(&mut player, &mut host);
    settle_player(&mut player, &mut host);
    player.dispatch_pointer(touch("up", (20., 60.))).unwrap();
    settle_player(&mut player, &mut host);
    assert!(player.presentation.bridge.root().needs_anim());
    let before = player.story_mut().unwrap().driver().adapter().vm().stats();
    let presented = host.presents;
    let animation_started = host.now;
    for _ in 0..80 {
        if player.needs_redraw() {
            accept_frame(&mut player, &mut host);
            settle_player(&mut player, &mut host);
            continue;
        }
        if !player.presentation.bridge.root().needs_anim() {
            break;
        }
        let Some(deadline) = player.story_deadline(host.now) else {
            assert!(
                !player.presentation.bridge.root().needs_anim(),
                "animation lost its deadline: model={:?}, ready={}, redraw={}",
                player.presentation.demand.model,
                player.session.as_ref().unwrap().frame_ready(),
                player.needs_redraw()
            );
            break;
        };
        assert!(deadline > host.now, "native animation busy-looped");
        host.now = deadline;
        player.tick(&mut host).unwrap();
        if player.needs_redraw() {
            accept_frame(&mut player, &mut host);
        }
        settle_player(&mut player, &mut host);
    }
    assert!(!player.presentation.bridge.root().needs_anim());
    assert!(host.presents > presented + 2);
    let driver = player.story_mut().unwrap().driver();
    if animated {
        let background = driver
            .adapter()
            .stage()
            .nodes()
            .find(|node| node.props.width == Some(10.))
            .unwrap();
        assert!(
            background.props.x > 20.,
            "native UI animation starved the story animation"
        );
        let cadence_frames = (host.now.0 - animation_started.0) / 16_666_666;
        assert!(
            host.presents - presented <= cadence_frames as usize + 3,
            "native animation created a second frame cadence"
        );
    } else {
        let after = driver.adapter().vm().stats();
        assert_eq!(after.slices, before.slices);
        assert_eq!(after.native_entries, before.native_entries);
        assert!(player.story_deadline(host.now).is_none());
    }
}

fn animated_x(player: &mut Player<Backend>) -> f64 {
    player
        .story_mut()
        .unwrap()
        .driver()
        .adapter()
        .stage()
        .nodes()
        .find(|node| node.props.width == Some(64.) && node.props.height == Some(64.))
        .unwrap()
        .props
        .x
}

#[test]
fn queued_clock_projection_coalesces_until_the_next_candidate_slot() {
    let (mut player, mut host) = ready_player(
        r##"fn main() {
            game.configure(fonts:["400.ttf"]);
            let timer=clock(interval:1.0/60,paused:true);
            stage.replace(stack(width:64,height:64,background:"#ff0000",
                x:derive(||timer.elapsed*1000)));
            await stage.presented(); timer.resume();
            await delay(5).run(); timer.stop();
        }"##,
    );
    accept_frame(&mut player, &mut host);
    settle_player(&mut player, &mut host);
    host.now = MonoTime(20_000_000);
    settle_player(&mut player, &mut host);
    let sampled = animated_x(&mut player);
    let slices = player
        .story_mut()
        .unwrap()
        .driver()
        .adapter()
        .vm()
        .stats()
        .slices;
    assert!(sampled > 0.);
    for ms in [40, 100, 500] {
        host.now = MonoTime(ms * 1_000_000);
        settle_player(&mut player, &mut host);
        assert_eq!(animated_x(&mut player), sampled);
        assert_eq!(
            player
                .story_mut()
                .unwrap()
                .driver()
                .adapter()
                .vm()
                .stats()
                .slices,
            slices
        );
    }
    assert!(player.story_deadline(host.now).unwrap() > host.now);
    accept_frame(&mut player, &mut host);
    settle_player(&mut player, &mut host);
    assert!(
        (animated_x(&mut player) - 500.).abs() < 0.001,
        "missed clock intervals combine into the current sample"
    );
}

#[test]
fn retained_candidate_keeps_finite_activity_deadlines_running() {
    let (mut player, mut host) = ready_player(
        r##"fn main() {
            game.configure(fonts:["400.ttf"]);
            let timer=clock(interval:1.0/60,paused:true);
            let node=stack(width:64,height:64,background:"#ff0000",
                x:derive(||timer.elapsed*1000));
            stage.replace(node); await stage.presented(); timer.resume();
            await delay(0.15).run(); timer.stop(); node.opacity=0.25;
            await stage.presented();
        }"##,
    );
    accept_frame(&mut player, &mut host);
    settle_player(&mut player, &mut host);
    host.now = MonoTime(20_000_000);
    settle_player(&mut player, &mut host);
    let sampled = animated_x(&mut player);
    player.backend.preparing = true;
    player.render(&mut host).unwrap();
    assert!(player.presentation.is_preparing());
    host.now = MonoTime(100_000_000);
    settle_player(&mut player, &mut host);
    assert_eq!(animated_x(&mut player), sampled);
    assert_eq!(player.story_deadline(host.now), Some(MonoTime(150_000_000)));
    host.now = MonoTime(150_000_001);
    settle_player(&mut player, &mut host);
    let story = player.story_mut().unwrap();
    assert_eq!(
        story
            .driver()
            .adapter()
            .stage()
            .nodes()
            .next()
            .unwrap()
            .props
            .opacity,
        0.25
    );
    assert!(
        player.presentation.is_preparing(),
        "the recorded candidate retains its own lifetime"
    );
}

#[test]
fn queued_projection_preserves_clock_next_and_input_wakes() {
    let (mut player, mut host) = ready_player(
        r##"fn main() {
            game.configure(fonts:["400.ttf"]);
            let state=model({clicked:0.0});
            let timer=clock(interval:1.0/60,paused:true);
            let view=button(width:64,height:64,
                x:derive(||state.clicked*1000),
                on_click:|_|{state.clicked=timer.elapsed;});
            stage.replace(view); await stage.presented(); timer.resume();
            view.opacity=0.75;
            timer.next(); view.opacity=0.5;
            await delay(5).run(); timer.stop();
        }"##,
    );
    accept_frame(&mut player, &mut host);
    settle_player(&mut player, &mut host);
    assert_eq!(
        player.presentation.demand.model,
        crate::demand::Model::Pending
    );
    host.now = MonoTime(20_000_000);
    settle_player(&mut player, &mut host);
    assert_eq!(
        player
            .story_mut()
            .unwrap()
            .driver()
            .adapter()
            .stage()
            .nodes()
            .next()
            .unwrap()
            .props
            .opacity,
        0.5
    );
    host.now = MonoTime(100_000_000);
    for kind in ["down", "up"] {
        player.dispatch_pointer(mouse(kind, (30., 30.))).unwrap();
    }
    settle_player(&mut player, &mut host);
    assert!(
        (animated_x(&mut player) - 100.).abs() < 0.001,
        "the callback reads a fresh settled clock despite the queued drawing"
    );
}

#[test]
fn unchanged_projection_leaves_the_next_sampling_slot_available() {
    let (mut player, mut host) = ready_player(
        r##"fn main() {
            game.configure(fonts:["400.ttf"]);
            let timer=clock(interval:1.0/60,paused:true);
            stage.replace(stack(width:64,height:64,background:"#ff0000",
                x:derive(||if timer.elapsed<1 {5.0}else{10.0})));
            await stage.presented(); timer.resume();
            await delay(5).run(); timer.stop();
        }"##,
    );
    accept_frame(&mut player, &mut host);
    settle_player(&mut player, &mut host);
    let revision = player
        .story_mut()
        .unwrap()
        .driver()
        .adapter()
        .stage()
        .revision();
    for ms in [20, 200] {
        host.now = MonoTime(ms * 1_000_000);
        settle_player(&mut player, &mut host);
        let story = player.story_mut().unwrap();
        assert_eq!(story.driver().adapter().stage().revision(), revision);
        assert_eq!(player.presentation.demand.model, crate::demand::Model::Idle);
        assert!(
            player.story_deadline(host.now).unwrap() > host.now,
            "an unchanged drawing still advances the clock to its next interval"
        );
    }
}
