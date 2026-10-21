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
    let Some(InteractionView::Dialogue(dialogue)) =
        story.driver().ui().session().view().interaction
    else {
        panic!("expected dialogue")
    };
    dialogue.text()
}

#[test]
fn sentence_timing_preserves_pauses_and_waits_for_presentation_before_continuing() {
    let source = r#"
    let d = game.dialogue();
    fn main() {
        game.configure(text_speed: 4);
        stage.replace(text(value: bind(d.text), reveal: bind(d.reveal)));
        narrator(rich("[speed=0]A[/speed][pause=0.5][speed=2]BC[/speed][speed=2x]DE[/speed]F[pause=0.5][next]"));
        narrator(rich("[pause=10]manual[pause=10]"));
        say([character("A").line(rich("[speed=0]fast[/speed][next]")),
             character("B").line(rich("[speed=1]slow[/speed]"))]);
    }"#;
    let checked = reel_cli::check(source, "sentence-timing.aside").unwrap();
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
        let (mut bridge, root) = bridge(story.driver());
        present(story.driver_mut().unwrap(), &mut bridge, root, 1);
        let visible = |story: &StoryRuntime| {
            let Some(InteractionView::Dialogue(d)) =
                story.driver().ui().session().view().interaction
            else {
                panic!("dialogue");
            };
            d.lines()[0].visible_text().to_owned()
        };
        tick(&mut story, 0);
        assert_eq!(visible(&story), "A");
        assert_eq!(story.tick_interval(), Duration::from_millis(500));
        tick(&mut story, 250_000_000);
        assert_eq!(story.tick_interval(), Duration::from_millis(250));
        story.suspend(250_000_000).unwrap();
        tick(&mut story, 10_250_000_000);
        story.resume(10_250_000_000);
        tick(&mut story, 10_250_000_000);
        assert_eq!(story.tick_interval(), Duration::from_millis(250));
        tick(&mut story, 10_500_000_000);
        assert_eq!(visible(&story), "A");
        tick(&mut story, 11_000_000_000);
        assert_eq!(visible(&story), "AB");
        tick(&mut story, 11_500_000_000);
        assert_eq!(visible(&story), "ABC");
        tick(&mut story, 11_625_000_000);
        assert_eq!(visible(&story), "ABCD");
        tick(&mut story, 11_750_000_000);
        assert_eq!(visible(&story), "ABCDE");
        tick(&mut story, 12_000_000_000);
        assert_eq!(visible(&story), "ABCDEF");
        assert_eq!(story.tick_interval(), Duration::from_millis(500));
        tick(&mut story, 12_500_000_000);
        tick(&mut story, 20_000_000_000);
        assert_eq!(
            text(&story),
            "ABCDEF",
            "completed text has not been presented"
        );
        present(story.driver_mut().unwrap(), &mut bridge, root, 2);
        tick(&mut story, 20_000_000_000);
        assert_eq!(text(&story), "manual");
        assert!(!story.driver().ui().session().reading().automatic());
        present(story.driver_mut().unwrap(), &mut bridge, root, 3);
        tick(&mut story, 20_000_000_000);
        assert_eq!(story.tick_interval(), Duration::from_secs(10));
        click_at(story.driver_mut().unwrap(), &mut bridge, 500., 300.);
        story.wake_input();
        assert_eq!(visible(&story), "manual", "one click bypasses all timing");
        present(story.driver_mut().unwrap(), &mut bridge, root, 4);
        assert!(!story.needs_tick(), "manual line still waits for advance");
        click_at(story.driver_mut().unwrap(), &mut bridge, 500., 300.);
        story.wake_input();
        tick(&mut story, 20_000_000_000);
        present(story.driver_mut().unwrap(), &mut bridge, root, 5);
        tick(&mut story, 20_000_000_000);
        assert_eq!(visible(&story), "fast");
        tick(&mut story, 24_000_000_000);
        present(story.driver_mut().unwrap(), &mut bridge, root, 6);
        assert!(
            !story.needs_tick(),
            "every grouped line must request automatic continuation"
        );
        let history = &story.driver().ui().session().view().history;
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].lines()[0].content().styled().cues().len(), 8);
    }
}

