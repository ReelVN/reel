use super::*;
use aside_vm::snapshot::SnapshotBudget;
use reel_ui::stage::{InteractionState, NodeInteraction, NodeKind};

mod audio {
    use super::*;
    use crate::{audio::*, media::*};
    use std::{cell::RefCell, rc::Rc, time::Duration};
    #[derive(Clone, Default)]
    struct Host(Rc<RefCell<Vec<AudioPlayback>>>, Rc<RefCell<Vec<MediaId>>>);
    impl MediaBackend for Host {
        fn commit_audio(&mut self, id: MediaId) {
            self.1.borrow_mut().push(id);
        }
        fn play_audio(&mut self, request: AudioRequest) -> Result<MediaId, String> {
            let mut entries = self.0.borrow_mut();
            entries.push(AudioPlayback {
                request,
                queue: Vec::new(),
                paused: false,
                result: None,
            });
            Ok(MediaId(entries.len() as u64))
        }
        fn queue_audio(&mut self, id: MediaId, item: AudioQueueItem) -> Result<(), String> {
            self.0.borrow_mut()[id.0 as usize - 1].queue.push(item);
            Ok(())
        }
        fn snapshot(&self, id: MediaId) -> Result<MediaPlayback, String> {
            Ok(MediaPlayback::Audio(
                self.0.borrow()[id.0 as usize - 1].clone(),
            ))
        }
        fn position(&self, id: MediaId) -> Option<Duration> {
            Some(self.0.borrow()[id.0 as usize - 1].request.start_position)
        }
        fn command(&mut self, id: MediaId, command: MediaCommand) -> Result<(), String> {
            let mut entries = self.0.borrow_mut();
            let entry = &mut entries[id.0 as usize - 1];
            match command {
                MediaCommand::Pause => entry.paused = true,
                MediaCommand::Resume => entry.paused = false,
                MediaCommand::Seek(position) => entry.request.start_position = position,
                MediaCommand::Stop => entry.result = Some(MediaResult::Stopped),
                _ => return Err("unsupported fixture command".into()),
            }
            Ok(())
        }
        fn group_volume(
            &mut self,
            _: Option<AudioGroup>,
            _: f64,
            _: Duration,
        ) -> Result<(), String> {
            Ok(())
        }
        fn cancel(&mut self, id: MediaId) {
            self.0.borrow_mut()[id.0 as usize - 1].result = Some(MediaResult::Cancelled);
        }
        fn maintain(&mut self, _: Duration) {}
        fn take_result(&mut self, id: MediaId) -> Option<MediaCompletion> {
            let entries = self.0.borrow();
            let entry = &entries[id.0 as usize - 1];
            entry.result.clone().map(|result| MediaCompletion {
                result,
                position: entry.request.start_position,
            })
        }
        fn poll_after(&self) -> Option<Duration> {
            None
        }
        fn suspend(&mut self) {}
        fn resume(&mut self) {}
        fn close(&mut self) {}
        fn cleanup_pending(&self) -> bool {
            false
        }
    }

