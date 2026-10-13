//! Event-loop scheduling for the existing Aside session driver, without native handles.
use crate::aside_session::{AsideSessionUi, StoryProgress, StoryState};
use reel_engine::aside::{GcBudget, RunOutcome, SliceBudget};

type Error = Box<dyn std::error::Error>;

pub(crate) struct StoryRuntime {
    pub(crate) driver: AsideSessionUi,
    runnable: bool,
    collecting: bool,
    gc_turn: bool,
    retry_after_collection: bool,
    retried_collection: bool,
    closing: bool,
}

impl StoryRuntime {
    pub(crate) fn new(driver: AsideSessionUi) -> Self {
        Self {
            driver,
            runnable: true,
            collecting: false,
            gc_turn: false,
            retry_after_collection: false,
            retried_collection: false,
            closing: false,
        }
    }

    pub(crate) fn needs_tick(&self) -> bool {
        self.runnable || self.collecting || self.driver.cleanup_pending()
    }

    pub(crate) fn wake_input(&mut self) {
        if self.driver.state() == StoryState::Running
            && self.driver.ui().session().view().interaction.is_none()
        {
            self.runnable = true;
        }
    }

    pub(crate) fn close(&mut self) -> Result<(), Error> {
        self.closing = true;
        self.runnable = false;
        self.collecting = false;
        self.driver.cancel()
    }

    pub(crate) fn close_complete(&self) -> bool {
        self.closing && !self.driver.cleanup_pending()
    }

    /// Run bounded VM/GC slices until the window deadline or an idle wait. Native
    /// continuations share that deadline, so they do not each cost a window frame.
    /// Without a deadline, run exactly one slice for manually scheduled hosts.
    pub(crate) fn tick(
        &mut self,
        budget: SliceBudget,
        mut now: impl FnMut() -> u64,
    ) -> Result<(), Error> {
        loop {
            if budget.deadline_ns.is_some_and(|deadline| now() >= deadline) {
                return Ok(());
            }
            self.step(budget, &mut now)?;
            if budget.deadline_ns.is_none() || !self.needs_tick() {
                return Ok(());
            }
        }
    }

