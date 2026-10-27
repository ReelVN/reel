//! Event-loop scheduling for the existing Aside session driver, without native handles.
use crate::aside_session::{AsideSessionUi, StoryProgress, StoryState};
use reel_engine::aside::{GcBudget, RunOutcome, SliceBudget};
#[cfg(test)]
mod save_cost;
mod saves;
mod start;
#[cfg(test)]
mod transition_timing;

type Error = Box<dyn std::error::Error>;

/// Backend-independent VM, activity and GC scheduler. Hosts supply monotonic
/// time and call `wake_input` after submitting input to the session driver.
pub struct StoryRuntime {
    driver: Option<AsideSessionUi>,
    saves: Option<crate::saves::RuntimeSaves>,
    capture: Option<crate::saves::Capture>,
    capture_started: Option<u64>,
    frame_capture_enabled: bool,
    window_control_enabled: bool,
    media_factory: Option<Box<dyn Fn() -> Box<dyn reel_engine::media::MediaBackend>>>,
    retiring: Option<AsideSessionUi>,
    starting: Option<reel_engine::aside::source::StoryFunction>,
    runnable: bool,
    collecting: bool,
    gc_turn: bool,
    retry_after_collection: bool,
    retried_collection: bool,
    closing: bool,
    logical_ns: u64,
    clock_turn_pending: bool,
    time_offset_ns: u64,
    suspended_at: Option<u64>,
    suspended_ns: u64,
}

impl StoryRuntime {
    pub fn new(driver: AsideSessionUi) -> Self {
        Self {
            driver: Some(driver),
            saves: None,
            capture: None,
            capture_started: None,
            frame_capture_enabled: false,
            window_control_enabled: false,
            media_factory: None,
            retiring: None,
            starting: None,
            runnable: true,
            collecting: false,
            gc_turn: false,
            retry_after_collection: false,
            retried_collection: false,
            closing: false,
            logical_ns: 0,
            clock_turn_pending: false,
            time_offset_ns: 0,
            suspended_at: None,
            suspended_ns: 0,
        }
    }

    pub fn driver(&self) -> &AsideSessionUi {
        self.driver
            .as_ref()
            .unwrap_or_else(|| self.capture.as_ref().unwrap().owner())
    }
    pub fn capturing(&self) -> bool {
        self.capture.is_some()
    }
    pub fn driver_mut(&mut self) -> Result<&mut AsideSessionUi, Error> {
        self.interrupt_capture()?;
        Ok(self.driver.as_mut().unwrap())
    }
    /// Accept the actual compiled frame at the host's monotonic nanosecond time.
    /// Transition starts use the same restored offset and suspension accounting
    /// as story ticks, without advancing activities or executing script here.
    pub fn presented_frame(
        &mut self,
        bridge: &reel_ui::bridge::PresentationBridge,
        receipt: &crate::session::RecordedProjection,
        frame: &reel_ui::frame::RenderFrame,
        accepted_host_ns: u64,
    ) -> Result<(), Error> {
        if self.capturing() {
            return Ok(());
        }
        let accepted_time = self.logical_time_at(accepted_host_ns) as f64 / 1_000_000_000.;
        self.driver
            .as_mut()
            .unwrap()
            .presented_frame(bridge, receipt, frame, accepted_time)?;
        self.wake_input();
        Ok(())
    }
    fn logical_time_at(&self, host_ns: u64) -> u64 {
        self.logical_ns.max(
            self.suspended_at
                .unwrap_or(host_ns)
                .saturating_add(self.time_offset_ns)
                .saturating_sub(self.suspended_ns),
        )
    }
    pub fn with_saves(mut self, saves: crate::saves::RuntimeSaves) -> Self {
        self.saves = Some(saves);
        self
    }
    pub fn set_saves(&mut self, saves: crate::saves::RuntimeSaves) {
        self.saves = Some(saves);
    }
    /// Enable only when the host drives CaptureService against its accepted frame.
    /// Otherwise capture requests fail explicitly instead of waiting for a backend.
    pub fn set_frame_capture_enabled(&mut self, enabled: bool) {
        self.frame_capture_enabled = enabled;
    }
    /// Enable only when the host consumes and completes window requests.
    pub fn set_window_control_enabled(&mut self, enabled: bool) {
        self.window_control_enabled = enabled;
    }
    /// Supply fresh device adapters for new sessions and restored saves.
    /// This is independent of whether the host installs save storage.
    pub fn set_media_factory(
        &mut self,
        factory: impl Fn() -> Box<dyn reel_engine::media::MediaBackend> + 'static,
    ) {
        self.media_factory = Some(Box::new(factory));
    }
    pub fn with_media_factory(
        mut self,
        factory: impl Fn() -> Box<dyn reel_engine::media::MediaBackend> + 'static,
    ) -> Self {
        self.set_media_factory(factory);
        self
    }
    pub fn needs_tick(&self) -> bool {
        if self.starting.is_some()
            || self.retiring.is_some()
            || self.capture.is_some()
            || self.saves.as_ref().is_some_and(|s| s.busy())
        {
            return true;
        }
        (self.suspended_at.is_none() || self.closing)
            && (self.needs_work()
                || self.driver().adapter().has_save_requests()
                || self.driver().adapter().has_capture_requests()
                || self.driver().adapter().has_window_requests()
                || self.driver().adapter().media_poll_after().is_some()
                || self.driver().adapter().localization_pending()
                || self.driver().activities_need_time()
                || self.driver().ui().dialogue_needs_time())
    }