    #[test]
    fn effect_scope_audio_retirement_cancels_uncommitted_sound_and_preserves_explicit_owner() {
        let code = r#"fn opening() {
            let node=stack(); stage.replace(node);
            let handles={owned:null,escaped:null};
            scope(body: || {
                handles.owned=audio.play("owned");
                handles.escaped=audio.play("escaped",owner:node);
            });
            let mut cancelled=false;
            try { await handles.owned; } catch error { cancelled=true; }
            if !cancelled { throw "scope sound survived"; }
            stop(handles.escaped);
            await handles.escaped;
        }"#;
        for encoded in [false, true] {
            let (mut session, mut vm) = models::prepared(code, encoded);
            let host = Host::default();
            vm.set_media(Box::new(host.clone())).unwrap();
            models::drive(&mut vm, &mut session, false);
            assert_eq!(host.0.borrow()[0].result, Some(MediaResult::Cancelled));
            assert_eq!(host.0.borrow()[1].result, Some(MediaResult::Stopped));
            assert_eq!(*host.1.borrow(), vec![MediaId(2)]);
            assert!(!vm.media.active());
        }
    }

    #[test]
    fn audio_snapshot_restores_all_groups_current_position_queue_and_terminal_once() {
        let code = r#"fn opening() {
        let voice = audio.play("voice-current.wav", group: "voice", start: 0.42);
        audio.queue(voice, "voice-next.wav", start: 0.1);
        audio.queue(voice, "voice-last.wav", looping: true, loop_start: 0.2, loop_end: 0.8);
        pause(voice);
        let effect = audio.play("effect.wav", start: 0.15);
        let ended = audio.play("ended.wav", group: "ui", start: 0.25);
        narrator("save");
        if audio.position(voice) != 0.42 || audio.position(effect) != 0.15 { throw "audio position reset"; }
        if await ended != "Completed" || await ended != "Completed" { throw "ended audio replayed"; }
        narrator("restored audio");
    }"#;
        let (mut session, mut runtime) = models::prepared(code, true);
        let original = Host::default();
        runtime.set_media(Box::new(original.clone())).unwrap();
        models::drive(&mut runtime, &mut session, false);
        original.0.borrow_mut()[2].result = Some(MediaResult::Completed);
        let program = runtime.vm.program().clone();
        let mut capture = runtime.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let bytes = capture.finish().unwrap().encode().unwrap();
        let mut image = snapshot::SessionSnapshot::decode(&bytes).unwrap();
        let restored_host = Host::default();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            program,
            VmLimits::default(),
            SessionIdentity {
                session_id: SessionId(41),
                epoch: Epoch(20),
            },
            crate::ReadProgress::default(),
            Some(Box::new(restored_host.clone())),
            None,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut session, mut runtime) = restore.finish().unwrap();
        {
            let played = restored_host.0.borrow();
            assert_eq!(played.len(), 2, "ended audio must not be reissued");
            let voice = played
                .iter()
                .find(|entry| entry.request.group == AudioGroup::Voice)
                .unwrap();
            assert_eq!(voice.request.start_position, Duration::from_millis(420));
            assert!(voice.paused);
            assert_eq!(
                voice
                    .queue
                    .iter()
                    .map(|item| item.source.as_str())
                    .collect::<Vec<_>>(),
                ["voice-next.wav", "voice-last.wav"]
            );
            assert_eq!(
                voice.queue[1].loop_region.unwrap().end,
                Some(Duration::from_millis(800))
            );
            assert!(
                played
                    .iter()
                    .any(|entry| entry.request.group == AudioGroup::Effects
                        && entry.request.start_position == Duration::from_millis(150))
            );
        }
        let interaction = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(interaction))
                .unwrap();
        }
        models::drive(&mut runtime, &mut session, false);
        assert!(
            matches!(session.view().interaction, Some(crate::InteractionView::Dialogue(d)) if d.text() == "restored audio")
        );
    }

    #[test]
    fn uncommitted_audio_survives_midsegment_restore_and_native_polling() {
        let code = r#"fn opening() {
        let sound = audio.play("instant.wav");
        audio.queue(sound, "slow.wav");
        narrator("batch committed");
    }"#;
        let (mut session, mut runtime) = models::prepared(code, true);
        let host = Host::default();
        runtime.set_media(Box::new(host.clone())).unwrap();
        // Stop immediately after play. Every native and budget boundary may poll media.
        loop {
            let progress = runtime
                .step(
                    &mut session,
                    SliceBudget {
                        instructions: 1,
                        ..Default::default()
                    },
                    || 0,
                )
                .unwrap();
            if let SessionStep::Vm(RunOutcome::HostCall(ticket)) = progress {
                runtime.dispatch(&mut session, ticket, "en", 32).unwrap();
                runtime.maintain_media().unwrap();
                if !host.0.borrow().is_empty() {
                    break;
                }
            }
        }
        assert!(host.1.borrow().is_empty());
        let program = runtime.vm.program().clone();
        let mut capture = runtime.snapshot_begin(&session).unwrap();
        while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let mut image =
            snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap())
                .unwrap();
        let restored = Host::default();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            program,
            VmLimits::default(),
            SessionIdentity {
                session_id: SessionId(42),
                epoch: Epoch(21),
            },
            crate::ReadProgress::default(),
            Some(Box::new(restored.clone())),
            None,
        )
        .unwrap();
        while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
        let (mut session, mut runtime) = restore.finish().unwrap();
        assert!(
            restored.1.borrow().is_empty(),
            "restoring a pending batch must not commit it"
        );
        for _ in 0..10_000 {
            let progress = runtime
                .step(
                    &mut session,
                    SliceBudget {
                        instructions: 1,
                        ..Default::default()
                    },
                    || 0,
                )
                .unwrap();
            match progress {
                SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                    runtime.dispatch(&mut session, ticket, "en", 32).unwrap();
                    runtime.maintain_media().unwrap();
                    assert!(
                        restored.1.borrow().is_empty(),
                        "host calls cannot seal the task batch"
                    );
                }
                SessionStep::Vm(RunOutcome::BudgetYield(_)) => {
                    assert!(restored.1.borrow().is_empty())
                }
                SessionStep::Vm(RunOutcome::Waiting(_)) => break,
                SessionStep::Vm(RunOutcome::Fault(_)) => panic!("restored task faulted"),
                _ => {}
            }
        }
        assert_eq!(&*restored.1.borrow(), &[MediaId(1)]);
        assert_eq!(restored.0.borrow()[0].queue[0].source, "slow.wav");
    }

    #[test]
    fn ready_await_commits_audio_and_restores_its_scheduled_continuation() {
        let code = r#"fn opening() {
            let ready = completion(); ready.resolve(42);
            let state = {turns: 0};
            spawn(|| { state.turns += 1; });
            let sound = audio.play("first.wav");
            audio.queue(sound, "second.wav");
            let value = await ready;
            if value != 42 || state.turns != 1 { throw "ready await continuation or scheduling lost"; }
            audio.queue(sound, "after-await.wav");
            narrator("ready await restored");
        }"#;
        for encoded in [false, true] {
            let (mut session, mut runtime) = models::prepared(code, encoded);
            let original = Host::default();
            runtime.set_media(Box::new(original.clone())).unwrap();
            let mut at_boundary = false;
            for _ in 0..1000 {
                if let SessionStep::Vm(RunOutcome::HostCall(ticket)) = runtime
                    .step(
                        &mut session,
                        SliceBudget {
                            instructions: 1,
                            ..Default::default()
                        },
                        || 0,
                    )
                    .unwrap()
                {
                    let explicit = ticket.await_activation.is_some();
                    if explicit {
                        assert_eq!(
                            &*original.1.borrow(),
                            &[MediaId(1)],
                            "commit at await entry, even before Ready"
                        );
                        assert_eq!(original.0.borrow()[0].queue.len(), 1);
                    } else {
                        assert!(
                            original.1.borrow().is_empty(),
                            "ordinary native keeps the batch open"
                        );
                    }
                    runtime.dispatch(&mut session, ticket, "en", 32).unwrap();
                    runtime.maintain_media().unwrap();
                    if explicit {
                        assert!(!runtime.vm.wait_is_pending(ticket.wait_id()));
                        at_boundary = true;
                        break;
                    }
                }
            }
            assert!(at_boundary);
            let program = runtime.vm.program().clone();
            let mut capture = runtime.snapshot_begin(&session).unwrap();
            while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
            let mut image =
                snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap())
                    .unwrap();
            let host = Host::default();
            let mut restore = SessionVm::restore_begin(
                &mut image,
                program,
                VmLimits::default(),
                SessionIdentity {
                    session_id: SessionId(42),
                    epoch: Epoch(21),
                },
                Default::default(),
                Some(Box::new(host.clone())),
                None,
            )
            .unwrap();
            while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
            let (mut session, mut runtime) = restore.finish().unwrap();
            models::drive(&mut runtime, &mut session, false);
            assert!(
                matches!(session.view().interaction, Some(crate::InteractionView::Dialogue(d)) if d.text() == "ready await restored")
            );
            assert_eq!(
                &*host.1.borrow(),
                &[MediaId(1)],
                "restore submits the committed batch exactly once"
            );
            assert_eq!(
                host.0.borrow()[0]
                    .queue
                    .iter()
                    .map(|item| item.source.as_str())
                    .collect::<Vec<_>>(),
                ["second.wav", "after-await.wav"]
            );
            assert!(runtime.tasks.is_empty());
        }
    }

    #[test]
    fn callback_return_commits_but_fault_and_cancellation_discard_uncommitted_audio() {
        for (body, commits) in [
            (
                r#"let work = spawn(|| { let a = audio.play("a"); audio.queue(a, "b"); }); await work;"#,
                true,
            ),
            (
                r#"let work = spawn(|| { audio.play("a"); throw "failed"; }); try { await work; } catch error {}"#,
                false,
            ),
            (
                r#"let self_task = {value: null}; self_task.value = spawn(|| { audio.play("a"); self_task.value.cancel(); }); try { await self_task.value; } catch error {}"#,
                false,
            ),
        ] {
            let code = format!("fn opening() {{ {body} narrator(\"done\"); }}");
            let (mut session, mut runtime) = models::prepared(&code, true);
            let host = Host::default();
            runtime.set_media(Box::new(host.clone())).unwrap();
            models::drive(&mut runtime, &mut session, false);
            assert_eq!(host.1.borrow().len(), usize::from(commits));
            assert_eq!(
                host.0.borrow()[0].result,
                (!commits).then_some(MediaResult::Cancelled)
            );
        }
    }
}

