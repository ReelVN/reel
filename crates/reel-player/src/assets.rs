//! Shared author-resource IO and decoding. Adapters own platform work; consumers own waits.
mod atlas;
mod data;
mod decode;
pub use data::AssetDataLoader;
mod io;
mod origins;
pub(crate) use io::file;
mod stream;
pub use stream::{AssetStream, StreamControl};
#[cfg(test)]
pub(crate) mod tests;
pub(crate) mod worker;
pub use decode::ImageExecutor;
use hashbrown::HashMap;
pub use io::{DirectoryIo, PackIo};
use reel_core::{
    budget::{BudgetCategory, BudgetGroup, BudgetPool, OwnedBytes, QueueLimits},
    identity::{AssetKey, AssetSource, IoId, Priority, ResourceVersion, Scope, WorkId},
    io::{AssetInfo, AssetIo, DescribeRequest, IoError, IoEvent, IoPayload, ReadRequest},
    result::Terminal,
    wake::WakeSignal,
    work::{WorkBudget, WorkEvent, WorkExecutor, WorkRequest, WorkReservations},
};
use reel_core::{
    prepared_raster::RasterHeader,
    texture::{TextureHeader, TexturePayload},
};
use reel_ui::{
    frame::{RetainedAsset, RetainedAssetKind},
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
    /// Decoded source payload budget: 512 MiB on desktop hosts and 256 MiB
    /// elsewhere by default. Vita shares this allowance with archive metadata.
    pub resident_bytes: u64,
    /// Independent archive metadata admission ceiling, including parsing peak.
    /// The default 32 MiB is a limit; mounts reserve their actual capacities.
    pub metadata_bytes: u64,
    /// Total encoded input ceiling. Streaming reserves min(128 KiB, one quarter)
    /// within this ceiling; ordinary resources must fit the remaining capacity.
    pub input_bytes: u64,
    /// Temporary allowance shared by the archive reader and admitted image decodes.
    /// Compressed archives retain a fixed workspace; decode jobs share the remainder.
    pub decode_bytes: u64,
}
impl Default for AssetOptions {
    fn default() -> Self {
        Self {
            root: ".".into(),
            resident_bytes: if cfg!(any(
                target_os = "windows",
                target_os = "linux",
                target_os = "macos"
            )) {
                512 << 20
            } else {
                256 << 20
            },
            metadata_bytes: 32 << 20,
            input_bytes: 64 << 20,
            decode_bytes: 256 << 20,
        }
    }
}
impl AssetOptions {
    fn stream_input_bytes(&self) -> u64 {
        (128 << 10).min(self.input_bytes / 4)
    }
    /// Maximum encoded length of an individual non-streamed asset.
    pub fn ordinary_input_bytes(&self) -> u64 {
        self.input_bytes - self.stream_input_bytes()
    }
    fn resident_pools(&self, share_metadata: bool) -> (BudgetPool, BudgetPool) {
        let limits = |max_bytes| QueueLimits {
            max_requests: u32::MAX,
            max_bytes,
        };
        if share_metadata {
            let combined = self.resident_bytes.saturating_add(self.metadata_bytes);
            let group = BudgetGroup::new(combined);
            (
                group.pool(BudgetCategory::Metadata, limits(self.metadata_bytes)),
                group.pool(BudgetCategory::CpuOutput, limits(combined)),
            )
        } else {
            (
                BudgetPool::new(BudgetCategory::Metadata, limits(self.metadata_bytes)),
                BudgetPool::new(BudgetCategory::CpuOutput, limits(self.resident_bytes)),
            )
        }
    }
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
    DecodeBoundedImage,
    Decoding(WorkId),
    Done(Result<ResourceLease, String>),
    Bytes(ResourceVersion, Arc<OwnedBytes>),
    ImageManifest(reel_core::prepared_image::PreparedImage),
    AtlasResolve(atlas::ViewRequest),
    AtlasWait(atlas::ViewRequest),
}
/// A logical image path can resolve to raster storage or a tiled-image manifest.
pub(crate) enum ImageAsset {
    Raster(ResourceLease),
    Tiled(reel_core::prepared_image::PreparedImage),
}

