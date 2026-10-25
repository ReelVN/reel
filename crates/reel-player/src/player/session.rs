//! Session projection and event dispatch shared by platform hosts.
#[cfg(feature = "window")]
use crate::session::SessionHostDiagnostic;
use crate::{aside_session::AsideSessionUi, session::SessionUi, story_runtime::StoryRuntime};
use reel_engine::aside::SliceBudget;
use reel_ui::{
    bridge::PresentationBridge,
    core::{Handled, PointerEvent, TextEvent, WidgetId},
};

type Error = Box<dyn std::error::Error>;

pub(crate) enum PlayerSession {
    #[cfg(any(feature = "vita", test))]
    Launcher(Box<crate::launcher::Launcher>),
    #[cfg(feature = "window")]
    Diagnostic(Box<SessionHostDiagnostic>),
    Aside {
        story: Box<StoryRuntime>,
        hover: Option<PointerEvent>,
    },
}

impl PlayerSession {
    pub(crate) fn story_mut(&mut self) -> Option<&mut StoryRuntime> {
        match self {
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(_) => None,
            Self::Aside { story, .. } => Some(story),
            #[cfg(feature = "window")]
            Self::Diagnostic(_) => None,
        }
    }
    pub(crate) fn initial_frame_ready(&self) -> bool {
        match self {
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(_) => true,
            #[cfg(feature = "window")]
            Self::Diagnostic(_) => true,
            Self::Aside { story, .. } => {
                !story.driver().ui().images_pending()
                    && (story.driver().ui().has_stage_root()
                        || story.driver().projection_stamp().presentation_request != 0
                        || story.driver().state() == crate::aside_session::StoryState::Finished)
            }
        }
    }
    pub(crate) fn check_fonts(&self) -> Result<(), Error> {
        if let Self::Aside { story, .. } = self
            && story.driver().adapter().game_settings().fonts.is_empty()
            && (story.driver().adapter().stage().root().is_some()
                || story.driver().ui().session().view().interaction.is_some()
                || !story.needs_tick())
        {
            return Err("configure font files in main with game.configure(fonts: [\"fonts/body.ttf\"]) before presenting the story".into());
        }
        Ok(())
    }

    pub(crate) fn game_settings(
        &self,
    ) -> Option<&std::sync::Arc<reel_engine::aside::GameSettings>> {
        match self {
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(launcher) => Some(&launcher.settings),
            Self::Aside { story, .. } => Some(story.driver().adapter().game_settings()),
            #[cfg(feature = "window")]
            Self::Diagnostic(_) => None,
        }
    }
    pub(crate) fn handle_pointer(
        &mut self,
        bridge: &mut PresentationBridge,
        event: PointerEvent,
    ) -> Result<Handled, Error> {
        match self {
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(launcher) => launcher.handle_pointer(bridge, event),
            #[cfg(feature = "window")]
            Self::Diagnostic(host) => Ok(host.ui.handle_pointer(bridge, event)?.handled),
            Self::Aside { story, hover } => {
                // Capture owns the immutable session. Retain just the latest mouse
                // hover rather than cancelling a save when the cursor drifts.
                if story.capturing()
                    && matches!(&event, PointerEvent::Move(e)
                        if e.current.buttons.is_empty()
                            && matches!(e.pointer.pointer_type, reel_ui::core::PointerType::Mouse))
                {
                    *hover = Some(event);
                    return Ok(Handled::Yes);
                }
                *hover = None;
                story.driver_mut()?.handle_pointer(bridge, event)
            }
        }
    }
    pub(crate) fn handle_text(
        &mut self,
        bridge: &mut PresentationBridge,
        event: TextEvent,
    ) -> Result<Handled, Error> {
        match self {
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(launcher) => launcher.handle_text(bridge, event),
            #[cfg(feature = "window")]
            Self::Diagnostic(host) => Ok(host.ui.handle_text(bridge, event)?.handled),
            Self::Aside { story, hover } => {
                *hover = None;
                story.driver_mut()?.handle_text(bridge, event)
            }
        }
    }
    pub(crate) fn aside(driver: AsideSessionUi) -> Self {
        Self::Aside {
            story: Box::new(StoryRuntime::new(driver)),
            hover: None,
        }
    }

    pub(crate) fn ui(&self) -> &SessionUi {
        match self {
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(launcher) => &launcher.ui,
            #[cfg(feature = "window")]
            Self::Diagnostic(host) => &host.ui,
            Self::Aside { story, .. } => story.driver().ui(),
        }
    }

