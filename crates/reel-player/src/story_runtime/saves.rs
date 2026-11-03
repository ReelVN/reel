use super::*;
use crate::saves::{Candidate, Capture, RuntimeSaves, StorageJob, StorageResult};
use reel_engine::aside::{
    AdapterError,
    saves::{SaveOperation, SaveResult},
    snapshot::{OwnedSessionRestore, SnapshotBudget, SnapshotError},
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
        let result = self.drive_saves(&mut saves, now_ns, budget);
        self.saves = Some(saves);
        result
    }
    fn drive_saves(
        &mut self,
        saves: &mut RuntimeSaves,
        now_ns: u64,
        budget: SliceBudget,
    ) -> Result<bool, Error> {
        let quota = SnapshotBudget { units: 256 };
        if !self.closing && self.starting.is_none() && self.capture.is_none() {
            let adapter = self.driver.as_mut().unwrap().adapter_mut();
            if adapter.save_results_pending() {
                adapter.maintain_save_results(quota.units)?;
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
        }
        let mut events = Vec::new();
        saves.service.drain(&mut events, 8);
        for event in events {
            if self.closing || self.starting.is_some() {
                continue;
            }
            self.clock_sample_requested = true;
            let result = match event.result {
                Ok(StorageResult::Loaded(snapshot)) => {
                    let driver = self.driver.as_ref().unwrap();
                    let mut identity = driver.ui().session().identity();
                    saves.next_epoch = saves.next_epoch.max(identity.epoch.0) + 1;
                    identity.epoch = reel_engine::Epoch(saves.next_epoch);
                    let restored = OwnedSessionRestore::new(
                        *snapshot,
                        driver.adapter().vm().program().clone(),
                        driver.adapter().vm().limits(),
                        identity,
                        driver.ui().session().read_progress().clone(),
                        self.media_factory.as_ref().map(|factory| factory()),
                        Some(driver.catalog_loader()),
                    )
                    .and_then(|restore| {
                        restore
                            .with_source_catalogs(driver.adapter().source_catalogs())
                            .and_then(|restore| {
                                restore.with_effect_products(driver.adapter().effect_products())
                            })
                    });
                    match restored {
                        Ok(restore) => {
                            saves.restore = Some(restore);
                            continue;
                        }
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
                if !restore.step(quota)?.complete {
                    return Ok(false);
                }
                let (session, mut vm) = saves.restore.take().unwrap().finish()?;
                let live = self.driver.as_ref().unwrap();
                vm.set_window_info(live.adapter().window_info().clone());
                saves.candidate = Some(Candidate::new(AsideSessionUi::restored(
                    session,
                    vm,
                    live.locale().into(),
                    live.ui().assets(),
                    live.initializer(),
                    live.catalogs(),
                )?)?);
            }
            if let Some(candidate) = &mut saves.candidate {
                if !candidate.driver.prepare_restored(budget)? {
                    return Ok(false);
                }
                candidate
                    .driver
                    .ui_mut()
                    .sync(&mut candidate.bridge, candidate.root)?;
                return Ok(!candidate.driver.ui().images_pending());
            }
            Ok(false)
        })();
        match prepared {
            Ok(true) => {
                let mut candidate = saves.candidate.take().unwrap().driver;
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
                    Err(AdapterError::Snapshot(SnapshotError::Invalid(
                        "snapshot exceeds storage budget".into(),
                    )))
                } else {
                    Ok(complete)
                }
            });
            match progress {
                Ok(false) => return Ok(true),
                Ok(true) => {
                    let retained_bytes = capture.bytes;
                    let (driver, snapshot) = self.capture.take().unwrap().finish()?;
                    self.driver = Some(driver);
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
                            snapshot: Box::new(snapshot),
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
        if matches!(request.operation, SaveOperation::Save { .. }) {
            if self.suspended_at.is_some()
                || self.collecting
                || self.driver().activities_pending()
                || !self.driver().adapter().stage_ready()
            {
                return Ok(false);
            }
            let driver = self.driver.take().unwrap();
            match Capture::new(driver) {
                Ok(capture) => {
                    self.capture = Some(capture);
                    self.capture_started = Some(now_ns);
                    return Ok(true);
                }
                Err((driver, error)) => {
                    self.driver = Some(driver);
                    if !matches!(error, AdapterError::Snapshot(SnapshotError::Busy(_))) {
                        self.driver
                            .as_mut()
                            .unwrap()
                            .adapter_mut()
                            .complete_save_request(
                                request.id,
                                SaveResult::Failed(error.to_string()),
                            )?;
                        saves.pending = None;
                        self.runnable = true;
                    }
                }
            }
        } else {
            let job = match &request.operation {
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
                SaveOperation::WriteData { key, data } => {
                    StorageJob::WriteData(key.clone(), data.clone())
                }
                SaveOperation::DeleteData { key } => StorageJob::DeleteData(key.clone()),
                _ => unreachable!(),
            };
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
                r#"fn main() {
                    let retained=[]; for i in 0..2000 { retained.append(i); }
                    let music=audio.play("track");
                    try { await saves.save("slot"); } catch error { }
                    if retained.len()!=2000 { throw "retained data"; }
                    narrator("saved");
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
