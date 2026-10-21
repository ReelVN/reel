//! Portable execution and session snapshots. Device adapters are supplied by the host.
use super::*;
pub use aside_vm::snapshot::{SnapshotBudget, SnapshotError, SnapshotProgress};
use aside_vm::snapshot::{SnapshotMap, SnapshotRegistry, VmCapture, VmRestore, VmSnapshot};
use reel_ui::frame::SavedRetainedScene;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

fn registry() -> &'static SnapshotRegistry {
    static REGISTRY: LazyLock<SnapshotRegistry> = LazyLock::new(|| {
        let mut registry = SnapshotRegistry::default();
        aside_std::register_snapshots(&mut registry).unwrap();
        stage::StageBinding::register_snapshots(&mut registry).unwrap();
        for (name, validator) in [
            (
                "reel.character",
                dialogue::immutable as aside_vm::HostPropertyValidator,
            ),
            ("reel.activity", activity::readonly),
            ("reel.result", results::readonly),
            ("reel.media", media::readonly),
        ] {
            registry
                .register_validator(
                    name,
                    if name == "reel.media" {
                        3
                    } else if matches!(name, "reel.character" | "reel.result") {
                        2
                    } else {
                        1
                    },
                    validator,
                )
                .unwrap();
        }
        registry
    });
    &REGISTRY
}