#[derive(Clone, Copy)]
enum PreparedPixels {
    Raster(RasterHeader),
    Texture(TextureHeader),
}
impl PreparedPixels {
    fn parse(bytes: &[u8]) -> Option<Result<Self, String>> {
        if bytes.starts_with(RasterHeader::MAGIC) {
            return Some(
                RasterHeader::parse(bytes)
                    .map(|(header, _)| Self::Raster(header))
                    .ok_or_else(|| "invalid prepared raster dimensions or payload".into()),
            );
        }
        if bytes.starts_with(TextureHeader::MAGIC) {
            return Some(
                TextureHeader::parse(bytes)
                    .map(|(header, _)| Self::Texture(header))
                    .map_err(|error| error.to_string()),
            );
        }
        None
    }

    fn size(self) -> [u32; 2] {
        match self {
            Self::Raster(header) => header.size(),
            Self::Texture(header) => header.descriptor().layout.logical_size,
        }
    }

    fn header_len(self) -> usize {
        match self {
            Self::Raster(_) => RasterHeader::LEN,
            Self::Texture(_) => TextureHeader::LEN,
        }
    }

    fn payload_len(self) -> u64 {
        match self {
            Self::Raster(header) => header.byte_len(),
            Self::Texture(header) => header.payload_len(),
        }
    }
}
struct Load {
    source: AssetSource,
    reloadable: bool,
    reuse_uploaded_image: bool,
    kind: AssetKind,
    priority: Priority,
    prefetch_limit: u64,
    phase: Phase,
    input: Option<Arc<OwnedBytes>>,
    size: [u32; 2],
    prepared: Option<PreparedPixels>,
    output: u64,
    temporary: u64,
}
impl Load {
    fn prepare_pixels(&mut self, prepared: PreparedPixels) {
        self.prepared = Some(prepared);
        self.size = prepared.size();
        self.output = prepared.payload_len();
        self.temporary = 0;
        self.phase = Phase::Decode;
    }
}
type Notify = Arc<Mutex<Arc<dyn Fn() + Send + Sync>>>;
struct Service {
    io: Option<Box<dyn AssetIo>>,
    executor: Option<Box<dyn WorkExecutor>>,
    wake: WakeSignal,
    notify: Notify,
    input: BudgetPool,
    stream_input: BudgetPool,
    output: BudgetPool,
    metadata: BudgetPool,
    temporary: BudgetPool,
    io_workspace_bytes: u64,
    completion: BudgetPool,
    options: AssetOptions,
    next: u64,
    loads: BTreeMap<LoadId, Load>,
    io_loads: HashMap<IoId, LoadId>,
    work_loads: HashMap<WorkId, LoadId>,
    origins: origins::Origins,
    recent_images: origins::RecentImages,
    atlas_pages: atlas::Pages,
    streams: stream::Streams,
    io_events: Vec<IoEvent>,
    work_events: Vec<WorkEvent>,
    pending_bounded_decodes: Vec<(WorkId, AssetSource)>,
    // Only the built-in decoder shares these codec workspace estimates.
    builtin_image_decoder: bool,
    allow_png_pipeline: bool,
    #[cfg(test)]
    pumps: usize,
}
/// Cloneable main-thread handle. Closing it cancels all consumers using this service.
/// Dropping a clone leaves other consumers and their immutable leases alive.
#[derive(Clone)]
pub struct Assets(Rc<RefCell<Service>>);
impl Assets {
    /// Mount browser-supplied resource bytes under their ordinary logical paths.
    #[cfg(target_arch = "wasm32")]
    pub fn memory(
        options: AssetOptions,
        files: BTreeMap<String, Arc<[u8]>>,
    ) -> Result<Self, IoError> {
        let limits = options.queue_limits();
        let input_bytes = options.input_bytes;
        Self::with_image_executor(options, |wake, _, _| {
            Ok(Box::new(io::MemoryIo::new(
                files,
                input_bytes,
                limits,
                wake,
            )?))
        })
    }