    /// Suspend story time. Execution deadlines always remain on the unadjusted host clock.
    pub fn suspend(&mut self, now_ns: u64) -> Result<(), Error> {
        self.interrupt_capture()?;
        self.suspended_at.get_or_insert(now_ns);
        self.runnable |= self.driver().ui().session().reading().skipping();
        self.driver
            .as_mut()
            .unwrap()
            .ui_mut()
            .session_mut()
            .set_skip_reading(false, false, false);
        self.driver.as_mut().unwrap().ui_mut().pause_dialogue();
        self.driver.as_mut().unwrap().suspend_media();
        Ok(())
    }
    pub fn resume(&mut self, now_ns: u64) {
        self.driver.as_mut().unwrap().resume_media();
        if let Some(start) = self.suspended_at.take() {
            self.suspended_ns = self
                .suspended_ns
                .saturating_add(now_ns.saturating_sub(start));
        }
    }

    fn needs_work(&self) -> bool {
        self.runnable
            || self.driver().adapter().capture_results_pending()
            || self.driver().adapter().asset_reads_need_work()
            || self.driver().transitions_pending()
            || self.collecting
            || self.driver().cleanup_pending()
            || self.driver().activities_pending()
    }

    fn short_poll(&self) -> bool {
        self.starting.is_some()
            || self.retiring.is_some()
            || self.capture.is_some()
            || self.saves.as_ref().is_some_and(|s| s.busy())
            || self.needs_work()
    }

    fn media_interval(&self) -> Option<std::time::Duration> {
        let media = self.driver().adapter().media_poll_after();
        if self.driver().adapter().localization_pending() {
            Some(media.map_or(std::time::Duration::from_millis(10), |t| {
                t.min(std::time::Duration::from_millis(10))
            }))
        } else {
            media
        }
    }

    fn time_interval(&self) -> std::time::Duration {
        let reveal = self
            .driver()
            .ui()
            .dialogue_interval(self.driver().adapter().game_settings().text_speed);
        if let Some(interval) = self.driver().activities_poll_after() {
            if self.driver().ui().dialogue_needs_time() {
                reveal.min(interval)
            } else {
                interval
            }
        } else {
            reveal
        }
    }

    /// Remaining delay at the latest story-time sample. Hosts should schedule
    /// [`Self::wake_deadline`] so execution and recording time do not rearm timers.
    pub fn tick_interval(&self) -> std::time::Duration {
        if self.short_poll() {
            return std::time::Duration::from_millis(1);
        }
        let interval = self.time_interval();
        self.media_interval()
            .map_or(interval, |media| media.min(interval))
    }

    /// Next absolute host-clock wake, or `None` while idle or suspended.
    /// Story timers remain anchored to the latest logical-time sample; media
    /// polling and runnable work keep their relative maintenance intervals.
    pub fn wake_deadline(
        &self,
        now: reel_core::clock::MonoTime,
    ) -> Option<reel_core::clock::MonoTime> {
        use reel_core::clock::MonoTime;
        let after = |base: u64, interval: std::time::Duration| {
            base.saturating_add(u64::try_from(interval.as_nanos()).unwrap_or(u64::MAX))
        };
        if !self.needs_tick() {
            return None;
        }
        if self.short_poll() {
            return Some(MonoTime(after(now.0, std::time::Duration::from_millis(1))));
        }
        let sample = self
            .logical_ns
            .saturating_add(self.suspended_ns)
            .saturating_sub(self.time_offset_ns);
        let story = after(sample, self.time_interval());
        let deadline = self
            .media_interval()
            .map_or(story, |interval| story.min(after(now.0, interval)));
        Some(MonoTime(deadline.max(now.0)))
    }