#[derive(Serialize, Deserialize)]
struct Header {
    owner: OwnerHandle,
    root_owner: RootOwner,
    task: Option<TaskId>,
    derived_task: Option<TaskId>,
    reactive_ready: bool,
    main_wait: Option<WaitId>,
    schedule_cursor: usize,
    claimed: Option<Completion>,
    delivery: Option<DeliveryId>,
    delivery_task: Option<TaskId>,
    result_root: Option<RootToken>,
    game: std::sync::Arc<GameSettings>,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
pub(super) enum Part {
    Header,
    Task,
    Scopes,
    Scope,
    Pending,
    ChoiceKey,
    Narrative,
    NarrativeOption,
    Activities,
    ActivityEvent,
    ActivityWait,
    Results,
    ResultWait,
    Presentation,
    OwnedResult,
    Particles,
    Particle,
    Media,
    MediaWait,
    MediaCue,
    VideoPoster,
    Translations,
    Catalog,
    StageRoot,
    Node,
    Saves,
    SaveRequest,
    Transitions,
    Transition,
}
#[derive(Serialize, Deserialize)]
pub(super) struct Record {
    pub part: Part,
    bytes: Vec<u8>,
}
impl Record {
    pub fn new(part: Part, value: &impl Serialize) -> Result<Self, AdapterError> {
        Ok(Self {
            part,
            bytes: postcard::to_allocvec(value).map_err(SnapshotError::from)?,
        })
    }
    pub fn read<T: for<'a> Deserialize<'a>>(&self) -> Result<T, SnapshotError> {
        let (value, tail) = postcard::take_from_bytes(&self.bytes)?;
        if !tail.is_empty() {
            return Err(SnapshotError::Invalid("trailing session record".into()));
        }
        Ok(value)
    }
}

/// Owns detached records and immutable text, and can be encoded on a storage worker.
#[derive(Serialize, Deserialize)]
pub struct SessionSnapshot {
    version: u32,
    vm: VmSnapshot,
    session: Option<Session>,
    records: Vec<Record>,
    committed_save: Option<saves::SaveRequestId>,
    // Large immutable picture resources are encoded directly by the storage
    // worker, rather than copied into a synchronous adapter Record::bytes.
    pictures: Vec<Option<std::sync::Arc<SavedRetainedScene>>>,
}
impl SessionSnapshot {
    pub const FORMAT_VERSION: u32 = 29;
    /// Serialize directly into the host's compression/file writer on its worker.
    pub fn write_to(
        &self,
        mut writer: impl std::io::Write,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        writer.write_all(b"REELSAV1")?;
        postcard::to_io(self, writer)?;
        Ok(())
    }
    /// The storage service marks only the request whose snapshot it will commit.
    /// Dropping this image without publishing it leaves the live request untouched.
    pub fn commit_save(&mut self, request: saves::SaveRequestId) -> Result<(), SnapshotError> {
        if self
            .session
            .as_ref()
            .is_none_or(|session| session.identity() != request.session)
        {
            return Err(SnapshotError::Invalid("foreign save request".into()));
        }
        self.committed_save = Some(request);
        Ok(())
    }
    pub fn encode(&self) -> Result<Vec<u8>, SnapshotError> {
        Ok(postcard::to_extend(self, b"REELSAV1".to_vec())?)
    }
    /// Parsing and text reconstruction belong on the host's resource/storage worker.
    pub fn decode(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let bytes = bytes
            .strip_prefix(b"REELSAV1")
            .ok_or_else(|| SnapshotError::Invalid("save format".into()))?;
        // Read the fixed version prefix before decoding the nested VM. Postcard
        // does not retain custom serde error messages from that deserializer.
        let (version, vm_bytes): (u32, _) = postcard::take_from_bytes(bytes)?;
        if version != Self::FORMAT_VERSION {
            return Err(SnapshotError::Version {
                format: "session",
                expected: Self::FORMAT_VERSION,
                actual: version,
            });
        }
        let (vm_version, _): (u32, _) = postcard::take_from_bytes(vm_bytes)?;
        if vm_version != VmSnapshot::FORMAT_VERSION {
            return Err(SnapshotError::Version {
                format: "VM",
                expected: VmSnapshot::FORMAT_VERSION,
                actual: vm_version,
            });
        }
        let (snapshot, tail): (Self, _) = postcard::take_from_bytes(bytes)?;
        if snapshot.session.is_none() || !tail.is_empty() {
            return Err(SnapshotError::Invalid("session snapshot body".into()));
        }
        Ok(snapshot)
    }
}

pub(super) type RecordStream<'a> = Box<dyn Iterator<Item = Result<Record, AdapterError>> + 'a>;

pub struct SessionCapture<'a> {
    vm: VmCapture<'a>,
    session: Option<crate::snapshot::SessionCapture<'a>>,
    saved_session: Option<Session>,
    records: Vec<Record>,
    adapters: RecordStream<'a>,
    pictures: Vec<Option<std::sync::Arc<SavedRetainedScene>>>,
    picture_sizes: Option<Box<dyn Iterator<Item = u64> + 'a>>,
    vm_complete: bool,
    complete: bool,
}
impl SessionVm {
    fn snapshot_records(&self) -> RecordStream<'_> {
        use std::iter::once_with;
        let streams: [RecordStream<'_>; 12] = [
            Box::new(
                once_with(|| {
                    Record::new(
                        Part::Header,
                        &Header {
                            owner: self.owner,
                            root_owner: self.root_owner,
                            task: self.task,
                            derived_task: self.derived_task,
                            reactive_ready: self.reactive_ready,
                            main_wait: self.main_wait,
                            schedule_cursor: self.schedule_cursor,
                            claimed: self.claimed.clone(),
                            delivery: self.delivery,
                            delivery_task: self.delivery_task,
                            result_root: self.result_root,
                            game: self.game.clone(),
                        },
                    )
                })
                .chain(self.tasks.iter().map(|task| Record::new(Part::Task, task)))
                .chain(once_with(|| {
                    Record::new(Part::Scopes, &self.scopes.methods)
                }))
                .chain(
                    self.scopes
                        .live
                        .iter()
                        .map(|scope| Record::new(Part::Scope, scope)),
                ),
            ),
            Box::new(
                once_with(|| Record::new(Part::Pending, &self.pending)).chain(
                    self.pending
                        .iter()
                        .flat_map(|p| p.keys.iter())
                        .flat_map(|keys| keys.entries.iter())
                        .map(|key| Record::new(Part::ChoiceKey, key)),
                ),
            ),
            Box::new(
                once_with(|| Record::new(Part::Narrative, &self.narrative))
                    .chain(self.narrative.iter().flat_map(|n| n.snapshot_options())),
            ),
            self.activities.snapshot_records(),
            self.results.snapshot_records(),
            self.saves.snapshot_records(),
            self.media.snapshot_records(),
            self.translations.snapshot_records(),
            Box::new(once_with(|| {
                Record::new(Part::StageRoot, &self.stage.snapshot_root())
            })),
            Box::new(
                self.stage
                    .snapshot_nodes()
                    .map(|node| Record::new(Part::Node, &node)),
            ),
            self.particles.snapshot_records(),
            self.transition_snapshot_records(),
        ];
        Box::new(streams.into_iter().flatten())
    }
    /// The host freezes the logical clock and media transport while this borrow lives.
    /// Drop the capture to cancel it; no state in the live session has changed.
    pub fn snapshot_begin<'a>(
        &'a self,
        session: &'a Session,
    ) -> Result<SessionCapture<'a>, AdapterError> {
        self.live(session)?;
        if self.issued.is_some()
            || self.choice_reader.is_some()
            || self.dialogue_reader.is_some()
            || self.activities_pending()
            || self.particles.needs_work(self.stage.retirement_revision)
            || !self.activities.snapshot_ready()
            || self.exit_requested
            || self.start_requested.is_some()
            || self.save_results_pending()
            || self.captures.active()
            || self.window.active()
            || self.transitions_pending()
            || !self.transitions.snapshot_ready()
        {
            return Err(SnapshotError::Busy(
                "finish native admission and stage maintenance before capture",
            )
            .into());
        }
        Ok(SessionCapture {
            vm: self.vm.snapshot_begin(registry())?,
            session: Some(session.snapshot_begin()),
            saved_session: None,
            records: Vec::new(),
            adapters: self.snapshot_records(),
            pictures: self.transition_pictures(),
            picture_sizes: Some(Box::new(self.transition_picture_sizes())),
            vm_complete: false,
            complete: false,
        })
    }

    /// Builds an isolated candidate. The host installs its resources and publishes it
    /// only after localization, reactive projection and presentation are ready.
    pub fn restore_begin<'a>(
        image: &'a mut SessionSnapshot,
        program: VerifiedProgram,
        limits: VmLimits,
        identity: SessionIdentity,
        progress: crate::ReadProgress,
        media: Option<Box<dyn crate::media::MediaBackend>>,
        catalogs: Option<Box<dyn crate::localization::CatalogLoader>>,
    ) -> Result<SessionRestore<'a>, AdapterError> {
        let session = image
            .session
            .take()
            .ok_or(AdapterError::Input("snapshot already consumed"))?;
        Self::restore_session(
            image, session, program, limits, identity, progress, media, catalogs,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn restore_session<'a>(
        image: &'a SessionSnapshot,
        mut session: Session,
        program: VerifiedProgram,
        limits: VmLimits,
        identity: SessionIdentity,
        progress: crate::ReadProgress,
        media: Option<Box<dyn crate::media::MediaBackend>>,
        catalogs: Option<Box<dyn crate::localization::CatalogLoader>>,
    ) -> Result<SessionRestore<'a>, AdapterError> {
        let support = aside_host::runtime_support(
            &program,
            &registry::contracts(),
            &registry::capabilities(),
        )?;
        let vm = Vm::restore_begin(
            &image.vm,
            program,
            support,
            limits,
            SessionEpoch(identity.epoch.0),
            registry(),
        )?;
        session.restore_identity(identity, progress)?;
        Ok(SessionRestore {
            committed_save: image.committed_save.map(|id| id.serial),
            vm: Some(vm),
            map: None,
            candidate: None,
            session: Some(session),
            records: &image.records,
            pictures: &image.pictures,
            cursor: 0,
            children: Vec::new(),
            child: 0,
            child_index: 0,
            stage_root: None,
            media,
            catalogs,
            limits,
            complete: false,
        })
    }
}

