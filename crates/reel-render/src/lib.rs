//! Backend contracts, bounded semantic consumers, and an optional real wgpu adapter.
#![allow(
    clippy::result_large_err,
    reason = "upload refusal returns the intact owned batch and its budget tickets"
)]
/// Desktop retains shared source packing. Vita keeps independent source
/// textures until shared placement preserves the hardware pixel oracle.
#[cfg(any(feature = "wgpu", feature = "gxm", feature = "profiling"))]
const SHARED_SOURCE_ATLAS_DEFAULT: bool = !cfg!(target_os = "vita");

mod capture_color;
#[cfg(any(feature = "wgpu", feature = "gxm"))]
mod common;
#[cfg(feature = "profiling")]
pub mod profiling;
#[cfg(any(feature = "wgpu", feature = "gxm"))]
pub use common::usage::Usage;
/// CPU-only generation of complete prepared stage-effect fragments.
#[path = "common/effect.rs"]
pub mod effect;
#[cfg(feature = "wgpu")]
pub mod gpu;
#[cfg(all(feature = "gxm", target_os = "vita"))]
pub mod gxm;
#[cfg(all(test, any(feature = "wgpu", feature = "gxm")))]
mod test_support;
use reel_core::{
    budget::{BudgetCategory, BudgetPool, BudgetTicket, OwnedBytes, QueueLimits},
    identity::{FrameId, ResourceVersion, Scope, SurfaceEpoch},
    result::{RejectCode, RejectReason, Rejected, Terminal},
};
use reel_ui::bridge::{PresentationNotice, PresentationOutcome};
use reel_ui::frame::RenderFrame;
use reel_ui::resources::{ColorEncoding, ResourceHandle, ResourceLease, ResourceWeak};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UploadId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubmissionId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaMode {
    Opaque,
    Straight,
    Premultiplied,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceInfo {
    pub physical_size: [u32; 2],
    pub scale_factor: f64,
    pub surface_epoch: SurfaceEpoch,
    pub color_encoding: ColorEncoding,
    pub alpha_mode: AlphaMode,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsumerKind {
    Recording,
    Null,
    Device,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderCaps {
    pub consumer: ConsumerKind,
    pub rasterizes: bool,
    pub formats: &'static [PixelFormat],
    /// Accepted semantic records; rasterizes=false makes no device effect claim.
    pub effects: &'static [RenderEffect],
    pub max_texture_size: u32,
    pub max_offscreen_size: u32,
    pub upload_limits: QueueLimits,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderEffect {
    Groups,
    Clips,
    AlphaMask,
    LuminanceMask,
    ImageMask,
    /// Prepared typed subtree fragments with immutable code and live uniforms.
    ShaderEffect,
    Filters,
    External,
}
pub use reel_core::texture::TextureFormat as PixelFormat;
#[derive(Debug)]
pub enum UploadPayload {
    /// Whole prepared storage; compressed formats use block rows rather than pixel stride.
    Texture {
        descriptor: reel_core::texture::TextureDescriptor,
        bytes: OwnedBytes,
    },
    Pixels {
        size: [u32; 2],
        stride: u64,
        format: PixelFormat,
        encoding: ColorEncoding,
        alpha: AlphaMode,
        bytes: OwnedBytes,
    },
    Font {
        bytes: OwnedBytes,
    },
}
impl UploadPayload {
    fn bytes(&self) -> &OwnedBytes {
        match self {
            Self::Pixels { bytes, .. } | Self::Font { bytes } | Self::Texture { bytes, .. } => {
                bytes
            }
        }
    }
}
#[derive(Debug)]
pub struct UploadItem {
    pub resource: ResourceLease,
    pub payload: UploadPayload,
}
#[derive(Debug)]
pub struct UploadBatch {
    pub items: Vec<UploadItem>,
    pub scope: Scope,
    pub storage: BudgetTicket,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderError {
    InvalidSurface,
    WrongSurface,
    ResourcesUnavailable,
    Busy,
    IdentityExhausted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// Retry after submission completion frees shared capacity.
    Pressure,
    /// The current frame cannot be rendered; an accepted frame may be repainted.
    Refused,
    /// Recreate/reconfigure the native surface under a new input epoch.
    SurfaceLost,
    DeviceLost,
    Fatal,
}
/// Budgeted missing CPU pixels required by an exact render or capture attempt.
#[derive(Debug)]
pub struct PixelDemand {
    resources: Vec<ResourceLease>,
    _storage: BudgetTicket,
}
impl PixelDemand {
    /// Admit a nonempty, capacity-accounted list, deduplicated by resource handle.
    /// Rejection returns the original vector and ticket to the producer.
    pub fn new(
        mut resources: Vec<ResourceLease>,
        storage: BudgetTicket,
    ) -> Result<Self, (Vec<ResourceLease>, BudgetTicket)> {
        let capacity = resources.capacity().checked_mul(size_of::<ResourceLease>());
        if resources.is_empty() || capacity.is_none_or(|bytes| bytes as u64 > storage.bytes()) {
            return Err((resources, storage));
        }
        resources.sort_unstable_by_key(ResourceLease::handle);
        resources.dedup_by_key(|resource| resource.handle());
        Ok(Self {
            resources,
            _storage: storage,
        })
    }
    /// Test whether the demand includes this immutable resource identity.
    pub fn contains(&self, handle: ResourceHandle) -> bool {
        self.resources
            .binary_search_by_key(&handle, ResourceLease::handle)
            .is_ok()
    }
    /// The immutable source identities that must be restored before retrying.
    pub fn resources(&self) -> &[ResourceLease] {
        &self.resources
    }
}

pub trait BackendError: std::error::Error {
    fn kind(&self) -> FailureKind;
    /// Extract an asynchronous pixel demand while preserving every other error.
    fn into_pixel_demand(self) -> Result<PixelDemand, Self>
    where
        Self: Sized,
    {
        Err(self)
    }
}
impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for RenderError {}
impl BackendError for RenderError {
    fn kind(&self) -> FailureKind {
        match self {
            Self::Busy => FailureKind::Pressure,
            Self::IdentityExhausted => FailureKind::Fatal,
            _ => FailureKind::Refused,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    NullConsumer,
    Suspended,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentOutcome {
    PresentAccepted(FrameId),
    /// Internal GPU work continues preparation of this exact candidate.
    /// Keep its acceptance and geometry pending, then retry it after
    /// `ResourcesAvailable`. Scripts and input may continue meanwhile.
    Preparing,
    Skipped(SkipReason),
    SurfaceUnavailable,
}
#[derive(Debug)]
pub enum RenderEvent<E = RenderError> {
    /// Internal uploads completed and released temporary capacity. This wakes
    /// deferred presentation without accepting a frame or changing input geometry.
    ResourcesAvailable,
    UploadFinished {
        id: UploadId,
        scope: Scope,
        result: Terminal<(), E>,
    },
    /// Submission completion, distinct from presentation acceptance.
    RenderFinished {
        id: SubmissionId,
        frame: FrameId,
    },
    ResourcesRetired {
        versions: Vec<ResourceVersion>,
        storage: Arc<BudgetTicket>,
    },
    DeviceLost(E),
}
pub mod capture;
pub trait RenderBackend {
    /// Identity of the current device; a replacement device uses a new value.
    fn device_generation(&self) -> u64 {
        0
    }
    /// Some reports the current setting when this backend can change synchronization.
    fn vsync(&self) -> Option<bool> {
        None
    }
    /// None means this target does not support changing presentation synchronization.
    fn set_vsync(&mut self, _enabled: bool) -> Result<Option<bool>, Self::Error> {
        Ok(None)
    }
    type Error: BackendError + 'static;
    fn capabilities(&self) -> RenderCaps;
    fn configure(&mut self, info: SurfaceInfo) -> Result<(), Self::Error>;
    fn upload(&mut self, batch: UploadBatch) -> Result<UploadId, Rejected<UploadBatch>>;
    /// Call before_present only after successful submission, immediately before present.
    fn render_present(
        &mut self,
        frame: &RenderFrame,
        before_present: impl FnOnce(),
    ) -> Result<PresentOutcome, Self::Error>;
    /// Cancel continuation of this exact candidate. Submitted GPU owners remain
    /// retained until completion; this never accepts a frame or changes input.
    fn cancel_preparation(&mut self, _frame: &RenderFrame) {}
    fn retire(&mut self, resources: &[ResourceVersion]);
    fn poll(
        &mut self,
        out: &mut Vec<RenderEvent<Self::Error>>,
        limit: usize,
    ) -> Result<(), Self::Error>;
    fn has_pending(&self) -> bool;
    fn suspend(&mut self);
}
/// A player may borrow a process-owned device without taking its lifetime.
impl<B: RenderBackend + ?Sized> RenderBackend for &mut B {
    type Error = B::Error;
    fn device_generation(&self) -> u64 {
        (**self).device_generation()
    }
    fn vsync(&self) -> Option<bool> {
        (**self).vsync()
    }
    fn set_vsync(&mut self, enabled: bool) -> Result<Option<bool>, Self::Error> {
        (**self).set_vsync(enabled)
    }
    fn capabilities(&self) -> RenderCaps {
        (**self).capabilities()
    }
    fn configure(&mut self, info: SurfaceInfo) -> Result<(), Self::Error> {
        (**self).configure(info)
    }
    fn upload(&mut self, batch: UploadBatch) -> Result<UploadId, Rejected<UploadBatch>> {
        (**self).upload(batch)
    }
    fn render_present(
        &mut self,
        frame: &RenderFrame,
        before_present: impl FnOnce(),
    ) -> Result<PresentOutcome, Self::Error> {
        (**self).render_present(frame, before_present)
    }
    fn retire(&mut self, resources: &[ResourceVersion]) {
        (**self).retire(resources);
    }
    fn cancel_preparation(&mut self, frame: &RenderFrame) {
        (**self).cancel_preparation(frame);
    }
    fn poll(
        &mut self,
        out: &mut Vec<RenderEvent<Self::Error>>,
        limit: usize,
    ) -> Result<(), Self::Error> {
        (**self).poll(out, limit)
    }
    fn has_pending(&self) -> bool {
        (**self).has_pending()
    }
    fn suspend(&mut self) {
        (**self).suspend();
    }
}
/// The coordinator supplies the current surface epoch and the exact borrowed frame key.
/// Upload/build completion never calls this acceptance gate. `Preparing` has no
/// terminal notice: its candidate and geometry remain pending for continuation.
pub fn presentation_notice<E>(
    frame: &RenderFrame,
    epoch: SurfaceEpoch,
    result: Result<PresentOutcome, E>,
) -> Option<PresentationNotice> {
    if matches!(result, Ok(PresentOutcome::Preparing)) {
        return None;
    }
    Some(PresentationNotice::new(
        frame.key(),
        epoch,
        match result {
            Ok(PresentOutcome::PresentAccepted(id)) => PresentationOutcome::Accepted(id),
            Ok(PresentOutcome::Preparing) => unreachable!("continuation has no terminal notice"),
            Ok(PresentOutcome::Skipped(_)) => PresentationOutcome::Skipped,
            Ok(PresentOutcome::SurfaceUnavailable) => PresentationOutcome::SurfaceUnavailable,
            Err(_) => PresentationOutcome::Error,
        },
    ))
}
#[derive(Clone, Copy, Debug)]
pub struct ConsumerLimits {
    pub max_uploads: u32,
    pub max_resources: usize,
    pub max_frames: usize,
    pub max_events: usize,
    /// Aggregate payload-capacity tickets in the pending upload queue.
    pub max_upload_bytes: u64,
    pub max_retained_bytes: u64,
    pub max_metadata_bytes: u64,
    pub max_fixed_bytes: u64,
}
struct PendingUpload {
    id: UploadId,
    batch: UploadBatch,
    metadata: Arc<BudgetTicket>,
}
struct Resident {
    handle: ResourceHandle,
    version: ResourceVersion,
    source: ResourceWeak,
    lease: Option<ResourceLease>,
    payload: UploadPayload,
    retired: bool,
    metadata: Arc<BudgetTicket>,
}
struct Submission {
    id: SubmissionId,
    frame: RenderFrame,
}
pub struct RecordingBackend {
    limits: ConsumerLimits,
    surface: Option<SurfaceInfo>,
    suspended: bool,
    uploads: Vec<PendingUpload>,
    resident: Vec<Resident>,
    inflight: Vec<Submission>,
    events: Vec<RenderEvent>,
    metadata: BudgetPool,
    next_upload: u64,
    next_submit: u64,
    last_submission: Option<SubmissionId>,
    _fixed: BudgetTicket,
}
impl RecordingBackend {
    pub fn new(limits: ConsumerLimits) -> Result<Self, RenderError> {
        let bytes = (limits.max_uploads as usize)
            .checked_mul(size_of::<PendingUpload>())
            .and_then(|n| n.checked_add(limits.max_resources.checked_mul(size_of::<Resident>())?))
            .and_then(|n| n.checked_add(limits.max_frames.checked_mul(size_of::<Submission>())?))
            .and_then(|n| n.checked_add(limits.max_events.checked_mul(size_of::<RenderEvent>())?))
            .ok_or(RenderError::Busy)?;
        let fixed = BudgetPool::new(
            BudgetCategory::Temporary,
            QueueLimits {
                max_requests: 1,
                max_bytes: limits.max_fixed_bytes,
            },
        )
        .reserve(bytes as u64)
        .map_err(|_| RenderError::Busy)?;
        let requests = limits
            .max_resources
            .checked_add(limits.max_uploads as usize)
            .and_then(|n| n.try_into().ok())
            .ok_or(RenderError::Busy)?;
        let metadata = BudgetPool::new(
            BudgetCategory::Temporary,
            QueueLimits {
                max_requests: requests,
                max_bytes: limits.max_metadata_bytes,
            },
        );
        Ok(Self {
            limits,
            surface: None,
            suspended: false,
            uploads: Vec::with_capacity(limits.max_uploads as usize),
            resident: Vec::with_capacity(limits.max_resources),
            inflight: Vec::with_capacity(limits.max_frames),
            events: Vec::with_capacity(limits.max_events),
            metadata,
            next_upload: 1,
            next_submit: 1,
            last_submission: None,
            _fixed: fixed,
        })
    }
    pub fn last_submission(&self) -> Option<SubmissionId> {
        self.last_submission
    }
    pub fn inflight_count(&self) -> usize {
        self.inflight.len()
    }
    pub fn resident_count(&self) -> usize {
        self.resident.len()
    }
    pub fn pending_upload_count(&self) -> usize {
        self.uploads.len()
    }
    pub fn retained_bytes(&self) -> u64 {
        self.resident
            .iter()
            .map(|r| r.payload.bytes().ticket().bytes())
            .sum::<u64>()
            + self.queued_upload_bytes()
    }
    pub fn queued_upload_bytes(&self) -> u64 {
        self.uploads
            .iter()
            .flat_map(|u| &u.batch.items)
            .map(|i| i.payload.bytes().ticket().bytes())
            .sum()
    }
    fn obligations(&self) -> usize {
        self.events.len()
            + self.inflight.len()
            + self.uploads.len()
            + self.resident.len()
            + self
                .uploads
                .iter()
                .map(|u| u.batch.items.len())
                .sum::<usize>()
    }
    fn slots(&self) -> usize {
        self.resident.len()
            + self
                .uploads
                .iter()
                .map(|u| u.batch.items.len())
                .sum::<usize>()
    }
    fn has_resource(&self, handle: ResourceHandle) -> bool {
        self.resident.iter().any(|r| r.handle == handle)
            || self
                .uploads
                .iter()
                .any(|u| u.batch.items.iter().any(|i| i.resource.handle() == handle))
    }
    /// Caller-driven logical upload completion. The recording consumer retains CPU
    /// upload storage as its simulated residency; it allocates no GPU storage.
    pub fn complete_upload(&mut self, id: UploadId, result: Terminal<(), RenderError>) -> bool {
        let Some(index) = self.uploads.iter().position(|u| u.id == id) else {
            return false;
        };
        let PendingUpload {
            batch, metadata, ..
        } = self.uploads.remove(index);
        let UploadBatch {
            items,
            scope,
            storage,
        } = batch;
        if matches!(result, Terminal::Completed(())) {
            for item in items {
                let resource = item.resource;
                let retired = resource.is_retired();
                self.resident.push(Resident {
                    handle: resource.handle(),
                    version: resource.version().clone(),
                    source: resource.downgrade(),
                    lease: if retired { None } else { Some(resource) },
                    payload: item.payload,
                    retired,
                    metadata: metadata.clone(),
                });
            }
        } else {
            // Return vector capacity only after its items and payloads are freed.
            drop(items);
        }
        drop(storage);
        self.events
            .push(RenderEvent::UploadFinished { id, scope, result });
        self.collect_retired();
        true
    }
    /// PresentAccepted alone retains resources. Only this separate simulated
    /// completion releases the backend's owned frame lease.
    pub fn complete_render(&mut self, id: SubmissionId) -> bool {
        let Some(index) = self.inflight.iter().position(|s| s.id == id) else {
            return false;
        };
        let submission = self.inflight.remove(index);
        let frame = submission.frame.frame_id();
        drop(submission);
        self.events.push(RenderEvent::RenderFinished { id, frame });
        self.collect_retired();
        true
    }
    fn collect_retired(&mut self) {
        let mut index = 0;
        while index < self.resident.len() {
            if self.resident[index].retired && self.resident[index].source.is_released() {
                let Resident {
                    version,
                    payload,
                    metadata,
                    ..
                } = self.resident.remove(index);
                // Version vector storage was included in the accepted metadata reservation.
                drop(payload);
                self.events.push(RenderEvent::ResourcesRetired {
                    versions: vec![version],
                    storage: metadata,
                });
            } else {
                index += 1;
            }
        }
    }
}
fn validate_batch(batch: &UploadBatch, max_bytes: u64) -> Result<u64, RejectCode> {
    if batch.items.is_empty()
        || (batch.items.capacity() as u64)
            .checked_mul(size_of::<UploadItem>() as u64)
            .is_none_or(|n| n > batch.storage.bytes())
    {
        return Err(RejectCode::InvalidRequest);
    }
    let mut bytes = 0u64;
    for (index, item) in batch.items.iter().enumerate() {
        if batch.items[..index]
            .iter()
            .any(|other| other.resource.handle() == item.resource.handle())
        {
            return Err(RejectCode::InvalidRequest);
        }
        bytes = bytes
            .checked_add(item.payload.bytes().ticket().bytes())
            .filter(|n| *n <= max_bytes)
            .ok_or(RejectCode::InvalidRequest)?;
        match &item.payload {
            UploadPayload::Texture { descriptor, bytes } => {
                let Some(source) = item.resource.texture_descriptor() else {
                    return Err(RejectCode::Unsupported);
                };
                if *descriptor != source || descriptor.validate(bytes.as_slice().len()).is_err() {
                    return Err(RejectCode::InvalidRequest);
                }
            }
            UploadPayload::Pixels {
                size,
                stride,
                format,
                bytes,
                ..
            } => {
                let Some(image) = item.resource.image_descriptor() else {
                    return Err(RejectCode::Unsupported);
                };
                let row = (size[0] as u64)
                    .checked_mul(format.pixel_bytes().ok_or(RejectCode::Unsupported)?)
                    .ok_or(RejectCode::InvalidRequest)?;
                let len = stride
                    .checked_mul(size[1] as u64)
                    .ok_or(RejectCode::InvalidRequest)?;
                if *size != [image.width, image.height]
                    || row > *stride
                    || len != bytes.as_slice().len() as u64
                {
                    return Err(RejectCode::InvalidRequest);
                }
            }
            UploadPayload::Font { bytes } => {
                let reel_ui::resources::ResourceView::Font(font) = item.resource.view() else {
                    return Err(RejectCode::Unsupported);
                };
                if bytes.as_slice().len() != font.len() {
                    return Err(RejectCode::InvalidRequest);
                }
            }
        }
    }
    Ok(bytes)
}
impl RenderBackend for RecordingBackend {
    type Error = RenderError;
    fn capabilities(&self) -> RenderCaps {
        RenderCaps {
            consumer: ConsumerKind::Recording,
            rasterizes: false,
            formats: &[
                PixelFormat::Rgba8,
                PixelFormat::Bgra8,
                PixelFormat::LinearRgba16Float,
                PixelFormat::Bc1Rgb,
                PixelFormat::Bc3Rgba,
            ],
            effects: &[
                RenderEffect::Groups,
                RenderEffect::Clips,
                RenderEffect::AlphaMask,
                RenderEffect::LuminanceMask,
                RenderEffect::ImageMask,
                RenderEffect::ShaderEffect,
                RenderEffect::Filters,
                RenderEffect::External,
            ],
            max_texture_size: u32::MAX,
            max_offscreen_size: 0,
            upload_limits: QueueLimits {
                max_requests: self.limits.max_uploads,
                max_bytes: self.limits.max_upload_bytes,
            },
        }
    }
    fn configure(&mut self, info: SurfaceInfo) -> Result<(), RenderError> {
        if !info.scale_factor.is_finite()
            || info.scale_factor <= 0.
            || self
                .surface
                .is_some_and(|s| info.surface_epoch < s.surface_epoch)
        {
            return Err(RenderError::InvalidSurface);
        }
        self.surface = Some(info);
        self.suspended = false;
        Ok(())
    }
    fn upload(&mut self, batch: UploadBatch) -> Result<UploadId, Rejected<UploadBatch>> {
        let reject = |batch, code| Rejected {
            request: batch,
            reason: RejectReason::new(code, "recording upload rejected before acceptance"),
        };
        if batch.items.len() > self.limits.max_resources {
            return Err(reject(batch, RejectCode::Busy));
        }
        let bytes = match validate_batch(&batch, self.limits.max_upload_bytes) {
            Ok(bytes) => bytes,
            Err(code) => return Err(reject(batch, code)),
        };
        if self.uploads.len() >= self.limits.max_uploads as usize
            || self
                .queued_upload_bytes()
                .checked_add(bytes)
                .is_none_or(|n| n > self.limits.max_upload_bytes)
            || batch.items.len() > self.limits.max_resources - self.slots()
            || batch.items.len() + 1 > self.limits.max_events - self.obligations()
            || self
                .retained_bytes()
                .checked_add(bytes)
                .is_none_or(|n| n > self.limits.max_retained_bytes)
            || batch
                .items
                .iter()
                .any(|i| self.has_resource(i.resource.handle()))
        {
            return Err(reject(batch, RejectCode::Busy));
        }
        let Some(next) = self.next_upload.checked_add(1) else {
            return Err(reject(batch, RejectCode::Unavailable));
        };
        let meta = batch.items.iter().try_fold(
            size_of::<BudgetTicket>() + 2 * size_of::<usize>(),
            |n, i| n.checked_add(size_of::<ResourceVersion>() + i.resource.version().key.0.len()),
        );
        let Some(meta) = meta else {
            return Err(reject(batch, RejectCode::OutOfMemory));
        };
        let metadata = match self.metadata.reserve(meta as u64) {
            Ok(ticket) => Arc::new(ticket),
            Err(_) => return Err(reject(batch, RejectCode::Busy)),
        };
        let id = UploadId(self.next_upload);
        self.next_upload = next;
        self.uploads.push(PendingUpload {
            id,
            batch,
            metadata,
        });
        Ok(id)
    }
    fn render_present(
        &mut self,
        frame: &RenderFrame,
        before_present: impl FnOnce(),
    ) -> Result<PresentOutcome, RenderError> {
        let Some(surface) = self.surface else {
            return Ok(PresentOutcome::SurfaceUnavailable);
        };
        if frame.surface_epoch() != surface.surface_epoch
            || frame.info().physical_size != surface.physical_size
        {
            return Err(RenderError::WrongSurface);
        }
        if self.suspended {
            return Ok(PresentOutcome::Skipped(SkipReason::Suspended));
        }
        if surface.physical_size.contains(&0) {
            return Ok(PresentOutcome::SurfaceUnavailable);
        }
        if frame.resources().leases().iter().any(|r| {
            !matches!(r.view(), reel_ui::resources::ResourceView::External { .. })
                && !self.resident.iter().any(|ready| ready.handle == r.handle())
        }) {
            return Err(RenderError::ResourcesUnavailable);
        }
        if self.inflight.len() >= self.limits.max_frames
            || self.obligations() >= self.limits.max_events
        {
            return Err(RenderError::Busy);
        }
        let next = self
            .next_submit
            .checked_add(1)
            .ok_or(RenderError::IdentityExhausted)?;
        let id = SubmissionId(self.next_submit);
        self.next_submit = next;
        self.inflight.push(Submission {
            id,
            frame: frame.clone(),
        });
        self.last_submission = Some(id);
        before_present();
        Ok(PresentOutcome::PresentAccepted(frame.frame_id()))
    }
    fn retire(&mut self, resources: &[ResourceVersion]) {
        for upload in &self.uploads {
            for item in &upload.batch.items {
                if resources.contains(item.resource.version()) {
                    item.resource.prohibit_new_leases();
                }
            }
        }
        for resident in &mut self.resident {
            if resources.contains(&resident.version) {
                resident.retired = true;
                if let Some(lease) = resident.lease.take() {
                    lease.prohibit_new_leases();
                }
            }
        }
        self.collect_retired();
    }
    fn poll(&mut self, out: &mut Vec<RenderEvent>, limit: usize) -> Result<(), RenderError> {
        self.collect_retired();
        out.extend(self.events.drain(..limit.min(self.events.len())));
        Ok(())
    }
    fn has_pending(&self) -> bool {
        !self.inflight.is_empty()
            || !self.uploads.is_empty()
            || !self.events.is_empty()
            || self.resident.iter().any(|resource| resource.retired)
    }
    fn suspend(&mut self) {
        self.suspended = true;
    }
}
/// A logical consumer that always reports an explicit skip, so it cannot install
/// input geometry or stand in for a device's presentation success.
pub struct NullBackend(RecordingBackend);
impl NullBackend {
    pub fn new(limits: ConsumerLimits) -> Result<Self, RenderError> {
        Ok(Self(RecordingBackend::new(limits)?))
    }
    pub fn complete_upload(&mut self, id: UploadId, result: Terminal<(), RenderError>) -> bool {
        self.0.complete_upload(id, result)
    }
}
impl RenderBackend for NullBackend {
    type Error = RenderError;
    fn capabilities(&self) -> RenderCaps {
        RenderCaps {
            consumer: ConsumerKind::Null,
            ..self.0.capabilities()
        }
    }
    fn configure(&mut self, info: SurfaceInfo) -> Result<(), RenderError> {
        self.0.configure(info)
    }
    fn upload(&mut self, batch: UploadBatch) -> Result<UploadId, Rejected<UploadBatch>> {
        self.0.upload(batch)
    }
    fn render_present(
        &mut self,
        _: &RenderFrame,
        _: impl FnOnce(),
    ) -> Result<PresentOutcome, RenderError> {
        Ok(PresentOutcome::Skipped(SkipReason::NullConsumer))
    }
    fn retire(&mut self, resources: &[ResourceVersion]) {
        self.0.retire(resources)
    }
    fn poll(&mut self, out: &mut Vec<RenderEvent>, limit: usize) -> Result<(), RenderError> {
        self.0.poll(out, limit)
    }
    fn has_pending(&self) -> bool {
        self.0.has_pending()
    }
    fn suspend(&mut self) {
        self.0.suspend()
    }
}