    pub(crate) fn ui_mut(&mut self) -> Result<&mut SessionUi, Error> {
        Ok(match self {
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(launcher) => &mut launcher.ui,
            #[cfg(feature = "window")]
            Self::Diagnostic(host) => &mut host.ui,
            Self::Aside { story, .. } => story.driver_mut()?.ui_mut(),
        })
    }

    pub(crate) fn after_input(
        &mut self,
        bridge: &mut PresentationBridge,
        root: WidgetId,
    ) -> Result<(), Error> {
        match self {
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(launcher) => launcher.sync(bridge, root),
            #[cfg(feature = "window")]
            Self::Diagnostic(host) => host.after_input(bridge, root),
            Self::Aside { story, .. } => {
                story.driver_mut()?.sync_input(bridge, root)?;
                story.wake_input();
                Ok(())
            }
        }
    }

    pub(crate) fn sync(
        &mut self,
        bridge: &mut PresentationBridge,
        root: WidgetId,
    ) -> Result<(), Error> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::UiSync);
        match self {
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(launcher) => launcher.sync(bridge, root),
            #[cfg(feature = "window")]
            Self::Diagnostic(host) => host.ui.sync(bridge, root),
            Self::Aside { story, .. } if story.capturing() => Ok(()),
            Self::Aside { story, hover } => {
                if let Some(event) = hover.take() {
                    story.driver_mut()?.handle_pointer(bridge, event)?;
                    story.wake_input();
                }
                story.driver_mut()?.sync(bridge, root)?;
                // Publishing the reveal clock can invalidate derived UI even
                // while the narrative task is waiting for the next click.
                story.wake_input();
                Ok(())
            }
        }
    }

    pub(crate) fn record(
        &self,
        bridge: &mut PresentationBridge,
        root: WidgetId,
        id: reel_core::identity::FrameId,
    ) -> Result<
        Option<(
            reel_ui::bridge::RecordedSource,
            crate::session::RecordedProjection,
        )>,
        Error,
    > {
        match self {
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(launcher) => launcher.ui.record(bridge, root, id).map(Some),
            #[cfg(feature = "window")]
            Self::Diagnostic(host) => host.ui.record(bridge, root, id).map(Some),
            Self::Aside { story, .. } => story.driver().record(bridge, root, id),
        }
    }
    pub(crate) fn frame_ready(&self) -> bool {
        match self {
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(launcher) => !launcher.ui.images_pending(),
            #[cfg(feature = "window")]
            Self::Diagnostic(_) => true,
            Self::Aside { story, .. } => story.driver().frame_ready(),
        }
    }
    pub(crate) fn presented(
        &mut self,
        bridge: &PresentationBridge,
        frame: &crate::session::RecordedProjection,
        drawing: &reel_ui::frame::RenderFrame,
        accepted_host_ns: u64,
    ) -> Result<(), Error> {
        match self {
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(launcher) => {
                launcher.presented(bridge, frame);
            }
            #[cfg(feature = "window")]
            Self::Diagnostic(host) => {
                host.ui.presented(bridge, frame);
            }
            Self::Aside { story, .. } => {
                story.presented_frame(bridge, frame, drawing, accepted_host_ns)?;
            }
        }
        Ok(())
    }
    pub(crate) fn transition_resources(&self) -> &[reel_ui::resources::ResourceLease] {
        match self {
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(_) => &[],
            Self::Aside { story, .. } => story.driver().transition_resources(),
            #[cfg(feature = "window")]
            Self::Diagnostic(_) => &[],
        }
    }
    pub(crate) fn needs_tick(&self) -> bool {
        match self {
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(launcher) => launcher.needs_tick(),
            Self::Aside { story, .. } => story.needs_tick(),
            #[cfg(feature = "window")]
            Self::Diagnostic(host) => host.ui.dialogue_needs_time(),
        }
    }
    pub(crate) fn suspend(&mut self, now: u64) -> Result<(), Error> {
        self.ui_mut()?.pause_dialogue();
        if let Some(story) = self.story_mut() {
            story.suspend(now)?;
        }
        Ok(())
    }
    pub(crate) fn resume(&mut self, now: u64) {
        if let Some(story) = self.story_mut() {
            story.resume(now);
        }
    }

    pub(crate) fn wake_deadline(
        &self,
        now: reel_core::clock::MonoTime,
    ) -> Option<reel_core::clock::MonoTime> {
        match self {
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(_) => None,
            Self::Aside { story, .. } => story.wake_deadline(now),
            #[cfg(feature = "window")]
            Self::Diagnostic(host) => self.needs_tick().then(|| {
                reel_core::clock::MonoTime(now.0.saturating_add(
                    u64::try_from(host.ui.dialogue_interval(30.).as_nanos()).unwrap_or(u64::MAX),
                ))
            }),
        }
    }

    pub(crate) fn tick(
        &mut self,
        budget: SliceBudget,
        now: impl FnMut() -> u64,
    ) -> Result<(), Error> {
        match self {
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(launcher) => launcher.poll()?,
            Self::Aside { story, .. } => story.tick(budget, now)?,
            #[cfg(feature = "window")]
            Self::Diagnostic(host) => {
                let mut now = now;
                host.ui.advance_dialogue(now(), 30.)?;
            }
        }
        Ok(())
    }

    pub(crate) fn close(&mut self) -> Result<(), Error> {
        match self {
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(launcher) => {
                launcher.ui.cancel();
                Ok(())
            }
            #[cfg(feature = "window")]
            Self::Diagnostic(host) => {
                host.ui.cancel();
                Ok(())
            }
            Self::Aside { story, .. } => story.close(),
        }
    }

    pub(crate) fn close_complete(&self) -> bool {
        match self {
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(launcher) => launcher.close_complete(),
            #[cfg(feature = "window")]
            Self::Diagnostic(_) => true,
            Self::Aside { story, .. } => story.close_complete(),
        }
    }

    pub(crate) fn exit_requested(&self) -> bool {
        match self {
            Self::Aside { story, .. } => story.driver().adapter().exit_requested(),
            #[cfg(any(feature = "vita", test))]
            Self::Launcher(launcher) => launcher.exit_requested(),
            #[cfg(feature = "window")]
            Self::Diagnostic(_) => false,
        }
    }

    #[cfg(all(
        test,
        feature = "window",
        any(target_os = "linux", target_os = "windows")
    ))]
    pub(crate) fn diagnostic_step(&self) -> u8 {
        match self {
            #[cfg(feature = "window")]
            Self::Diagnostic(host) => host.diagnostic_step(),
            Self::Launcher(_) => unreachable!("diagnostic-only fixture"),
            Self::Aside { .. } => unreachable!("diagnostic-only fixture"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_engine::{Epoch, Session, SessionId, aside::VmLimits};

    #[test]
    fn stage_sync_preserves_idle_ui_and_timed_waits_without_dialogue() {
        use crate::saves::Candidate;
        use reel_core::clock::MonoTime;

        for timed in [false, true] {
            let ending = if timed {
                "await run(delay(2)); panel.opacity = 0.5;"
            } else {
                ""
            };
            let driver = AsideSessionUi::from_source(
                Session::new(SessionId(1), Epoch(1)).unwrap(),
                &format!(
                    "fn main() {{ let panel = button(width: 80, height: 40, on_click: |event| {{ event.target.opacity = 0.25; }}); stage.replace(panel); {ending} }}"
                ),
                "idle_ui",
                1,
                "main",
                "en",
                VmLimits::default(),
            )
            .unwrap();
            let Candidate {
                driver,
                mut bridge,
                root,
            } = Candidate::new(driver).unwrap();
            let mut session = PlayerSession::aside(driver);
            let settle = |session: &mut PlayerSession, bridge: &mut PresentationBridge, now| {
                for _ in 0..1000 {
                    session.tick(SliceBudget::default(), || now).unwrap();
                    session.sync(bridge, root).unwrap();
                    let deadline = session.wake_deadline(MonoTime(now));
                    if deadline.is_none_or(|deadline| deadline.0 > now + 1_000_000) {
                        return;
                    }
                }
                panic!("stage synchronization kept an idle VM runnable");
            };
            settle(&mut session, &mut bridge, 0);
            let expected = timed.then_some(MonoTime(2_000_000_000));
            assert_eq!(session.wake_deadline(MonoTime(0)), expected);
            let initial = session.ui().projection_stamp();
            for _ in 0..100 {
                session.sync(&mut bridge, root).unwrap();
                assert_eq!(session.wake_deadline(MonoTime(0)), expected);
                assert_eq!(session.ui().projection_stamp(), initial);
            }
            if timed {
                settle(&mut session, &mut bridge, 2_000_000_000);
                assert_eq!(session.wake_deadline(MonoTime(2_000_000_000)), None);
                assert_ne!(session.ui().projection_stamp(), initial);
            }
            session.close().unwrap();
            settle(&mut session, &mut bridge, 2_000_000_000);
            assert!(session.close_complete());
        }
    }

    #[test]
    fn mouse_hover_during_capture_preserves_save_and_coalesces_pending_motion() {
        use crate::saves::{Candidate, DirectorySaves, RuntimeSaves, SaveService, SaveStorage};
        use reel_core::{budget::QueueLimits, wake::WakeSignal};
        use reel_ui::core::{PointerInfo, PointerState, PointerType, PointerUpdate};
        use std::sync::Arc;
        let driver = AsideSessionUi::from_source(
            Session::new(SessionId(1), Epoch(1)).unwrap(),
            r#"fn main() { await saves.save("hover"); stage.replace(text("saved")); }"#,
            "hover_save",
            1,
            "main",
            "en",
            VmLimits::default(),
        )
        .unwrap();
        let Candidate {
            driver,
            mut bridge,
            root,
        } = Candidate::new(driver).unwrap();
        let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/reel-artifacts/save-tests")
            .join(format!("hover-{}", std::process::id()));
        let storage = Arc::new(DirectorySaves::new(&folder, 16 << 20));
        let wake = WakeSignal::new(|| {});
        let service = SaveService::new(
            storage.clone(),
            QueueLimits {
                max_requests: 1,
                max_bytes: 16 << 20,
            },
            wake.clone(),
        );
        let runtime = StoryRuntime::new(driver)
            .with_saves(RuntimeSaves::new(service, 16 << 20))
            .with_media_factory(move || {
                Box::new(crate::media::MediaPlayer::new(
                    crate::assets::Assets::directory(Default::default()),
                    Default::default(),
                    wake.clone(),
                    |_| Err("test has no sound output".into()),
                ))
            });
        let mut window = PlayerSession::Aside {
            story: Box::new(runtime),
            hover: None,
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut captured = false;
        while window.needs_tick() {
            assert!(std::time::Instant::now() < deadline, "save did not settle");
            window.tick(SliceBudget::default(), || 0).unwrap();
            if matches!(&window, PlayerSession::Aside { story, .. } if story.capturing()) {
                captured = true;
                for x in [10., 20., 30.] {
                    window
                        .handle_pointer(
                            &mut bridge,
                            PointerEvent::Move(PointerUpdate {
                                pointer: PointerInfo {
                                    pointer_id: None,
                                    persistent_device_id: None,
                                    pointer_type: PointerType::Mouse,
                                },
                                current: PointerState {
                                    position: (x, 10.).into(),
                                    ..Default::default()
                                },
                                coalesced: vec![],
                                predicted: vec![],
                            }),
                        )
                        .unwrap();
                }
                assert!(
                    matches!(&window, PlayerSession::Aside { story, hover: Some(PointerEvent::Move(e)) }
                    if story.capturing() && e.current.position.x == 30.)
                );
            }
            window.sync(&mut bridge, root).unwrap();
            std::thread::yield_now();
        }
        assert!(captured);
        assert!(
            matches!(&window, PlayerSession::Aside { story, hover: None }
            if story.driver().state() == crate::aside_session::StoryState::Finished)
        );
        assert_eq!(storage.list().unwrap().len(), 1);
        window.close().unwrap();
        storage.delete("hover").unwrap();
        std::fs::remove_dir(folder).unwrap();
    }

    #[test]
    fn font_configuration_is_required_at_presentation_and_can_run_after_startup_slices() {
        for (body, supplied) in [
            ("stage.replace(text(\"hello\"));", false),
            ("", false),
            (
                "let mut n=0; while n<100 {n+=1;} game.configure(fonts: [\"body.ttf\", \"cjk.ttf\"]); try {game.configure(fonts: []);} catch error {} stage.replace(text(\"hello\"));",
                true,
            ),
        ] {
            let driver = AsideSessionUi::from_source(
                Session::new(SessionId(1), Epoch(1)).unwrap(),
                &format!("fn main(){{{body}}}"),
                "fonts",
                1,
                "main",
                "en",
                VmLimits::default(),
            )
            .unwrap();
            let mut session = PlayerSession::aside(driver);
            assert!(
                session.check_fonts().is_ok(),
                "main must be allowed to initialize"
            );
            for _ in 0..10_000 {
                if !session.needs_tick() {
                    break;
                }
                session.tick(SliceBudget::default(), || 0).unwrap();
                if supplied {
                    session.check_fonts().unwrap();
                }
            }
            assert!(!session.needs_tick());
            assert_eq!(session.check_fonts().is_ok(), supplied);
        }
    }
}