type RestoreCursor<'a> = Option<SessionRestore<'a>>;
self_cell::self_cell! {
    struct RestoreCell {
        owner: SessionSnapshot,
        #[not_covariant]
        dependent: RestoreCursor,
    }
}
/// Movable restoration state for event loops; each step uses the same work budget.
pub struct OwnedSessionRestore(RestoreCell);
impl OwnedSessionRestore {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mut image: SessionSnapshot,
        program: VerifiedProgram,
        limits: VmLimits,
        identity: SessionIdentity,
        progress: crate::ReadProgress,
        media: Option<Box<dyn crate::media::MediaBackend>>,
        catalogs: Option<Box<dyn crate::localization::CatalogLoader>>,
    ) -> Result<Self, AdapterError> {
        let session = image
            .session
            .take()
            .ok_or(AdapterError::Input("snapshot already consumed"))?;
        Ok(Self(RestoreCell::try_new(image, |image| {
            SessionVm::restore_session(
                image, session, program, limits, identity, progress, media, catalogs,
            )
            .map(Some)
        })?))
    }
    pub fn step(&mut self, budget: SnapshotBudget) -> Result<SnapshotProgress, AdapterError> {
        self.0
            .with_dependent_mut(|_, cursor| cursor.as_mut().unwrap().step(budget))
    }
    pub fn finish(mut self) -> Result<(Session, SessionVm), AdapterError> {
        self.0
            .with_dependent_mut(|_, cursor| cursor.take().unwrap().finish())
    }
}
impl SessionCapture<'_> {
    pub fn step(&mut self, budget: SnapshotBudget) -> Result<SnapshotProgress, AdapterError> {
        let mut progress = SnapshotProgress::default();
        while !self.complete && progress.units < budget.units {
            progress.units += 1;
            if let Some(sizes) = &mut self.picture_sizes {
                if let Some(bytes) = sizes.next() {
                    progress.bytes += bytes;
                } else {
                    self.picture_sizes = None;
                }
                continue;
            }
            if let Some(session) = &mut self.session {
                if session.step() {
                    self.saved_session = Some(self.session.take().unwrap().finish());
                }
                continue;
            }
            if !self.vm_complete {
                let p = self.vm.step(SnapshotBudget { units: 1 })?;
                progress.bytes += p.bytes;
                self.vm_complete = p.complete;
                continue;
            }
            if let Some(record) = self.adapters.next() {
                let record = record?;
                progress.bytes += record.bytes.len() as u64;
                self.records.push(record);
            } else {
                self.complete = true;
            }
        }
        progress.complete = self.complete;
        Ok(progress)
    }
    pub fn finish(self) -> Result<SessionSnapshot, AdapterError> {
        if !self.complete {
            return Err(SnapshotError::Busy("session capture incomplete").into());
        }
        Ok(SessionSnapshot {
            committed_save: None,
            version: SessionSnapshot::FORMAT_VERSION,
            vm: self.vm.finish()?,
            session: self.saved_session,
            records: self.records,
            pictures: self.pictures,
        })
    }
}