#[test]
fn source_and_asb_reading_controls_wait_for_presentation_and_preserve_choices() {
    let source = r#"
    let r = game.reading();
    let d = game.dialogue();
    let c = game.choice();
    let menu = model({open: false});
    fn main() {
        game.configure(text_speed: 0);
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
        let (mut bridge, root) = bridge(story.driver());
        assert!(!story.needs_tick(), "no reading before first presentation");
        present(story.driver_mut().unwrap(), &mut bridge, root, 1);
        tick(&mut story, 0); // reveal full text
        tick(&mut story, 5_000_000_000);
        assert_eq!(
            text(&story),
            "first",
            "unpresented full text cannot auto advance"
        );
        present(story.driver_mut().unwrap(), &mut bridge, root, 2);
        tick(&mut story, 5_000_000_000);
        assert_eq!(story.tick_interval(), Duration::from_millis(500));
        tick(&mut story, 5_250_000_000);
        assert_eq!(story.tick_interval(), Duration::from_millis(250));
        story
            .driver_mut()
            .unwrap()
            .handle_pointer(&mut bridge, history_press(true))
            .unwrap();
        assert!(!story.driver().ui().dialogue_needs_time());
        tick(&mut story, 99_000_000_000);
        assert_eq!(
            text(&story),
            "first",
            "held gesture must not consume the auto deadline"
        );
        story
            .driver_mut()
            .unwrap()
            .handle_pointer(&mut bridge, history_press(false))
            .unwrap();
        settle(story.driver_mut().unwrap());
        story.wake_input();
        present(story.driver_mut().unwrap(), &mut bridge, root, 3);
        assert!(story.driver().ui().session().reading().paused());
        assert!(!story.needs_tick());
        assert!(
            story
                .driver()
                .adapter()
                .stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("first".into()))
        );
        tick(&mut story, 100_000_000_000);
        assert_eq!(text(&story), "first");
        click_at(story.driver_mut().unwrap(), &mut bridge, 30., 30.);
        story.wake_input();
        present(story.driver_mut().unwrap(), &mut bridge, root, 4);
        tick(&mut story, 100_000_000_000);
        assert_eq!(story.tick_interval(), Duration::from_millis(500));
        click_at(story.driver_mut().unwrap(), &mut bridge, 310., 30.);
        story.wake_input();
        present(story.driver_mut().unwrap(), &mut bridge, root, 5);
        assert!(!story.driver().ui().session().reading().paused());
        assert!(
            !story.needs_tick(),
            "a visible modal blocks auto even without explicit pause"
        );
        tick(&mut story, 110_000_000_000);
        assert_eq!(text(&story), "first");
        click_at(story.driver_mut().unwrap(), &mut bridge, 30., 30.);
        story.wake_input();
        present(story.driver_mut().unwrap(), &mut bridge, root, 6);
        tick(&mut story, 110_000_000_000);
        assert_eq!(story.tick_interval(), Duration::from_millis(500));
        story
            .driver_mut()
            .unwrap()
            .handle_text(&mut bridge, TextEvent::WindowFocusChange(false))
            .unwrap();
        assert!(!story.needs_tick());
        tick(&mut story, 200_000_000_000);
        story
            .driver_mut()
            .unwrap()
            .handle_text(&mut bridge, TextEvent::WindowFocusChange(true))
            .unwrap();
        tick(&mut story, 200_000_000_000);
        tick(&mut story, 200_499_999_999);
        assert_eq!(text(&story), "first");
        tick(&mut story, 200_500_000_000);
        assert_eq!(text(&story), "second");
        assert_eq!(story.driver().ui().session().view().history.len(), 2);
        present(story.driver_mut().unwrap(), &mut bridge, root, 7);
        tick(&mut story, 201_000_000_000);
        present(story.driver_mut().unwrap(), &mut bridge, root, 8);
        tick(&mut story, 201_000_000_000);
        story.suspend(201_200_000_000).unwrap();
        tick(&mut story, 301_000_000_000);
        assert!(!story.needs_tick());
        story.resume(301_200_000_000);
        tick(&mut story, 301_200_000_000);
        tick(&mut story, 301_699_999_999);
        assert_eq!(text(&story), "second");
        tick(&mut story, 301_700_000_000);
        assert!(matches!(
            story.driver().ui().session().view().interaction,
            Some(InteractionView::Choice(_))
        ));
        present(story.driver_mut().unwrap(), &mut bridge, root, 9);
        tick(&mut story, 401_000_000_000);
        assert!(
            !story.needs_tick(),
            "choice must sleep and wait for actual input"
        );
        // Choice controls remain ordinary navigable widgets.
        gamepad_press(
            story.driver_mut().unwrap(),
            &mut bridge,
            GamepadButton::DpadDown,
        );
        click_at(story.driver_mut().unwrap(), &mut bridge, 30., 240.);
        story.wake_input();
        tick(&mut story, 401_000_000_000);
        assert_eq!(text(&story), "last");
        present(story.driver_mut().unwrap(), &mut bridge, root, 10);
        // Authors can disable automatic reading without changing their dialogue UI.
        click_at(story.driver_mut().unwrap(), &mut bridge, 170., 30.);
        story.wake_input();
        tick(&mut story, 402_000_000_000);
        present(story.driver_mut().unwrap(), &mut bridge, root, 11);
        tick(&mut story, 500_000_000_000);
        assert_eq!(text(&story), "last");
        assert!(!story.driver().ui().session().reading().automatic());
        story.close().unwrap();
        tick(&mut story, 500_000_000_000);
        assert!(story.close_complete());
        assert_eq!(story.driver().state(), StoryState::Cancelled);
    }
}

