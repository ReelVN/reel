use super::*;
use reel_core::identity::{FrameId, SurfaceEpoch};
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::{BridgeLimits, NoticeResult, PresentationNotice, PresentationOutcome},
    dpi::{PhysicalPosition, PhysicalSize},
};
use std::sync::Arc;

pub(crate) fn session() -> Session {
    Session::new(SessionId(42), Epoch(3)).unwrap()
}
fn say(session: &mut Session, text: &str) -> InteractionId {
    session
        .begin_say(
            session.root_owner(),
            &[DialogueLineInput {
                history: Default::default(),
                speaker_id: Some("host"),
                speaker_name: "Host",
                content_id: "test/dialogue",
                text: text.into(),
            }],
            "en",
            true,
        )
        .unwrap()
}
pub(crate) fn mount(ui: &SessionUi) -> (PresentationBridge, WidgetId) {
    mount_with_font(ui, None)
}
pub(super) fn mount_with_font(
    ui: &SessionUi,
    font: Option<reel_ui::peniko::Blob<u8>>,
) -> (PresentationBridge, WidgetId) {
    let widget = ui.widget().prepare();
    let id = widget.id();
    let root = RenderRoot::new(
        widget,
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(640, 420),
            scale_factor: 1.,
            default_font: font,
        },
    );
    (
        PresentationBridge::new(
            root,
            SurfaceEpoch(1),
            BridgeLimits {
                geometry: reel_ui::app::GeometryLimits { max_bytes: 1 << 20 },
                max_candidates: 4,
                max_bridge_bytes: 64 << 10,
                max_snapshot_bytes: 8 << 20,
            },
        )
        .unwrap(),
        id,
    )
}
pub(super) fn accept(
    ui: &mut SessionUi,
    bridge: &mut PresentationBridge,
    root: WidgetId,
    frame: u64,
) {
    let (source, stamp) = ui.record(bridge, root, FrameId(frame)).unwrap();
    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            source.key(),
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(frame))
        )),
        NoticeResult::Accepted
    );
    ui.presented(bridge, &stamp);
}
#[test]
fn feedback_receipt_does_not_authorize_capture_but_scrolled_complete_receipt_does() {
    let mut ui = SessionUi::new(session()).unwrap();
    let mut stage = reel_ui::stage::Stage::default();
    let node = stage
        .create(reel_ui::stage::NodeKind::Stack, Default::default(), vec![])
        .unwrap();
    stage.replace(node).unwrap();
    ui.set_stage(&stage);
    let (mut bridge, root) = mount(&ui);
    ui.sync(&mut bridge, root).unwrap();
    // The dialogue/history scroll Rc participates in the actual widget stamp.
    ui.scroll.set(125.);
    let (source, mut receipt) = ui.record(&mut bridge, root, FrameId(1)).unwrap();
    assert_eq!(receipt.stamp.scroll_bits, 125f64.to_bits());
    receipt.current = false;
    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            source.key(),
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(1))
        )),
        NoticeResult::Accepted
    );
    assert!(ui.presented(&bridge, &receipt));
    assert!(!ui.current_presentation_ready());
    assert!(ui.binding.borrow().control_accepted.is_some());
    accept(&mut ui, &mut bridge, root, 2);
    assert!(ui.current_presentation_ready());
}
#[test]
fn initial_record_waits_for_preparation_then_uses_explicit_root() {
    let mut driver = crate::aside_session::AsideSessionUi::from_source(
        session(),
        "fn main() { stage.replace(stack(width:40,height:30)); await stage.presented(); }",
        "first_frame",
        1,
        "main",
        "en",
        reel_engine::aside::VmLimits::default(),
    )
    .unwrap();
    let (mut bridge, root) = mount(driver.ui());
    assert!(driver.ui().images_pending());
    assert!(
        driver
            .record(&mut bridge, root, FrameId(1))
            .unwrap()
            .is_none()
    );
    driver.await_initial_stage();
    assert!(driver.adapter().stage().root().is_none());
    driver.sync(&mut bridge, root).unwrap(); // Must take awaiting_stage's early return.
    assert!(
        driver
            .record(&mut bridge, root, FrameId(2))
            .unwrap()
            .is_none()
    );
    for _ in 0..1000 {
        let progress = driver
            .tick(reel_engine::aside::SliceBudget::default(), 256, || 0)
            .unwrap();
        if matches!(
            progress,
            crate::aside_session::StoryProgress::Vm(reel_engine::aside::RunOutcome::Waiting(_))
        ) {
            break;
        }
        assert_eq!(driver.state(), crate::aside_session::StoryState::Running);
    }
    assert_eq!(driver.adapter().presentation_request(), 1);
    driver.sync(&mut bridge, root).unwrap();
    let (source, receipt) = driver
        .record(&mut bridge, root, FrameId(3))
        .unwrap()
        .expect("prepared author frame");
    assert_eq!(receipt.stamp.presentation_request, 1);
    assert_eq!(receipt.key.frame_id(), source.key().frame_id());
    assert_eq!(
        receipt.stage_widget,
        SessionUi::stage_widget(&bridge, root).unwrap()
    );
    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            source.key(),
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(3))
        )),
        NoticeResult::Accepted
    );
    driver.presented(&bridge, &receipt).unwrap();
    let authored = driver.adapter().stage();
    let props = &authored.node(authored.root().unwrap()).unwrap().props;
    assert_eq!((props.width, props.height), (Some(40.), Some(30.)));
}
pub(super) fn mouse(kind: &str, point: (f64, f64)) -> PointerEvent {
    let pointer = PointerInfo {
        pointer_id: Some(PointerId::PRIMARY),
        persistent_device_id: None,
        pointer_type: PointerType::Mouse,
    };
    let state = PointerState {
        position: PhysicalPosition::new(point.0, point.1),
        ..Default::default()
    };
    match kind {
        "move" => PointerEvent::Move(PointerUpdate {
            pointer,
            current: state,
            coalesced: vec![],
            predicted: vec![],
        }),
        "down" => PointerEvent::Down(PointerButtonEvent {
            pointer,
            state,
            button: Some(PointerButton::Primary),
        }),
        "up" => PointerEvent::Up(PointerButtonEvent {
            pointer,
            state,
            button: Some(PointerButton::Primary),
        }),
        _ => PointerEvent::Cancel(pointer),
    }
}
fn click(ui: &mut SessionUi, bridge: &mut PresentationBridge, point: (f64, f64)) {
    for kind in ["move", "down", "up"] {
        ui.handle_pointer(bridge, mouse(kind, point)).unwrap();
    }
}
fn visible(ui: &SessionUi) -> usize {
    ui.projection.lines[0].visible.end
}

