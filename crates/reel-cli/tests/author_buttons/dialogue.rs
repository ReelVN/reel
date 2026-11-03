//! Dialogue interaction and history contracts.
use super::*;

#[test]
fn source_and_asb_dialogue_confirmation_uses_presented_pairing() {
    const STORY: &str = r#"fn main() { narrator("First"); narrator("Second"); }"#;
    let checked = reel_cli::check(STORY, "gamepad.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
        reel_engine::aside::source::compile_story(STORY, "main", 1, "main").unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        let (mut bridge, root) = bridge(&driver);
        present(&mut driver, &mut bridge, root, 1);
        let first = driver.ui().session().view().interaction.unwrap().id();
        gamepad_press(
            &mut driver,
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        driver.sync(&mut bridge, root).unwrap();
        // First confirmation finishes reveal. Only an accepted new projection
        // can admit the next confirmation which advances to another dialogue.
        gamepad_press(
            &mut driver,
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        assert_eq!(
            driver.ui().session().view().interaction.unwrap().id(),
            first
        );
        present(&mut driver, &mut bridge, root, 2);
        gamepad_press(
            &mut driver,
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        assert_ne!(
            driver.ui().session().view().interaction.unwrap().id(),
            first
        );
    }
}

#[test]
fn source_and_asb_characters_share_history_unicode_playback_and_group_advance() {
    use reel_engine::InteractionView;
    use reel_player::story_runtime::StoryRuntime;
    const DIALOGUE: &str = include_str!("../../../../examples/author_dialogue/src/main.aside");
    let checked = reel_cli::check(DIALOGUE, "author_dialogue.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
        reel_engine::aside::source::compile_story(DIALOGUE, "main", 1, "main").unwrap(),
    ] {
        let driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        let mut runtime = StoryRuntime::new(driver);
        let tick = |runtime: &mut StoryRuntime, ns| {
            runtime
                .tick(
                    SliceBudget {
                        deadline_ns: Some(ns + 1),
                        ..Default::default()
                    },
                    || ns,
                )
                .unwrap()
        };
        let reveal = |driver: &AsideSessionUi| {
            let Some(InteractionView::Dialogue(d)) = driver.ui().session().view().interaction
            else {
                panic!("dialogue expected")
            };
            d.lines()
                .iter()
                .map(|l| l.visible_text().to_owned())
                .collect::<Vec<_>>()
        };
        // Startup can yield after a complete stage batch. Settle those host
        // turns without accepting a drawing before checking reveal admission.
        for _ in 0..1024 {
            tick(&mut runtime, 0);
            if !runtime.needs_tick() {
                break;
            }
        }
        assert!(
            !runtime.needs_tick(),
            "no reveal before an accepted presentation"
        );
        let (mut bridge, root) = bridge(runtime.driver());
        present(runtime.driver_mut().unwrap(), &mut bridge, root, 1);
        tick(&mut runtime, 0);
        let history = &runtime.driver().ui().session().view().history[0];
        assert_eq!(history.lines()[0].speaker_id(), Some("mio"));
        assert_eq!(history.lines()[0].speaker_name(), "Mio");
        tick(&mut runtime, 100_000_000);
        assert_eq!(reveal(runtime.driver()), ["A"]);
        tick(&mut runtime, 200_000_000);
        assert_eq!(reveal(runtime.driver()), ["Aé"]);
        runtime.suspend(200_000_000).unwrap();
        assert!(!runtime.needs_tick());
        runtime.resume(10_200_000_000);
        tick(&mut runtime, 10_200_000_000);
        assert_eq!(reveal(runtime.driver()), ["Aé"]);
        tick(&mut runtime, 10_300_000_000);
        assert_eq!(reveal(runtime.driver()), ["Aé👨‍👩‍👧‍👦"]);
        runtime.driver_mut().unwrap().begin_collection().unwrap();
        while !runtime
            .driver_mut()
            .unwrap()
            .gc_step(Default::default())
            .unwrap()
            .cycle_complete
        {}
        present(runtime.driver_mut().unwrap(), &mut bridge, root, 2);
        gamepad_press(
            runtime.driver_mut().unwrap(),
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        settle(runtime.driver_mut().unwrap());
        assert_eq!(reveal(runtime.driver()), ["Aé👨‍👩‍👧‍👦雨"]);
        assert!(!runtime.needs_tick(), "full text sleeps");
        present(runtime.driver_mut().unwrap(), &mut bridge, root, 3);
        gamepad_press(
            runtime.driver_mut().unwrap(),
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        settle(runtime.driver_mut().unwrap());
        assert_eq!(runtime.driver().ui().session().view().history.len(), 2);
        present(runtime.driver_mut().unwrap(), &mut bridge, root, 4);
        tick(&mut runtime, 11_000_000_000);
        tick(&mut runtime, 11_100_000_000);
        assert_eq!(reveal(runtime.driver()), ["一", "四"]);
        let history = &runtime.driver().ui().session().view().history[1];
        assert_eq!(history.lines()[0].speaker_name(), "Mio");
        assert_eq!(history.lines()[1].speaker_name(), "Eri");
        assert_ne!(
            history.lines()[0].content().id(),
            history.lines()[1].content().id()
        );
        // A held confirmation freezes the accepted text until release.
        present(runtime.driver_mut().unwrap(), &mut bridge, root, 5);
        runtime
            .driver_mut()
            .unwrap()
            .handle_text(
                &mut bridge,
                TextEvent::Navigation(reel_ui::input::ActionEvent {
                    action: reel_ui::input::NavigationAction::Confirm,
                    value: 1.,
                    phase: reel_ui::input::ActionPhase::Pressed,
                }),
            )
            .unwrap();
        tick(&mut runtime, 20_000_000_000);
        assert_eq!(reveal(runtime.driver()), ["一", "四"]);
        runtime
            .driver_mut()
            .unwrap()
            .handle_text(
                &mut bridge,
                TextEvent::Navigation(reel_ui::input::ActionEvent {
                    action: reel_ui::input::NavigationAction::Confirm,
                    value: 0.,
                    phase: reel_ui::input::ActionPhase::Released,
                }),
            )
            .unwrap();
        settle(runtime.driver_mut().unwrap());
        assert_eq!(reveal(runtime.driver()), ["一二三", "四五"]);
        present(runtime.driver_mut().unwrap(), &mut bridge, root, 6);
        gamepad_press(
            runtime.driver_mut().unwrap(),
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        settle(runtime.driver_mut().unwrap());
        present(runtime.driver_mut().unwrap(), &mut bridge, root, 7);
        tick(&mut runtime, 21_000_000_000);
        gamepad_press(
            runtime.driver_mut().unwrap(),
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        settle(runtime.driver_mut().unwrap());
        assert_eq!(
            reveal(runtime.driver()),
            [""],
            "protected reveal consumes advance"
        );
        for frame in 8..=28 {
            tick(&mut runtime, 21_000_000_000 + (frame - 7) * 100_000_000);
            present(runtime.driver_mut().unwrap(), &mut bridge, root, frame);
        }
        assert_eq!(reveal(runtime.driver()), ["Listen."]);
        gamepad_press(
            runtime.driver_mut().unwrap(),
            &mut bridge,
            reel_ui::input::GamepadButton::South,
        );
        settle(runtime.driver_mut().unwrap());
        present(runtime.driver_mut().unwrap(), &mut bridge, root, 29);
        tick(&mut runtime, 24_000_000_000);
        assert_eq!(
            reveal(runtime.driver()),
            ["The conversation stays in history."]
        );
        assert!(!runtime.needs_tick());
        for (index, expected) in [
            vec!["The conversation stays in history."],
            vec!["……"],
            vec!["A fleeting thought."],
            vec!["Noise.", "Stay."],
        ]
        .into_iter()
        .enumerate()
        {
            let frame = 30 + index as u64 * 2;
            present(runtime.driver_mut().unwrap(), &mut bridge, root, frame);
            tick(&mut runtime, 25_000_000_000 + index as u64 * 1_000_000_000);
            assert_eq!(reveal(runtime.driver()), expected);
            present(runtime.driver_mut().unwrap(), &mut bridge, root, frame + 1);
            gamepad_press(
                runtime.driver_mut().unwrap(),
                &mut bridge,
                reel_ui::input::GamepadButton::South,
            );
            settle(runtime.driver_mut().unwrap());
        }
        assert_eq!(runtime.driver().state(), StoryState::Finished);
        let history = &runtime.driver().ui().session().view().history;
        assert_eq!(history.len(), 7);
        assert_eq!(
            history[4].lines()[0].resolved_text(),
            "I remember the rain."
        );
        assert_eq!(
            history[5].lines()[0].resolved_text(),
            "The train is arriving."
        );
        assert_eq!(history[6].lines().len(), 1);
        assert_eq!(history[6].lines()[0].speaker_id(), Some("eri"));
        assert_eq!(history[6].lines()[0].resolved_text(), "Stay.");
    }
}

#[test]
fn dialogue_validation_is_atomic_and_callable_characters_keep_native_boundaries() {
    const DIALOGUE: &str = r#"
        let speaker = character("Same name", id: "one");
        fn helper(voice, ...words) { voice(text: words[0]); }
        fn main() {
            let mut failures = 0;
            let invalid_id: any = 5;
            try { character("bad", id: invalid_id); } catch e { failures += 1; }
            try { speaker.name = "changed"; } catch e { failures += 1; }
            try { say([]); } catch e { failures += 1; }
            try { say([speaker.line("valid"), text("wrong")]); } catch e { failures += 1; }
            try { let _ = column() { speaker("cannot wait in children"); }; } catch e { failures += 1; }
            try { game.configure(text_speed: -1); } catch e { failures += 1; }
            if failures != 6 { throw failures; }
            if speaker.name != "Same name" || speaker.id != "one" { throw "changed character"; }
            let line = speaker.line;
            let rows = [];
            for i in 0..128 { rows.append(line(text: f"Line {i}")); }
            say(lines: rows);
            helper(speaker, "after group");
        }
    "#;
    let checked = reel_cli::check(DIALOGUE, "dialogue-validation.aside").unwrap();
    let artifact = reel_cli::build(&checked, Some("main::main")).unwrap();
    for story in [
        reel_player::distribution::decode_story(&artifact.bytes).unwrap(),
        reel_engine::aside::source::compile_story(DIALOGUE, "main", 1, "main").unwrap(),
    ] {
        let mut driver = AsideSessionUi::from_compiled(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            story,
            "en",
            VmLimits::default(),
        )
        .unwrap();
        settle(&mut driver);
        let view = driver.ui().session().view();
        assert_eq!(
            view.history.len(),
            1,
            "failed groups and NoWait do not publish history"
        );
        assert_eq!(view.history[0].lines().len(), 128);
        let (mut bridge, root) = bridge(&driver);
        present(&mut driver, &mut bridge, root, 1);
        click_at(&mut driver, &mut bridge, 100., 300.);
        present(&mut driver, &mut bridge, root, 2);
        click_at(&mut driver, &mut bridge, 100., 300.);
        assert_eq!(driver.ui().session().view().history.len(), 2);
        assert_eq!(
            driver.ui().session().view().history[1].lines()[0].speaker_id(),
            Some("one")
        );
        driver.cancel().unwrap();
        for _ in 0..10_000 {
            if !driver.cleanup_pending() {
                break;
            }
            driver
                .cleanup_step(256, SliceBudget::default(), || 0)
                .unwrap();
        }
        assert!(!driver.cleanup_pending());
        assert_eq!(driver.state(), StoryState::Cancelled);
    }
}