pub struct SessionRestore<'a> {
    committed_save: Option<u64>,
    vm: Option<VmRestore<'a>>,
    map: Option<SnapshotMap>,
    candidate: Option<SessionVm>,
    session: Option<Session>,
    records: &'a [Record],
    pictures: &'a [Option<std::sync::Arc<SavedRetainedScene>>],
    cursor: usize,
    children: Vec<reel_ui::stage::NodeId>,
    child: usize,
    child_index: u32,
    stage_root: Option<RootToken>,
    media: Option<Box<dyn crate::media::MediaBackend>>,
    catalogs: Option<Box<dyn crate::localization::CatalogLoader>>,
    limits: VmLimits,
    complete: bool,
}
impl SessionRestore<'_> {
    pub fn step(&mut self, budget: SnapshotBudget) -> Result<SnapshotProgress, AdapterError> {
        let mut progress = SnapshotProgress::default();
        while !self.complete && progress.units < budget.units {
            progress.units += 1;
            if let Some(vm) = &mut self.vm {
                let p = vm.step(SnapshotBudget { units: 1 })?;
                progress.bytes += p.bytes;
                if p.complete {
                    let (vm, map) = self.vm.take().unwrap().finish()?;
                    self.candidate = Some(SessionVm::from_vm(
                        self.session.as_ref().unwrap(),
                        vm,
                        self.limits,
                    )?);
                    let runtime = self.candidate.as_mut().unwrap();
                    if let Some(audio) = self.media.take() {
                        runtime.set_media(audio)?;
                        runtime.suspend_media();
                    }
                    if let Some(loader) = self.catalogs.take() {
                        runtime.set_catalog_loader(loader)?;
                    }
                    self.map = Some(map);
                }
                continue;
            }
            let map = self.map.as_ref().unwrap();
            let runtime = self.candidate.as_mut().unwrap();
            if let Some(record) = self.records.get(self.cursor) {
                match record.part {
                    Part::Transitions | Part::Transition => {
                        runtime.restore_transition_record(record, map, self.pictures)?;
                    }
                    Part::Saves | Part::SaveRequest => {
                        runtime.restore_save_record(record, map, self.committed_save)?;
                    }
                    Part::Header => {
                        let mut header: Header = record.read()?;
                        header.owner.rebase(runtime.identity);
                        runtime.owner = header.owner;
                        runtime.root_owner = header.root_owner;
                        runtime.task = header.task.map(|t| map.task(t)).transpose()?;
                        runtime.derived_task =
                            header.derived_task.map(|t| map.task(t)).transpose()?;
                        runtime.reactive_ready = header.reactive_ready;
                        runtime.main_wait = header.main_wait.map(|w| map.wait(w)).transpose()?;
                        runtime.schedule_cursor = header.schedule_cursor;
                        if let Some(c) = &mut header.claimed {
                            c.rebase(runtime.identity);
                        }
                        runtime.claimed = header.claimed;
                        runtime.delivery = header.delivery.map(|d| map.delivery(d)).transpose()?;
                        runtime.delivery_task =
                            header.delivery_task.map(|t| map.task(t)).transpose()?;
                        runtime.result_root =
                            header.result_root.map(|r| map.root(r)).transpose()?;
                        runtime.game = header.game;
                    }
                    Part::Task => {
                        let mut task: callbacks::ScriptTask = record.read()?;
                        task.task = map.task(task.task)?;
                        task.owner.rebase(runtime.identity);
                        task.result = task.result.map(|r| map.root(r)).transpose()?;
                        task.wait = task.wait.map(|w| map.wait(w)).transpose()?;
                        runtime.tasks.push(task);
                    }
                    Part::Scopes => runtime.scopes.methods = record.read()?,
                    Part::Scope => {
                        runtime
                            .scopes
                            .restore(record.read()?, map, runtime.vm.heap())?
                    }
                    Part::Pending => {
                        let mut pending: Option<PendingInteraction> = record.read()?;
                        if let Some(p) = &mut pending {
                            p.interaction.rebase(runtime.identity);
                            p.task = map.task(p.task)?;
                            p.wait = map.wait(p.wait)?;
                            if let Some(keys) = &mut p.keys {
                                keys.root = map.root(keys.root)?;
                            }
                        }
                        runtime.pending = pending;
                    }
                    Part::ChoiceKey => {
                        let mut key: RetainedKey = record.read()?;
                        key.value = map.value(key.value)?;
                        runtime
                            .pending
                            .as_mut()
                            .and_then(|p| p.keys.as_mut())
                            .ok_or(AdapterError::Input("choice key without interaction"))?
                            .entries
                            .push(key);
                    }
                    Part::Narrative => {
                        runtime.narrative = record.read()?;
                        if let Some(n) = &mut runtime.narrative {
                            n.restore(map, runtime.identity)?;
                        }
                    }
                    Part::NarrativeOption => {
                        runtime
                            .narrative
                            .as_mut()
                            .ok_or(AdapterError::Input("choice identity without narrative"))?
                            .restore_option(record, map, runtime.identity)?;
                    }
                    Part::Activities | Part::ActivityEvent | Part::ActivityWait => {
                        runtime.activities.restore_record(record, map)?;
                    }
                    Part::Results | Part::ResultWait | Part::Presentation | Part::OwnedResult => {
                        runtime
                            .results
                            .restore_record(record, map, runtime.vm.heap_mut())?;
                    }
                    Part::Particles | Part::Particle => {
                        runtime.particles.restore_record(
                            record,
                            map,
                            runtime.vm.heap(),
                            &mut runtime.stage.scene,
                            runtime.stage.retirement_revision,
                        )?;
                    }
                    Part::Media | Part::MediaWait | Part::MediaCue | Part::VideoPoster => {
                        runtime
                            .media
                            .restore_record(record, map, runtime.vm.heap_mut())?;
                    }
                    Part::Translations | Part::Catalog => {
                        runtime.translations.restore_record(record, map)?;
                    }
                    Part::StageRoot => {
                        self.stage_root = record
                            .read::<Option<RootToken>>()?
                            .map(|r| map.root(r))
                            .transpose()?;
                    }
                    Part::Node => {
                        if let Some(node) = runtime.stage.restore_node(
                            runtime.vm.heap_mut(),
                            map,
                            record.read()?,
                            runtime.read_limit,
                        )? {
                            runtime.restore_virtual_presentation(node)?;
                            self.children.push(node);
                        }
                    }
                }
                progress.bytes += record.bytes.len() as u64;
                self.cursor += 1;
                continue;
            }
            if let Some(&node) = self.children.get(self.child) {
                if runtime
                    .stage
                    .restore_child(runtime.vm.heap(), node, self.child_index)?
                {
                    self.child_index += 1;
                } else {
                    self.child += 1;
                    self.child_index = 0;
                }
                continue;
            }
            runtime
                .stage
                .restore_root(runtime.vm.heap(), self.stage_root)?;
            if runtime.reactive_ready && runtime.derived_task.is_none() {
                runtime.sync_narrative(self.session.as_ref().unwrap())?;
            }
            self.complete = true;
        }
        progress.complete = self.complete;
        Ok(progress)
    }
    pub fn finish(self) -> Result<(Session, SessionVm), AdapterError> {
        if !self.complete {
            return Err(SnapshotError::Busy("session restoration incomplete").into());
        }
        Ok((self.session.unwrap(), self.candidate.unwrap()))
    }
}