    /// Alternate VM and incremental GC work. A wait finishes the active collection
    /// before sleeping, and never repeatedly executes the waiting VM.
    fn step(&mut self, budget: SliceBudget, now: impl FnMut() -> u64) -> Result<(), Error> {
        if self.driver.cleanup_pending() {
            self.driver.cleanup_step(256, budget, now)?;
            return Ok(());
        }
        if self.collecting && (self.gc_turn || !self.runnable) {
            if self.driver.gc_step(GcBudget::default())?.cycle_complete {
                self.collecting = false;
                if self.retry_after_collection {
                    self.runnable = true;
                    self.retry_after_collection = false;
                }
            }
            self.gc_turn = false;
            return Ok(());
        }
        if !self.runnable {
            return Ok(());
        }
        let progress = self.driver.tick(budget, 256, now)?;
        match progress {
            StoryProgress::Interaction(_)
            | StoryProgress::Vm(RunOutcome::Waiting(_))
            | StoryProgress::Finished
            | StoryProgress::Cancelled => self.runnable = false,
            StoryProgress::HostFailed(_) => {
                self.runnable = false;
                let failure = self.driver.host_failure().expect("host failure recorded");
                return Err(format!("story host {:?}: {}", failure.stage, failure.error).into());
            }
            StoryProgress::Faulted(_) => {
                self.runnable = false;
                let fault = self.driver.fault()?.expect("fault delivery remains rooted");
                let location = if let Some(span) = fault.location.source {
                    let package = self.driver.adapter().vm().program().package();
                    let source = &package.sources[span.file.index()];
                    let before = &source.text[..span.start as usize];
                    let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
                    let column = before.rsplit('\n').next().unwrap().chars().count() + 1;
                    format!("{}:{line}:{column}", package.symbols[source.path.index()],)
                } else {
                    format!("{:?}", fault.location)
                };
                let value = self.driver.adapter().format_fault_value(fault)?;
                return Err(format!("{location}: story fault {:?}: {value}", fault.kind).into());
            }
            StoryProgress::Vm(RunOutcome::ResourceBlocked(blocked)) => {
                self.runnable = false;
                if self.retried_collection {
                    // Active catch/unwind capacity cannot be reclaimed by repeated GC.
                    // Keep the complete VM state available for cancellation, without spinning.
                    eprintln!("story resource blocked: {blocked:?}; close the window to cancel");
                } else {
                    if !self.collecting {
                        self.driver.begin_collection()?;
                        self.collecting = true;
                    }
                    self.retried_collection = true;
                    self.retry_after_collection = true;
                }
                return Ok(());
            }
            _ => self.retried_collection = false,
        }
        if self.driver.state() == StoryState::Running {
            if !self.collecting {
                self.driver.begin_collection()?;
                self.collecting = true;
            }
            self.gc_turn = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_engine::{
        Epoch, InputAction, InteractionView, OwnerLifecycle, Session, SessionId, aside::VmLimits,
    };

    fn runtime(text: &str) -> StoryRuntime {
        StoryRuntime::new(
            AsideSessionUi::from_source(
                Session::new(SessionId(73), Epoch(1)).unwrap(),
                text,
                "window_story",
                1,
                "main",
                "en",
                VmLimits::default(),
            )
            .unwrap(),
        )
    }

    fn drain(runtime: &mut StoryRuntime) {
        for _ in 0..1_000_000 {
            if !runtime.needs_tick() {
                return;
            }
            runtime.tick(SliceBudget::default(), || 0).unwrap();
        }
        panic!("fixture did not reach a wait or retirement");
    }

    fn window_tick(runtime: &mut StoryRuntime) -> u64 {
        let mut clock = 0;
        runtime
            .tick(
                SliceBudget {
                    deadline_ns: Some(2_000_000),
                    ..Default::default()
                },
                || {
                    clock += 1_000;
                    clock
                },
            )
            .unwrap();
        clock
    }

    #[test]
    fn story_scheduling_wait_resume_finish_and_close_retire_real_vm() {
        let text = r#"
            use reel::prelude::*;
            fn main() {
                let mut n = 0;
                while n < 40 { n += 1; }
                narrator("Read this");
                let answer = choose([{key: "right", label: "Continue"}]);
                if answer != "right" { throw "wrong result"; }
                let mut scene = 0;
                while scene < 1200 {
                    stage.replace(text("Temporary scene"));
                    scene += 1;
                }
            }
        "#;
        let mut story = runtime(text);
        window_tick(&mut story);
        assert_eq!(story.driver.ui().session().view().history.len(), 1);
        drain(&mut story);
        assert_eq!(story.driver.ui().session().view().history.len(), 1);
        assert!(matches!(
            story.driver.ui().session().view().interaction,
            Some(InteractionView::Dialogue(_))
        ));
        story
            .tick(SliceBudget::default(), || panic!("waiting must not run VM"))
            .unwrap();
        let owner = story.driver.ui().session().root_owner();
        let id = story.driver.ui().session().view().interaction.unwrap().id();
        for _ in 0..2 {
            story
                .driver
                .ui_mut()
                .session_mut()
                .handle_local_input(owner, InputAction::Advance(id))
                .unwrap();
        }
        story.wake_input();
        drain(&mut story);
        let Some(InteractionView::Choice(choice)) = story.driver.ui().session().view().interaction
        else {
            panic!("choice")
        };
        let option = choice.options()[0].id().clone();
        story
            .driver
            .ui_mut()
            .session_mut()
            .handle_local_input(owner, InputAction::Select(option))
            .unwrap();
        story.wake_input();
        let clock = window_tick(&mut story);
        // Publishing requires both text() and stage.replace() to resume in this tick.
        // The synthetic clock counts VM checks, including parameter defaults; it
        // measures yielding, not how many full scenes real hardware can construct.
        assert!(story.driver.adapter().stage().revision() > 0);
        assert_eq!(story.driver.state(), StoryState::Running);
        assert!(clock >= 2_000_000);
        drain(&mut story);
        assert_eq!(story.driver.state(), StoryState::Finished);
        assert_eq!(
            story.driver.ui().session().owner_lifecycle(owner),
            Some(OwnerLifecycle::Retired)
        );
        story.close().unwrap();
        assert!(story.close_complete());

        let mut waiting = runtime(text);
        drain(&mut waiting);
        let owner = waiting.driver.ui().session().root_owner();
        waiting.close().unwrap();
        drain(&mut waiting);
        assert!(waiting.close_complete());
        assert_eq!(
            waiting.driver.ui().session().owner_lifecycle(owner),
            Some(OwnerLifecycle::Retired)
        );
        assert!(waiting.driver.adapter().task().is_none());

        let mut faulted = runtime("fn main() {\n    throw \"boom\";\n}");
        let mut failure = None;
        for _ in 0..100 {
            if let Err(error) = faulted.tick(SliceBudget::default(), || 0) {
                failure = Some(error.to_string());
                break;
            }
        }
        let failure = failure.unwrap();
        assert!(failure.contains("window_story:2:5:"), "{failure}");
        assert!(failure.contains("Thrown: boom"), "{failure}");
        assert!(matches!(faulted.driver.state(), StoryState::Faulted(_)));
        faulted.close().unwrap();
        drain(&mut faulted);
        assert!(faulted.close_complete());
    }
}
