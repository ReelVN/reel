//! Development checkpoints reuse the production session snapshot and restore path.

use std::io::{Read, Write};

use reel_engine::{
    ReadProgress,
    aside::snapshot::{OwnedSessionRestore, SessionSnapshot, SnapshotBudget},
};

use super::{Error, SliceBudget, StoryRuntime};
use crate::saves::Candidate;

/// Detached development checkpoint, including branch-local reading progress.
///
/// Serialize this value to temporary storage and release it before exploring a
/// successor. Restoring shares the runtime's verified program and rebuilds one
/// isolated candidate through the production session restoration implementation.
pub struct RuntimeCheckpoint {
    image: SessionSnapshot,
    read_ids: Vec<String>,
    logical_ns: u64,
}

impl RuntimeCheckpoint {
    /// Writes the v1 development envelope and production session snapshot.
    pub fn write_to(&self, mut writer: impl Write) -> Result<(), Error> {
        writer.write_all(b"REELCHK1")?;
        let metadata = postcard::to_allocvec(&(self.logical_ns, &self.read_ids))?;
        writer.write_all(&(metadata.len() as u64).to_le_bytes())?;
        writer.write_all(&metadata)?;
        self.image
            .write_to(writer)
            .map_err(|error| -> Error { error })
    }

    /// Reads one development checkpoint from host-controlled temporary storage.
    pub fn read_from(mut reader: impl Read) -> Result<Self, Error> {
        let mut magic = [0; 8];
        reader.read_exact(&mut magic)?;
        if &magic != b"REELCHK1" {
            return Err("unsupported runtime checkpoint format".into());
        }
        let mut length = [0; 8];
        reader.read_exact(&mut length)?;
        let length = u64::from_le_bytes(length);
        let mut metadata = Vec::new();
        reader.by_ref().take(length).read_to_end(&mut metadata)?;
        if metadata.len() as u64 != length {
            return Err("truncated runtime checkpoint metadata".into());
        }
        let (logical_ns, read_ids) = postcard::from_bytes(&metadata)?;
        let mut image = Vec::new();
        reader.read_to_end(&mut image)?;
        Ok(Self {
            image: SessionSnapshot::decode(&image)?,
            read_ids,
            logical_ns,
        })
    }
}

impl StoryRuntime {
    /// Drains already submitted save work before branch storage is replaced.
    /// Script execution and story time remain fixed while background IO settles.
    pub fn settle_checkpoint_storage(
        &mut self,
        host_now_ns: u64,
        mut wait_for_io: impl FnMut(),
    ) -> Result<(), Error> {
        while self.saves.as_ref().is_some_and(|saves| saves.busy()) {
            self.maintain_saves(host_now_ns, SliceBudget::default())?;
            if self
                .saves
                .as_ref()
                .is_some_and(|saves| !saves.service.is_idle())
            {
                wait_for_io();
            }
        }
        Ok(())
    }

    /// Whether a settled input boundary can be captured without pending storage.
    pub fn checkpoint_ready(&self) -> bool {
        self.capture.is_none()
            && self.starting.is_none()
            && self.retiring.is_none()
            && !self.closing
            && !self.collecting
            && self.suspended_at.is_none()
            && self.saves.as_ref().is_none_or(|saves| !saves.busy())
            && !self.driver().adapter().has_save_requests()
            && !self.driver().adapter().save_results_pending()
            && !self.driver().has_runnable_tasks()
            && !self.driver().activities_pending()
            && self.driver().adapter().stage_ready()
    }

    /// Captures a settled input boundary with the production snapshot walker.
    ///
    /// This synchronous development operation freezes media while copying and
    /// advances the existing bounded snapshot steps without executing script.
    pub fn checkpoint(&mut self) -> Result<RuntimeCheckpoint, Error> {
        if !self.checkpoint_ready() {
            return Err(
                "runtime checkpoint requires a settled input boundary and idle storage".into(),
            );
        }
        self.driver.as_mut().unwrap().suspend_media();
        let captured = (|| {
            let mut capture = self.driver().snapshot_begin()?;
            while !capture.step(SnapshotBudget { units: 256 })?.complete {}
            let image = capture.finish()?;
            let mut read_ids = self
                .driver()
                .ui()
                .session()
                .read_progress()
                .ids()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            read_ids.sort();
            Ok(RuntimeCheckpoint {
                image,
                read_ids,
                logical_ns: self.logical_ns,
            })
        })();
        self.driver.as_mut().unwrap().resume_media();
        captured
    }