#[test]
fn author_scene_renders_edits_original_nodes_and_keeps_final_picture() {
    use crate::aside_session::{AsideSessionUi, StoryProgress, StoryState};
    use reel_core::budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits};
    use reel_core::identity::{AssetKey, ContentVersion, ResourceVersion};
    use reel_engine::aside::{SliceBudget, VmLimits};
    use reel_ui::frame::{BuildLimits, FrameBuild, FrameDraw, FrameInfo, FrameOp, StepBudget};
    use reel_ui::resources::{ResourceHandle, ResourceRegistry};

    fn labels(widget: WidgetRef<'_, dyn Widget>, out: &mut Vec<(WidgetId, String)>) {
        if let Some(label) = widget.downcast::<Label>() {
            out.push((widget.id(), label.text().to_string()));
        }
        for child in widget.children() {
            labels(child, out);
        }
    }
    fn present(
        driver: &mut AsideSessionUi,
        bridge: &mut PresentationBridge,
        root: WidgetId,
        frame: u64,
        resources: (&ResourceRegistry, &[ResourceHandle]),
    ) -> Vec<(WidgetId, String)> {
        driver.sync(bridge, root).unwrap();
        let (source, receipt) = driver
            .record(bridge, root, FrameId(frame))
            .unwrap()
            .expect("settled frame");
        let key = source.key();

        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 16,
                max_bytes: 32 << 20,
            },
        );
        let mut build = FrameBuild::new(
            source,
            resources.0.freeze(resources.1, &pool).unwrap(),
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
        if std::env::var_os("REEL_SESSION_FONT").is_some() {
            assert!(frame.commands().iter().any(|command| matches!(&command.op,
                FrameOp::Draw(FrameDraw::GlyphRun(run)) if run.font_size == 40. && !run.glyphs.is_empty()
            )), "the Chinese title must produce real glyphs from the supplied font");
        }
        assert!(frame.commands().iter().any(
            |command| matches!(&command.op, FrameOp::PushGroup(group) if group.composite.alpha < 1.)
        ));
        assert_eq!(
            bridge.resolve(PresentationNotice::new(
                key,
                SurfaceEpoch(1),
                PresentationOutcome::Accepted(frame.frame_id())
            )),
            NoticeResult::Accepted
        );
        driver.presented(bridge, &receipt).unwrap();
        let root_widget = bridge
            .root()
            .get_widget(root)
            .unwrap()
            .downcast::<SessionWidget>()
            .unwrap();
        let mut output = Vec::new();
        labels(
            bridge.root().get_widget(root_widget.stage).unwrap(),
            &mut output,
        );
        output
    }
    fn advance(
        driver: &mut AsideSessionUi,
        bridge: &mut PresentationBridge,
        root: WidgetId,
        frame: &mut u64,
        resources: (&ResourceRegistry, &[ResourceHandle]),
    ) {
        for _ in 0..2 {
            let widget = bridge
                .root()
                .get_widget(root)
                .unwrap()
                .downcast::<SessionWidget>()
                .unwrap();
            let point = widget.dialogue_rect.center() - Vec2::new(0., widget.scroll.get());
            for kind in ["move", "down", "up"] {
                driver
                    .handle_pointer(bridge, mouse(kind, (point.x, point.y)))
                    .unwrap();
            }
            *frame += 1;
            present(driver, bridge, root, *frame, resources);
        }
    }
    fn run_to_wait(driver: &mut AsideSessionUi) {
        for _ in 0..10_000 {
            match driver.tick(SliceBudget::default(), 256, || 0).unwrap() {
                StoryProgress::Finished => return,
                StoryProgress::HostFailed(stage) => {
                    panic!("host failed: {stage:?}: {:?}", driver.host_failure())
                }
                StoryProgress::Faulted(_) => {
                    panic!("script fault: {:?}", driver.fault().unwrap())
                }
                _ => {}
            }
            if driver.adapter().stage_ready() && !driver.has_runnable_tasks() {
                return;
            }
        }
        panic!("author example did not yield");
    }
    let mut driver = AsideSessionUi::from_source(
        session(),
        include_str!("../../../../examples/author_stage/src/main.aside"),
        "main",
        1,
        "main",
        "zh-CN",
        VmLimits::default(),
    )
    .unwrap();
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 32,
            max_bytes: 128 << 20,
        },
    );
    let mut registry = ResourceRegistry::new(16, 1 << 20).unwrap();
    let mut handles = Vec::new();
    let font = std::env::var_os("REEL_SESSION_FONT").map(|path| {
        let bytes = std::fs::read(path).unwrap();
        let ticket = pool.reserve(bytes.len() as u64).unwrap();
        let handle = registry
            .register_font(
                ResourceVersion {
                    key: AssetKey("author-scene/font".into()),
                    version: ContentVersion([1; 32]),
                },
                OwnedBytes::new(bytes, ticket).unwrap(),
            )
            .unwrap();
        handles.push(handle);
        let lease = registry.acquire(handle).unwrap();
        let reel_ui::resources::ResourceView::Font(font) = lease.view() else {
            unreachable!()
        };
        font.clone()
    });
    let resources = (&registry, handles.as_slice());
    let (mut bridge, root) = mount_with_font(driver.ui(), font);
    let mut frame = 1;
    run_to_wait(&mut driver);
    let before = present(&mut driver, &mut bridge, root, frame, resources);
    let title_id = before
        .iter()
        .find(|(_, text)| text == "画面，由你定义")
        .unwrap()
        .0;
    advance(&mut driver, &mut bridge, root, &mut frame, resources);
    run_to_wait(&mut driver);
    frame += 1;
    let after = present(&mut driver, &mut bridge, root, frame, resources);
    assert!(
        after
            .iter()
            .any(|(id, text)| *id == title_id && text == "同一幅画面，新的语气")
    );
    assert_eq!(
        before.iter().map(|(id, _)| id).collect::<Vec<_>>(),
        after.iter().map(|(id, _)| id).collect::<Vec<_>>()
    );
    advance(&mut driver, &mut bridge, root, &mut frame, resources);
    run_to_wait(&mut driver);
    assert_eq!(driver.state(), StoryState::Finished);
    frame += 1;
    assert_eq!(
        present(&mut driver, &mut bridge, root, frame, resources),
        after
    );
}

