use super::*;
use reel_engine::{Epoch, Session, aside::source::CompiledStory};

impl StoryRuntime {
    pub(super) fn start_session(&mut self, now_ns: u64) -> Result<(), Error> {
        // A worker may still be committing an old save. Drain it before request
        // IDs can be reused by the replacement VM. Never queue retiring VMs.
        if self.retiring.is_some() || self.saves.as_ref().is_some_and(|s| s.busy()) {
            return Ok(());
        }
        let old = self.driver.as_ref().unwrap();
        let identity = old.ui().session().identity();
        let previous_epoch = self.saves.as_ref().map_or(identity.epoch.0, |saves| {
            saves.next_epoch.max(identity.epoch.0)
        });
        let epoch = previous_epoch
            .checked_add(1)
            .ok_or("session epoch exhausted")?;
        if let Some(saves) = &mut self.saves {
            saves.next_epoch = epoch;
        }
        let mut session = Session::new_with_read_progress(
            identity.session_id,
            Epoch(epoch),
            old.ui().session().read_progress().clone(),
        )?;
        session.set_history_limits(old.ui().session().history_limits());
        let story = CompiledStory {
            program: old.adapter().vm().program().clone(),
            resume: old.adapter().resume_table().clone(),
            initializer: old.initializer(),
            entry: self.starting.unwrap(),
            catalogs: Default::default(),
            effects: old.adapter().effect_products(),
            visual_plan: old.adapter().visual_plan(),
        };
        let mut next = AsideSessionUi::from_compiled(
            session,
            story,
            old.locale(),
            old.adapter().vm().limits(),
        )?
        .with_asset_context(old.ui().assets(), old.catalogs());
        next.adapter_mut()
            .set_source_catalogs(old.adapter().source_catalogs())?;
        next.adapter_mut()
            .inherit_game_settings(old.adapter().game_settings().clone());
        next.adapter_mut()
            .set_window_info(old.adapter().window_info().clone());
        if let Some(factory) = &self.media_factory {
            next = next.with_media(factory())?;
        }
        next.await_initial_stage();
        next.ui_mut().adopt_presentation();
        if self.suspended_at.is_some() {
            next.suspend_media();
            self.suspended_at = Some(now_ns);
        }
        self.driver.as_mut().unwrap().cancel()?;
        self.retiring = self.driver.replace(Box::new(next));
        self.starting = None;
        self.logical_ns = 0;
        self.clock_sample_requested = true;
        self.clock_turn_pending = false;
        #[cfg(feature = "profiling")]
        {
            self.stage_profile = None;
        }
        self.time_offset_ns = 0;
        self.suspended_ns = now_ns;
        self.runnable = true;
        self.collecting = false;
        self.gc_turn = false;
        self.retry_after_collection = false;
        self.retried_collection = false;
        Ok(())
    }
}