#[test]
fn persisted_save_resumes_its_wait_once_and_interrupts_other_external_operations() {
    let code = r#"
    let mut calls = 0;
    fn opening() {
        calls += 1;
        let other = saves.delete("old");
        let request = saves.save("chapter/一", title: "第一章");
        let label = text(value: bind(request.status));
        stage.replace(label);
        await request;
        if request.status != "succeeded" || request.error != null || !request.restored || calls != 1 {
            throw "save replay or result lost";
        }
        let mut interrupted = false;
        try { await other; } catch error { interrupted = error == "OperationInterrupted"; }
        if !interrupted || other.status != "failed" { throw "other write replayed"; }
        let slots = await saves.list();
        if slots.len() != 1 || slots[0].id != "chapter/一" || slots[0].title != "第一章" {
            throw "slot metadata";
        }
        narrator("restored once");
    }
    "#;
    let (mut session, mut runtime) = models::prepared(code, true);
    models::drive(&mut runtime, &mut session, false);
    let _delete = runtime.take_save_request().unwrap();
    let save = runtime.take_save_request().unwrap();
    assert!(runtime.take_save_request().is_none());
    let program = runtime.vm.program().clone();
    let mut capture = runtime.snapshot_begin(&session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let mut image = capture.finish().unwrap();
    image.commit_save(save.id).unwrap();
    let bytes = image.encode().unwrap();
    let mut image = snapshot::SessionSnapshot::decode(&bytes).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        program,
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(41),
            epoch: Epoch(20),
        },
        crate::ReadProgress::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (mut restored_session, mut restored) = restore.finish().unwrap();
    assert!(
        !restored
            .complete_save_request(save.id, saves::SaveResult::Done)
            .unwrap()
    );
    models::drive(&mut restored, &mut restored_session, false);
    let list = restored.take_save_request().unwrap();
    assert!(matches!(list.operation, saves::SaveOperation::List));
    assert!(
        restored
            .complete_save_request(
                list.id,
                saves::SaveResult::Slots(vec![saves::SaveSlot {
                    id: "chapter/一".into(),
                    title: "第一章".into(),
                    saved_at: 42,
                    has_thumbnail: false,
                }])
            )
            .unwrap()
    );
    models::drive(&mut restored, &mut restored_session, false);
    assert!(
        matches!(restored_session.view().interaction, Some(crate::InteractionView::Dialogue(d)) if d.text() == "restored once")
    );
    // Persisted-copy normalization never completes the real pending write.
    assert!(
        matches!(runtime.stage().nodes().find(|n| matches!(&n.kind, NodeKind::Text(_))).map(|n| &n.kind),
        Some(NodeKind::Text(text)) if text.as_ref() == "pending")
    );
}