#[test]
fn presented_dialogue_reveals_then_advances_once_and_old_frame_cannot_start_successor() {
    let mut session = session();
    say(&mut session, "A full unchanged line");
    let mut ui = SessionUi::preview(session).unwrap();
    let (mut bridge, id) = mount(&ui);
    click(&mut ui, &mut bridge, (100., 300.));
    assert_eq!(visible(&ui), 0);
    assert!(ui.session.take_completion().is_none());
    accept(&mut ui, &mut bridge, id, 1);
    click(&mut ui, &mut bridge, (100., 300.));
    ui.sync(&mut bridge, id).unwrap();
    assert_eq!(visible(&ui), "A full unchanged line".len());
    assert_eq!(ui.session.view().history.len(), 1);
    assert!(ui.session.take_completion().is_none());
    // Accepted frame 1 still depicts the previous reveal projection.
    click(&mut ui, &mut bridge, (100., 300.));
    assert!(ui.session.take_completion().is_none());
    accept(&mut ui, &mut bridge, id, 2);
    click(&mut ui, &mut bridge, (100., 300.));
    let completion = ui.session.take_completion().unwrap();
    assert_eq!(completion.outcome(), &CompletionOutcome::Advanced);
    assert!(ui.session.take_completion().is_none());
    say(&mut ui.session, "The unseen successor");
    ui.sync(&mut bridge, id).unwrap();
    click(&mut ui, &mut bridge, (100., 300.));
    assert_eq!(visible(&ui), 0);
    assert_eq!(ui.session.view().history.len(), 2);
    accept(&mut ui, &mut bridge, id, 3);
    click(&mut ui, &mut bridge, (100., 300.));
    ui.sync(&mut bridge, id).unwrap();
    assert_eq!(visible(&ui), "The unseen successor".len());
}
#[test]
fn auto_condition_preserves_print_clock_and_restarts_only_auto_delay() {
    let mut session = session();
    say(&mut session, "Long enough text");
    session.set_auto_reading(true, Some(std::time::Duration::from_secs(2)));
    let mut ui = SessionUi::preview(session).unwrap();
    let (mut bridge, root) = mount(&ui);
    accept(&mut ui, &mut bridge, root, 1);
    ui.advance_dialogue(0, 10.).unwrap();
    ui.session.set_auto_condition(false);
    ui.advance_dialogue(100_000_000, 10.).unwrap();
    ui.sync(&mut bridge, root).unwrap();
    assert_eq!(visible(&ui), 1, "gate must preserve printing clock");
    accept(&mut ui, &mut bridge, root, 2);
    ui.advance_dialogue(10_000_000_000, 0.).unwrap();
    ui.sync(&mut bridge, root).unwrap();
    accept(&mut ui, &mut bridge, root, 3);
    assert!(!ui.dialogue_needs_time());
    ui.session.set_auto_condition(true);
    ui.advance_dialogue(20_000_000_000, 0.).unwrap();
    ui.advance_dialogue(21_000_000_000, 0.).unwrap();
    ui.session.set_auto_condition(false);
    ui.session.set_auto_condition(true);
    ui.advance_dialogue(22_000_000_000, 0.).unwrap();
    ui.advance_dialogue(23_999_999_999, 0.).unwrap();
    assert!(ui.session.take_completion().is_none());
    ui.advance_dialogue(24_000_000_000, 0.).unwrap();
    assert_eq!(
        ui.session.take_completion().unwrap().outcome(),
        &CompletionOutcome::Advanced
    );
}

