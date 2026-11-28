use super::*;
use crate::saves::{Candidate, Capture, RuntimeSaves, StorageJob, StorageResult};
use reel_engine::aside::{
    checkpoint::{CheckpointError, CheckpointLoad, CheckpointRecord},
    saves::{SaveOperation, SaveResult},
};

impl StoryRuntime {
    pub(super) fn interrupt_capture(&mut self) -> Result<(), Error> {
        if let Some(capture) = self.capture.take() {
            let driver = capture.cancel();
            let request = self.saves.as_mut().unwrap().pending.take().unwrap();
            self.driver = Some(driver);
            self.driver
                .as_mut()
                .unwrap()
                .adapter_mut()
                .complete_save_request(
                    request.id,
                    SaveResult::Failed("CaptureBusy: input changed while capturing".into()),
                )?;
            self.runnable = true;
        }
        Ok(())
    }
    pub(super) fn maintain_saves(
        &mut self,
        now_ns: u64,
        budget: SliceBudget,
        now: &mut impl FnMut() -> u64,
    ) -> Result<bool, Error> {
        let Some(mut saves) = self.saves.take() else {
            if self.starting.is_some() {
                return Ok(false);
            }
            if let Some(request) = self
                .driver
                .as_mut()
                .unwrap()
                .adapter_mut()
                .take_save_request()
            {
                self.driver
                    .as_mut()
                    .unwrap()
                    .adapter_mut()
                    .complete_save_request(
                        request.id,
                        SaveResult::Failed("save storage is not installed by this host".into()),
                    )?;
                self.runnable = true;
            }
            return Ok(false);
        };
        let result = self.drive_saves(&mut saves, now_ns, budget, now);
        self.saves = Some(saves);
        result
    }
    fn start_restore(
        &self,
        saves: &mut RuntimeSaves,
        checkpoint: CheckpointRecord,
    ) -> Result<(), Error> {
        let live = self.driver.as_ref().unwrap();
        let identity = live.ui().session().identity();
        saves.next_epoch = saves.next_epoch.max(identity.epoch.0) + 1;
        let mut session = reel_engine::Session::new_with_read_progress(
            identity.session_id,
            reel_engine::Epoch(saves.next_epoch),
            live.ui().session().read_progress().clone(),
        )?;
        session.set_history_limits(live.ui().session().history_limits());
        let story = reel_engine::aside::source::CompiledStory {
            program: live.adapter().vm().program().clone(),
            resume: live.adapter().resume_table().clone(),
            initializer: live.initializer(),
            entry: live.entry_function(),
            catalogs: Default::default(),
            effects: live.adapter().effect_products(),
            visual_plan: live.adapter().visual_plan(),
        };
        let mut candidate = AsideSessionUi::from_compiled(
            session,
            story,
            live.locale(),
            live.adapter().vm().limits(),
        )?
        .with_asset_context(live.ui().assets(), live.catalogs());
        candidate
            .adapter_mut()
            .set_source_catalogs(live.adapter().source_catalogs())?;
        candidate
            .adapter_mut()
            .inherit_game_settings(live.adapter().game_settings().clone());
        candidate
            .adapter_mut()
            .set_window_info(live.adapter().window_info().clone());
        if let Some(factory) = &self.media_factory {
            candidate = candidate.with_media(factory())?;
        }
        candidate.suspend_media();
        let restore = CheckpointLoad::new(checkpoint, candidate.adapter())?;
        saves.candidate = Some(Candidate::new(candidate)?);
        saves.restore = Some(restore);
        Ok(())
    }