    /// Resume ready callbacks and completed waits after host input or a resource
    /// wake. A stage containing only event handlers remains asleep until a
    /// callback or completed wait becomes runnable.
    pub fn wake_input(&mut self) {
        if self.driver().state() == StoryState::Running
            && (self.driver().has_runnable_tasks() || self.driver().adapter().asset_reads_pending())
        {
            self.runnable = true;
        }
    }

    pub fn close(&mut self) -> Result<(), Error> {
        self.interrupt_capture()?;
        self.closing = true;
        self.starting = None;
        self.runnable = false;
        self.collecting = false;
        self.driver.as_mut().unwrap().cancel()
    }

    pub fn close_complete(&self) -> bool {
        self.closing
            && self.retiring.is_none()
            && !self.driver().cleanup_pending()
            && self.saves.as_ref().is_none_or(|s| !s.busy())
    }

    /// Run bounded VM/GC slices until the window deadline or an idle wait. Native
    /// continuations share that deadline, so they do not each cost a window frame.
    /// Without a deadline, run exactly one slice for manually scheduled hosts.
    pub fn tick(&mut self, budget: SliceBudget, mut now: impl FnMut() -> u64) -> Result<(), Error> {
        if !self.needs_tick() {
            return Ok(());
        }
        let now_ns = now();
        if let Some(retiring) = &mut self.retiring {
            retiring.cleanup_step(256, budget, || now_ns)?;
            if !retiring.cleanup_pending() {
                self.retiring = None;
            }
        }
        if self.capture.is_none()
            && let Some(start) = self.capture_started.take()
        {
            self.suspended_ns += now_ns.saturating_sub(start);
        }
        if !self.frame_capture_enabled && self.capture.is_none() {
            let adapter = self.driver.as_mut().unwrap().adapter_mut();
            if let Some(request) = adapter.take_capture_request()? {
                adapter.complete_capture_request(
                    request.id,
                    Err("frame capture is not installed by this host".into()),
                )?;
                self.runnable = true;
            }
        }
        if !self.window_control_enabled && self.capture.is_none() {
            let adapter = self.driver.as_mut().unwrap().adapter_mut();
            if let Some(request) = adapter.take_window_request(true)? {
                adapter.complete_window_request(
                    request.id,
                    Err("window control is not installed by this host".into()),
                )?;
                self.runnable = true;
            }
        }
        if self.maintain_saves(now_ns, budget)? {
            return Ok(());
        }
        if self.starting.is_some() {
            self.start_session(now_ns)?;
            return Ok(());
        }
        if self.suspended_at.is_some() && !self.closing {
            return Ok(());
        }
        if self.driver().adapter().capture_results_pending() {
            self.driver
                .as_mut()
                .unwrap()
                .adapter_mut()
                .maintain_capture_results(64)?;
            self.runnable = true;
        }
        if self.driver.as_mut().unwrap().maintain_media()?
            | self.driver.as_mut().unwrap().maintain_localization()?
            | self
                .driver
                .as_mut()
                .unwrap()
                .adapter_mut()
                .maintain_asset_reads(64)?
            || (self.driver().adapter().task().is_none()
                && !self.driver().adapter().has_live_work()
                && self.driver().state() == StoryState::Running)
        {
            self.runnable = true;
        }
        if !self.closing && self.needs_tick() && !self.clock_turn_pending {
            self.advance_clock(now())?;
        }
        loop {
            if budget.deadline_ns.is_some_and(|deadline| now() >= deadline) {
                return Ok(());
            }
            self.step(budget, &mut now)?;
            if self.driver().adapter().virtual_feedback_boundary() {
                return Ok(());
            }
            if !self.needs_work() {
                self.clock_turn_pending = false;
                return Ok(());
            }
            if self.starting.is_some() || budget.deadline_ns.is_none() {
                return Ok(());
            }
        }
    }

    fn advance_clock(&mut self, now_ns: u64) -> Result<(), Error> {
        // Keep one clock batch across host deadlines until its observers settle
        // and a script instruction completes. Then sample current time again.
        self.clock_turn_pending = true;
        self.logical_ns = self.logical_time_at(now_ns);
        let speed = self.driver().adapter().game_settings().text_speed;
        let reading_changed = self
            .driver
            .as_mut()
            .unwrap()
            .ui_mut()
            .advance_dialogue(self.logical_ns, speed)?;
        if reading_changed && self.driver().has_runnable_tasks() {
            self.runnable = true;
        }
        let progress = self
            .driver
            .as_mut()
            .unwrap()
            .advance_time(self.logical_ns as f64 / 1_000_000_000., 64)?;
        if progress.woke
            || (progress.work != 0
                && self.driver().adapter().task().is_none()
                && !self.driver().adapter().has_live_work())
        {
            self.runnable = true;
        }
        Ok(())
    }