#[test]
fn explicit_next_continues_with_auto_condition_closed() {
    let mut session = session();
    let id = session
        .begin_say(
            session.root_owner(),
            &[DialogueLineInput {
                history: Default::default(),
                speaker_id: None,
                speaker_name: "",
                content_id: "next",
                text: (&reel_core::text::Text::parse("Continue[next]").unwrap()).into(),
            }],
            "en",
            true,
        )
        .unwrap();
    session
        .reveal_more(session.root_owner(), id, usize::MAX)
        .unwrap();
    session.set_auto_condition(false);
    let mut ui = SessionUi::preview(session).unwrap();
    let (mut bridge, root) = mount(&ui);
    accept(&mut ui, &mut bridge, root, 1);
    assert!(ui.dialogue_needs_time());
    ui.advance_dialogue(0, 30.).unwrap();
    assert_eq!(
        ui.session.take_completion().unwrap().outcome(),
        &CompletionOutcome::Advanced
    );
}

#[test]
fn closed_auto_condition_allows_manual_reveal_advance_and_skip() {
    for skip in [false, true] {
        let mut session = session();
        say(&mut session, "Gate closed");
        session.set_auto_condition(false);
        let mut ui = SessionUi::preview(session).unwrap();
        let (mut bridge, root) = mount(&ui);
        accept(&mut ui, &mut bridge, root, 1);
        if skip {
            ui.session.set_skip_reading(true, true, false);
            ui.advance_dialogue(0, 30.).unwrap();
            ui.sync(&mut bridge, root).unwrap();
            assert_eq!(visible(&ui), "Gate closed".len());
            assert!(!ui.dialogue_needs_time());
            assert!(ui.session.view().interaction.is_some());
            accept(&mut ui, &mut bridge, root, 2);
            ui.advance_dialogue(0, 30.).unwrap();
        } else {
            click(&mut ui, &mut bridge, (100., 300.));
            ui.sync(&mut bridge, root).unwrap();
            assert_eq!(visible(&ui), "Gate closed".len());
            accept(&mut ui, &mut bridge, root, 2);
            click(&mut ui, &mut bridge, (100., 300.));
        }
        assert_eq!(
            ui.session.take_completion().unwrap().outcome(),
            &CompletionOutcome::Advanced
        );
    }
}

