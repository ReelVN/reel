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
    host.now = MonoTime(16_000_000);
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
    for ms in 17..32 {
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
    host.now = MonoTime(32_000_000);
    player.tick(&mut host).unwrap();
    assert!(x(&mut player) > sampled);
}
