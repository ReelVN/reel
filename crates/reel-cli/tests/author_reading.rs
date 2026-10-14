//! Creator-owned history and controls share the runtime's presentation-aware clock.
#[path = "support/author.rs"]
mod support;
use reel_engine::{
    Epoch, InteractionView, Session, SessionId,
    aside::{SliceBudget, VmLimits},
};
use reel_player::{
    aside_session::{AsideSessionUi, StoryState},
    story_runtime::StoryRuntime,
};
use reel_ui::{core::*, dpi::PhysicalPosition, input::GamepadButton, stage::NodeKind};
use std::time::Duration;
use support::*;

fn tick(story: &mut StoryRuntime, ns: u64) {
    story
        .tick(
            SliceBudget {
                deadline_ns: Some(ns + 1),
                ..Default::default()
            },
            || ns,
        )
        .unwrap();
}
fn history_press(down: bool) -> PointerEvent {
    let event = PointerButtonEvent {
        pointer: PointerInfo {
            pointer_id: Some(PointerId::PRIMARY),
            persistent_device_id: None,
            pointer_type: PointerType::Mouse,
        },
        state: PointerState {
            position: PhysicalPosition::new(30., 30.),
            ..Default::default()
        },
        button: Some(PointerButton::Primary),
    };
    if down {
        PointerEvent::Down(event)
    } else {
        PointerEvent::Up(event)
    }
}
fn text(story: &StoryRuntime) -> &str {
    let Some(InteractionView::Dialogue(dialogue)) = story.driver.ui().session().view().interaction
    else {
        panic!("expected dialogue")
    };
    dialogue.text()
}