#[test]
fn manual_dialogue_input_stops_reading_modes_after_admission() {
    for skip in [false, true] {
        let mut session = session();
        say(&mut session, "Manual interruption");
        if skip {
            session.set_skip_reading(true, true, false);
        } else {
            session.set_auto_reading(true, Some(std::time::Duration::ZERO));
        }
        let mut ui = SessionUi::preview(session).unwrap();
        let (mut bridge, root) = mount(&ui);
        // No accepted frame: a stale/unseen click must not change policy.
        click(&mut ui, &mut bridge, (100., 300.));
        assert_eq!(ui.session.reading().skipping(), skip);
        assert_eq!(ui.session.reading().automatic(), !skip);
        accept(&mut ui, &mut bridge, root, 1);
        click(&mut ui, &mut bridge, (100., 300.));
        assert!(!ui.session.reading().skipping());
        assert!(!ui.session.reading().automatic());
        ui.sync(&mut bridge, root).unwrap();
        assert_eq!(visible(&ui), "Manual interruption".len());
        accept(&mut ui, &mut bridge, root, 2);
        ui.advance_dialogue(10_000_000_000, 30.).unwrap();
        assert!(
            ui.session.view().interaction.is_some(),
            "manual takeover must stay on this line"
        );
    }
}
#[test]
fn down_cannot_be_reinterpreted_by_a_new_accepted_interaction() {
    let mut session = session();
    let first = say(&mut session, "First");
    session
        .reveal_more(session.root_owner(), first, usize::MAX)
        .unwrap();
    let mut ui = SessionUi::preview(session).unwrap();
    let (mut bridge, id) = mount(&ui);
    accept(&mut ui, &mut bridge, id, 1);
    ui.handle_pointer(&mut bridge, mouse("move", (100., 300.)))
        .unwrap();
    ui.handle_pointer(&mut bridge, mouse("down", (100., 300.)))
        .unwrap();
    ui.session
        .handle_input(InputEvent {
            source_owner: ui.session.root_owner(),
            sequence: InputSequence(1),
            action: InputAction::Advance(first),
        })
        .unwrap();
    ui.session.take_completion().unwrap();
    say(&mut ui.session, "Second");
    ui.sync(&mut bridge, id).unwrap();
    accept(&mut ui, &mut bridge, id, 2);
    ui.handle_pointer(&mut bridge, mouse("up", (100., 300.)))
        .unwrap();
    assert_eq!(visible(&ui), 0);
    assert!(ui.session.take_completion().is_none());
    click(&mut ui, &mut bridge, (100., 300.));
    ui.sync(&mut bridge, id).unwrap();
    assert_eq!(visible(&ui), "Second".len());
}
#[test]
fn actual_button_choice_id_settles_once_consumes_click_and_revokes_removed_node_owner() {
    let mut session = session();
    session
        .begin_choice(
            session.root_owner(),
            &[
                ChoiceInput {
                    key: "door",
                    label: "Door",
                },
                ChoiceInput {
                    key: "window",
                    label: "Window",
                },
            ],
        )
        .unwrap();
    let mut ui = SessionUi::preview(session).unwrap();
    let (mut bridge, root) = mount(&ui);
    let old_owners = ui.choice_owners.clone();
    accept(&mut ui, &mut bridge, root, 1);
    let buttons = bridge
        .root()
        .get_widget(root)
        .unwrap()
        .downcast::<SessionWidget>()
        .unwrap();
    let button_id = buttons.choices[1].button.id();
    let reel_ui::app::InputGeometrySource::Presented(Some(geometry)) =
        bridge.root().input_geometry_source()
    else {
        panic!()
    };
    assert_eq!(geometry.hit_test(Point::new(100., 320.)), Some(button_id));
    click(&mut ui, &mut bridge, (100., 320.));
    let completion = ui.session.take_completion().unwrap();
    assert_eq!(
        completion.outcome(),
        &CompletionOutcome::Selected("window".into())
    );
    say(&mut ui.session, "After choice");
    ui.sync(&mut bridge, root).unwrap();
    assert_eq!(
        visible(&ui),
        0,
        "choice Up cannot advance the replacement dialogue"
    );
    assert!(
        old_owners
            .iter()
            .all(|owner| !ui.session.is_owner_live(*owner))
    );
    click(&mut ui, &mut bridge, (100., 320.));
    assert_eq!(visible(&ui), 0);
    assert!(ui.session.take_completion().is_none());
}
#[test]
fn cancelled_session_and_cleared_surface_have_no_input_permission() {
    let mut session = session();
    say(&mut session, "Cancel this");
    let mut ui = SessionUi::preview(session).unwrap();
    let (mut bridge, root) = mount(&ui);
    accept(&mut ui, &mut bridge, root, 1);
    bridge.clear();
    ui.clear_presented();
    click(&mut ui, &mut bridge, (100., 300.));
    assert_eq!(visible(&ui), 0);
    assert!(ui.cancel());
    assert!(!ui.cancel());
    ui.sync(&mut bridge, root).unwrap();
    click(&mut ui, &mut bridge, (100., 300.));
    assert!(matches!(
        ui.session.take_completion().unwrap().outcome(),
        CompletionOutcome::Cancelled(_)
    ));
    assert!(ui.session.take_completion().is_none());
    assert_eq!(ui.session.view().history.len(), 1);
}
#[test]
fn revoked_source_owner_input_is_filtered_without_a_window_error() {
    let mut session = session();
    say(&mut session, "A live main interaction");
    let mut ui = SessionUi::preview(session).unwrap();
    let (mut bridge, root) = mount(&ui);
    accept(&mut ui, &mut bridge, root, 1);
    ui.session.revoke_owner(ui.source_owner).unwrap();
    click(&mut ui, &mut bridge, (100., 300.));
    assert_eq!(visible(&ui), 0);
    assert!(ui.session.take_completion().is_none());
    assert_eq!(ui.session.status(), reel_engine::SessionStatus::Running);
    assert_eq!(ui.session.view().history.len(), 1);
}

