//! Shared author-resource IO and decoding. Adapters own platform work; consumers own waits.
mod decode;
mod io;
#[cfg(test)]
pub(crate) mod tests;
pub(crate) mod worker;
pub use decode::ImageExecutor;
pub use io::DirectoryIo;
use reel_core::{
    budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
    identity::{AssetKey, AssetSource, IoId, Priority, ResourceVersion, Scope, WorkId},
    io::{AssetIo, DescribeRequest, IoEvent, IoPayload, ReadRequest},
    result::Terminal,
    wake::WakeSignal,
    work::{WorkBudget, WorkEvent, WorkExecutor, WorkRequest, WorkReservations},
};
use reel_ui::{
    peniko::{ImageAlphaType, ImageFormat},
    resources::{ColorEncoding, ResourceLease, ResourceRegistry},
};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug)]
pub struct AssetOptions {
    pub root: PathBuf,
    pub resident_bytes: u64,
    pub input_bytes: u64,
    pub decode_bytes: u64,
}
impl Default for AssetOptions {
    fn default() -> Self {
        Self {
            root: ".".into(),
            resident_bytes: 256 << 20,
            input_bytes: 64 << 20,
            decode_bytes: 256 << 20,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LoadId(u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetKind {
    Image,
    Font,
    Bytes,
}
enum Phase {
    Describe,
    Describing(IoId),
    Read(u32),
    Reading(IoId),
    Probe,
    Probing(WorkId),
    Decode,
    Decoding(WorkId),
    Done(Result<ResourceLease, String>),
    Bytes(ResourceVersion, Arc<OwnedBytes>),
}
struct Load {
    source: AssetSource,
    kind: AssetKind,
    phase: Phase,
    input: Option<Arc<OwnedBytes>>,
    size: [u32; 2],
    output: u64,
    temporary: u64,
}
type Notify = Arc<Mutex<Arc<dyn Fn() + Send + Sync>>>;
struct Service {
    io: Option<Box<dyn AssetIo>>,
    executor: Option<Box<dyn WorkExecutor>>,
    wake: WakeSignal,
    notify: Notify,
    input: BudgetPool,
    output: BudgetPool,
    temporary: BudgetPool,
    completion: BudgetPool,
    options: AssetOptions,
    next: u64,
    loads: BTreeMap<LoadId, Load>,
    io_events: Vec<IoEvent>,
    work_events: Vec<WorkEvent>,
}
/// Cloneable main-thread handle. Closing it cancels all consumers using this service.
/// Dropping a clone leaves other consumers and their immutable leases alive.
#[derive(Clone)]
pub struct Assets(Rc<RefCell<Service>>);
impl Assets {
    pub fn directory(options: AssetOptions) -> Self {
        let notify: Notify = Arc::new(Mutex::new(Arc::new(|| {})));
        let callback = notify.clone();
        let wake = WakeSignal::new(move || {
            let notify = callback.lock().unwrap().clone();
            notify();
        });
        let limits = QueueLimits {
            max_requests: 32,
            max_bytes: options
                .input_bytes
                .saturating_add(options.decode_bytes)
                .saturating_add(options.resident_bytes)
                .saturating_add(1 << 20),
        };
        let io = Box::new(DirectoryIo::new(options.root.clone(), limits, wake.clone()));
        let executor = Box::new(ImageExecutor::new(limits, wake.clone()));
        Self::assemble(options, io, executor, wake, notify)
    }
    pub fn with_adapters(
        options: AssetOptions,
        create: impl FnOnce(WakeSignal) -> (Box<dyn AssetIo>, Box<dyn WorkExecutor>),
    ) -> Self {
        let notify: Notify = Arc::new(Mutex::new(Arc::new(|| {})));
        let callback = notify.clone();
        let wake = WakeSignal::new(move || {
            let notify = callback.lock().unwrap().clone();
            notify();
        });
        let (io, executor) = create(wake.clone());
        Self::assemble(options, io, executor, wake, notify)
    }
    fn assemble(
        options: AssetOptions,
        io: Box<dyn AssetIo>,
        executor: Box<dyn WorkExecutor>,
        wake: WakeSignal,
        notify: Notify,
    ) -> Self {
        let pool = |category, bytes| {
            BudgetPool::new(
                category,
                QueueLimits {
                    max_requests: u32::MAX,
                    max_bytes: bytes,
                },
            )
        };
        Self(Rc::new(RefCell::new(Service {
            io: Some(io),
            executor: Some(executor),
            wake,
            notify,
            input: pool(BudgetCategory::CpuInput, options.input_bytes),
            output: pool(BudgetCategory::CpuOutput, options.resident_bytes),
            temporary: pool(BudgetCategory::Temporary, options.decode_bytes),
            completion: pool(BudgetCategory::Completion, 1 << 20),
            options,
            next: 0,
            loads: BTreeMap::new(),
            io_events: Vec::with_capacity(32),
            work_events: Vec::with_capacity(32),
        })))
    }
    pub fn set_waker(&self, notify: Arc<dyn Fn() + Send + Sync>) {
        *self.0.borrow().notify.lock().unwrap() = notify;
    }
    pub(crate) fn wake_signal(&self) -> WakeSignal {
        self.0.borrow().wake.clone()
    }
    pub(crate) fn worker_limits(&self) -> QueueLimits {
        QueueLimits {
            max_requests: 32,
            max_bytes: self.0.borrow().options.input_bytes,
        }
    }
    pub fn maintain(&self) {
        self.0.borrow_mut().pump();
    }
    pub fn needs_poll(&self) -> bool {
        let service = self.0.borrow();
        (service.io.is_some() && service.wake.is_pending()) || !service.loads.is_empty()
    }
    pub fn close(&self) {
        let mut service = self.0.borrow_mut();
        service.loads.clear();
        service.io = None;
        service.executor = None;
        service.io_events.clear();
        service.work_events.clear();
    }
    pub fn request(&self, source: impl Into<String>, kind: AssetKind) -> LoadId {
        let mut service = self.0.borrow_mut();
        service.next = service
            .next
            .checked_add(1)
            .expect("asset request identity exhausted");
        let id = LoadId(service.next);
        let phase = if service.io.is_some() {
            Phase::Describe
        } else {
            Phase::Done(Err("asset service closed".into()))
        };
        service.loads.insert(
            id,
            Load {
                source: AssetSource {
                    key: AssetKey(source.into()),
                    version: None,
                },
                kind,
                phase,
                input: None,
                size: [0; 2],
                output: 0,
                temporary: 0,
            },
        );
        id
    }
    pub fn poll(&self, id: LoadId) -> Result<Option<ResourceLease>, String> {
        let mut service = self.0.borrow_mut();
        service.pump();
        let load = service
            .loads
            .get(&id)
            .ok_or("asset request no longer active")?;
        if matches!(load.phase, Phase::Done(_)) {
            let Phase::Done(result) = service.loads.remove(&id).unwrap().phase else {
                unreachable!()
            };
            result.map(Some)
        } else {
            Ok(None)
        }
    }
    /// Take raw bytes after calling maintain; does not re-run the shared pump.
    pub fn poll_bytes(
        &self,
        id: LoadId,
    ) -> Result<Option<(ResourceVersion, Arc<OwnedBytes>)>, String> {
        let mut service = self.0.borrow_mut();
        let load = service
            .loads
            .get(&id)
            .ok_or("asset request no longer active")?;
        match &load.phase {
            Phase::Bytes(_, _) => {
                let Phase::Bytes(version, bytes) = service.loads.remove(&id).unwrap().phase else {
                    unreachable!()
                };
                Ok(Some((version, bytes)))
            }
            Phase::Done(Err(_)) => {
                let Phase::Done(Err(error)) = service.loads.remove(&id).unwrap().phase else {
                    unreachable!()
                };
                Err(error)
            }
            _ if load.kind != AssetKind::Bytes => Err("asset is not a raw byte request".into()),
            _ => Ok(None),
        }
    }
    /// Includes cancelled IO whose owned buffers/completions have not returned yet.
    pub fn pending_io(&self) -> bool {
        let service = self.0.borrow();
        service.completion.usage().tickets != 0 || service.input.usage().tickets != 0
    }
    pub fn cancel(&self, id: LoadId) {
        let mut service = self.0.borrow_mut();
        if let Some(load) = service.loads.remove(&id) {
            match load.phase {
                Phase::Describing(id) | Phase::Reading(id) => {
                    if let Some(io) = &mut service.io {
                        io.cancel(id);
                    }
                }
                Phase::Probing(id) | Phase::Decoding(id) => {
                    if let Some(executor) = &mut service.executor {
                        executor.cancel(id);
                    }
                }
                _ => {}
            }
        }
    }
    pub fn output_pool(&self) -> BudgetPool {
        self.0.borrow().output.clone()
    }
    pub(crate) fn resident_available(&self) -> u64 {
        let service = self.0.borrow();
        service
            .options
            .resident_bytes
            .saturating_sub(service.output.usage().bytes)
    }
    pub fn temporary_pool(&self) -> BudgetPool {
        self.0.borrow().temporary.clone()
    }
}
impl Service {
    fn pump(&mut self) {
        if self.io.is_none() {
            return;
        }
        self.wake.acknowledge();
        let progress = self.executor.as_mut().unwrap().advance(WorkBudget {
            max_units: 64,
            max_bytes: 64 << 10,
            deadline: None,
        });
        self.io.as_mut().unwrap().poll(&mut self.io_events, 32);
        self.executor
            .as_mut()
            .unwrap()
            .drain(&mut self.work_events, 32);
        let more_completions = self.io_events.len() == 32 || self.work_events.len() == 32;
        for event in self.io_events.drain(..) {
            if event.scope != Scope::Process {
                continue;
            }
            let Some(load) = self.loads.values_mut().find(|load| matches!(load.phase, Phase::Describing(id) | Phase::Reading(id) if id == event.id)) else { continue; };
            let result = match event.result {
                Terminal::Completed(IoPayload::Described(info)) => {
                    load.source.version = info.version;
                    match info.length.and_then(|n| u32::try_from(n).ok()) {
                        Some(length) => {
                            load.phase = Phase::Read(length);
                            Ok(())
                        }
                        None => Err("asset must have a bounded length for preparation".into()),
                    }
                }
                Terminal::Completed(IoPayload::Read(read)) => {
                    let version_matches = load
                        .source
                        .version
                        .is_none_or(|version| version == read.version);
                    load.source.version = Some(read.version);
                    load.input = Some(Arc::new(read.bytes));
                    load.phase = if load.kind == AssetKind::Font {
                        Phase::Decode
                    } else {
                        Phase::Probe
                    };
                    if !version_matches {
                        Err("asset adapter returned a different version".into())
                    } else if read.eof {
                        Ok(())
                    } else {
                        Err("asset adapter returned an incomplete full read".into())
                    }
                }
                Terminal::Failed(error) => Err(error.message),
                Terminal::Cancelled => Err("asset read cancelled".into()),
                Terminal::Completed(_) => Err("unexpected asset IO result".into()),
            };
            if let Err(error) = result {
                load.phase = Phase::Done(Err(error));
                load.input = None;
            }
        }
        for event in self.work_events.drain(..) {
            if event.scope != Scope::Process {
                continue;
            }
            let Some(load) = self.loads.values_mut().find(|load| matches!(load.phase, Phase::Probing(id) | Phase::Decoding(id) if id == event.id)) else { continue; };
            let result = match event.result {
                Terminal::Completed(data) if matches!(load.phase, Phase::Probing(_)) => {
                    let bytes = data.as_slice();
                    if bytes.len() != 24 {
                        Err("invalid image probe result".into())
                    } else {
                        load.size = [
                            u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
                            u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
                        ];
                        load.output = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
                        match u64::from_le_bytes(bytes[16..24].try_into().unwrap())
                            .checked_add(load.output)
                        {
                            Some(bytes) => {
                                load.temporary = bytes;
                                load.phase = Phase::Decode;
                                Ok(())
                            }
                            None => Err("image size overflow".into()),
                        }
                    }
                }
                Terminal::Completed(data) => {
                    load.phase = Phase::Done(register(load, data));
                    load.input = None;
                    Ok(())
                }
                Terminal::Failed(error) => Err(error.message),
                Terminal::Cancelled => Err("asset decode cancelled".into()),
            };
            if let Err(error) = result {
                load.phase = Phase::Done(Err(error));
                load.input = None;
            }
        }
        let ids = self.loads.keys().copied().collect::<Vec<_>>();
        for id in ids {
            let mut load = self.loads.remove(&id).unwrap();
            if let Err(error) = self.advance(&mut load) {
                load.phase = Phase::Done(Err(error));
                load.input = None;
            }
            self.loads.insert(id, load);
        }
        if progress.has_work || more_completions {
            self.wake.wake();
        }
    }
    fn advance(&mut self, load: &mut Load) -> Result<(), String> {
        let completion = || self.completion.reserve(512).map_err(|e| e.message);
        match load.phase {
            Phase::Describe => match self.io.as_mut().unwrap().describe(DescribeRequest {
                resource: load.source.clone(),
                priority: Priority::Visible,
                scope: Scope::Process,
                completion: completion()?,
            }) {
                Ok(id) => load.phase = Phase::Describing(id),
                Err(error) if error.reason.retryable() => {}
                Err(error) => return Err(error.reason.message),
            },
            Phase::Read(len) => match self.io.as_mut().unwrap().read(ReadRequest {
                resource: load.source.clone(),
                offset: 0,
                len,
                priority: Priority::Visible,
                scope: Scope::Process,
                output: self
                    .input
                    .reserve(u64::from(len))
                    .map_err(|e| format!("asset input budget: {}", e.message))?,
                completion: completion()?,
            }) {
                Ok(id) => load.phase = Phase::Reading(id),
                Err(error) if error.reason.retryable() => {}
                Err(error) => return Err(error.reason.message),
            },
            Phase::Probe if load.kind == AssetKind::Bytes => {
                load.phase = Phase::Bytes(
                    ResourceVersion {
                        key: load.source.key.clone(),
                        version: load.source.version.unwrap(),
                    },
                    load.input.take().unwrap(),
                );
            }
            Phase::Decode if load.kind == AssetKind::Font => {
                let input = load.input.take().unwrap();
                let ticket = self
                    .output
                    .reserve(input.as_slice().len() as u64)
                    .map_err(|e| format!("font resident budget: {}", e.message))?;
                let (bytes, old_ticket) = Arc::try_unwrap(input)
                    .expect("font input has one owner")
                    .into_parts();
                let bytes = OwnedBytes::new(bytes, ticket).map_err(|e| e.reason.message)?;
                drop(old_ticket);
                load.phase = Phase::Done(register(load, bytes));
            }
            Phase::Probe | Phase::Decode => {
                let probe = matches!(load.phase, Phase::Probe);
                let temporary_bytes = if probe {
                    self.options.decode_bytes
                } else {
                    load.temporary
                };
                if temporary_bytes > self.options.decode_bytes {
                    return Err("image decode limit exceeded".into());
                }
                if self.temporary.usage().bytes != 0 {
                    return Ok(());
                }
                let output = self
                    .output
                    .reserve(if probe { 24 } else { load.output })
                    .map_err(|e| format!("image resident budget: {}", e.message))?;
                let temporary = self
                    .temporary
                    .reserve(temporary_bytes)
                    .map_err(|e| format!("image decode limit: {}", e.message))?;
                let request = WorkRequest {
                    kind: if probe {
                        decode::image_probe()
                    } else {
                        decode::image_decode()
                    },
                    input: load.input.as_ref().unwrap().clone(),
                    scope: Scope::Process,
                    priority: Priority::Visible,
                    reservations: WorkReservations {
                        output,
                        temporary,
                        completion: completion()?,
                    },
                };
                match self.executor.as_mut().unwrap().submit(request) {
                    Ok(id) => {
                        load.phase = if probe {
                            Phase::Probing(id)
                        } else {
                            Phase::Decoding(id)
                        }
                    }
                    Err(error) if error.reason.retryable() => {}
                    Err(error) => return Err(error.reason.message),
                }
            }
            _ => {}
        }
        Ok(())
    }
}
fn register(load: &Load, data: OwnedBytes) -> Result<ResourceLease, String> {
    let version = ResourceVersion {
        key: load.source.key.clone(),
        version: load.source.version.expect("read returns a fixed version"),
    };
    let mut registry = ResourceRegistry::new(1, 1024 + version.key.0.capacity() as u64)
        .map_err(|e| format!("asset descriptor: {e:?}"))?;
    let handle = match load.kind {
        AssetKind::Bytes => return Err("raw bytes do not use the UI registry".into()),
        AssetKind::Font => registry.register_font(version, data),
        AssetKind::Image => registry.register_image(
            version,
            data,
            load.size,
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        ),
    }
    .map_err(|e| format!("asset resource: {e:?}"))?;
    registry
        .acquire(handle)
        .map_err(|e| format!("asset lease: {e:?}"))
}
