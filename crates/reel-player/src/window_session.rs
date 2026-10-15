//! The native shell shares one session projection and event path across hosts.
use crate::{
    aside_session::AsideSessionUi,
    session::{SessionHostDiagnostic, SessionUi},
    story_runtime::StoryRuntime,
};
use reel_engine::aside::SliceBudget;
use reel_ui::{
    bridge::PresentationBridge,
    core::{Handled, PointerEvent, TextEvent, WidgetId},
};

type Error = Box<dyn std::error::Error>;

pub(crate) enum WindowSession {
    Diagnostic(Box<SessionHostDiagnostic>),
    Aside(Box<StoryRuntime>),
}

impl WindowSession {
    pub(crate) fn check_fonts(&self) -> Result<(), Error> {
        if let Self::Aside(story) = self
            && story.driver.adapter().game_settings().fonts.is_empty()
            && (story.driver.adapter().stage().root().is_some()
                || story.driver.ui().session().view().interaction.is_some()
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
            Self::Aside(story) => Some(story.driver.adapter().game_settings()),
            Self::Diagnostic(_) => None,
        }
    }
    pub(crate) fn handle_pointer(
        &mut self,
        bridge: &mut PresentationBridge,
        event: PointerEvent,
    ) -> Result<Handled, Error> {
        match self {
            Self::Diagnostic(host) => Ok(host.ui.handle_pointer(bridge, event)?.handled),
            Self::Aside(story) => story.driver.handle_pointer(bridge, event),
        }
    }
    pub(crate) fn handle_text(
        &mut self,
        bridge: &mut PresentationBridge,
        event: TextEvent,
    ) -> Result<Handled, Error> {
        match self {
            Self::Diagnostic(host) => Ok(host.ui.handle_text(bridge, event)?.handled),
            Self::Aside(story) => story.driver.handle_text(bridge, event),
        }
    }
    pub(crate) fn aside(driver: AsideSessionUi) -> Self {
        Self::Aside(Box::new(StoryRuntime::new(driver)))
    }

    pub(crate) fn ui(&self) -> &SessionUi {
        match self {
            Self::Diagnostic(host) => &host.ui,
            Self::Aside(story) => story.driver.ui(),
        }
    }

    pub(crate) fn ui_mut(&mut self) -> &mut SessionUi {
        match self {
            Self::Diagnostic(host) => &mut host.ui,
            Self::Aside(story) => story.driver.ui_mut(),
        }
    }

    pub(crate) fn after_input(
        &mut self,
        bridge: &mut PresentationBridge,
        root: WidgetId,
    ) -> Result<(), Error> {
        match self {
            Self::Diagnostic(host) => host.after_input(bridge, root),
            Self::Aside(story) => {
                story.driver.sync_input(bridge, root)?;
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
        match self {
            Self::Diagnostic(host) => host.ui.sync(bridge, root),
            Self::Aside(story) => story.driver.sync(bridge, root),
        }
    }

    pub(crate) fn presented(
        &mut self,
        stamp: crate::session::ProjectionStamp,
    ) -> Result<(), Error> {
        match self {
            Self::Diagnostic(host) => host.ui.presented(stamp),
            Self::Aside(story) => {
                story.driver.presented(stamp)?;
                story.wake_input();
            }
        }
        Ok(())
    }
    pub(crate) fn needs_tick(&self) -> bool {
        match self {
            Self::Aside(story) => story.needs_tick(),
            Self::Diagnostic(host) => host.ui.dialogue_needs_time(),
        }
    }
    pub(crate) fn suspend(&mut self, now: u64) {
        self.ui_mut().pause_dialogue();
        if let Self::Aside(story) = self {
            story.suspend(now);
        }
    }
    pub(crate) fn resume(&mut self, now: u64) {
        if let Self::Aside(story) = self {
            story.resume(now);
        }
    }

    pub(crate) fn tick_interval(&self) -> std::time::Duration {
        match self {
            Self::Aside(story) => story.tick_interval(),
            Self::Diagnostic(host) => host.ui.dialogue_interval(30.),
        }
    }

    pub(crate) fn tick(
        &mut self,
        budget: SliceBudget,
        mut now: impl FnMut() -> u64,
    ) -> Result<(), Error> {
        match self {
            Self::Aside(story) => story.tick(budget, now)?,
            Self::Diagnostic(host) => {
                host.ui.advance_dialogue(now(), 30.)?;
            }
        }
        Ok(())
    }

    pub(crate) fn close(&mut self) -> Result<(), Error> {
        match self {
            Self::Diagnostic(host) => {
                host.ui.cancel();
                Ok(())
            }
            Self::Aside(story) => story.close(),
        }
    }

    pub(crate) fn close_complete(&self) -> bool {
        match self {
            Self::Diagnostic(_) => true,
            Self::Aside(story) => story.close_complete(),
        }
    }

    pub(crate) fn exit_requested(&self) -> bool {
        matches!(self, Self::Aside(story) if story.driver.adapter().exit_requested())
    }

    #[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
    pub(crate) fn diagnostic_step(&self) -> u8 {
        match self {
            Self::Diagnostic(host) => host.diagnostic_step(),
            Self::Aside(_) => unreachable!("diagnostic-only fixture"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_engine::{Epoch, Session, SessionId, aside::VmLimits};

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
            let mut session = WindowSession::aside(driver);
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
