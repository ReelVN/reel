use super::*;
use crate::session::tests::{accept, mount, session};
use reel_core::identity::{FrameId, SurfaceEpoch};
use reel_ui::bridge::{NoticeResult, PresentationNotice, PresentationOutcome};

#[test]
fn delayed_frames_keep_dialogue_time_across_newer_animation_projections() {
    let mut session = session();
    session
        .begin_say(
            session.root_owner(),
            &[DialogueLineInput {
                history: Default::default(),
                speaker_id: None,
                speaker_name: "",
                content_id: "timed-dialogue",
                text: "A deliberately long line for delayed presentation".into(),
            }],
            "en",
            true,
        )
        .unwrap();
    let mut ui = SessionUi::preview(session).unwrap();
    let (mut bridge, root) = mount(&ui);
    accept(&mut ui, &mut bridge, root, 1);
    ui.advance_dialogue(0, 10.).unwrap();
    for turn in 1..=10 {
        ui.sync(&mut bridge, root).unwrap();
        let id = FrameId(turn + 1);
        let (source, receipt) = ui.record(&mut bridge, root, id).unwrap();
        // An animated cursor updates paint while this candidate waits for GPU room.
        ui.invalidate_projection();
        ui.sync(&mut bridge, root).unwrap();
        assert_ne!(receipt.stamp, ui.projection_stamp());
        assert!(receipt.stamp.same_advance(ui.projection_stamp()));
        assert_eq!(
            bridge.resolve(PresentationNotice::new(
                source.key(),
                SurfaceEpoch(1),
                PresentationOutcome::Accepted(id),
            )),
            NoticeResult::Accepted,
        );
        assert!(ui.presented(&bridge, &receipt));
        assert!(
            !ui.current_presentation_ready(),
            "capture still needs the exact projection"
        );
        ui.advance_dialogue(turn * 100_000_000, 10.).unwrap();
        ui.sync(&mut bridge, root).unwrap();
        assert_eq!(ui.projection.lines[0].visible.end, turn as usize);
    }
}