#[test]
fn source_and_asb_reading_controls_wait_for_presentation_and_preserve_choices() {
    let source = r#"
    let r = game.reading();
    let d = game.dialogue();
    let c = game.choice();
    let menu = model({open: false});
    fn main() {
        game.configure(text_speed: 0, default_dialogue: false, default_choices: false);
        stage.replace(stack(width: 640, height: 420) {
            button("History", x: 10, y: 10, width: 120, height: 40,
                   on_click: |e| game.pause_reading(true));
            button("Auto", x: 150, y: 10, width: 120, height: 40,
                   on_click: |e| game.set_auto(!r.auto));
            button("Menu", x: 290, y: 10, width: 120, height: 40,
                   on_click: |e| { menu.open = true; });
            text(value: bind(d.text), reveal: bind(d.reveal), x: 10, y: 160, width: 600);
            view(|| column() {
                for option in c.options { button(option.label, width: 120, height: 40,
                    on_click: |e| c.select(option.id)); }
            }, x: 10, y: 220);
            view(|| {
                if !r.paused && !menu.open { return null; }
                stack(modal: true, width: 640, height: 420) {
                    button("Close", x: 10, y: 10, width: 120, height: 40,
                           on_click: |e| { game.pause_reading(false); menu.open = false; });
                    scroll(x: 10, y: 80, width: 600, height: 300) {
                        column() { for group in game.history() { for line in group.lines { text(line.text); } } }
                    }
                }
            });
        });
        game.set_auto(true, delay: 0.5);
        narrator("first");
        d.clear();
        narrator("second");
        if choose([{key: "ok", label: "Continue"}]) != "ok" { throw "wrong choice"; }
        narrator("last");
    }"#;
    let checked = reel_cli::check(source, "reading.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for compiled in [
        reel_cli::into_story(checked, "main::main").unwrap(),
        reel_cli::decode_story(&artifact.bytes).unwrap(),
    ] {
        let driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            compiled,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        let mut story = StoryRuntime::new(driver);
        tick(&mut story, 0);
        let (mut bridge, root) = bridge(&story.driver);
        assert!(!story.needs_tick(), "no reading before first presentation");
        present(&mut story.driver, &mut bridge, root, 1);
        tick(&mut story, 0); // reveal full text
        tick(&mut story, 5_000_000_000);
        assert_eq!(
            text(&story),
            "first",
            "unpresented full text cannot auto advance"
        );
        present(&mut story.driver, &mut bridge, root, 2);
        tick(&mut story, 5_000_000_000);
        assert_eq!(story.tick_interval(), Duration::from_millis(500));
        tick(&mut story, 5_250_000_000);
        assert_eq!(story.tick_interval(), Duration::from_millis(250));
        story
            .driver
            .handle_pointer(&mut bridge, history_press(true))
            .unwrap();
        assert!(!story.driver.ui().dialogue_needs_time());
        tick(&mut story, 99_000_000_000);
        assert_eq!(
            text(&story),
            "first",
            "held gesture must not consume the auto deadline"
        );
        story
            .driver
            .handle_pointer(&mut bridge, history_press(false))
            .unwrap();
        settle(&mut story.driver);
        story.wake_input();
        present(&mut story.driver, &mut bridge, root, 3);
        assert!(story.driver.ui().session().reading().paused());
        assert!(!story.needs_tick());
        assert!(
            story
                .driver
                .adapter()
                .stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("first".into()))
        );
        tick(&mut story, 100_000_000_000);
        assert_eq!(text(&story), "first");
        click_at(&mut story.driver, &mut bridge, 30., 30.);
        story.wake_input();
        present(&mut story.driver, &mut bridge, root, 4);
        tick(&mut story, 100_000_000_000);
        assert_eq!(story.tick_interval(), Duration::from_millis(500));
        click_at(&mut story.driver, &mut bridge, 310., 30.);
        story.wake_input();
        present(&mut story.driver, &mut bridge, root, 5);
        assert!(!story.driver.ui().session().reading().paused());
        assert!(
            !story.needs_tick(),
            "a visible modal blocks auto even without explicit pause"
        );
        tick(&mut story, 110_000_000_000);
        assert_eq!(text(&story), "first");
        click_at(&mut story.driver, &mut bridge, 30., 30.);
        story.wake_input();
        present(&mut story.driver, &mut bridge, root, 6);
        tick(&mut story, 110_000_000_000);
        assert_eq!(story.tick_interval(), Duration::from_millis(500));
        story
            .driver
            .handle_text(&mut bridge, TextEvent::WindowFocusChange(false))
            .unwrap();
        assert!(!story.needs_tick());
        tick(&mut story, 200_000_000_000);
        story
            .driver
            .handle_text(&mut bridge, TextEvent::WindowFocusChange(true))
            .unwrap();
        tick(&mut story, 200_000_000_000);
        tick(&mut story, 200_499_999_999);
        assert_eq!(text(&story), "first");
        tick(&mut story, 200_500_000_000);
        assert_eq!(text(&story), "second");
        assert_eq!(story.driver.ui().session().view().history.len(), 2);
        present(&mut story.driver, &mut bridge, root, 7);
        tick(&mut story, 201_000_000_000);
        present(&mut story.driver, &mut bridge, root, 8);
        tick(&mut story, 201_000_000_000);
        story.suspend(201_200_000_000);
        tick(&mut story, 301_000_000_000);
        assert!(!story.needs_tick());
        story.resume(301_200_000_000);
        tick(&mut story, 301_200_000_000);
        tick(&mut story, 301_699_999_999);
        assert_eq!(text(&story), "second");
        tick(&mut story, 301_700_000_000);
        assert!(matches!(
            story.driver.ui().session().view().interaction,
            Some(InteractionView::Choice(_))
        ));
        present(&mut story.driver, &mut bridge, root, 9);
        tick(&mut story, 401_000_000_000);
        assert!(
            !story.needs_tick(),
            "choice must sleep and wait for actual input"
        );
        // Choice controls remain ordinary navigable widgets.
        gamepad_press(&mut story.driver, &mut bridge, GamepadButton::DpadDown);
        click_at(&mut story.driver, &mut bridge, 30., 240.);
        story.wake_input();
        tick(&mut story, 401_000_000_000);
        assert_eq!(text(&story), "last");
        present(&mut story.driver, &mut bridge, root, 10);
        // Authors can disable automatic reading without changing their dialogue UI.
        click_at(&mut story.driver, &mut bridge, 170., 30.);
        story.wake_input();
        tick(&mut story, 402_000_000_000);
        present(&mut story.driver, &mut bridge, root, 11);
        tick(&mut story, 500_000_000_000);
        assert_eq!(text(&story), "last");
        assert!(!story.driver.ui().session().reading().automatic());
        story.close().unwrap();
        tick(&mut story, 500_000_000_000);
        assert!(story.close_complete());
        assert_eq!(story.driver.state(), StoryState::Cancelled);
    }
}

#[test]
fn creator_reading_example_builds_as_source_and_asb() {
    let source = include_str!("../../../examples/author_reading.aside");
    let checked = reel_cli::check(source, "author_reading.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for compiled in [
        reel_cli::into_story(checked, "main::main").unwrap(),
        reel_cli::decode_story(&artifact.bytes).unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(2), Epoch(1)).unwrap(),
            compiled,
            "zh-CN",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        assert!(driver.ui().session().view().interaction.is_some());
        assert!(!driver.ui().session().reading().automatic());
    }
}