    /// Alternate VM and incremental GC work. Resource pressure completes a full
    /// collection before retrying; persistent hard-capacity failure ends the run.
    fn step(&mut self, budget: SliceBudget, now: impl FnMut() -> u64) -> Result<(), Error> {
        if self.driver().cleanup_pending() {
            self.driver
                .as_mut()
                .unwrap()
                .cleanup_step(256, budget, now)?;
            return Ok(());
        }
        if self.driver().activities_pending() {
            if self
                .driver
                .as_mut()
                .unwrap()
                .advance_time(self.logical_ns as f64 / 1_000_000_000., 64)?
                .woke
            {
                self.runnable = true;
            }
            return Ok(());
        }
        if self.collecting
            && (self.gc_turn
                || self.retry_after_collection
                || !self.runnable
                || self.driver().adapter().vm().heap().stats().pending_bytes != 0)
        {
            if self
                .driver
                .as_mut()
                .unwrap()
                .gc_step(GcBudget::default())?
                .cycle_complete
            {
                self.collecting = false;
                if self.retry_after_collection {
                    self.runnable = true;
                    self.retry_after_collection = false;
                }
                self.start_collection_if_due()?;
            }
            self.gc_turn = false;
            return Ok(());
        }
        if !self.runnable {
            return Ok(());
        }
        let settled = self.driver().adapter().stage_ready();
        let progress = self.driver.as_mut().unwrap().tick(budget, 256, now)?;
        if settled
            && !matches!(&progress, StoryProgress::Vm(RunOutcome::BudgetYield(usage))
                if usage.instructions == 0)
            && !matches!(
                &progress,
                StoryProgress::Decoding { .. } | StoryProgress::NativeBudgetYield { .. }
            )
        {
            self.clock_turn_pending = false;
        }
        match progress {
            StoryProgress::StartRequested(entry) => {
                self.starting = Some(entry);
                self.runnable = false;
                self.collecting = false;
                return Ok(());
            }
            StoryProgress::ExitRequested => {
                self.close()?;
                return Ok(());
            }
            StoryProgress::Interaction(_)
            | StoryProgress::Vm(RunOutcome::Waiting(_))
            | StoryProgress::Finished
            | StoryProgress::Cancelled
            | StoryProgress::Idle => self.runnable = self.driver().has_runnable_tasks(),
            StoryProgress::HostFailed(_) => {
                self.runnable = false;
                let failure = self.driver().host_failure().expect("host failure recorded");
                return Err(format!("story host {:?}: {}", failure.stage, failure.error).into());
            }
            StoryProgress::Faulted(_) => {
                self.runnable = false;
                let fault = self
                    .driver()
                    .fault()?
                    .expect("fault delivery remains rooted");
                let location = self.location_text(fault.location);
                let value = self.driver().adapter().format_fault_value(fault)?;
                return Err(format!("{location}: story fault {:?}: {value}", fault.kind).into());
            }
            StoryProgress::Vm(RunOutcome::ResourceBlocked(blocked)) => {
                self.runnable = false;
                if self.retried_collection {
                    let vm = self.driver().adapter().vm();
                    let location = vm
                        .task_location(blocked.task)?
                        .map(|location| self.location_text(location))
                        .unwrap_or_else(|| "story".into());
                    return Err(format!(
                        "{location}: story resource {:?}: used {}, requires {} after garbage collection",
                        blocked.resource, blocked.current, blocked.required
                    ).into());
                } else {
                    if !self.collecting {
                        self.driver.as_mut().unwrap().begin_collection()?;
                        self.collecting = true;
                    }
                    self.retried_collection = true;
                    self.retry_after_collection = true;
                }
                return Ok(());
            }
            _ => self.retried_collection = false,
        }
        if self.driver().state() == StoryState::Running {
            self.start_collection_if_due()?;
            self.gc_turn = true;
        }
        Ok(())
    }

    fn start_collection_if_due(&mut self) -> Result<(), Error> {
        if !self.collecting && self.driver().adapter().vm().heap().collection_due() {
            self.driver.as_mut().unwrap().begin_collection()?;
            self.collecting = true;
        }
        Ok(())
    }