#[test]
fn restore_real_story_stage_callbacks_narrative_random_and_concurrent_activities() {
    let code = r##"
    let mut entries = 0;
    fn opening() {
        entries += 1;
        let data = model({score: 1});
        let rng = random(seed: 42);
        let first = rng.int(0, 1000000);
        let done = completion();
        let background = image("scene.png", stretch: "both", blend: "screen");
        let snow = particles("flake.png", rate: 5, lifetime: 2);
        let emission = emit(snow);
        let mover = run(tween(background, x: 100, invert: 1, duration: 4, easing: "linear"));
        let task = spawn(|| { await run(delay(2)); data.score += 3; done.resolve(9); });
        let caption = text("score", x: bind(data.score), max_width: 140, align_x: 0.5);
        stage.replace(column() {
            background; snow;
            button("increase", on_click: |e| { data.score += 1; });
            caption;
            slider(value: bind(data.score), max: 20);
            checkbox("enabled", value: true);
            camera(width: 400, height: 300) { scroll(height: 100) { text("scroll"); } }
        });
        let dialogue = game.dialogue(); let history = game.history(); let reading = game.reading();
        narrator(rich("[color=#ff0088]保存[/color][pause=0.5]之后"));
        let picked = choose([{key: "next", label: "继续"}]);
        if picked != "next" || await done != 9 { throw "saved completion or choice"; }
        await task;
        finish(mover); stop(emission);
        if caption.max_width != 140 || caption.align_x != 0.5 { throw "lost layout constraints"; }
        if background.blend != "screen" { throw "lost blend mode"; }
        if entries != 1 || data.score != 5 || background.x != 100 || background.invert != 1 { throw "replayed or lost state"; }
        narrator(f"done {rng.int(0, 1000000)}");
    }
    "##;
    let (mut session, mut runtime) = models::prepared(code, true);
    models::drive(&mut runtime, &mut session, false);
    activities::advance_clock(&mut runtime, &session, 0.5, 1);
    models::drive(&mut runtime, &mut session, false);
    let button = runtime.stage().nodes().find(|n| matches!(&n.kind, NodeKind::Button { label: Some(label), .. } if label == "increase")).unwrap().id;
    runtime
        .stage
        .interaction(
            runtime.vm.heap_mut(),
            &NodeInteraction {
                node: button,
                state: InteractionState {
                    hover: true,
                    pressed: true,
                    ..Default::default()
                },
            },
        )
        .unwrap();
    models::drive(&mut runtime, &mut session, false);
    let old_interaction = session.view().interaction.unwrap().id();
    let expected = runtime
        .stage()
        .nodes()
        .map(|n| (n.id, n.props.clone(), n.children.clone()))
        .collect::<Vec<_>>();
    let bytecode = bc::encode(runtime.vm.program().package(), bc::Limits::default()).unwrap();
    let mut capture = runtime.snapshot_begin(&session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let image = capture.finish().unwrap();
    fn send<T: Send>(_: &T) {}
    send(&image);
    let bytes = image.encode().unwrap();
    let mut image = snapshot::SessionSnapshot::decode(&bytes).unwrap();
    let program = bc::verify(
        bc::decode(&bytecode, bc::Limits::default()).unwrap(),
        bc::Limits::default(),
    )
    .unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        program,
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(41),
            epoch: Epoch(10),
        },
        crate::ReadProgress::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let (mut restored_session, mut restored) = restore.finish().unwrap();
    models::drive(&mut restored, &mut restored_session, false);
    assert_eq!(
        restored
            .stage()
            .nodes()
            .map(|n| (n.id, n.props.clone(), n.children.clone()))
            .collect::<Vec<_>>(),
        expected
    );
    assert!(
        restored_session
            .handle_local_input(
                restored_session.root_owner(),
                InputAction::Advance(old_interaction)
            )
            .is_err()
    );
    let host = restored.stage.value(restored.vm.heap(), button).unwrap();
    let field = restored.vm.heap().host_fields(host).unwrap();
    assert!((0..restored.vm.heap().table_len(field).unwrap()).any(|i| {
        let (key, value) = restored.vm.heap().table_entry(field, i).unwrap();
        text::read_string(restored.vm.heap(), key, 32).unwrap() == "hover"
            && value.as_bool() == Some(false)
    }));
    for (vm, session) in [
        (&mut runtime, &mut session),
        (&mut restored, &mut restored_session),
    ] {
        vm.click(session, button).unwrap();
        models::drive(vm, session, false);
        activities::advance_clock(vm, session, 2.5, 1);
        models::drive(vm, session, false);
        let interaction = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(interaction))
                .unwrap();
        }
        models::drive(vm, session, false);
        let crate::InteractionView::Choice(choice) = session.view().interaction.unwrap() else {
            panic!("choice after restore")
        };
        let choice = choice.options()[0].id().clone();
        session
            .handle_local_input(session.root_owner(), InputAction::Select(choice))
            .unwrap();
        models::drive(vm, session, false);
    }
    let text = |session: &Session| match session.view().interaction.unwrap() {
        crate::InteractionView::Dialogue(d) => d.text().to_owned(),
        _ => unreachable!(),
    };
    assert_eq!(text(&session), text(&restored_session));
    assert_eq!(
        session.view().history.len(),
        restored_session.view().history.len()
    );
    eprintln!(
        "session snapshot {} bytes; {} nodes; continuation and RNG match",
        bytes.len(),
        expected.len()
    );
}