#[test]
fn first_ui_click_after_session_handoff_uses_the_consumed_input_watermark() {
    let mut session = session();
    let interaction = say(&mut session, "Already revealed before UI attachment");
    assert_eq!(
        session.handle_input(InputEvent {
            source_owner: session.root_owner(),
            sequence: InputSequence(100),
            action: InputAction::Advance(interaction),
        }),
        Ok(reel_engine::InputOutcome::Revealed)
    );
    let mut ui = SessionUi::preview(session).unwrap();
    let (mut bridge, root) = mount(&ui);
    accept(&mut ui, &mut bridge, root, 1);
    click(&mut ui, &mut bridge, (100., 300.));
    assert_eq!(
        ui.session_mut().take_completion().unwrap().outcome(),
        &CompletionOutcome::Advanced
    );
    assert!(ui.session_mut().take_completion().is_none());
    assert_eq!(ui.session().view().history.len(), 1);
    ui.sync(&mut bridge, root).unwrap();
    click(&mut ui, &mut bridge, (100., 300.));
    assert!(ui.session_mut().take_completion().is_none());
}

#[test]
fn inputs_through_session_mut_and_ui_share_one_sequence_source() {
    let mut session = session();
    let first = say(&mut session, "First");
    let mut ui = SessionUi::preview(session).unwrap();
    let (mut bridge, root) = mount(&ui);
    accept(&mut ui, &mut bridge, root, 1);
    click(&mut ui, &mut bridge, (100., 300.));
    ui.sync(&mut bridge, root).unwrap();
    assert_eq!(visible(&ui), "First".len());
    let source_owner = ui.session().root_owner();
    let mut foreign = Session::new(SessionId(99), Epoch(3)).unwrap();
    let foreign_interaction = say(&mut foreign, "Foreign interaction");
    // A live source's invalid action still consumes the global watermark.
    assert_eq!(
        ui.session_mut().handle_input(InputEvent {
            source_owner,
            sequence: InputSequence(100),
            action: InputAction::Advance(foreign_interaction),
        }),
        Err(reel_engine::InputRejection::StaleInteraction)
    );
    accept(&mut ui, &mut bridge, root, 2);
    click(&mut ui, &mut bridge, (100., 300.));
    let completion = ui.session_mut().take_completion().unwrap();
    assert_eq!(completion.interaction_id(), first);
    assert_eq!(completion.outcome(), &CompletionOutcome::Advanced);
    assert!(ui.session_mut().take_completion().is_none());
    say(ui.session_mut(), "After external input");
    ui.sync(&mut bridge, root).unwrap();
    assert_eq!(
        ui.session_mut().handle_input(InputEvent {
            source_owner,
            sequence: InputSequence(1_000),
            action: InputAction::Advance(first),
        }),
        Err(reel_engine::InputRejection::StaleInteraction)
    );
    accept(&mut ui, &mut bridge, root, 3);
    click(&mut ui, &mut bridge, (100., 300.));
    ui.sync(&mut bridge, root).unwrap();
    assert_eq!(visible(&ui), "After external input".len());
    assert!(ui.session_mut().take_completion().is_none());
    accept(&mut ui, &mut bridge, root, 4);
    click(&mut ui, &mut bridge, (100., 300.));
    assert_eq!(
        ui.session_mut().take_completion().unwrap().outcome(),
        &CompletionOutcome::Advanced
    );
    assert_eq!(ui.session().view().history.len(), 2);
}

#[test]
fn zero_line_dialogue_advances_through_the_presented_pointer_path_once() {
    let mut session = session();
    let interaction = session
        .begin_say(session.root_owner(), &[], "en", true)
        .unwrap();
    let mut ui = SessionUi::preview(session).unwrap();
    let (mut bridge, root) = mount(&ui);
    click(&mut ui, &mut bridge, (100., 300.));
    assert!(ui.session_mut().take_completion().is_none());
    accept(&mut ui, &mut bridge, root, 1);
    click(&mut ui, &mut bridge, (100., 300.));
    let completion = ui.session_mut().take_completion().unwrap();
    assert_eq!(completion.interaction_id(), interaction);
    assert_eq!(completion.outcome(), &CompletionOutcome::Advanced);
    assert_eq!(ui.session().view().history.len(), 1);
    assert!(ui.session().view().history[0].lines().is_empty());
    assert!(ui.session_mut().take_completion().is_none());
    say(ui.session_mut(), "Unseen successor");
    ui.sync(&mut bridge, root).unwrap();
    click(&mut ui, &mut bridge, (100., 300.));
    assert_eq!(visible(&ui), 0);
    accept(&mut ui, &mut bridge, root, 2);
    click(&mut ui, &mut bridge, (100., 300.));
    ui.sync(&mut bridge, root).unwrap();
    assert_eq!(visible(&ui), "Unseen successor".len());
    assert_eq!(ui.session().view().history.len(), 2);
}