    fn location_text(&self, location: aside_vm::executor::Location) -> String {
        let package = self.driver().adapter().vm().program().package();
        if let Some(span) = location.source {
            let source = &package.sources[span.file.index()];
            let before = &source.text[..span.start as usize];
            let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
            let column = before.rsplit('\n').next().unwrap().chars().count() + 1;
            format!("{}:{line}:{column}", package.symbols[source.path.index()])
        } else {
            let id = location.code.function;
            let function = &package.functions[id.index()];
            let module = &package.modules[function.module.index()];
            format!(
                "{}::{} [function {}, pc {}]",
                package.symbols[module.path.index()],
                package.symbols[function.name.index()],
                id.0,
                location.pc.0
            )
        }
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

    #[test]
    fn hard_capacity_finishes_collection_and_reports_the_blocked_source() {
        let code =
            "fn main() { try { throw 1; } catch a { try { throw 2; } catch b { throw 3; } } }";
        let limits = VmLimits {
            max_unwinds: 1,
            ..Default::default()
        };
        let driver = AsideSessionUi::from_source(
            Session::new(SessionId(74), Epoch(1)).unwrap(),
            code,
            "pressure",
            1,
            "main",
            "en",
            limits,
        )
        .unwrap();
        let mut story = StoryRuntime::new(driver);
        let collections = story.driver().adapter().vm().heap().stats().collections;
        let mut error = None;
        for _ in 0..10_000 {
            // UI synchronization can wake work while the forced collection runs.
            story.wake_input();
            if let Err(failure) = story.tick(SliceBudget::default(), || 0) {
                error = Some(failure.to_string());
                break;
            }
        }
        let error = error.expect("persistent hard-capacity pressure must reach the host");
        assert!(error.contains("pressure:1:"), "{error}");
        assert!(error.contains("CatchRecords"), "{error}");
        assert!(error.contains("after garbage collection"), "{error}");
        assert!(story.driver().adapter().vm().heap().stats().collections > collections);
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
    fn particles_keep_producers_live_and_wake_awaiters_after_window_suspension() {
        for wait in [false, true] {
            let ending = if wait {
                "if await active != \"Completed\" { throw \"terminal\"; }"
            } else {
                ""
            };
            let mut story = runtime(&format!(
                "fn main() {{ let snow = particles(\"snow.png\"); stage.replace(snow); let active = emit(snow, burst:0, rate:10, lifetime:1, duration: 1); {ending} }}"
            ));
            for _ in 0..10000 {
                if !story.needs_work() {
                    break;
                }
                story.tick(SliceBudget::default(), || 0).unwrap();
            }
            assert_eq!(story.driver().state(), StoryState::Running);
            assert!(story.needs_tick());
            story.tick(SliceBudget::default(), || 500_000_000).unwrap();
            story.suspend(500_000_000).unwrap();
            assert!(!story.needs_tick());
            story.resume(20_500_000_000);
            for _ in 0..10000 {
                if !story.needs_tick() {
                    break;
                }
                story
                    .tick(SliceBudget::default(), || 22_000_000_000)
                    .unwrap();
            }
            assert_eq!(story.driver().state(), StoryState::Finished);
        }
    }

    #[test]
    fn repeated_same_time_ticks_finish_activity_maintenance_and_run_callbacks() {
        let mut story = runtime(
            r#"
            fn main() {
                let node = stack();
                stage.replace(node);
                for index in 0 .. 40 { delay(10).run(owner: node); }
                let gate = completion();
                spawn(|| gate.resolve("done"), owner: node);
                if await gate != "done" { throw "callback result"; }
                narrator("callback resumed");
            }
            "#,
        );
        for _ in 0..10_000 {
            story.tick(SliceBudget::default(), || 0).unwrap();
            if story.driver().ui().session().view().interaction.is_some() {
                break;
            }
        }
        assert_eq!(story.logical_ns, 0);
        assert!(matches!(
            story.driver().ui().session().view().interaction,
            Some(InteractionView::Dialogue(_))
        ));
    }

    #[test]
    fn fractional_activity_deadlines_round_up_to_the_host_clock() {
        use reel_core::clock::MonoTime;
        let mut story = runtime(
            "fn main() { await run(delay(0.1)); await run(delay(0.2)); narrator(\"ready\"); }",
        );
        let settle = |story: &mut StoryRuntime, time| {
            for _ in 0..10_000 {
                if !story.needs_work() {
                    return;
                }
                story.tick(SliceBudget::default(), || time).unwrap();
            }
            panic!("fractional delay did not reach a timed wait");
        };
        settle(&mut story, 0);
        assert_eq!(
            story.wake_deadline(MonoTime(0)),
            Some(MonoTime(100_000_000))
        );
        story.tick(SliceBudget::default(), || 100_000_000).unwrap();
        settle(&mut story, 100_000_000);
        let deadline = story.wake_deadline(MonoTime(100_000_000)).unwrap();
        assert_eq!(deadline, MonoTime(300_000_001));
        story.tick(SliceBudget::default(), || deadline.0).unwrap();
        settle(&mut story, deadline.0);
        assert!(matches!(
            story.driver().ui().session().view().interaction,
            Some(InteractionView::Dialogue(_))
        ));
    }

    #[test]
    fn pure_delays_sleep_at_deadlines_and_suspension_preserves_remaining_time() {
        use reel_core::clock::MonoTime;
        let mut story = runtime("fn main() { await run(delay(2)); }");
        for _ in 0..10_000 {
            if !story.needs_work() {
                break;
            }
            story.tick(SliceBudget::default(), || 0).unwrap();
        }
        assert!(!story.needs_work());
        assert_eq!(story.tick_interval(), std::time::Duration::from_secs(2));
        assert_eq!(
            story.wake_deadline(MonoTime(0)),
            Some(MonoTime(2_000_000_000))
        );
        // Time spent executing or recording after a tick consumes the same wait.
        assert_eq!(
            story.wake_deadline(MonoTime(500_000_000)),
            Some(MonoTime(2_000_000_000))
        );
        story.tick(SliceBudget::default(), || 500_000_000).unwrap();
        assert_eq!(
            story.tick_interval(),
            std::time::Duration::from_millis(1500)
        );
        assert_eq!(
            story.wake_deadline(MonoTime(750_000_000)),
            Some(MonoTime(2_000_000_000))
        );
        story.suspend(500_000_000).unwrap();
        assert!(!story.needs_tick());
        assert_eq!(story.wake_deadline(MonoTime(20_000_000_000)), None);
        story
            .tick(SliceBudget::default(), || panic!("suspended timer polled"))
            .unwrap();
        story.resume(20_500_000_000);
        assert_eq!(
            story.wake_deadline(MonoTime(20_500_000_000)),
            Some(MonoTime(22_000_000_000))
        );
        story
            .tick(SliceBudget::default(), || 21_000_000_000)
            .unwrap();
        assert_eq!(story.tick_interval(), std::time::Duration::from_secs(1));
        assert_eq!(
            story.wake_deadline(MonoTime(21_900_000_000)),
            Some(MonoTime(22_000_000_000))
        );
        assert_eq!(
            story.wake_deadline(MonoTime(22_100_000_000)),
            Some(MonoTime(22_100_000_000))
        );
        for _ in 0..10_000 {
            if !story.needs_tick() {
                break;
            }
            story
                .tick(SliceBudget::default(), || 22_000_000_000)
                .unwrap();
        }
        assert_eq!(story.driver().state(), StoryState::Finished);
        assert!(!story.needs_tick());
        assert_eq!(story.wake_deadline(MonoTime(22_000_000_000)), None);
    }

    #[test]
    fn completed_speech_reveal_wakes_the_waiting_story_without_polling_incomplete_playback() {
        use reel_core::identity::{FrameId, SurfaceEpoch};
        use reel_ui::{
            app::{RenderRoot, RenderRootOptions},
            bridge::{
                BridgeLimits, NoticeResult, PresentationBridge, PresentationNotice,
                PresentationOutcome,
            },
            core::Widget,
        };

        let code = r#"fn main() {
            stage.replace(stack(width:100,height:100));
            let speech=narrator("AB",wait:false);
            speech.revealed().wait();
            narrator("next");
        }"#;
        for encoded in [false, true] {
            for manual in [false, true] {
                let mut compiled =
                    reel_engine::aside::source::compile_story(code, "reveal_wake", 1, "main")
                        .unwrap();
                if encoded {
                    use aside_vm::bytecode as bc;
                    let bytes =
                        bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
                    compiled.program = bc::verify(
                        bc::decode(&bytes, bc::Limits::default()).unwrap(),
                        bc::Limits::default(),
                    )
                    .unwrap();
                }
                let driver = AsideSessionUi::from_compiled(
                    Session::new(SessionId(73), Epoch(1)).unwrap(),
                    compiled,
                    "en",
                    VmLimits::default(),
                )
                .unwrap();
                let widget = driver.widget().prepare();
                let root = widget.id();
                let tree = RenderRoot::new(
                    widget,
                    |_| {},
                    RenderRootOptions {
                        default_properties: std::sync::Arc::new(Default::default()),
                        use_system_fonts: false,
                        size_policy: reel_ui::app::WindowSizePolicy::User,
                        size: reel_ui::dpi::PhysicalSize::new(640, 420),
                        scale_factor: 1.,
                        default_font: None,
                    },
                );
                let mut bridge = PresentationBridge::new(
                    tree,
                    SurfaceEpoch(1),
                    BridgeLimits {
                        geometry: reel_ui::app::GeometryLimits { max_bytes: 1 << 20 },
                        max_candidates: 1,
                        max_bridge_bytes: 64 << 10,
                        max_snapshot_bytes: 8 << 20,
                    },
                )
                .unwrap();
                let mut story = StoryRuntime::new(driver);
                let settle = |story: &mut StoryRuntime, time| {
                    for _ in 0..10_000 {
                        if !story.needs_work() {
                            return;
                        }
                        story.tick(SliceBudget::default(), || time).unwrap();
                    }
                    panic!("speech playback did not settle");
                };
                settle(&mut story, 0);
                let driver = story.driver_mut().unwrap();
                driver.sync(&mut bridge, root).unwrap();
                let (source, receipt) = driver
                    .record(&mut bridge, root, FrameId(1))
                    .unwrap()
                    .expect("initial speech frame");
                assert_eq!(
                    bridge.resolve(PresentationNotice::new(
                        source.key(),
                        SurfaceEpoch(1),
                        PresentationOutcome::Accepted(FrameId(1)),
                    )),
                    NoticeResult::Accepted
                );
                driver.presented(&bridge, &receipt).unwrap();
                story.wake_input();
                assert!(
                    !story.needs_work(),
                    "incomplete reveal must leave the VM asleep"
                );
                assert!(story.needs_tick(), "accepted text needs playback time");
                let session = story.driver().ui().session();
                let owner = session.root_owner();
                let id = session.view().interaction.unwrap().id();
                if manual {
                    assert_eq!(
                        story
                            .driver_mut()
                            .unwrap()
                            .ui_mut()
                            .session_mut()
                            .handle_local_input(owner, InputAction::Advance(id))
                            .unwrap(),
                        reel_engine::InputOutcome::Revealed
                    );
                    story.wake_input();
                } else {
                    story.tick(SliceBudget::default(), || 0).unwrap();
                    story.tick(SliceBudget::default(), || 40_000_000).unwrap();
                    assert!(matches!(story.driver().ui().session().view().interaction,
                        Some(InteractionView::Dialogue(d)) if d.lines()[0].visible_text()=="A"
                            && !d.lines()[0].playback_complete() && !d.awaiting_advance()));
                    assert!(
                        !story.needs_work(),
                        "partial reveal must leave the VM asleep"
                    );
                    story.tick(SliceBudget::default(), || 100_000_000).unwrap();
                }
                settle(&mut story, 100_000_000);
                assert!(matches!(story.driver().ui().session().view().interaction,
                    Some(InteractionView::Dialogue(d)) if d.id()==id && d.text()=="AB"
                        && d.lines()[0].playback_complete() && d.awaiting_advance()));
                assert!(
                    story
                        .driver()
                        .ui()
                        .session()
                        .read_progress()
                        .ids()
                        .next()
                        .is_none()
                );
                assert!(
                    !story.needs_tick(),
                    "a completed reveal must sleep while awaiting input"
                );
                story
                    .tick(SliceBudget::default(), || panic!("advance wait must sleep"))
                    .unwrap();
                assert_eq!(
                    story
                        .driver_mut()
                        .unwrap()
                        .ui_mut()
                        .session_mut()
                        .handle_local_input(owner, InputAction::Advance(id))
                        .unwrap(),
                    reel_engine::InputOutcome::Completed
                );
                story.wake_input();
                settle(&mut story, 100_000_000);
                assert!(matches!(story.driver().ui().session().view().interaction,
                    Some(InteractionView::Dialogue(d)) if d.text()=="next"));
                story.close().unwrap();
                drain(&mut story);
            }
        }
    }

    #[test]
    fn animations_progress_with_dialogue_and_paused_windows_sleep() {
        let mut story = runtime(
            r#"fn main() {
            let node = text("moving");
            stage.replace(node);
            let activity = run(tween(node, x: 100, duration: 1));
            narrator("moving dialogue");
            if node.x != 50 { throw "dialogue blocked animation"; }
            pause(activity);
            narrator("paused dialogue");
            if node.x != 50 { throw "pause advanced"; }
            resume(activity);
            if await activity != "Completed" || node.x != 100 { throw "await not resumed"; }
        }"#,
        );
        let work = |story: &mut StoryRuntime, ns| {
            for _ in 0..10_000 {
                if !story.needs_work() {
                    return;
                }
                story.tick(SliceBudget::default(), || ns).unwrap();
            }
            panic!("window work did not settle");
        };
        let input = |story: &mut StoryRuntime| {
            let owner = story.driver().ui().session().root_owner();
            let id = story
                .driver()
                .ui()
                .session()
                .view()
                .interaction
                .unwrap()
                .id();
            for _ in 0..2 {
                story
                    .driver_mut()
                    .unwrap()
                    .ui_mut()
                    .session_mut()
                    .handle_local_input(owner, InputAction::Advance(id))
                    .unwrap();
            }
            story.wake_input();
        };
        work(&mut story, 0);
        assert!(story.needs_tick());
        assert_eq!(story.tick_interval(), std::time::Duration::from_millis(16));
        story.tick(SliceBudget::default(), || 500_000_000).unwrap();
        assert!(
            !story.needs_work(),
            "animation must not busy-run the waiting VM"
        );
        input(&mut story);
        work(&mut story, 500_000_000);
        assert!(!story.needs_tick());
        story
            .tick(SliceBudget::default(), || {
                panic!("paused window should sleep")
            })
            .unwrap();
        input(&mut story);
        work(&mut story, 10_000_000_000);
        assert!(story.needs_tick());
        story
            .tick(SliceBudget::default(), || 10_500_000_000)
            .unwrap();
        work(&mut story, 10_500_000_000);
        assert_eq!(story.driver().state(), StoryState::Finished);
        assert!(!story.needs_tick());
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
        assert_eq!(story.driver().ui().session().view().history.len(), 1);
        drain(&mut story);
        assert_eq!(story.driver().ui().session().view().history.len(), 1);
        assert!(matches!(
            story.driver().ui().session().view().interaction,
            Some(InteractionView::Dialogue(_))
        ));
        story
            .tick(SliceBudget::default(), || panic!("waiting must not run VM"))
            .unwrap();
        let owner = story.driver().ui().session().root_owner();
        let id = story
            .driver()
            .ui()
            .session()
            .view()
            .interaction
            .unwrap()
            .id();
        for _ in 0..2 {
            story
                .driver_mut()
                .unwrap()
                .ui_mut()
                .session_mut()
                .handle_local_input(owner, InputAction::Advance(id))
                .unwrap();
        }
        story.wake_input();
        drain(&mut story);
        let Some(InteractionView::Choice(choice)) =
            story.driver().ui().session().view().interaction
        else {
            panic!("choice")
        };
        let option = choice.options()[0].id().clone();
        story
            .driver_mut()
            .unwrap()
            .ui_mut()
            .session_mut()
            .handle_local_input(owner, InputAction::Select(option))
            .unwrap();
        story.wake_input();
        let clock = window_tick(&mut story);
        // The synthetic clock measures yielding, not scene construction time.
        // A constructor's parameter defaults may span several bounded ticks.
        for _ in 0..8 {
            if story.driver().adapter().stage().revision() > 0 {
                break;
            }
            window_tick(&mut story);
        }
        assert!(story.driver().adapter().stage().revision() > 0);
        assert_eq!(story.driver().state(), StoryState::Running);
        assert!(clock >= 2_000_000);
        drain(&mut story);
        assert_eq!(story.driver().state(), StoryState::Finished);
        let heap = story.driver().adapter().vm().heap().stats();
        eprintln!(
            "1200 scene replacements: peak_charged_bytes={}, peak_pending_bytes={}",
            heap.peak_charged_bytes, heap.peak_pending_bytes
        );
        assert_eq!(
            story.driver().ui().session().owner_lifecycle(owner),
            Some(OwnerLifecycle::Retired)
        );
        story.close().unwrap();
        assert!(story.close_complete());

        let mut waiting = runtime(text);
        drain(&mut waiting);
        let owner = waiting.driver().ui().session().root_owner();
        waiting.close().unwrap();
        drain(&mut waiting);
        assert!(waiting.close_complete());
        assert_eq!(
            waiting.driver().ui().session().owner_lifecycle(owner),
            Some(OwnerLifecycle::Retired)
        );
        assert!(waiting.driver().adapter().task().is_none());

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
        assert!(matches!(faulted.driver().state(), StoryState::Faulted(_)));
        faulted.close().unwrap();
        drain(&mut faulted);
        assert!(faulted.close_complete());
    }
}
