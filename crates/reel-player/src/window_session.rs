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
                story.wake_input();
                story.driver.sync(bridge, root)
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

    pub(crate) fn needs_tick(&self) -> bool {
        matches!(self, Self::Aside(story) if story.needs_tick())
    }
    pub(crate) fn suspend(&mut self, now: u64) {
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
            Self::Diagnostic(_) => std::time::Duration::from_millis(1),
        }
    }

    pub(crate) fn tick(
        &mut self,
        budget: SliceBudget,
        now: impl FnMut() -> u64,
    ) -> Result<(), Error> {
        if let Self::Aside(story) = self {
            story.tick(budget, now)?;
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

    #[cfg(all(test, any(target_os = "linux", target_os = "windows")))]
    pub(crate) fn diagnostic_step(&self) -> u8 {
        match self {
            Self::Diagnostic(host) => host.diagnostic_step(),
            Self::Aside(_) => unreachable!("diagnostic-only fixture"),
        }
    }
}