fn scroll(ui: &mut SessionUi, bridge: &mut PresentationBridge, delta: f32) {
    let PointerEvent::Down(event) = mouse("down", (100., 300.)) else {
        unreachable!()
    };
    ui.handle_pointer(
        bridge,
        PointerEvent::Scroll(PointerScrollEvent {
            pointer: event.pointer,
            state: event.state,
            delta: ScrollDelta::LineDelta(0., delta),
        }),
    )
    .unwrap();
}

#[test]
fn many_dialogue_rows_retain_full_text_and_scroll_without_negative_layout() {
    let mut ui = SessionUi::preview(session()).unwrap();
    let (mut bridge, root) = mount(&ui);
    accept(&mut ui, &mut bridge, root, 1);
    let text = "A complete long dialogue line. ".repeat(40);
    let line = DialogueLineInput {
        history: Default::default(),
        speaker_id: None,
        speaker_name: "",
        content_id: "large-group",
        text: (&text).into(),
    };
    ui.session
        .begin_say(ui.session.root_owner(), &[line; 6], "en", true)
        .unwrap();
    ui.sync(&mut bridge, root).unwrap();
    accept(&mut ui, &mut bridge, root, 2);
    assert_eq!(ui.projection.lines.len(), 6);
    assert!(ui.projection.lines.iter().all(|row| row.text == text));
    scroll(&mut ui, &mut bridge, 100_000.);
    accept(&mut ui, &mut bridge, root, 3);
    assert!(ui.scroll.get() > 0.);
    click(&mut ui, &mut bridge, (100., 300.));
    ui.sync(&mut bridge, root).unwrap();
    assert!(
        ui.projection
            .lines
            .iter()
            .all(|row| row.visible.end == text.len())
    );
    accept(&mut ui, &mut bridge, root, 4);
    click(&mut ui, &mut bridge, (100., 300.));
    assert_eq!(
        ui.session.take_completion().unwrap().outcome(),
        &CompletionOutcome::Advanced
    );
    assert_eq!(ui.session.view().history[0].lines().len(), 6);
}

#[test]
fn all_choices_are_reachable_and_scroll_requires_new_presented_geometry() {
    let mut session = session();
    let count = 1025;
    let keys: Vec<_> = (0..count).map(|i| format!("choice-{i}")).collect();
    let rows: Vec<_> = keys
        .iter()
        .map(|key| ChoiceInput { key, label: key })
        .collect();
    session.begin_choice(session.root_owner(), &rows).unwrap();
    let mut ui = SessionUi::preview(session).unwrap();
    assert_eq!(
        ui.choice_owners.len(),
        1,
        "one projection lifetime, not one owner per option"
    );
    let (mut bridge, root) = mount(&ui);
    accept(&mut ui, &mut bridge, root, 1);
    ui.handle_pointer(&mut bridge, mouse("down", (100., 270.)))
        .unwrap();
    scroll(&mut ui, &mut bridge, 100_000.);
    assert!(ui.binding.borrow().accepted.is_none());
    click(&mut ui, &mut bridge, (100., 350.));
    assert!(ui.session.take_completion().is_none());
    accept(&mut ui, &mut bridge, root, 2);
    ui.handle_pointer(&mut bridge, mouse("up", (100., 350.)))
        .unwrap();
    assert!(ui.session.take_completion().is_none());
    let widget = bridge
        .root()
        .get_widget(root)
        .unwrap()
        .downcast::<SessionWidget>()
        .unwrap();
    let last = widget.choices.last().unwrap().button.id();
    let reel_ui::app::InputGeometrySource::Presented(Some(geometry)) =
        bridge.root().input_geometry_source()
    else {
        panic!()
    };
    assert_eq!(geometry.hit_test(Point::new(100., 350.)), Some(last));
    click(&mut ui, &mut bridge, (100., 350.));
    assert_eq!(
        ui.session.take_completion().unwrap().outcome(),
        &CompletionOutcome::Selected(keys.last().unwrap().clone())
    );
    ui.sync(&mut bridge, root).unwrap();
    assert_eq!(ui.scroll.get(), 0.);
}

