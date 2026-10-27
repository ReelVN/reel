//! Shared author-resource IO and decoding. Adapters own platform work; consumers own waits.
mod data;
mod decode;
pub use data::AssetDataLoader;
mod io;
pub(crate) use io::file;
mod stream;
pub use stream::{AssetStream, StreamControl};
#[cfg(test)]
pub(crate) mod tests;
pub(crate) mod worker;
pub use decode::ImageExecutor;
pub use io::{DirectoryIo, PackIo};
use reel_core::texture::{TextureCodec, TextureDescriptor, TextureHeader, TexturePayload};
use reel_core::{
    budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
    identity::{AssetKey, AssetSource, IoId, Priority, ResourceVersion, Scope, WorkId},
    io::{AssetIo, DescribeRequest, IoError, IoEvent, IoPayload, ReadRequest},
    result::Terminal,
    wake::WakeSignal,
    work::{WorkBudget, WorkEvent, WorkExecutor, WorkRequest, WorkReservations},
};
use reel_ui::{
    frame::{RetainedAsset, RetainedAssetKind},
    peniko::{ImageAlphaType, ImageFormat},
    resources::{ColorEncoding, ResourceHandle, ResourceLease, ResourceRegistry, ResourceWeak},
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
    /// Temporary allowance shared by the archive reader and active image decoder.
    /// Compressed archives retain a fixed workspace; the decoder uses the remainder.
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
impl AssetOptions {
    fn queue_limits(&self) -> QueueLimits {
        QueueLimits {
            max_requests: 32,
            max_bytes: self
                .input_bytes
                .saturating_add(self.decode_bytes)
                .saturating_add(self.resident_bytes)
                .saturating_add(1 << 20),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LoadId(u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetKind {
    Image,
    Font,
    EffectShader,
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
    ImageManifest(reel_core::prepared_image::PreparedImage),
}
/// A logical image path can resolve to raster storage or a tiled-image manifest.
pub(crate) enum ImageAsset {
    Raster(ResourceLease),
    Tiled(reel_core::prepared_image::PreparedImage),
}
struct Load {
    source: AssetSource,
    reloadable: bool,
    kind: AssetKind,
    phase: Phase,
    input: Option<Arc<OwnedBytes>>,
    size: [u32; 2],
    texture: Option<TextureDescriptor>,
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
    io_workspace_bytes: u64,
    completion: BudgetPool,
    options: AssetOptions,
    next: u64,
    loads: BTreeMap<LoadId, Load>,
    origins: BTreeMap<ResourceHandle, (ResourceWeak, RetainedAssetKind)>,
    streams: stream::Streams,
    io_events: Vec<IoEvent>,
    work_events: Vec<WorkEvent>,
}
/// Cloneable main-thread handle. Closing it cancels all consumers using this service.
/// Dropping a clone leaves other consumers and their immutable leases alive.
#[derive(Clone)]
pub struct Assets(Rc<RefCell<Service>>);
impl Assets {
    pub fn directory(options: AssetOptions) -> Self {
        let limits = options.queue_limits();
        let root = options.root.clone();
        Self::with_adapters(options, |wake| {
            (
                Box::new(DirectoryIo::new(root, limits, wake.clone())),
                Box::new(ImageExecutor::new(limits, wake)),
            )
        })
    }
    /// Uses a resource target overlay with the current platform's image decoder.
    pub fn directory_for_target(options: AssetOptions, target: String) -> Result<Self, IoError> {
        let limits = options.queue_limits();
        let root = options.root.clone();
        Self::try_with_adapters(options, |wake, _temporary| {
            Ok((
                Box::new(DirectoryIo::for_target(root, target, limits, wake.clone())?),
                Box::new(ImageExecutor::new(limits, wake)),
            ))
        })
    }
    /// Uses prepared logical resources directly from their physical files.
    /// Optional archives supply catalogs and other entries outside the selection.
    pub fn selected(
        options: AssetOptions,
        files: BTreeMap<String, PathBuf>,
        packages: Option<Vec<PathBuf>>,
    ) -> Result<Self, IoError> {
        let limits = options.queue_limits();
        Self::try_with_adapters(options, |wake, temporary| {
            let io: Box<dyn AssetIo> = match packages {
                Some(paths) => Box::new(PackIo::selected(
                    paths,
                    files,
                    temporary,
                    limits,
                    wake.clone(),
                )?),
                None => Box::new(DirectoryIo::selected(files, limits, wake.clone())),
            };
            Ok((io, Box::new(ImageExecutor::new(limits, wake))))
        })
    }
    /// Release archives are the sole source unless a loose-file overlay is explicit.
    pub fn packaged(
        options: AssetOptions,
        paths: impl IntoIterator<Item = PathBuf>,
        overlay: Option<PathBuf>,
    ) -> Result<Self, IoError> {
        Self::packaged_for_target(options, paths, overlay, std::env::consts::OS.into())
    }
    /// Mounts selected archives and resolves loose overrides for a resource target.
    pub fn packaged_for_target(
        options: AssetOptions,
        paths: impl IntoIterator<Item = PathBuf>,
        overlay: Option<PathBuf>,
        target: String,
    ) -> Result<Self, IoError> {
        let limits = options.queue_limits();
        Self::try_with_adapters(options, |wake, temporary| {
            Ok((
                Box::new(PackIo::open(
                    paths,
                    overlay,
                    target,
                    temporary,
                    limits,
                    wake.clone(),
                )?),
                Box::new(ImageExecutor::new(limits, wake)),
            ))
        })
    }
    pub fn with_adapters(
        options: AssetOptions,
        create: impl FnOnce(WakeSignal) -> (Box<dyn AssetIo>, Box<dyn WorkExecutor>),
    ) -> Self {
        Self::try_with_adapters(options, |wake, _temporary| {
            Ok::<_, std::convert::Infallible>(create(wake))
        })
        .unwrap()
    }
    fn try_with_adapters<E>(
        options: AssetOptions,
        create: impl FnOnce(
            WakeSignal,
            &BudgetPool,
        ) -> Result<(Box<dyn AssetIo>, Box<dyn WorkExecutor>), E>,
    ) -> Result<Self, E> {
        let notify: Notify = Arc::new(Mutex::new(Arc::new(|| {})));
        let callback = notify.clone();
        let wake = WakeSignal::new(move || {
            let notify = callback.lock().unwrap().clone();
            notify();
        });
        let temporary = BudgetPool::new(
            BudgetCategory::Temporary,
            QueueLimits {
                max_requests: u32::MAX,
                max_bytes: options.decode_bytes,
            },
        );
        let (io, executor) = create(wake.clone(), &temporary)?;
        Ok(Self::assemble(
            options, io, executor, wake, notify, temporary,
        ))
    }
    fn assemble(
        options: AssetOptions,
        io: Box<dyn AssetIo>,
        executor: Box<dyn WorkExecutor>,
        wake: WakeSignal,
        notify: Notify,
        temporary: BudgetPool,
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
            io_workspace_bytes: temporary.usage().bytes,
            temporary,
            completion: pool(BudgetCategory::Completion, 1 << 20),
            options,
            next: 0,
            loads: BTreeMap::new(),
            origins: BTreeMap::new(),
            streams: stream::Streams::default(),
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
        (service.io.is_some() && service.wake.is_pending())
            || !service.loads.is_empty()
            || service.streams.pending()
    }
    pub fn close(&self) {
        let mut service = self.0.borrow_mut();
        service.loads.clear();
        service.origins.clear();
        service.streams.close();
        service.io = None;
        service.executor = None;
        service.io_events.clear();
        service.work_events.clear();
    }
    /// Create a media reader using this service's adapters and budgets.
    /// Read/seek may block a decoder worker or native media callback. Maintain
    /// stays on the host, which must keep polling until the reader closes.
    pub fn stream(
        &self,
        source: impl Into<String>,
    ) -> Result<(StreamControl, AssetStream), String> {
        let mut service = self.0.borrow_mut();
        if service.io.is_none() {
            return Err("asset service closed".into());
        }
        let wake = service.wake.clone();
        let chunk = service.options.input_bytes.min(64 << 10) as u32;
        if chunk == 0 {
            return Err("streaming requires a nonempty resource input budget".into());
        }
        service.streams.create(source.into(), wake, chunk)
    }
    pub fn request(&self, source: impl Into<String>, kind: AssetKind) -> LoadId {
        self.request_source(
            AssetSource {
                key: AssetKey(source.into()),
                version: None,
            },
            kind,
        )
    }
    /// Restore exactly the asset version retained by an accepted drawing.
    pub fn request_version(&self, version: ResourceVersion, kind: AssetKind) -> LoadId {
        self.request_source(
            AssetSource {
                key: version.key,
                version: Some(version.version),
            },
            kind,
        )
    }
    fn request_source(&self, source: AssetSource, kind: AssetKind) -> LoadId {
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
                source,
                reloadable: true,
                kind,
                phase,
                input: None,
                size: [0; 2],
                texture: None,
                output: 0,
                temporary: 0,
            },
        );
        id
    }
    /// Origin is registered by the producer; keys never imply file provenance.
    pub fn retained_asset(&self, lease: &ResourceLease) -> Option<RetainedAsset> {
        let service = self.0.borrow();
        let (_, kind) = service.origins.get(&lease.handle())?;
        Some(RetainedAsset {
            version: lease.version().clone(),
            kind: *kind,
        })
    }
    /// Encoded immutable author images use the same probe/decode/resident budgets as assets.
    pub fn request_encoded(&self, key: String, bytes: &[u8]) -> Result<LoadId, String> {
        self.try_request_encoded(key, bytes)?
            .ok_or_else(|| "image input budget temporarily occupied".into())
    }
    pub(crate) fn try_request_encoded(
        &self,
        key: String,
        bytes: &[u8],
    ) -> Result<Option<LoadId>, String> {
        let mut service = self.0.borrow_mut();
        if service.executor.is_none() {
            return Err("asset service closed".into());
        }
        if bytes.len() as u64 > service.options.input_bytes {
            return Err(format!(
                "image {key:?} requires {} input bytes; configured capacity is {}",
                bytes.len(),
                service.options.input_bytes
            ));
        }
        let Ok(ticket) = service.input.reserve(bytes.len() as u64) else {
            return Ok(None);
        };
        let input = OwnedBytes::new(bytes.to_vec(), ticket).map_err(|e| e.reason.message)?;
        service.next += 1;
        let id = LoadId(service.next);
        service.loads.insert(
            id,
            Load {
                source: AssetSource {
                    key: AssetKey(key),
                    version: Some(reel_core::identity::ContentVersion([0; 32])),
                },
                reloadable: false,
                kind: AssetKind::Image,
                phase: Phase::Probe,
                input: Some(Arc::new(input)),
                size: [0; 2],
                texture: None,
                output: 0,
                temporary: 0,
            },
        );
        Ok(Some(id))
    }
    pub fn poll(&self, id: LoadId) -> Result<Option<ResourceLease>, String> {
        self.maintain();
        self.take_ready(id)
    }
    /// Take a completed image/font after maintain, without pumping once per consumer.
    pub(crate) fn take_ready(&self, id: LoadId) -> Result<Option<ResourceLease>, String> {
        let mut service = self.0.borrow_mut();
        let load = service
            .loads
            .get(&id)
            .ok_or("asset request no longer active")?;
        if matches!(load.phase, Phase::Done(_)) {
            let load = service.loads.remove(&id).unwrap();
            let Phase::Done(result) = load.phase else {
                unreachable!()
            };
            result.map(Some)
        } else if matches!(load.phase, Phase::ImageManifest(_)) {
            service.loads.remove(&id);
            Err("a tiled image must be loaded through the stage image service".into())
        } else {
            Ok(None)
        }
    }
    /// Take a decoded raster or a content-identified tile manifest after maintain.
    pub(crate) fn take_image(&self, id: LoadId) -> Result<Option<ImageAsset>, String> {
        {
            let mut service = self.0.borrow_mut();
            let load = service
                .loads
                .get(&id)
                .ok_or("asset request no longer active")?;
            if let Phase::ImageManifest(manifest) = load.phase {
                service.loads.remove(&id);
                return Ok(Some(ImageAsset::Tiled(manifest)));
            }
        }
        self.take_ready(id)
            .map(|value| value.map(ImageAsset::Raster))
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
        for mut event in self.io_events.drain(..) {
            if event.scope != Scope::Process {
                continue;
            }
            if self.streams.deliver(&mut event) {
                continue;
            }
            let Some(load) = self.loads.values_mut().find(|load| matches!(load.phase, Phase::Describing(id) | Phase::Reading(id) if id == event.id)) else { continue; };
            let result = match event.result {
                Terminal::Completed(IoPayload::Described(info)) => {
                    if load
                        .source
                        .version
                        .is_some_and(|version| info.version != Some(version))
                    {
                        Err("asset adapter described a different version".into())
                    } else {
                        load.source.version = info.version;
                        match info.length.and_then(|n| u32::try_from(n).ok()) {
                            Some(length) => {
                                load.phase = shared_resource(&self.origins, load)
                                    .map_or(Phase::Read(length), |lease| Phase::Done(Ok(lease)));
                                Ok(())
                            }
                            None => Err("asset must have a bounded length for preparation".into()),
                        }
                    }
                }
                Terminal::Completed(IoPayload::Read(read)) => {
                    let described_version = load.source.version.is_some();
                    let version_matches = load
                        .source
                        .version
                        .is_none_or(|version| version == read.version);
                    load.source.version = Some(read.version);
                    load.input = Some(Arc::new(read.bytes));
                    load.phase = if matches!(load.kind, AssetKind::Font | AssetKind::EffectShader) {
                        Phase::Decode
                    } else {
                        Phase::Probe
                    };
                    if !version_matches {
                        Err("asset adapter returned a different version".into())
                    } else if read.eof {
                        if !described_version
                            && let Some(lease) = shared_resource(&self.origins, load)
                        {
                            load.input = None;
                            load.phase = Phase::Done(Ok(lease));
                        }
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
                    if bytes.len() == TextureHeader::LEN {
                        TextureHeader::decode(bytes)
                            .map_err(|e| e.to_string())
                            .map(|header| {
                                let descriptor = header.descriptor();
                                load.texture = Some(descriptor);
                                load.size = descriptor.layout.logical_size;
                                load.output =
                                    descriptor.byte_len().expect("validated texture header");
                                load.temporary = if header.codec() == TextureCodec::BcCrunch {
                                    decode::BC_CRUNCH_WORKSPACE_BYTES
                                } else {
                                    0
                                };
                                load.phase = Phase::Decode;
                            })
                    } else if bytes.len() != 24 {
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
                    let result = register(load, data);
                    if load.reloadable
                        && let Ok(lease) = &result
                    {
                        remember_resource(&mut self.origins, load.kind, lease);
                    }
                    load.phase = Phase::Done(result);
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
        // Advancing requests borrows the adapters and budgets while retaining
        // the request map's nodes and traversal order.
        let mut loads = std::mem::take(&mut self.loads);
        for load in loads.values_mut() {
            if let Err(error) = self.advance(load) {
                load.phase = Phase::Done(Err(error));
                load.input = None;
            }
        }
        self.loads = loads;
        self.streams.submit(
            self.io.as_mut().unwrap().as_mut(),
            &self.input,
            &self.completion,
        );
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
            Phase::Read(len) => {
                if u64::from(len) > self.options.input_bytes {
                    return Err(format!(
                        "asset {:?} requires {len} input bytes; configured capacity is {}",
                        load.source.key.0, self.options.input_bytes
                    ));
                }
                let Ok(output) = self.input.reserve(u64::from(len)) else {
                    // Other reads/decoders own the input tickets. Keep this
                    // request pending until their existing maintenance releases them.
                    return Ok(());
                };
                match self.io.as_mut().unwrap().read(ReadRequest {
                    resource: load.source.clone(),
                    offset: 0,
                    len,
                    priority: Priority::Visible,
                    scope: Scope::Process,
                    output,
                    completion: completion()?,
                }) {
                    Ok(id) => load.phase = Phase::Reading(id),
                    Err(error) if error.reason.retryable() => {}
                    Err(error) => return Err(error.reason.message),
                }
            }
            Phase::Probe if load.kind == AssetKind::Bytes => {
                load.phase = Phase::Bytes(
                    ResourceVersion {
                        key: load.source.key.clone(),
                        version: load.source.version.unwrap(),
                    },
                    load.input.take().unwrap(),
                );
            }
            Phase::Decode if matches!(load.kind, AssetKind::Font | AssetKind::EffectShader) => {
                let input = load.input.take().unwrap();
                let ticket = self
                    .output
                    .reserve(input.as_slice().len() as u64)
                    .map_err(|e| format!("asset resident budget: {}", e.message))?;
                let (bytes, old_ticket) = Arc::try_unwrap(input)
                    .expect("undecoded resource input has one owner")
                    .into_parts();
                let bytes = OwnedBytes::new(bytes, ticket).map_err(|e| e.reason.message)?;
                drop(old_ticket);
                let result = register(load, bytes);
                if load.reloadable
                    && let Ok(lease) = &result
                {
                    remember_resource(&mut self.origins, load.kind, lease);
                }
                load.phase = Phase::Done(result);
            }
            Phase::Probe | Phase::Decode => {
                let probe = matches!(load.phase, Phase::Probe);
                if probe
                    && load
                        .input
                        .as_ref()
                        .unwrap()
                        .as_slice()
                        .starts_with(reel_core::prepared_image::PreparedImage::MAGIC)
                {
                    let manifest = reel_core::prepared_image::PreparedImage::decode(
                        load.input.as_ref().unwrap().as_slice(),
                    )
                    .map_err(str::to_owned)?;
                    load.input = None;
                    load.phase = Phase::ImageManifest(manifest);
                    return Ok(());
                }
                let decode_bytes = self.options.decode_bytes - self.io_workspace_bytes;
                if !probe && load.temporary > decode_bytes {
                    return Err("image decode limit exceeded".into());
                }
                if self.temporary.usage().bytes > self.io_workspace_bytes {
                    return Ok(());
                }
                // A concurrent request may have completed while this one waited
                // for the decoder lane. Check once when this lane is available,
                // rather than scanning resources on every pending host tick.
                if let Some(lease) = shared_resource(&self.origins, load) {
                    load.input = None;
                    load.phase = Phase::Done(Ok(lease));
                    return Ok(());
                }
                let output = self
                    .output
                    .reserve(if probe {
                        TextureHeader::LEN as u64
                    } else {
                        load.output
                    })
                    .map_err(|e| format!("image resident budget: {}", e.message))?;
                let prepared = load
                    .input
                    .as_ref()
                    .unwrap()
                    .as_slice()
                    .starts_with(TextureHeader::MAGIC);
                let temporary = self
                    .temporary
                    .reserve(if prepared {
                        if probe { 0 } else { load.temporary }
                    } else {
                        decode_bytes
                    })
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
impl Drop for Service {
    fn drop(&mut self) {
        self.streams.close();
    }
}
fn register(load: &Load, data: OwnedBytes) -> Result<ResourceLease, String> {
    let version = ResourceVersion {
        key: load.source.key.clone(),
        version: load.source.version.expect("read returns a fixed version"),
    };
    let (data, shader) = if load.kind == AssetKind::EffectShader {
        let (descriptor, raw) = reel_ui::resources::EffectShaderDescriptor::decode_payload(
            data.as_slice(),
            data.as_slice().len(),
        )
        .map_err(|e| format!("effect shader: {e}"))?;
        let (mut bytes, ticket) = data.into_parts();
        let len = raw.len();
        bytes.copy_within(raw, 0);
        bytes.truncate(len);
        (
            OwnedBytes::new(bytes, ticket).map_err(|e| e.reason.message)?,
            Some(descriptor),
        )
    } else {
        (data, None)
    };
    let schema_bytes = shader.as_ref().map_or(0, |shader| {
        shader.parameters.capacity() * size_of::<reel_ui::stage::effect::EffectType>()
    });
    let mut registry = ResourceRegistry::new(
        1,
        1024 + version.key.0.capacity() as u64 + schema_bytes as u64,
    )
    .map_err(|e| format!("asset descriptor: {e:?}"))?;
    let handle = match load.kind {
        AssetKind::Bytes => return Err("raw bytes do not use the UI registry".into()),
        AssetKind::Font => registry.register_font(version, data),
        AssetKind::EffectShader => registry.register_effect_shader(version, shader.unwrap(), data),
        AssetKind::Image if load.texture.is_some() => registry.register_texture(
            version,
            TexturePayload::new(load.texture.unwrap(), data).map_err(|e| e.to_string())?,
        ),
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

fn shared_resource(
    origins: &BTreeMap<ResourceHandle, (ResourceWeak, RetainedAssetKind)>,
    load: &Load,
) -> Option<ResourceLease> {
    if !load.reloadable {
        return None;
    }
    let version = load.source.version?;
    // The path alone is insufficient: Describe or Read selected this immutable
    // version. Upgrade only a matching kind without retaining any strong cache.
    origins.values().find_map(|(weak, kind)| {
        if !matches!(
            (load.kind, kind),
            (AssetKind::Image, RetainedAssetKind::Image)
                | (AssetKind::Font, RetainedAssetKind::Font)
                | (AssetKind::EffectShader, RetainedAssetKind::EffectShader)
        ) {
            return None;
        }
        weak.upgrade().filter(|lease| {
            lease.version().key == load.source.key && lease.version().version == version
        })
    })
}

fn remember_resource(
    origins: &mut BTreeMap<ResourceHandle, (ResourceWeak, RetainedAssetKind)>,
    kind: AssetKind,
    lease: &ResourceLease,
) {
    let kind = match kind {
        AssetKind::Image => RetainedAssetKind::Image,
        AssetKind::Font => RetainedAssetKind::Font,
        AssetKind::EffectShader => RetainedAssetKind::EffectShader,
        AssetKind::Bytes => unreachable!(),
    };
    origins.retain(|_, (weak, _)| !weak.is_released());
    origins.insert(lease.handle(), (lease.downgrade(), kind));
}