    /// Restores a development branch while sharing the existing verified code.
    ///
    /// The host must first restore this branch's isolated save-storage files.
    /// Candidate preparation completes before replacing the live session. The
    /// host clock stays monotonic; its offset maps to captured story time.
    /// Interaction observables retain their saved values. The host restores
    /// matching pointer and focus state before resuming input or story work.
    pub fn restore_checkpoint(
        &mut self,
        checkpoint: RuntimeCheckpoint,
        host_now_ns: u64,
        mut wait_for_io: impl FnMut(),
    ) -> Result<(), Error> {
        if self.capture.is_some()
            || self.retiring.is_some()
            || self.saves.as_ref().is_some_and(|saves| saves.busy())
        {
            return Err("runtime checkpoint restore requires idle capture and storage".into());
        }
        let RuntimeCheckpoint {
            image,
            read_ids,
            logical_ns,
        } = checkpoint;
        let live = self.driver();
        let mut identity = live.ui().session().identity();
        let previous_epoch = self.saves.as_ref().map_or(identity.epoch.0, |saves| {
            saves.next_epoch.max(identity.epoch.0)
        });
        identity.epoch.0 = previous_epoch
            .checked_add(1)
            .ok_or("session epoch exhausted")?;
        let mut restore = OwnedSessionRestore::new(
            image,
            live.adapter().vm().program().clone(),
            live.adapter().vm().limits(),
            identity,
            ReadProgress::from_ids(read_ids)?,
            self.media_factory.as_ref().map(|factory| factory()),
            Some(live.catalog_loader()),
        )?
        .with_source_catalogs(live.adapter().source_catalogs())?
        .with_preserved_interactions()?;
        while !restore.step(SnapshotBudget { units: 256 })?.complete {}
        let (session, mut vm) = restore.finish()?;
        vm.set_host_environment(live.adapter().host_environment());
        vm.set_window_info(live.adapter().window_info().clone());
        let mut candidate = Candidate::new(crate::aside_session::AsideSessionUi::restored(
            session,
            vm,
            live.locale().into(),
            live.ui().assets(),
            live.initializer(),
            live.catalogs(),
        )?)?;
        loop {
            if candidate.driver.prepare_restored(SliceBudget::default())? {
                candidate
                    .driver
                    .ui_mut()
                    .sync(&mut candidate.bridge, candidate.root)?;
                if !candidate.driver.ui().images_pending() {
                    break;
                }
            }
            if candidate.driver.ui().assets().pending_io() {
                wait_for_io();
            }
        }
        let mut old = self.driver.take().unwrap();
        if let Err(error) = old.cancel() {
            self.driver = Some(old);
            return Err(error);
        }
        while old.cleanup_pending() {
            if let Err(error) = old.cleanup_step(256, SliceBudget::default(), || host_now_ns) {
                self.driver = Some(old);
                return Err(error);
            }
        }
        candidate.driver.ui_mut().adopt_presentation();
        candidate.driver.resume_media();
        self.driver = Some(candidate.driver);
        self.starting = None;
        self.capture_started = None;
        self.logical_ns = logical_ns;
        self.time_offset_ns = logical_ns;
        self.suspended_ns = host_now_ns;
        self.suspended_at = None;
        self.runnable = true;
        self.collecting = false;
        self.gc_turn = false;
        self.retry_after_collection = false;
        self.retried_collection = false;
        self.resource_blocked = None;
        self.closing = false;
        if let Some(saves) = &mut self.saves {
            saves.next_epoch = identity.epoch.0;
        }
        Ok(())
    }
}
