//! The actual example runs from source and ASB; mouse motion never writes VM coordinates.
#[path = "support/author.rs"]
#[allow(dead_code)]
mod support;
use reel_engine::{Epoch, Session, SessionId, aside::VmLimits};
use reel_player::aside_session::AsideSessionUi;
use reel_ui::{core::*, dpi::PhysicalPosition, stage::NodeKind};
use support::*;

#[test]
fn cursor_example_source_and_asb_animate_retire_and_move_without_vm_allocation() {
    let checked = reel_cli::check(
        include_str!("../../../examples/author_cursor/src/main.aside"),
        "author_cursor.aside",
    )
    .unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for compiled in [
        reel_cli::into_story(checked, "main::main").unwrap(),
        reel_cli::decode_story(&artifact.bytes).unwrap(),
    ] {
        let mut d = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            compiled,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut d);
        let (mut b, root) = bridge(&d);
        present(&mut d, &mut b, root, 1);
        assert_eq!(
            d.adapter()
                .stage()
                .nodes()
                .filter(|n| n.kind == NodeKind::Cursor)
                .count(),
            2
        );
        assert!(d.activities_need_time());
        let heap = d.adapter().vm().heap().stats();
        let revision = d.adapter().stage().revision();
        let started = std::time::Instant::now();
        for frame in 2..202 {
            d.handle_pointer(
                &mut b,
                PointerEvent::Move(PointerUpdate {
                    pointer: PointerInfo {
                        pointer_id: Some(PointerId::PRIMARY),
                        persistent_device_id: None,
                        pointer_type: PointerType::Mouse,
                    },
                    current: PointerState {
                        position: PhysicalPosition::new(80. + (frame % 50) as f64, 120.),
                        ..Default::default()
                    },
                    coalesced: vec![],
                    predicted: vec![],
                }),
            )
            .unwrap();
            present(&mut d, &mut b, root, frame);
        }
        let after = d.adapter().vm().heap().stats();
        assert_eq!(after.chunk_allocations, heap.chunk_allocations);
        assert_eq!(after.charged_bytes, heap.charged_bytes);
        assert_eq!(d.adapter().stage().revision(), revision);
        eprintln!(
            "cursor 200 mouse/present cycles: {:?}; VM chunks delta=0, VM bytes delta=0, stage revision delta=0",
            started.elapsed()
        );
        while d.advance_time(0.15, 16).unwrap().pending {}
        settle(&mut d);
        let pointer = d
            .adapter()
            .stage()
            .nodes()
            .find(|n| n.kind == NodeKind::Cursor && !n.children.is_empty())
            .unwrap();
        let mark = d.adapter().stage().node(pointer.children[0]).unwrap();
        assert!(
            (mark.props.opacity - 0.675).abs() < 0.001,
            "ordinary cursor subtree animation"
        );
        present(&mut d, &mut b, root, 202);
        // The second button owns ordinary input outside the cursor's panel.
        click_at(&mut d, &mut b, 250., 260.);
        present(&mut d, &mut b, root, 203);
        assert_eq!(
            d.adapter()
                .stage()
                .nodes()
                .filter(|n| n.kind == NodeKind::Cursor)
                .count(),
            0
        );
        assert!(
            !d.activities_need_time(),
            "retirement cancels the cursor's ordinary activity"
        );
        d.cancel().unwrap();
    }
}