    /// Open fixed loose resources for this run. File identities use source paths;
    /// preparation reads only requested data. Restart after changing assets.
    pub fn directory(options: AssetOptions) -> Self {
        let limits = options.queue_limits();
        let root = options.root.clone();
        Self::with_image_executor(options, |wake, _temporary, _metadata| {
            Ok::<_, std::convert::Infallible>(Box::new(DirectoryIo::new(root, limits, wake)))
        })
        .unwrap()
    }
    /// Uses a resource target overlay with the current platform's image decoder.
    pub fn directory_for_target(options: AssetOptions, target: String) -> Result<Self, IoError> {
        let limits = options.queue_limits();
        let root = options.root.clone();
        Self::with_image_executor(options, |wake, _temporary, _metadata| {
            Ok(Box::new(DirectoryIo::for_target(
                root, target, limits, wake,
            )?))
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
        Self::with_image_executor(options, |wake, temporary, metadata| {
            let io: Box<dyn AssetIo> = match packages {
                Some(paths) => Box::new(PackIo::selected(
                    paths, files, temporary, metadata, limits, wake,
                )?),
                None => Box::new(DirectoryIo::selected(files, limits, wake)),
            };
            Ok(io)
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
        Self::with_image_executor(options, |wake, temporary, metadata| {
            Ok(Box::new(PackIo::open(
                paths, overlay, target, temporary, metadata, limits, wake,
            )?))
        })
    }
    fn with_image_executor<E>(
        options: AssetOptions,
        open_io: impl FnOnce(WakeSignal, &BudgetPool, &BudgetPool) -> Result<Box<dyn AssetIo>, E>,
    ) -> Result<Self, E> {
        let limits = options.queue_limits();
        let threads = decode::available_image_threads();
        Self::try_with_adapters(options, Some(threads), |wake, temporary, metadata| {
            let io = open_io(wake.clone(), temporary, metadata)?;
            Ok((
                io,
                Box::new(ImageExecutor::with_threads(limits, wake, threads)),
            ))
        })
    }

    pub fn with_adapters(
        options: AssetOptions,
        create: impl FnOnce(WakeSignal) -> (Box<dyn AssetIo>, Box<dyn WorkExecutor>),
    ) -> Self {
        Self::try_with_adapters(options, None, |wake, _temporary, _metadata| {
            Ok::<_, std::convert::Infallible>(create(wake))
        })
        .unwrap()
    }
    fn try_with_adapters<E>(
        options: AssetOptions,
        image_threads: Option<usize>,
        create: impl FnOnce(
            WakeSignal,
            &BudgetPool,
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
        let (metadata, output) = options.resident_pools(cfg!(target_os = "vita"));
        let (io, executor) = create(wake.clone(), &temporary, &metadata)?;
        let input_group = BudgetGroup::new(options.input_bytes);
        let input_pool = |bytes| {
            input_group.pool(
                BudgetCategory::CpuInput,
                QueueLimits {
                    max_requests: u32::MAX,
                    max_bytes: bytes,
                },
            )
        };
        Ok(Self(Rc::new(RefCell::new(Service {
            io: Some(io),
            executor: Some(executor),
            wake,
            notify,
            input: input_pool(options.ordinary_input_bytes()),
            stream_input: input_pool(options.stream_input_bytes()),
            output,
            origins: origins::Origins::new(metadata.clone()),
            recent_images: Default::default(),
            atlas_pages: atlas::Pages::default(),
            metadata,
            io_workspace_bytes: temporary.usage().bytes,
            temporary,
            completion: BudgetPool::new(
                BudgetCategory::Completion,
                QueueLimits {
                    max_requests: u32::MAX,
                    max_bytes: 1 << 20,
                },
            ),
            options,
            next: 0,
            loads: BTreeMap::new(),
            io_loads: HashMap::new(),
            work_loads: HashMap::new(),
            streams: stream::Streams::default(),
            io_events: Vec::with_capacity(32),
            work_events: Vec::with_capacity(32),
            pending_bounded_decodes: Vec::new(),
            builtin_image_decoder: image_threads.is_some(),
            allow_png_pipeline: image_threads
                .is_some_and(|threads| threads > decode::MAX_IMAGE_WORKERS),
            #[cfg(test)]
            pumps: 0,
        }))))
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
        service.io_loads.clear();
        service.work_loads.clear();
        service.pending_bounded_decodes.clear();
        service.atlas_pages.clear();
        service.recent_images = Default::default();
        service.origins.clear();
        service.streams.close();
        service.io = None;
        service.executor = None;
        service.io_events.clear();
        service.work_events.clear();
    }
    pub(crate) fn remember_stage_image(&self, source: String, lease: ResourceLease) {
        let mut service = self.0.borrow_mut();
        if service.io.is_some()
            && service.origins.kind(lease.handle()) == Some(RetainedAssetKind::Image)
        {
            let decoded_limit = service.options.resident_bytes / 32;
            service.recent_images.insert(source, lease, decoded_limit);
        }
    }
    pub(crate) fn take_recent_image(&self, source: &str) -> Option<ResourceLease> {
        let mut service = self.0.borrow_mut();
        service
            .recent_images
            .take(source)
            .or_else(|| service.origins.stage_image(source))
    }
    /// Create a media reader using this service's adapters and budgets.
    /// Read/seek may block a decoder worker or native media callback. Its reader
    /// shares the adapter's bounded IO lane and progresses without host polling.
    /// Adapters must provide [`AssetIo::background_reader`].
    /// Creation admits one 512-byte completion ticket, reused until cancellation
    /// or reader drop; insufficient completion capacity rejects creation.
    pub fn stream(
        &self,
        source: impl Into<String>,
    ) -> Result<(StreamControl, AssetStream), String> {
        let mut service = self.0.borrow_mut();
        if service.io.is_none() {
            return Err("asset service closed".into());
        }
        let chunk = service
            .options
            .stream_input_bytes()
            .div_ceil(2)
            .min(64 << 10) as u32;
        if chunk == 0 {
            return Err("streaming requires a nonempty reserved input allowance".into());
        }
        let Service {
            streams,
            io,
            stream_input,
            completion,
            metadata,
            ..
        } = &mut *service;
        streams.create(
            source.into(),
            io.as_ref().unwrap().as_ref(),
            stream_input.clone(),
            completion.clone(),
            metadata.clone(),
            chunk,
        )
    }
    pub fn request(&self, source: impl Into<String>, kind: AssetKind) -> LoadId {
        self.request_source(
            AssetSource {
                key: AssetKey(source.into()),
                version: None,
            },
            kind,
            Priority::Visible,
            u64::MAX,
            false,
        )
    }
    /// Stage presentation can share an uploaded raster's identity while the
    /// renderer retains its pixels. A miss uses ordinary exact-version recovery.
    pub(crate) fn request_stage_asset(&self, source: String, kind: AssetKind) -> LoadId {
        self.request_source(
            AssetSource {
                key: AssetKey(source),
                version: None,
            },
            kind,
            Priority::Visible,
            u64::MAX,
            kind == AssetKind::Image,
        )
    }
    /// Restore a retained asset from the selected fixed source.
    pub fn request_version(&self, version: ResourceVersion, kind: AssetKind) -> LoadId {
        self.request_source(
            AssetSource {
                key: version.key,
                version: Some(version.version),
            },
            kind,
            Priority::Visible,
            u64::MAX,
            false,
        )
    }
    pub(crate) fn prefetch(&self, source: String, kind: AssetKind, limit: u64) -> LoadId {
        self.request_source(
            AssetSource {
                key: AssetKey(source),
                version: None,
            },
            kind,
            Priority::Prefetch,
            limit,
            kind == AssetKind::Image,
        )
    }
    /// Raise the existing request to visible priority and keep its admitted work.
    pub(crate) fn promote_prefetch(&self, id: LoadId) -> bool {
        self.0.borrow_mut().promote_prefetch(id)
    }
    fn request_source(
        &self,
        source: AssetSource,
        kind: AssetKind,
        priority: Priority,
        prefetch_limit: u64,
        reuse_uploaded_image: bool,
    ) -> LoadId {
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
                reuse_uploaded_image,
                kind,
                priority,
                prefetch_limit,
                phase,
                input: None,
                size: [0; 2],
                prepared: None,
                output: 0,
                temporary: 0,
            },
        );
        id
    }
    /// Origin is registered by the producer; keys never imply file provenance.
    pub fn retained_asset(&self, lease: &ResourceLease) -> Option<RetainedAsset> {
        let service = self.0.borrow();
        let kind = service.origins.kind(lease.handle())?;
        Some(RetainedAsset {
            version: lease.version().clone(),
            kind,
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
        self.encoded(key, bytes, Priority::Visible, u64::MAX)
    }
    pub(crate) fn prefetch_encoded(
        &self,
        key: String,
        bytes: &[u8],
        limit: u64,
    ) -> Result<LoadId, String> {
        self.encoded(key, bytes, Priority::Prefetch, limit)?
            .ok_or_else(|| "prefetch input capacity exhausted".into())
    }
    fn encoded(
        &self,
        key: String,
        bytes: &[u8],
        priority: Priority,
        prefetch_limit: u64,
    ) -> Result<Option<LoadId>, String> {
        let mut service = self.0.borrow_mut();
        if service.executor.is_none() {
            return Err("asset service closed".into());
        }
        if bytes.len() as u64 > service.options.ordinary_input_bytes() {
            return Err(format!(
                "image {key:?} requires {} input bytes; ordinary input capacity is {} ({} reserved for streaming)",
                bytes.len(),
                service.options.ordinary_input_bytes(),
                service.options.stream_input_bytes()
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
                reuse_uploaded_image: false,
                kind: AssetKind::Image,
                priority,
                prefetch_limit,
                phase: Phase::Probe,
                input: Some(Arc::new(input)),
                size: [0; 2],
                prepared: None,
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
    pub(crate) fn pending_image_size(&self, id: LoadId) -> Option<[u32; 2]> {
        let service = self.0.borrow();
        let load = service.loads.get(&id)?;
        (load.kind == AssetKind::Image && load.size != [0; 2]).then_some(load.size)
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
        service.completion.usage().tickets != 0
            || service.input.usage().tickets != 0
            || service.stream_input.usage().tickets != 0
    }
    pub fn cancel(&self, id: LoadId) {
        self.0.borrow_mut().cancel_load(id);
    }
    pub fn output_pool(&self) -> BudgetPool {
        self.0.borrow().output.clone()
    }
    pub(crate) fn prefetch_available(&self, hidden_bytes: u64) -> u64 {
        let service = self.0.borrow();
        (service.options.resident_bytes / 4)
            .saturating_sub(service.recent_images.decoded_bytes())
            .saturating_sub(hidden_bytes)
    }
    pub(crate) fn resident_available(&self) -> u64 {
        self.0.borrow().output.available_bytes()
    }
    pub fn temporary_pool(&self) -> BudgetPool {
        self.0.borrow().temporary.clone()
    }
    /// Archive index residency and mounting peak, independent of decode workspace.
    /// Tickets remain charged until the IO worker releases their physical storage.
    pub fn metadata_pool(&self) -> BudgetPool {
        self.0.borrow().metadata.clone()
    }
}
impl Service {
    fn cancel_load(&mut self, id: LoadId) {
        if let Some(load) = self.loads.remove(&id) {
            if let Phase::AtlasWait(view) = &load.phase {
                self.release_atlas(&view.manifest.page);
            }
            match load.phase {
                Phase::Describing(id) | Phase::Reading(id) => {
                    self.io_loads.remove(&id);
                    self.io.as_mut().unwrap().cancel(id);
                }
                Phase::Probing(id) | Phase::Decoding(id) => {
                    self.work_loads.remove(&id);
                    self.executor.as_mut().unwrap().cancel(id);
                }
                _ => {}
            }
        }
    }
    fn pump(&mut self) {
        if self.io.is_none() {
            return;
        }
        // Cancelled requests keep their completion tickets until workers drain.
        // Their cleanup remains live even after the last consumer was removed.
        if self.loads.is_empty() && !self.wake.is_pending() && self.completion.usage().tickets == 0
        {
            return;
        }
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::AssetPump);
        #[cfg(test)]
        {
            self.pumps += 1;
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
            let Some(id) = self.io_loads.remove(&event.id) else {
                continue;
            };
            let load = self.loads.get_mut(&id).unwrap();
            let result = match event.result {
                Terminal::Completed(IoPayload::Described(info)) => {
                    accept_description(&self.origins, load, info)
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
                        if !described_version && let Some(lease) = self.origins.shared(load) {
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
            self.pending_bounded_decodes
                .retain(|(id, _)| *id != event.id);
            let Some(id) = self.work_loads.remove(&event.id) else {
                continue;
            };
            let load = self.loads.get_mut(&id).unwrap();
            let result = match event.result {
                Terminal::Completed(data) if matches!(load.phase, Phase::Probing(_)) => {
                    let bytes = data.as_slice();
                    if bytes.len() == TextureHeader::LEN {
                        TextureHeader::decode(bytes)
                            .map_err(|e| e.to_string())
                            .map(|header| load.prepare_pixels(PreparedPixels::Texture(header)))
                    } else if let Ok(bytes) = <&[u8; RasterHeader::LEN]>::try_from(bytes) {
                        RasterHeader::decode(bytes)
                            .ok_or_else(|| "invalid prepared raster probe result".into())
                            .map(|header| load.prepare_pixels(PreparedPixels::Raster(header)))
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
                        self.origins.remember(load.kind, lease);
                    }
                    load.phase = Phase::Done(result);
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
        let mut has_atlas = false;
        for (&id, load) in &mut loads {
            if let Err(error) = self.advance(id, load) {
                load.phase = Phase::Done(Err(error));
                load.input = None;
            }
            has_atlas |= matches!(load.phase, Phase::AtlasResolve(_) | Phase::AtlasWait(_));
        }
        self.loads = loads;
        if has_atlas {
            self.advance_atlases();
        }
        if progress.has_work || more_completions {
            self.wake.wake();
        }
    }
    fn reclaim_decoded_for(&mut self, priority: Priority, bytes: u64) {
        if priority != Priority::Prefetch {
            self.recent_images.make_room(&self.output, bytes);
        }
    }
    fn resident_error(&self, kind: &str, requested: u64, message: &str) -> String {
        let output = self.output.snapshot();
        let mut detail = format!(
            "{kind} resident budget: {message}; requested {requested} bytes, resident {} / {} bytes",
            output.usage.bytes, output.limits.max_bytes
        );
        if let Some((limit, usage)) = output.group {
            use std::fmt::Write;
            let _ = write!(
                detail,
                ", resident and metadata {} / {limit} bytes",
                usage.bytes
            );
        }
        #[cfg(target_os = "vita")]
        {
            use std::fmt::Write;
            let main = vita_native::allocator::main_usage();
            let _ = write!(
                detail,
                ", physical MAIN {} / {} bytes",
                main.committed_bytes, main.capacity_bytes
            );
            if let Ok(free) = vita_native::app::memory::free_memory() {
                let _ = write!(detail, ", kernel MAIN free {} bytes", free.main_bytes);
            }
        }
        detail
    }
    fn advance(&mut self, load_id: LoadId, load: &mut Load) -> Result<(), String> {
        let completion = |pool: &BudgetPool| pool.reserve(512).map_err(|e| e.message);
        if matches!(load.phase, Phase::Describe)
            && let Some(info) = self.io.as_ref().unwrap().indexed_info(&load.source)
        {
            accept_description(&self.origins, load, info)?;
        }
        match load.phase {
            Phase::Describe => match self.io.as_mut().unwrap().describe(DescribeRequest {
                resource: load.source.clone(),
                priority: load.priority,
                scope: Scope::Process,
                completion: completion(&self.completion)?,
            }) {
                Ok(id) => {
                    self.io_loads.insert(id, load_id);
                    load.phase = Phase::Describing(id);
                }
                Err(error) if error.reason.retryable() => {}
                Err(error) => return Err(error.reason.message),
            },
            Phase::Read(len) => {
                if u64::from(len) > self.options.ordinary_input_bytes() {
                    return Err(format!(
                        "asset {:?} requires {len} input bytes; ordinary input capacity is {} ({} reserved for streaming)",
                        load.source.key.0,
                        self.options.ordinary_input_bytes(),
                        self.options.stream_input_bytes()
                    ));
                }
                let Ok(output) = self.input.reserve(u64::from(len)) else {
                    if load.priority == Priority::Prefetch {
                        return Err("prefetch input capacity exhausted".into());
                    }
                    // Other reads/decoders own the input tickets. Keep this
                    // request pending until their existing maintenance releases them.
                    return Ok(());
                };
                match self.io.as_mut().unwrap().read(ReadRequest {
                    deadline: None,
                    resource: load.source.clone(),
                    offset: 0,
                    len,
                    priority: load.priority,
                    scope: Scope::Process,
                    output,
                    completion: completion(&self.completion)?,
                }) {
                    Ok(id) => {
                        self.io_loads.insert(id, load_id);
                        load.phase = Phase::Reading(id);
                    }
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
                load.output = load.input.as_ref().unwrap().as_slice().len() as u64;
                self.admit_prefetch(load)?;
                // Another request may have registered this version during this pump.
                if let Some(lease) = self.origins.shared(load) {
                    load.input = None;
                    load.phase = Phase::Done(Ok(lease));
                    return Ok(());
                }
                let input = load.input.take().unwrap();
                let mut bytes =
                    Arc::try_unwrap(input).expect("undecoded resource input has one owner");
                let requested = bytes.ticket().bytes();
                self.reclaim_decoded_for(load.priority, requested);
                bytes
                    .transfer_to(&self.output)
                    .map_err(|error| self.resident_error("asset", requested, &error.message))?;
                let result = register(load, bytes);
                if load.reloadable
                    && let Ok(lease) = &result
                {
                    self.origins.remember(load.kind, lease);
                }
                load.phase = Phase::Done(result);
            }
            Phase::Probe | Phase::Decode | Phase::DecodeBoundedImage => {
                let mut probe = matches!(load.phase, Phase::Probe);
                let decode_bytes = self.options.decode_bytes - self.io_workspace_bytes;
                if probe
                    && load
                        .input
                        .as_ref()
                        .unwrap()
                        .as_slice()
                        .starts_with(reel_core::prepared_atlas::PreparedAtlasView::MAGIC)
                {
                    let input = load.input.as_ref().unwrap().as_slice();
                    let mut storage = self
                        .metadata
                        .reserve(input.len() as u64 + 512)
                        .map_err(|e| e.message)?;
                    let manifest = reel_core::prepared_atlas::PreparedAtlasView::decode(input)
                        .map_err(str::to_owned)?;
                    storage
                        .resize((manifest.page.capacity() + 512) as u64)
                        .map_err(|e| e.message)?;
                    load.input = None;
                    load.phase = Phase::AtlasResolve(atlas::ViewRequest {
                        manifest,
                        _storage: storage,
                    });
                    return Ok(());
                }
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
                if probe
                    && let Some(png) =
                        crate::artwork::png_shape(load.input.as_ref().unwrap().as_slice())
                {
                    load.size = png.size;
                    load.output = png.output;
                    load.temporary = png.temporary(
                        decode_bytes,
                        self.builtin_image_decoder,
                        self.allow_png_pipeline
                            .then_some(decode_bytes / decode::MAX_IMAGE_WORKERS as u64),
                    );
                    load.phase = if png.bounded_metadata {
                        Phase::DecodeBoundedImage
                    } else {
                        Phase::Decode
                    };
                    probe = false;
                }
                if probe
                    && self.builtin_image_decoder
                    && let Some((size, output)) =
                        crate::artwork::qoi_shape(load.input.as_ref().unwrap().as_slice())
                {
                    load.size = size;
                    load.output = output;
                    load.temporary = 0;
                    load.phase = Phase::DecodeBoundedImage;
                    probe = false;
                }
                if probe
                    && self.builtin_image_decoder
                    && let Some((size, output, temporary)) =
                        crate::artwork::webp_shape(load.input.as_ref().unwrap().as_slice())
                    && temporary <= decode_bytes
                {
                    load.size = size;
                    load.output = output;
                    load.temporary = temporary;
                    load.phase = Phase::DecodeBoundedImage;
                    probe = false;
                }
                if probe
                    && let Some(prepared) =
                        PreparedPixels::parse(load.input.as_ref().unwrap().as_slice())
                {
                    let prepared = prepared?;
                    load.prepare_pixels(prepared);
                    probe = false;
                    self.admit_prefetch(load)?;
                    if let Some(lease) = self.origins.shared(load) {
                        load.input = None;
                        load.phase = Phase::Done(Ok(lease));
                        return Ok(());
                    }
                    let input = load.input.as_mut().unwrap();
                    self.reclaim_decoded_for(load.priority, input.ticket().bytes());
                    if let Some(input) = Arc::get_mut(input)
                        && input.transfer_to(&self.output).is_ok()
                    {
                        // Preserve the original allocation and expose its
                        // payload range. Retaining the small file header is
                        // charged too. If that capacity cannot fit, the old
                        // worker-copy path below retains its exact admission.
                        let input = Arc::try_unwrap(load.input.take().unwrap())
                            .expect("raw prepared input has one owner");
                        let end = input.as_slice().len();
                        let data = input
                            .into_slice(prepared.header_len()..end)
                            .map_err(|e| e.reason.message)?;
                        let result = register(load, data);
                        if load.reloadable
                            && let Ok(lease) = &result
                        {
                            self.origins.remember(load.kind, lease);
                        }
                        load.phase = Phase::Done(result);
                        return Ok(());
                    }
                }
                if !probe {
                    self.admit_prefetch(load)?;
                }
                if !probe && load.temporary > decode_bytes {
                    return Err("image decode limit exceeded".into());
                }
                let bounded_image = matches!(load.phase, Phase::DecodeBoundedImage);
                let workspace = if bounded_image || load.prepared.is_some() {
                    load.temporary
                } else {
                    decode_bytes
                };
                if bounded_image {
                    if self.pending_bounded_decodes.len() >= decode::MAX_IMAGE_WORKERS
                        || self
                            .pending_bounded_decodes
                            .iter()
                            .any(|(_, source)| source == &load.source)
                        || self.temporary.available_bytes() < workspace
                    {
                        return Ok(());
                    }
                } else if self.temporary.usage().bytes > self.io_workspace_bytes {
                    return Ok(());
                }
                // A concurrent request may have completed while this one waited
                // for the decoder lane. Check once when this lane is available,
                // rather than scanning resources on every pending host tick.
                if let Some(lease) = self.origins.shared(load) {
                    load.input = None;
                    load.phase = Phase::Done(Ok(lease));
                    return Ok(());
                }
                let output_bytes = if probe {
                    TextureHeader::LEN as u64
                } else {
                    load.output
                };
                self.reclaim_decoded_for(load.priority, output_bytes);
                let output = match self.output.reserve(output_bytes) {
                    Ok(ticket) => ticket,
                    Err(_) if !self.pending_bounded_decodes.is_empty() => return Ok(()),
                    Err(error) => {
                        return Err(self.resident_error("image", output_bytes, &error.message));
                    }
                };
                let temporary = self
                    .temporary
                    .reserve(workspace)
                    .map_err(|e| format!("image decode limit: {}", e.message))?;
                let completion = completion(&self.completion)?;
                let input = if probe {
                    load.input.as_ref().unwrap().clone()
                } else {
                    load.input.take().unwrap()
                };
                let request = WorkRequest {
                    kind: if probe {
                        decode::image_probe()
                    } else {
                        decode::image_decode()
                    },
                    input,
                    scope: Scope::Process,
                    priority: load.priority,
                    reservations: WorkReservations {
                        output,
                        temporary,
                        completion,
                    },
                };
                match self.executor.as_mut().unwrap().submit(request) {
                    Ok(id) => {
                        self.work_loads.insert(id, load_id);
                        if bounded_image {
                            self.pending_bounded_decodes.push((id, load.source.clone()));
                        }
                        load.phase = if probe {
                            Phase::Probing(id)
                        } else {
                            Phase::Decoding(id)
                        }
                    }
                    Err(error) if error.reason.retryable() => {
                        if !probe {
                            load.input = Some(error.request.input);
                        }
                    }
                    Err(error) => return Err(error.reason.message),
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl Service {
    fn admit_prefetch(&self, load: &Load) -> Result<(), String> {
        if load.priority != Priority::Prefetch {
            return Ok(());
        }
        let header = load
            .prepared
            .map_or(0, |prepared| prepared.header_len() as u64);
        let limit = load.prefetch_limit;
        #[cfg(target_os = "vita")]
        let limit = limit.min(
            self.options
                .resident_bytes
                .saturating_sub(self.output.usage().bytes),
        );
        if load.output.saturating_add(header) > limit {
            return Err("prefetch resident capacity exhausted".into());
        }
        Ok(())
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.streams.close();
    }
}
fn accept_description(
    origins: &origins::Origins,
    load: &mut Load,
    info: AssetInfo,
) -> Result<(), String> {
    if load
        .source
        .version
        .is_some_and(|version| info.version != Some(version))
    {
        return Err("asset adapter described a different version".into());
    }
    let length = info
        .length
        .and_then(|length| u32::try_from(length).ok())
        .ok_or("asset must have a bounded length for preparation")?;
    load.source.version = info.version;
    load.phase = origins
        .shared(load)
        .map_or(Phase::Read(length), |lease| Phase::Done(Ok(lease)));
    Ok(())
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
    let handle = match (load.kind, load.prepared) {
        (AssetKind::Bytes, _) => return Err("raw bytes do not use the UI registry".into()),
        (AssetKind::Font, _) => registry.register_font(version, data),
        (AssetKind::EffectShader, _) => {
            registry.register_effect_shader(version, shader.unwrap(), data)
        }
        (AssetKind::Image, Some(PreparedPixels::Texture(texture))) => registry.register_texture(
            version,
            TexturePayload::new(texture.descriptor(), data).map_err(|e| e.to_string())?,
        ),
        (AssetKind::Image, _) => registry.register_image(
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