#[test]
fn gamepad_focus_scrolls_choices_into_view_and_selects_the_presented_last_item() {
    use reel_ui::input::{ActionPhase as P, NavigationAction as A, NavigationEvent};
    let nav = |action, phase| {
        TextEvent::Navigation(NavigationEvent {
            action,
            phase,
            value: if phase == P::Released { 0. } else { 1. },
        })
    };
    let mut session = session();
    let keys: Vec<_> = (0..24).map(|i| format!("choice-{i}")).collect();
    let rows: Vec<_> = keys
        .iter()
        .map(|key| ChoiceInput { key, label: key })
        .collect();
    session.begin_choice(session.root_owner(), &rows).unwrap();
    let mut ui = SessionUi::preview(session).unwrap();
    let (mut bridge, root) = mount(&ui);
    accept(&mut ui, &mut bridge, root, 1);
    for i in 0..keys.len() {
        ui.handle_text(&mut bridge, nav(A::Down, P::Pressed))
            .unwrap();
        ui.handle_text(&mut bridge, nav(A::Down, P::Released))
            .unwrap();
        ui.sync(&mut bridge, root).unwrap();
        accept(&mut ui, &mut bridge, root, i as u64 + 2);
        let focused = bridge.root().focused_widget().unwrap();
        let reel_ui::app::InputGeometrySource::Presented(Some(geometry)) =
            bridge.root().input_geometry_source()
        else {
            panic!("accepted choices")
        };
        let record = geometry.get(focused).unwrap();
        let point = record.window_transform * record.size.to_rect().center();
        assert_eq!(geometry.hit_test(point), Some(focused));
        assert!(
            (0. ..=420.).contains(&point.y),
            "choice {i} must be visible after focus scrolling"
        );
    }
    assert!(ui.scroll.get() > 0.);
    ui.handle_text(&mut bridge, nav(A::Confirm, P::Pressed))
        .unwrap();
    ui.handle_text(&mut bridge, nav(A::Confirm, P::Released))
        .unwrap();
    assert_eq!(
        ui.session.take_completion().unwrap().outcome(),
        &CompletionOutcome::Selected(keys.last().unwrap().clone())
    );
}

#[test]
fn existing_owner_count_does_not_block_choice_projection() {
    let mut session = Session::new(SessionId(42), Epoch(3)).unwrap();
    for _ in 0..100 {
        session.create_owner().unwrap();
    }
    let mut ui = SessionUi::preview(session).unwrap();
    let (mut bridge, root) = mount(&ui);
    accept(&mut ui, &mut bridge, root, 1);
    ui.session
        .begin_choice(
            ui.session.root_owner(),
            &[
                ChoiceInput {
                    key: "a",
                    label: "A",
                },
                ChoiceInput {
                    key: "b",
                    label: "B",
                },
            ],
        )
        .unwrap();
    ui.sync(&mut bridge, root).unwrap();
    assert_eq!(ui.projection.choices.len(), 2);
    assert_ne!(ui.binding.borrow().accepted, Some(ui.projection_stamp()));
    accept(&mut ui, &mut bridge, root, 2);
    assert_eq!(ui.binding.borrow().accepted, Some(ui.projection_stamp()));
    assert!(ui.session.create_owner().is_ok());
    assert_eq!(ui.choice_owners.len(), 1);
    assert!(ui.session.is_owner_live(ui.source_owner));
}

fn transaction_projection_work(nodes: usize) -> (usize, u128) {
    let code = format!(
        r#"fn main() {{
        let root=stack(); let nodes=[];
        for i in 0..{nodes} {{ let child=stack(width:1,height:1); root.add(child); nodes.append(child); }}
        stage.replace(root);
        let gate=completion(); gate.resolve(); await gate;
        for pass in 1..=4 {{ for node in nodes {{ node.x=pass; node.y=pass; }} }}
        await completion();
    }}"#
    );
    let mut driver = crate::aside_session::AsideSessionUi::from_source(
        session(),
        &code,
        "transaction_projection",
        1,
        "main",
        "en",
        reel_engine::aside::VmLimits::default(),
    )
    .unwrap();
    let (mut bridge, root) = mount(driver.ui());
    let mut premature = 0;
    let mut sync_us = 0;
    let mut settled = false;
    for _ in 0..100_000 {
        driver
            .tick(
                reel_engine::aside::SliceBudget {
                    instructions: 32,
                    ..Default::default()
                },
                256,
                || 0,
            )
            .unwrap();
        assert!(!matches!(
            driver.state(),
            crate::aside_session::StoryState::Faulted(_)
                | crate::aside_session::StoryState::HostFailed(_)
        ));
        let blocked =
            !driver.adapter().stage_ready() && !driver.adapter().virtual_feedback_boundary();
        let before = driver.adapter().stage().revision();
        let started = std::time::Instant::now();
        driver.sync(&mut bridge, root).unwrap();
        sync_us += started.elapsed().as_micros();
        premature += usize::from(blocked && driver.adapter().stage().revision() != before);
        if driver.adapter().stage_ready() && !driver.has_runnable_tasks() {
            let stage = driver.adapter().stage();
            let Some(root) = stage.root() else {
                continue;
            };
            let children = &stage.node(root).unwrap().children;
            if children.len() == nodes
                && children.iter().all(|id| {
                    let props = &stage.node(*id).unwrap().props;
                    props.x == 4. && props.y == 4.
                })
            {
                settled = true;
                break;
            }
        }
    }
    assert!(settled, "complete transaction was not published");
    (premature, sync_us)
}

#[test]
fn unfinished_script_transactions_defer_property_projection() {
    assert_eq!(transaction_projection_work(16).0, 0);
}

#[test]
#[ignore = "release CPU measurement of UI synchronization across script budget slices"]
fn transaction_projection_cost() {
    let (premature, sync_us) = transaction_projection_work(512);
    println!("transaction_projection nodes=512 passes=4 premature={premature} sync_us={sync_us}");
}