    fn drive_saves(
        &mut self,
        saves: &mut RuntimeSaves,
        now_ns: u64,
        budget: SliceBudget,
        now: &mut impl FnMut() -> u64,
    ) -> Result<bool, Error> {
        let quota = 256;
        if !self.closing && self.starting.is_none() && self.capture.is_none() {
            let adapter = self.driver.as_mut().unwrap().adapter_mut();
            if adapter.save_results_pending() {
                adapter.maintain_save_results(quota)?;
                self.runnable = true;
                if adapter.save_results_pending() {
                    return Ok(true);
                }
            }
        }
        if self.closing || self.starting.is_some() {
            saves.restore = None;
            saves.candidate = None;
            saves.pending = None;
            saves.history.reset();
        }
        let mut events = Vec::new();
        saves.service.drain(&mut events, 8);
        for event in events {
            saves.captured = None;
            if self.closing || self.starting.is_some() {
                continue;
            }
            self.clock_sample_requested = true;
            if let Some(candidate) = &mut saves.candidate
                && event.request.session == candidate.driver.ui().session().identity()
            {
                let result = match event.result {
                    Ok(StorageResult::Saved | StorageResult::Deleted) => SaveResult::Done,
                    Ok(StorageResult::Slots(slots)) => SaveResult::Slots(slots),
                    Ok(StorageResult::Data(data)) => SaveResult::Data(data),
                    Ok(StorageResult::Loaded(_)) => {
                        SaveResult::Failed("nested load during Root preparation".into())
                    }
                    Err(error) => SaveResult::Failed(error),
                };
                candidate
                    .driver
                    .adapter_mut()
                    .complete_save_request(event.request, result)?;
                continue;
            }
            let result = match event.result {
                Ok(StorageResult::Loaded(checkpoint)) => {
                    match self.start_restore(saves, *checkpoint) {
                        Ok(()) => continue,
                        Err(error) => SaveResult::Failed(error.to_string()),
                    }
                }
                Ok(StorageResult::Saved | StorageResult::Deleted) => SaveResult::Done,
                Ok(StorageResult::Slots(slots)) => SaveResult::Slots(slots),
                Ok(StorageResult::Data(data)) => SaveResult::Data(data),
                Err(error) => SaveResult::Failed(error),
            };
            self.driver
                .as_mut()
                .unwrap()
                .adapter_mut()
                .complete_save_request(event.request, result)?;
            saves.pending = None;
            self.runnable = true;
        }
        if self.closing || self.starting.is_some() {
            return Ok(false);
        }
        if self
            .driver
            .as_ref()
            .is_some_and(|d| d.adapter().save_results_pending())
        {
            return Ok(true);
        }
        // Keep replacement ownership bounded to one retiring session.
        if self.retiring.is_some() {
            return Ok(false);
        }

        // Restore in isolation. Candidate resource errors settle the old request.
        let prepared = (|| -> Result<bool, Error> {
            if let Some(restore) = &mut saves.restore {
                let candidate = saves.candidate.as_mut().unwrap();
                let complete = candidate
                    .driver
                    .load_checkpoint_step(restore, budget, now)?;
                if saves.service.is_idle()
                    && let Some(request) = candidate.driver.adapter_mut().take_save_request()
                {
                    let job = match &request.operation {
                        operation @ (SaveOperation::List
                        | SaveOperation::Thumbnail { .. }
                        | SaveOperation::ReadData { .. }) => Some(storage_job(operation)),
                        _ => None,
                    };
                    if let Some(job) = job {
                        if let Err(rejected) =
                            saves
                                .service
                                .submit(request.id, job, saves.max_snapshot_bytes)
                        {
                            candidate.driver.adapter_mut().complete_save_request(
                                request.id,
                                SaveResult::Failed(format!("save queue: {:?}", rejected.reason)),
                            )?;
                        }
                    } else {
                        candidate.driver.adapter_mut().complete_save_request(
                            request.id,
                            SaveResult::Failed(
                                "saved Root preparation allows storage reads only".into(),
                            ),
                        )?;
                    }
                }
                if !complete {
                    return Ok(false);
                }
                saves.restore = None;
            }
            if let Some(candidate) = &mut saves.candidate {
                if !candidate.driver.prepare_restored(budget, now)? {
                    return Ok(false);
                }
                candidate
                    .driver
                    .sync(&mut candidate.bridge, candidate.root)?;
                return Ok(candidate.driver.adapter().stage_ready()
                    && !candidate.driver.ui().images_pending());
            }
            Ok(false)
        })();
        match prepared {
            Ok(true) => {
                let mut candidate = saves.candidate.take().unwrap().driver;
                match saves.pending.as_ref().map(|request| &request.operation) {
                    Some(SaveOperation::Jump { group }) => {
                        saves.history.adopt(&mut candidate, *group)
                    }
                    _ => saves.history.reset(),
                }
                let mut old = self.driver.take().unwrap();
                old.cancel()?;
                self.retiring = Some(old);
                self.logical_ns = (candidate.adapter().logical_time() * 1_000_000_000.) as u64;
                self.clock_sample_requested = true;
                self.clock_turn_pending = false;
                #[cfg(feature = "profiling")]
                {
                    self.stage_profile = None;
                }
                self.time_offset_ns = self.logical_ns;
                self.suspended_ns = now_ns;
                candidate.ui_mut().adopt_presentation();
                if self.suspended_at.is_none() {
                    candidate.resume_media();
                } else {
                    self.suspended_at = Some(now_ns);
                }
                self.driver = Some(candidate);
                self.runnable = true;
                self.collecting = false;
                self.gc_turn = false;
                self.retry_after_collection = false;
                self.retried_collection = false;
                saves.pending = None;
                // Present the saved scene before executing its successor statements.
                return Ok(true);
            }
            Err(error) => {
                saves.restore = None;
                saves.candidate = None;
                let request = saves.pending.take().unwrap();
                self.driver
                    .as_mut()
                    .unwrap()
                    .adapter_mut()
                    .complete_save_request(request.id, SaveResult::Failed(error.to_string()))?;
                self.runnable = true;
            }
            Ok(false) => {}
        }
        if saves.restore.is_some() || saves.candidate.is_some() {
            return Ok(true);
        }

        if let Some(capture) = &mut self.capture {
            let progress = capture.step(quota).and_then(|complete| {
                if capture.bytes > saves.max_snapshot_bytes {
                    Err(CheckpointError::Contract(
                        "checkpoint exceeds storage budget",
                    ))
                } else {
                    Ok(complete)
                }
            });
            match progress {
                Ok(false) => return Ok(true),
                Ok(true) => {
                    let retained_bytes = capture.bytes;
                    let (driver, checkpoint) = self.capture.take().unwrap().finish();
                    self.driver = Some(driver);
                    let checkpoint = match checkpoint {
                        Ok(checkpoint) => checkpoint,
                        Err(error) => {
                            let request = saves.pending.take().unwrap();
                            self.driver
                                .as_mut()
                                .unwrap()
                                .adapter_mut()
                                .complete_save_request(
                                    request.id,
                                    SaveResult::Failed(error.to_string()),
                                )?;
                            self.runnable = true;
                            return Ok(false);
                        }
                    };
                    self.suspended_ns +=
                        now_ns.saturating_sub(self.capture_started.take().unwrap());
                    let request = saves.pending.as_ref().unwrap();
                    let SaveOperation::Save {
                        slot,
                        title,
                        thumbnail,
                    } = &request.operation
                    else {
                        unreachable!()
                    };
                    if let Err(rejected) = saves.service.submit(
                        request.id,
                        StorageJob::Save {
                            slot: slot.clone(),
                            title: title.clone(),
                            thumbnail: thumbnail.clone(),
                            checkpoint: checkpoint.shared_record(),
                        },
                        retained_bytes.saturating_add(
                            thumbnail.as_ref().map_or(0, |data| data.retained_bytes()),
                        ),
                    ) {
                        let error = format!("save queue: {:?}", rejected.reason);
                        self.driver
                            .as_mut()
                            .unwrap()
                            .adapter_mut()
                            .complete_save_request(request.id, SaveResult::Failed(error))?;
                        saves.pending = None;
                        self.runnable = true;
                    } else {
                        saves.captured = Some(checkpoint);
                    }
                    return Ok(false);
                }
                Err(error) => {
                    let driver = self.capture.take().unwrap().cancel();
                    let request = saves.pending.take().unwrap();
                    self.driver = Some(driver);
                    self.driver
                        .as_mut()
                        .unwrap()
                        .adapter_mut()
                        .complete_save_request(request.id, SaveResult::Failed(error.to_string()))?;
                    self.runnable = true;
                    return Ok(false);
                }
            }
        }
        if saves.pending.is_none() && saves.service.is_idle() {
            saves.pending = self
                .driver
                .as_mut()
                .unwrap()
                .adapter_mut()
                .take_save_request();
        }
        let Some(request) = &saves.pending else {
            return Ok(false);
        };
        if !saves.service.is_idle() {
            return Ok(false);
        }
        if let SaveOperation::Jump { group } = request.operation {
            let id = request.id;
            let record = self
                .driver()
                .adapter()
                .tracks_history()
                .then(|| saves.history.record(self.driver().ui().session(), group))
                .flatten();
            let error = match record {
                Some(record) => match self.start_restore(saves, record) {
                    Ok(()) => return Ok(true),
                    Err(error) => error.to_string(),
                },
                None => "history checkpoint is no longer available".into(),
            };
            self.driver
                .as_mut()
                .unwrap()
                .adapter_mut()
                .complete_save_request(id, SaveResult::Failed(error))?;
            saves.pending = None;
            self.runnable = true;
            return Ok(false);
        }
        if matches!(request.operation, SaveOperation::Save { .. }) {
            if self.suspended_at.is_some()
                || self.collecting
                || self.driver().activities_pending()
                || !self.driver().adapter().stage_ready()
            {
                return Ok(false);
            }
            match Capture::new(&mut self.driver) {
                Ok(capture) => {
                    self.capture = Some(capture);
                    self.capture_started = Some(now_ns);
                    return Ok(true);
                }
                Err(error) => {
                    self.driver
                        .as_mut()
                        .unwrap()
                        .adapter_mut()
                        .complete_save_request(request.id, SaveResult::Failed(error.to_string()))?;
                    saves.pending = None;
                    self.runnable = true;
                }
            }
        } else {
            let job = storage_job(&request.operation);
            let retained = match &job {
                StorageJob::WriteData(_, data) => data.retained_bytes(),
                _ => saves.max_snapshot_bytes,
            };
            if let Err(rejected) = saves.service.submit(request.id, job, retained) {
                self.driver
                    .as_mut()
                    .unwrap()
                    .adapter_mut()
                    .complete_save_request(
                        request.id,
                        SaveResult::Failed(format!("save queue: {:?}", rejected.reason)),
                    )?;
                saves.pending = None;
                self.runnable = true;
            }
        }
        Ok(false)
    }
}

