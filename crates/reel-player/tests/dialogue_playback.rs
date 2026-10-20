use reel_core::identity::SurfaceEpoch;
use reel_engine::{DialogueLineInput, Epoch, InteractionView, Session, SessionId};
use reel_player::session::SessionUi;
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::{BridgeLimits, PresentationBridge},
    core::{DefaultProperties, Widget},
    dpi::PhysicalSize,
};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::{alloc::System, time::Instant};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
fn stable_typewriter_updates_do_not_copy_text_or_allocate_projection_storage() {
    stable_typewriter_cost(true);
    stable_typewriter_cost(false);
}

fn stable_typewriter_cost(reveal_skippable: bool) {
    let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
    let text = "雨".repeat(10_001);
    session
        .begin_say(
            session.root_owner(),
            &[DialogueLineInput {
                history: Default::default(),
                speaker_id: Some("mio"),
                speaker_name: "Mio",
                content_id: "cost",
                text: (&text).into(),
            }],
            "zh-CN",
            reveal_skippable,
        )
        .unwrap();
    let mut ui = SessionUi::new(session).unwrap();
    let widget = ui.widget().prepare();
    let root_id = widget.id();
    let root = RenderRoot::new(
        widget,
        |_| {},
        RenderRootOptions {
            use_system_fonts: false,
            size: PhysicalSize::new(640, 420),
            default_properties: std::sync::Arc::new(DefaultProperties::new()),
            size_policy: WindowSizePolicy::User,
            scale_factor: 1.,
            default_font: None,
        },
    );
    let mut bridge = PresentationBridge::new(
        root,
        SurfaceEpoch(1),
        BridgeLimits {
            geometry: reel_ui::app::GeometryLimits { max_bytes: 1 << 20 },
            max_candidates: 4,
            max_bridge_bytes: 64 << 10,
            max_snapshot_bytes: 8 << 20,
        },
    )
    .unwrap();
    ui.sync(&mut bridge, root_id).unwrap();
    // This host contract fixture acknowledges the projection directly; it measures
    // the playback + projection update, not GPU presentation or device behavior.
    accept(&mut ui, &mut bridge, root_id);
    ui.advance_dialogue(0, 1000.).unwrap();
    ui.advance_dialogue(1_000_000, 1000.).unwrap();
    ui.sync(&mut bridge, root_id).unwrap();
    let started = Instant::now();
    let region = Region::new(GLOBAL);
    for i in 2..=10_001 {
        assert!(ui.advance_dialogue(i * 1_000_000, 1000.).unwrap());
        ui.sync(&mut bridge, root_id).unwrap();
    }
    let stats = region.change();
    let elapsed = started.elapsed();
    assert_eq!((stats.allocations, stats.reallocations), (0, 0));
    let Some(InteractionView::Dialogue(dialogue)) = ui.session().view().interaction else {
        panic!("dialogue")
    };
    assert_eq!(dialogue.text(), text);
    assert_eq!(dialogue.reveal().revealed_count, 10_001);
    assert!(!ui.dialogue_needs_time());
    ui.session_mut()
        .set_auto_reading(true, Some(std::time::Duration::from_secs(3600)));
    accept(&mut ui, &mut bridge, root_id);
    ui.advance_dialogue(20_000_000_000, 1000.).unwrap();
    let auto_started = Instant::now();
    let auto_region = Region::new(GLOBAL);
    for i in 1..=10_000 {
        assert!(
            !ui.advance_dialogue(20_000_000_000 + i * 1_000_000, 1000.)
                .unwrap()
        );
    }
    let auto_stats = auto_region.change();
    ui.session_mut().set_skip_reading(true, true, false);
    let skip_region = Region::new(GLOBAL);
    assert!(ui.advance_dialogue(40_000_000_000, 1000.).unwrap());
    let skip_stats = skip_region.change();
    assert_eq!((skip_stats.allocations, skip_stats.reallocations), (0, 0));
    assert!(ui.session().read_progress().contains("cost"));

    assert_eq!((auto_stats.allocations, auto_stats.reallocations), (0, 0));
    eprintln!(
        "10000 auto clock updates: {:?}; allocations=0, reallocations=0",
        auto_started.elapsed()
    );

    eprintln!(
        "10000 typewriter + projection updates: {elapsed:?}; allocations={}, reallocations={}",
        stats.allocations, stats.reallocations
    );
}

fn accept(
    ui: &mut reel_player::session::SessionUi,
    bridge: &mut reel_ui::bridge::PresentationBridge,
    root: reel_ui::core::WidgetId,
) {
    use reel_core::identity::FrameId;
    use reel_ui::bridge::{PresentationNotice, PresentationOutcome};
    let id = FrameId(bridge.accepted_key().map_or(1, |key| key.frame_id().0 + 1));
    let (source, receipt) = ui.record(bridge, root, id).unwrap();
    bridge.resolve(PresentationNotice::new(
        source.key(),
        source.key().surface_epoch(),
        PresentationOutcome::Accepted(id),
    ));
    assert!(ui.presented(bridge, &receipt));
}

#[test]
fn protected_reveal_skip_uses_time_and_waits_for_complete_presentation() {
    let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
    let id = session
        .begin_say(
            session.root_owner(),
            &[DialogueLineInput {
                history: Default::default(),
                speaker_id: None,
                speaker_name: "",
                content_id: "protected",
                text: "AB".into(),
            }],
            "en",
            false,
        )
        .unwrap();
    let mut ui = SessionUi::new(session).unwrap();
    let widget = ui.widget().prepare();
    let root_id = widget.id();
    let root = RenderRoot::new(
        widget,
        |_| {},
        RenderRootOptions {
            use_system_fonts: false,
            size: PhysicalSize::new(640, 420),
            size_policy: WindowSizePolicy::User,
            default_properties: std::sync::Arc::new(DefaultProperties::new()),
            scale_factor: 1.,
            default_font: None,
        },
    );
    let mut bridge = PresentationBridge::new(
        root,
        SurfaceEpoch(1),
        BridgeLimits {
            geometry: reel_ui::app::GeometryLimits { max_bytes: 1 << 20 },
            max_candidates: 4,
            max_bridge_bytes: 64 << 10,
            max_snapshot_bytes: 16 << 20,
        },
    )
    .unwrap();
    ui.sync(&mut bridge, root_id).unwrap();
    accept(&mut ui, &mut bridge, root_id);
    ui.session_mut().set_skip_reading(true, true, false);
    assert!(!ui.advance_dialogue(0, 2.).unwrap());
    assert!(ui.advance_dialogue(500_000_000, 2.).unwrap());
    let Some(InteractionView::Dialogue(d)) = ui.session().view().interaction else {
        panic!()
    };
    assert_eq!(d.reveal().revealed_count, 1);
    let owner = ui.session().root_owner();
    assert_eq!(
        ui.session_mut()
            .handle_local_input(owner, reel_engine::InputAction::Advance(id))
            .unwrap(),
        reel_engine::InputOutcome::Pending
    );
    assert!(ui.advance_dialogue(1_000_000_000, 2.).unwrap());
    assert!(!ui.advance_dialogue(1_100_000_000, 2.).unwrap());
    assert!(ui.session().view().interaction.is_some());
    ui.sync(&mut bridge, root_id).unwrap();
    accept(&mut ui, &mut bridge, root_id);
    assert!(ui.advance_dialogue(1_200_000_000, 2.).unwrap());
    assert!(ui.session().view().interaction.is_none());
    assert!(ui.session().read_progress().contains("protected"));
}
