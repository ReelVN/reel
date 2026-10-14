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
    let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
    let text = "雨".repeat(10_001);
    session
        .begin_say(
            session.root_owner(),
            &[DialogueLineInput {
                speaker_id: Some("mio"),
                speaker_name: "Mio",
                content_id: "cost",
                text: &text,
            }],
            "zh-CN",
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
    ui.presented(ui.projection_stamp());
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
    eprintln!(
        "10000 typewriter + projection updates: {elapsed:?}; allocations={}, reallocations={}",
        stats.allocations, stats.reallocations
    );
}