#[test]
fn creator_reading_example_builds_as_source_and_asb() {
    let source = include_str!("../../../examples/author_reading/src/main.aside");
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
            actions: {skip: {
                bindings: ["ControlLeft", "RightShoulder"],
                on_press: || game.set_skip(true),
                on_release: || game.set_skip(false),
                on_cancel: || game.set_skip(false),
            }}) {
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
        let (mut bridge, root) = bridge(story.driver());
        present(story.driver_mut().unwrap(), &mut bridge, root, 1);
        tick(&mut story, 0);
        present(story.driver_mut().unwrap(), &mut bridge, root, 2);
        tick(&mut story, 0);
        assert_eq!(story.driver().ui().session().view().history.len(), 2);
        present(story.driver_mut().unwrap(), &mut bridge, root, 3);
        // Root input must work immediately, before clicking/focusing any controls.
        let key = |value| {
            TextEvent::Input(InputEvent::Value {
                device: InputDeviceId::Keyboard,
                control: Control::Key(keyboard::Code::ControlLeft),
                value,
            })
        };
        story
            .driver_mut()
            .unwrap()
            .handle_text(&mut bridge, key(1.))
            .unwrap();
        story.wake_input();
        tick(&mut story, 0);
        assert!(story.driver().ui().session().reading().skipping());
        tick(&mut story, 0);
        assert_eq!(story.driver().ui().session().view().history.len(), 3);
        assert_eq!(text(&story), "repeat");
        present(story.driver_mut().unwrap(), &mut bridge, root, 4);
        story
            .driver_mut()
            .unwrap()
            .handle_text(&mut bridge, key(0.))
            .unwrap();
        settle(story.driver_mut().unwrap());
        story.wake_input();
        tick(&mut story, 0);
        assert!(!story.driver().ui().session().reading().skipping());
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
        present(story.driver_mut().unwrap(), &mut bridge, root, 5);
        story
            .driver_mut()
            .unwrap()
            .handle_text(&mut bridge, pad(1.))
            .unwrap();
        story.wake_input();
        tick(&mut story, 0);
        assert!(story.driver().ui().session().reading().skipping());
        story
            .driver_mut()
            .unwrap()
            .handle_text(&mut bridge, TextEvent::WindowFocusChange(false))
            .unwrap();
        settle(story.driver_mut().unwrap());
        assert!(!story.driver().ui().session().reading().skipping());
        assert!(
            story
                .driver()
                .adapter()
                .stage()
                .nodes()
                .any(|n| n.kind == NodeKind::Text("skip:off".into()))
        );
        story
            .driver_mut()
            .unwrap()
            .handle_text(&mut bridge, TextEvent::WindowFocusChange(true))
            .unwrap();
        present(story.driver_mut().unwrap(), &mut bridge, root, 6);
        story
            .driver_mut()
            .unwrap()
            .handle_text(&mut bridge, pad(1.))
            .unwrap();
        story.wake_input();
        tick(&mut story, 0);
        tick(&mut story, 0);
        assert_eq!(text(&story), "unread");
        assert!(
            !story.driver().ui().session().reading().skipping(),
            "unread boundary clears held skip"
        );
        story
            .driver_mut()
            .unwrap()
            .handle_text(&mut bridge, pad(0.))
            .unwrap();
        settle(story.driver_mut().unwrap());
        present(story.driver_mut().unwrap(), &mut bridge, root, 7);
        click_at(story.driver_mut().unwrap(), &mut bridge, 30., 30.);
        assert!(story.driver().ui().session().reading().skip_unread());
        story.wake_input();
        tick(&mut story, 0);
        assert!(matches!(
            story.driver().ui().session().view().interaction,
            Some(InteractionView::Choice(_))
        ));
        assert!(
            !story.driver().ui().session().reading().skipping(),
            "skip all still stops at choices"
        );
        story.close().unwrap();
        tick(&mut story, 0);
        assert!(story.close_complete());
    }
}