#[test]
fn source_and_asb_skip_actions_follow_read_boundaries_release_and_choices() {
    use reel_ui::input::{Control, InputDeviceId, InputEvent};
    let source = r#"
    let r = game.reading();
    fn repeated() { narrator("repeat"); }
    fn main() {
        game.configure(text_speed: 0);
        let moving = text("moving");
        stage.replace(input(width: 640, height: 420,
            bindings: {skip: [binding("ControlLeft"), binding("RightShoulder")]},
            on_action: |e| {
                if e.phase == "pressed" { game.set_skip(true); }
                if e.phase == "released" || e.phase == "cancelled" { game.set_skip(false); }
            }) {
            moving;
            text(value: derive(|| if r.skipping { "skip:on" } else { "skip:off" }), y: 60);
            button("Skip all", x: 10, y: 10, width: 140, height: 40,
                   on_click: |e| game.set_skip(true, unread: true));
        });
        game.set_auto(true, delay: 0);
        repeated();
        game.set_auto(false);
        repeated();
        if await run(sequence(tween(moving, x: 20, duration: 100), tween(moving, x: 40, duration: 100))) != "Completed"
           || moving.x != 40 { throw "fast-forward finite scene"; }
        repeated();
        narrator("unread");
        choose([{key: "ok", label: "Continue"}]);
        narrator("last");
    }"#;
    let checked = reel_cli::check(source, "skip.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for compiled in [
        reel_cli::into_story(checked, "main::main").unwrap(),
        reel_cli::decode_story(&artifact.bytes).unwrap(),
    ] {
        let driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(3), Epoch(1)).unwrap(),
            compiled,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        let mut story = StoryRuntime::new(driver);
        tick(&mut story, 0);
        let (mut bridge, root) = bridge(&story.driver);
        present(&mut story.driver, &mut bridge, root, 1);
        tick(&mut story, 0);
        present(&mut story.driver, &mut bridge, root, 2);
        tick(&mut story, 0);
        assert_eq!(story.driver.ui().session().view().history.len(), 2);
        present(&mut story.driver, &mut bridge, root, 3);
        // Root input must work immediately, before clicking/focusing any controls.
        let key = |value| {
            TextEvent::Input(InputEvent::Value {
                device: InputDeviceId::Keyboard,
                control: Control::Key(keyboard::Code::ControlLeft),
                value,
            })
        };
        story.driver.handle_text(&mut bridge, key(1.)).unwrap();
        story.wake_input();
        tick(&mut story, 0);
        assert!(story.driver.ui().session().reading().skipping());
        tick(&mut story, 0);
        assert_eq!(story.driver.ui().session().view().history.len(), 3);
        assert_eq!(text(&story), "repeat");
        present(&mut story.driver, &mut bridge, root, 4);
        story.driver.handle_text(&mut bridge, key(0.)).unwrap();
        settle(&mut story.driver);
        story.wake_input();
        tick(&mut story, 0);
        assert!(!story.driver.ui().session().reading().skipping());
        assert_eq!(
            text(&story),
            "repeat",
            "release must not skip the next presented line"
        );
        let pad = |value| {
            TextEvent::Input(InputEvent::Value {
                device: InputDeviceId::Gamepad(7),
                control: Control::Button(GamepadButton::RightShoulder),
                value,
            })
        };
        present(&mut story.driver, &mut bridge, root, 5);
        story.driver.handle_text(&mut bridge, pad(1.)).unwrap();
        story.wake_input();
        tick(&mut story, 0);
        assert!(story.driver.ui().session().reading().skipping());
        story
            .driver
            .handle_text(&mut bridge, TextEvent::WindowFocusChange(false))
            .unwrap();
        settle(&mut story.driver);
        assert!(!story.driver.ui().session().reading().skipping());
        assert!(
            story
                .driver
                .adapter()
                .stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("skip:off".into()))
        );
        story
            .driver
            .handle_text(&mut bridge, TextEvent::WindowFocusChange(true))
            .unwrap();
        present(&mut story.driver, &mut bridge, root, 6);
        story.driver.handle_text(&mut bridge, pad(1.)).unwrap();
        story.wake_input();
        tick(&mut story, 0);
        tick(&mut story, 0);
        assert_eq!(text(&story), "unread");
        assert!(
            !story.driver.ui().session().reading().skipping(),
            "unread boundary clears held skip"
        );
        story.driver.handle_text(&mut bridge, pad(0.)).unwrap();
        settle(&mut story.driver);
        present(&mut story.driver, &mut bridge, root, 7);
        click_at(&mut story.driver, &mut bridge, 30., 30.);
        assert!(story.driver.ui().session().reading().skip_unread());
        story.wake_input();
        tick(&mut story, 0);
        assert!(matches!(
            story.driver.ui().session().view().interaction,
            Some(InteractionView::Choice(_))
        ));
        assert!(
            !story.driver.ui().session().reading().skipping(),
            "skip all still stops at choices"
        );
        story.close().unwrap();
        tick(&mut story, 0);
        assert!(story.close_complete());
    }
}