fn storage_job(operation: &SaveOperation) -> StorageJob {
    match operation {
        SaveOperation::Load { slot } => StorageJob::Load(slot.clone()),
        SaveOperation::Copy {
            source,
            destination,
        } => StorageJob::Copy {
            source: source.clone(),
            destination: destination.clone(),
        },
        SaveOperation::Delete { slot } => StorageJob::Delete(slot.clone()),
        SaveOperation::List => StorageJob::List,
        SaveOperation::Thumbnail { slot } => StorageJob::Thumbnail(slot.clone()),
        SaveOperation::ReadData { key } => StorageJob::ReadData(key.clone()),
        SaveOperation::WriteData { key, data } => StorageJob::WriteData(key.clone(), data.clone()),
        SaveOperation::DeleteData { key } => StorageJob::DeleteData(key.clone()),
        SaveOperation::Save { .. } | SaveOperation::Jump { .. } => {
            unreachable!("operation uses an in-process checkpoint")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_core::{budget::QueueLimits, wake::WakeSignal};
    use reel_engine::{
        Epoch, Session, SessionId,
        aside::VmLimits,
        audio::{AudioGroup, AudioPlayback, AudioRequest},
        media::{MediaBackend, MediaCommand, MediaCompletion, MediaId, MediaPlayback},
    };
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
        sync::Arc,
        time::Duration,
    };

    struct Media {
        transport_changes: Rc<Cell<usize>>,
        playback: RefCell<Option<AudioPlayback>>,
    }
    impl MediaBackend for Media {
        fn play_audio(&mut self, request: AudioRequest, paused: bool) -> Result<MediaId, String> {
            *self.playback.borrow_mut() = Some(AudioPlayback {
                request,
                queue: Vec::new(),
                paused,
                result: None,
            });
            Ok(MediaId(1))
        }
        fn commit_audio(&mut self, _: MediaId) {}
        fn snapshot(&self, _: MediaId) -> Result<MediaPlayback, String> {
            Ok(MediaPlayback::Audio(
                self.playback.borrow().as_ref().unwrap().clone(),
            ))
        }
        fn position(&self, _: MediaId) -> Option<Duration> {
            Some(Duration::ZERO)
        }
        fn command(&mut self, _: MediaId, _: MediaCommand) -> Result<(), String> {
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
        fn cancel(&mut self, _: MediaId) {}
        fn maintain(&mut self, _: Duration) {}
        fn take_result(&mut self, _: MediaId) -> Option<MediaCompletion> {
            None
        }
        fn poll_after(&self) -> Option<Duration> {
            None
        }
        fn suspend(&mut self) {
            self.transport_changes.set(self.transport_changes.get() + 1);
        }
        fn resume(&mut self) {
            self.transport_changes.set(self.transport_changes.get() + 1);
        }
        fn close(&mut self) {}
        fn cleanup_pending(&self) -> bool {
            false
        }
    }

    #[test]
    fn saving_and_cancelling_capture_leave_live_media_transport_running() {
        for cancel in [false, true] {
            let changes = Rc::new(Cell::new(0));
            let driver = AsideSessionUi::from_source(
                Session::new(SessionId(83), Epoch(1)).unwrap(),
                r#"
                use std::resume::{state,root,flow,point};
                class World {
                    let retained:array=[];
                    fn init(self){for i in 0..2000{self.retained.append(i);}}
                }
                state(World,"world");root(main,"game");
                fn main(world:World=model(World())) {
                    let music=audio.play("track");
                    reading(||{
                        spawn(||{
                            await when(||game.dialogue().active);
                            try {await saves.save("slot");} catch error {}
                        });
                        run_flow(story,world)
                    })
                }
                fn story(world:World){
                    flow("story");point("line");narrator("saved");
                    if world.retained.len()!=2000{throw "retained data";}
                }"#,
                "save_media",
                1,
                "main",
                "en",
                VmLimits::default(),
            )
            .unwrap()
            .with_media(Box::new(Media {
                transport_changes: changes.clone(),
                playback: RefCell::new(None),
            }))
            .unwrap();
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/reel-artifacts/save-media")
                .join(format!("{}-{cancel}", std::process::id()));
            let service = crate::saves::SaveService::new(
                Arc::new(crate::saves::DirectorySaves::new(path, 8 << 20)),
                QueueLimits {
                    max_requests: 1,
                    max_bytes: 8 << 20,
                },
                WakeSignal::new(|| {}),
            );
            let mut story =
                StoryRuntime::new(driver).with_saves(RuntimeSaves::new(service, 8 << 20));
            let mut captured = false;
            for _ in 0..10_000 {
                story.tick(SliceBudget::default(), || 0).unwrap();
                assert_eq!(changes.get(), 0);
                if story.capturing() {
                    captured = true;
                    if cancel {
                        story.interrupt_capture().unwrap();
                    }
                }
                if captured && !story.capturing() {
                    break;
                }
            }
            assert!(captured);
            assert!(!story.capturing());
            assert_eq!(changes.get(), 0);
        }
    }
}
