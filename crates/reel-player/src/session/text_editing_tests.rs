//! Accepted editing work passes through the real SessionUi input and projection gates.
use super::tests::{mount_with_font, session};
use super::*;
use crate::aside_session::{AsideSessionUi, StoryProgress, StoryState};
use reel_core::identity::{FrameId, SurfaceEpoch};
use reel_engine::aside::{
    SessionVm, SliceBudget, VmLimits,
    snapshot::{SessionSnapshot, SnapshotBudget},
    source,
};
use reel_engine::{Epoch, SessionId, SessionIdentity};
use reel_ui::{
    bridge::{NoticeResult, PresentationNotice, PresentationOutcome},
    stage::{NodeId, NodeKind},
};
use std::sync::Arc;

fn drive(driver: &mut AsideSessionUi) {
    for _ in 0..10_000 {
        let progress = driver
            .tick(
                SliceBudget {
                    instructions: 1,
                    ..Default::default()
                },
                1,
                || 0,
            )
            .unwrap();
        assert_eq!(driver.state(), StoryState::Running, "{progress:?}");
        if matches!(
            progress,
            StoryProgress::Idle | StoryProgress::Vm(reel_engine::aside::RunOutcome::Waiting(_))
        ) && !driver.has_runnable_tasks()
        {
            return;
        }
    }
    panic!("input callback did not settle");
}
fn present(
    driver: &mut AsideSessionUi,
    bridge: &mut PresentationBridge,
    root: WidgetId,
    frame: u64,
) {
    driver.sync(bridge, root).unwrap();
    let (recording, receipt) = driver
        .record(bridge, root, FrameId(frame))
        .unwrap()
        .expect("stable projection");
    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            recording.key(),
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(frame))
        )),
        NoticeResult::Accepted
    );
    driver.presented(bridge, &receipt).unwrap();
}
fn input(driver: &AsideSessionUi, node: NodeId) -> (&str, bool) {
    let NodeKind::TextInput { value, options, .. } =
        &driver.adapter().stage().node(node).unwrap().kind
    else {
        unreachable!()
    };
    (value, options.pending_edit)
}
fn restore(driver: &AsideSessionUi) -> AsideSessionUi {
    let mut capture = driver.snapshot_begin().unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let mut image = SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        driver.adapter().vm().program().clone(),
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(803),
            epoch: Epoch(4),
        },
        Default::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (session, vm) = restore.finish().unwrap();
    AsideSessionUi::restored(
        session,
        vm,
        "en".into(),
        driver.ui().assets(),
        driver.initializer(),
        driver.catalogs(),
    )
    .unwrap()
}

#[test]
fn accepted_text_replays_after_undo_outside_physical_dispatch_and_after_restore() {
    const CODE: &str = r#"fn main() {
        let data=model({body:"",changes:0});
        let editor=text_input(value:bind(data.body),width:300,height:40,font:"Reel Weight Fixture",on_change:||{data.changes+=1;});
        stage.replace(column{editor;text(derive(||f"changes:{data.changes}"));});
        narrator("wait");
    }"#;
    for encoded in [false, true] {
        for restored in [false, true] {
            let mut story = source::compile_story(CODE, "text_replay", 1, "main").unwrap();
            if encoded {
                use aside_vm::bytecode as bc;
                let bytes = bc::encode(story.program.package(), bc::Limits::default()).unwrap();
                story.program = bc::verify(
                    bc::decode(&bytes, bc::Limits::default()).unwrap(),
                    bc::Limits::default(),
                )
                .unwrap();
            }
            let mut driver =
                AsideSessionUi::from_compiled(session(), story, "en", VmLimits::default()).unwrap();
            drive(&mut driver);
            let node = driver
                .adapter()
                .stage()
                .nodes()
                .find(|n| matches!(n.kind, NodeKind::TextInput { .. }))
                .unwrap()
                .id;
            let font = || {
                Some(reel_ui::peniko::Blob::new(Arc::new(
                    include_bytes!("../../tests/fixtures/font-weight/400.ttf").to_vec(),
                )))
            };
            let (mut bridge, mut root) = mount_with_font(driver.ui(), font());
            present(&mut driver, &mut bridge, root, 1);
            driver
                .handle_text(
                    &mut bridge,
                    TextEvent::key_down(keyboard::Key::Named(keyboard::NamedKey::Tab)),
                )
                .unwrap();
            driver
                .handle_text(&mut bridge, TextEvent::ClipboardPaste("AA".into()))
                .unwrap();
            drive(&mut driver);
            present(&mut driver, &mut bridge, root, 2);
            assert_eq!(input(&driver, node), ("AA", false));
            let mut undo = TextEvent::key_down(keyboard::Key::Character("z".into()));
            let TextEvent::Keyboard(key) = &mut undo else {
                unreachable!()
            };
            key.modifiers = if cfg!(target_os = "macos") {
                keyboard::Modifiers::META
            } else {
                keyboard::Modifiers::CONTROL
            };
            driver.handle_text(&mut bridge, undo).unwrap();
            driver.sync(&mut bridge, root).unwrap();
            assert!(input(&driver, node).1, "undo owns an unfinished edit");
            for character in ["A", "B"] {
                driver
                    .handle_text(
                        &mut bridge,
                        TextEvent::key_down(keyboard::Key::Character(character.into())),
                    )
                    .unwrap();
            }
            driver
                .handle_text(&mut bridge, TextEvent::WindowFocusChange(false))
                .unwrap();
            if restored {
                driver = restore(&driver);
                (bridge, root) = mount_with_font(driver.ui(), font());
            }
            // No new physical input or focus reacquisition authorizes the replay.
            for _ in 0..12 {
                drive(&mut driver);
                driver.sync(&mut bridge, root).unwrap();
                drive(&mut driver);
                if input(&driver, node) == ("AB", false) {
                    break;
                }
            }
            driver.sync(&mut bridge, root).unwrap();
            assert_eq!(
                input(&driver, node),
                ("AB", false),
                "asb={encoded} restore={restored}: undo -> A -> B remains ordered"
            );
            assert!(
                driver
                    .adapter()
                    .stage()
                    .nodes()
                    .any(|n| matches!(&n.kind,NodeKind::Text(text) if text.as_ref()=="changes:4")),
                "one change per committed transaction"
            );
        }
    }
}
