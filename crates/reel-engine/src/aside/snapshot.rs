//! Portable execution and session snapshots. Device adapters are supplied by the host.
use super::*;
use aside_vm::snapshot::{
    SnapshotBudget, SnapshotError, SnapshotMap, SnapshotProgress, SnapshotRegistry, VmCapture,
    VmRestore, VmSnapshot,
};
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
            ("reel.audio", audio::readonly),
        ] {
            registry.register_validator(name, 1, validator).unwrap();
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
    main_wait: Option<WaitId>,
    schedule_cursor: usize,
    claimed: Option<Completion>,
    delivery: Option<DeliveryId>,
    delivery_task: Option<TaskId>,
    result_root: Option<RootToken>,
    game: std::sync::Arc<GameSettings>,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
enum Part {
    Header,
    Task,
    Pending,
    Narrative,
    Activities,
    Results,
    Particles,
    Audio,
    Translations,
    StageRoot,
    Node,
}
#[derive(Serialize, Deserialize)]
struct Record {
    part: Part,
    bytes: Vec<u8>,
}
impl Record {
    fn new(part: Part, value: &impl Serialize) -> Result<Self, SnapshotError> {
        Ok(Self {
            part,
            bytes: postcard::to_allocvec(value)?,
        })
    }
    fn read<T: for<'a> Deserialize<'a>>(&self) -> Result<T, SnapshotError> {
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
}
impl SessionSnapshot {
    pub fn encode(&self) -> Result<Vec<u8>, SnapshotError> {
        Ok(postcard::to_extend(self, b"REELSAV1".to_vec())?)
    }
    /// Parsing and text reconstruction belong on the host's resource/storage worker.
    pub fn decode(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let bytes = bytes
            .strip_prefix(b"REELSAV1")
            .ok_or_else(|| SnapshotError::Invalid("save format".into()))?;
        let (snapshot, tail): (Self, _) = postcard::take_from_bytes(bytes)?;
        if snapshot.version != 1 || snapshot.session.is_none() || !tail.is_empty() {
            return Err(SnapshotError::Invalid(
                "session snapshot version/body".into(),
            ));
        }
        Ok(snapshot)
    }
}

pub struct SessionCapture<'a> {
    source: &'a SessionVm,
    vm: VmCapture<'a>,
    session: Option<crate::snapshot::SessionCapture<'a>>,
    saved_session: Option<Session>,
    records: Vec<Record>,
    nodes: Box<dyn Iterator<Item = stage::snapshot::SavedNode> + 'a>,
    task: usize,
    part: u8,
    complete: bool,
}
impl SessionVm {
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
            || !self.stage_ready()
            || !self.activities.snapshot_ready()
            || self.exit_requested
        {
            return Err(SnapshotError::Busy(
                "finish native admission and stage maintenance before capture",
            )
            .into());
        }
        Ok(SessionCapture {
            source: self,
            vm: self.vm.snapshot_begin(registry())?,
            session: Some(session.snapshot_begin()),
            saved_session: None,
            records: Vec::new(),
            nodes: Box::new(self.stage.snapshot_nodes()),
            task: 0,
            part: 0,
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
        audio: Option<Box<dyn crate::audio::AudioBackend>>,
        catalogs: Option<Box<dyn crate::localization::CatalogLoader>>,
    ) -> Result<SessionRestore<'a>, AdapterError> {
        let support = aside_host::runtime_support(
            program.package(),
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
        let mut session = image
            .session
            .take()
            .ok_or(AdapterError::Input("snapshot already consumed"))?;
        session.restore_identity(identity, progress)?;
        Ok(SessionRestore {
            vm: Some(vm),
            map: None,
            candidate: None,
            session: Some(session),
            records: &image.records,
            cursor: 0,
            children: Vec::new(),
            child: 0,
            stage_root: None,
            audio,
            catalogs,
            limits,
            complete: false,
        })
    }
}
impl SessionCapture<'_> {
    pub fn step(&mut self, budget: SnapshotBudget) -> Result<SnapshotProgress, AdapterError> {
        let mut progress = SnapshotProgress::default();
        while !self.complete && progress.units < budget.units {
            progress.units += 1;
            if let Some(session) = &mut self.session {
                if session.step() {
                    self.saved_session = Some(self.session.take().unwrap().finish());
                }
                continue;
            }
            if self.part == 0 {
                let p = self.vm.step(SnapshotBudget { units: 1 })?;
                progress.bytes += p.bytes;
                if p.complete {
                    self.part = 1;
                }
                continue;
            }
            let source = self.source;
            let record = match self.part {
                1 => Record::new(
                    Part::Header,
                    &Header {
                        owner: source.owner,
                        root_owner: source.root_owner,
                        task: source.task,
                        main_wait: source.main_wait,
                        schedule_cursor: source.schedule_cursor,
                        claimed: source.claimed.clone(),
                        delivery: source.delivery,
                        delivery_task: source.delivery_task,
                        result_root: source.result_root,
                        game: source.game.clone(),
                    },
                )?,
                2 => {
                    if let Some(task) = source.tasks.get(self.task) {
                        self.task += 1;
                        Record::new(Part::Task, task)?
                    } else {
                        self.part += 1;
                        continue;
                    }
                }
                3 => Record::new(Part::Pending, &source.pending)?,
                4 => Record::new(Part::Narrative, &source.narrative)?,
                5 => Record::new(Part::Activities, &source.activities)?,
                6 => Record::new(Part::Results, &source.results)?,
                7 => Record::new(Part::Particles, &source.particles)?,
                8 => Record::new(Part::Audio, &source.audio.snapshot()?)?,
                9 => Record::new(Part::Translations, &source.translations.snapshot())?,
                10 => Record::new(Part::StageRoot, &source.stage.snapshot_root())?,
                11 => {
                    if let Some(node) = self.nodes.next() {
                        Record::new(Part::Node, &node)?
                    } else {
                        self.complete = true;
                        continue;
                    }
                }
                _ => unreachable!(),
            };
            progress.bytes += record.bytes.len() as u64;
            self.records.push(record);
            if self.part != 2 && self.part != 11 {
                self.part += 1;
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
            version: 1,
            vm: self.vm.finish()?,
            session: self.saved_session,
            records: self.records,
        })
    }
}

pub struct SessionRestore<'a> {
    vm: Option<VmRestore<'a>>,
    map: Option<SnapshotMap>,
    candidate: Option<SessionVm>,
    session: Option<Session>,
    records: &'a [Record],
    cursor: usize,
    children: Vec<reel_ui::stage::NodeId>,
    child: usize,
    stage_root: Option<RootToken>,
    audio: Option<Box<dyn crate::audio::AudioBackend>>,
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
                    self.map = Some(map);
                }
                continue;
            }
            let map = self.map.as_ref().unwrap();
            let runtime = self.candidate.as_mut().unwrap();
            if let Some(record) = self.records.get(self.cursor) {
                match record.part {
                    Part::Header => {
                        let mut header: Header = record.read()?;
                        header.owner.rebase(runtime.identity);
                        runtime.owner = header.owner;
                        runtime.root_owner = header.root_owner;
                        runtime.task = header.task.map(|t| map.task(t)).transpose()?;
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
                    Part::Pending => {
                        let mut pending: Option<PendingInteraction> = record.read()?;
                        if let Some(p) = &mut pending {
                            p.interaction.rebase(runtime.identity);
                            p.task = map.task(p.task)?;
                            p.wait = map.wait(p.wait)?;
                            if let Some(keys) = &mut p.keys {
                                keys.root = map.root(keys.root)?;
                                for key in &mut keys.entries {
                                    key.value = map.value(key.value)?;
                                }
                            }
                        }
                        runtime.pending = pending;
                    }
                    Part::Narrative => {
                        runtime.narrative = record.read()?;
                        if let Some(n) = &mut runtime.narrative {
                            n.restore(map, runtime.identity)?;
                        }
                    }
                    Part::Activities => {
                        runtime.activities = record.read()?;
                        runtime.activities.restore(map)?;
                    }
                    Part::Results => {
                        runtime.results = record.read()?;
                        runtime.results.restore(runtime.vm.heap(), map)?;
                    }
                    Part::Particles => {
                        runtime.particles = record.read()?;
                    }
                    Part::Audio => {
                        runtime.audio = audio::Audio::restore(
                            record.read()?,
                            self.audio.take(),
                            map,
                            runtime.vm.heap_mut(),
                        )?;
                    }
                    Part::Translations => {
                        runtime.translations = localization::Translations::restore(
                            record.read()?,
                            self.catalogs.take(),
                            map,
                        )?;
                    }
                    Part::StageRoot => {
                        self.stage_root = record
                            .read::<Option<RootToken>>()?
                            .map(|r| map.root(r))
                            .transpose()?;
                    }
                    Part::Node => {
                        runtime.stage.restore_node(
                            runtime.vm.heap_mut(),
                            map,
                            record.read()?,
                            runtime.read_limit,
                        )?;
                    }
                }
                progress.bytes += record.bytes.len() as u64;
                self.cursor += 1;
                continue;
            }
            if self.children.is_empty() {
                self.children = runtime.stage.scene.nodes().map(|node| node.id).collect();
            }
            if let Some(&node) = self.children.get(self.child) {
                runtime.stage.restore_children(runtime.vm.heap(), node)?;
                self.child += 1;
                continue;
            }
            runtime
                .stage
                .restore_root(runtime.vm.heap(), self.stage_root)?;
            runtime
                .particles
                .restore(runtime.vm.heap(), map, &mut runtime.stage.scene)?;
            runtime.sync_narrative(self.session.as_ref().unwrap())?;
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
