// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Prepared resource identities. Blob IDs are lookup hints, never content digests.
mod effect;
use crate::peniko::{Blob, FontData, ImageAlphaType, ImageData, ImageFormat};
pub use effect::{
    EFFECT_SHADER_ABI, EffectShaderDescriptor, EffectShaderFormat, EffectShaderPayloadError,
};
use reel_core::budget::{
    BudgetCategory, BudgetPool, BudgetTicket, BudgetUsage, OwnedBytes, QueueLimits,
};
pub use reel_core::identity::ResourceVersion;
use reel_core::texture::{TextureDescriptor, TexturePayload};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct ResourceHandle {
    pub registry: u64,
    pub slot: u32,
    pub generation: u64,
}
pub use reel_core::texture::ColorEncoding;
#[derive(Debug)]
pub enum ResourceData {
    /// A complete immutable platform fragment with its portable parameter schema.
    EffectShader {
        descriptor: EffectShaderDescriptor,
        bytes: Blob<u8>,
    },
    /// Validated native sample bytes, in portable row-major layout and linear premultiplied color.
    Texture {
        descriptor: TextureDescriptor,
        bytes: Blob<u8>,
    },
    Image {
        image: ImageData,
        encoding: ColorEncoding,
    },
    Yuv420(reel_core::image::Yuv420Image),
    Font(Blob<u8>),
    External {
        token: u64,
    },
}
impl ResourceData {
    fn blob(&self) -> Option<&Blob<u8>> {
        match self {
            Self::EffectShader { bytes, .. } => Some(bytes),
            Self::Texture { bytes, .. } => Some(bytes),
            Self::Image { image, .. } => Some(&image.data),
            Self::Font(font) => Some(font),
            Self::External { .. } | Self::Yuv420(_) => None,
        }
    }
    fn same_source(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::EffectShader {
                    descriptor: a,
                    bytes: ab,
                },
                Self::EffectShader {
                    descriptor: b,
                    bytes: bb,
                },
            ) => a == b && same_blob(ab, bb),
            (
                Self::Texture {
                    descriptor: a,
                    bytes: ab,
                },
                Self::Texture {
                    descriptor: b,
                    bytes: bb,
                },
            ) => a == b && same_blob(ab, bb),
            (
                Self::Image {
                    image: a,
                    encoding: ae,
                },
                Self::Image {
                    image: b,
                    encoding: be,
                },
            ) => ae == be && same_image(a, b),
            (Self::Font(a), Self::Font(b)) => same_blob(a, b),
            (Self::Yuv420(a), Self::Yuv420(b)) => a.same_storage(b),
            (Self::External { token: a }, Self::External { token: b }) => a == b,
            _ => false,
        }
    }
    fn bytes(&self) -> usize {
        match self {
            Self::Yuv420(image) => image.byte_len(),
            _ => self.blob().map_or(0, Blob::len),
        }
    }
    pub fn image_size(&self) -> Option<[u32; 2]> {
        match self {
            Self::Texture { descriptor, .. } => Some(descriptor.layout.logical_size),
            Self::Image { image, .. } => Some([image.width, image.height]),
            Self::Yuv420(image) => Some(image.size()),
            _ => None,
        }
    }
    /// Describe prepared texture storage independently of its authored extent.
    pub fn texture_descriptor(&self) -> Option<TextureDescriptor> {
        match self {
            Self::Texture { descriptor, .. } => Some(*descriptor),
            _ => None,
        }
    }
}
fn same_blob(a: &Blob<u8>, b: &Blob<u8>) -> bool {
    if a.id() != b.id() {
        return false;
    }
    let (aa, _) = a.clone().into_raw_parts();
    let (bb, _) = b.clone().into_raw_parts();
    Arc::ptr_eq(&aa, &bb)
}
fn same_image(a: &ImageData, b: &ImageData) -> bool {
    a.width == b.width
        && a.height == b.height
        && a.format == b.format
        && a.alpha_type == b.alpha_type
        && same_blob(&a.data, &b.data)
}
fn lookup_same_source(a: &ResourceData, b: &ResourceData) -> bool {
    match (a, b) {
        (ResourceData::Image { image: a, .. }, ResourceData::Image { image: b, .. }) => {
            same_image(a, b)
        }
        _ => a.same_source(b),
    }
}
#[derive(Debug)]
struct AccountedBlob {
    bytes: OwnedBytes,
    // Every producer/cache Blob alias also retains the backing allocation reservation.
    _storage: BudgetTicket,
}
impl AsRef<[u8]> for AccountedBlob {
    fn as_ref(&self) -> &[u8] {
        self.bytes.as_slice()
    }
}
#[derive(Debug)]
struct Resource {
    handle: ResourceHandle,
    version: ResourceVersion,
    data: ResourceData,
    retired: AtomicBool,
    // Opaque imported producer storage has its own accounting domain. This reservation
    // covers the frozen reference's declared extent; owned blobs carry their own ticket.
    _reference_ticket: Option<BudgetTicket>,
    descriptor_ticket: Arc<BudgetTicket>,
}
#[derive(Clone, Debug)]
pub struct ResourceLease(Arc<Resource>);

/// An explicit producer resource used as the brush of this widget's recorded
/// image fills. No encoded pixels or fabricated RGBA image descriptor is needed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResourceImageBrush {
    pub resource: ResourceHandle,
    pub sampler: crate::peniko::ImageSampler,
}
impl ResourceLease {
    pub fn handle(&self) -> ResourceHandle {
        self.0.handle
    }
    pub fn version(&self) -> &ResourceVersion {
        &self.0.version
    }
    pub fn data(&self) -> &ResourceData {
        &self.0.data
    }
    pub fn downgrade(&self) -> ResourceWeak {
        ResourceWeak {
            weak: Arc::downgrade(&self.0),
            _descriptor: Some(self.0.descriptor_ticket.clone()),
        }
    }
    pub fn is_retired(&self) -> bool {
        self.0.retired.load(Ordering::Acquire)
    }
    /// Coordinate backend retirement with the registry; existing leases remain valid.
    pub fn prohibit_new_leases(&self) {
        self.0.retired.store(true, Ordering::Release);
    }
}
#[derive(Clone, Debug)]
pub struct ResourceWeak {
    weak: Weak<Resource>,
    // Weak references retain the Arc allocation after Resource's data is dropped.
    _descriptor: Option<Arc<BudgetTicket>>,
}
impl ResourceWeak {
    /// Reuse an immutable resource while another consumer still owns it.
    /// Retirement closes this admission; the weak reference keeps no pixel or
    /// font storage alive after the final existing lease is released.
    pub fn upgrade(&self) -> Option<ResourceLease> {
        let resource = self.weak.upgrade()?;
        (!resource.retired.load(Ordering::Acquire)).then_some(ResourceLease(resource))
    }
    pub fn is_released(&self) -> bool {
        self.weak.strong_count() == 0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceError {
    Capacity,
    Bytes,
    InvalidData,
    ConflictingVersion,
    RetiredOrForeign,
    IdentityExhausted,
}
#[derive(Debug)]
struct Slot {
    generation: u64,
    active: Option<ResourceLease>,
    last: ResourceWeak,
}
#[derive(Debug)]
pub struct ResourceRegistry {
    id: u64,
    slots: Vec<Slot>,
    descriptors: BudgetPool,
    _storage: BudgetTicket,
}
static NEXT_REGISTRY: Mutex<u64> = Mutex::new(1);
impl ResourceRegistry {
    pub fn new(max_resources: u32, max_bytes: u64) -> Result<Self, ResourceError> {
        let descriptors = BudgetPool::new(
            BudgetCategory::Temporary,
            QueueLimits {
                max_requests: max_resources.saturating_mul(3).saturating_add(1),
                max_bytes,
            },
        );
        let bytes = (max_resources as usize)
            .checked_mul(size_of::<Slot>())
            .ok_or(ResourceError::Bytes)?;
        let storage = descriptors
            .reserve(bytes as u64)
            .map_err(|_| ResourceError::Bytes)?;
        let mut next = NEXT_REGISTRY
            .lock()
            .expect("resource registry identity poisoned");
        let id = *next;
        *next = next
            .checked_add(1)
            .ok_or(ResourceError::IdentityExhausted)?;
        let slots = (0..max_resources)
            .map(|_| Slot {
                generation: 0,
                active: None,
                last: ResourceWeak {
                    weak: Weak::new(),
                    _descriptor: None,
                },
            })
            .collect();
        Ok(Self {
            id,
            slots,
            descriptors,
            _storage: storage,
        })
    }
    pub fn usage(&self) -> BudgetUsage {
        self.descriptors.usage()
    }
    /// Import a producer-owned blob with a reservation for its declared retained extent.
    /// Opaque producer allocation/cache capacity remains in the producer's accounting domain.
    /// Prefer `register_image`/`register_font` for engine-owned, fully accounted bytes.
    /// No hashing or decoding occurs. An exact active version reuses its lease.
    pub fn register(
        &mut self,
        version: ResourceVersion,
        data: ResourceData,
        ticket: BudgetTicket,
    ) -> Result<ResourceHandle, ResourceError> {
        self.register_inner(version, data, Some(ticket))
    }
    /// Prepare immutable image bytes. The full vector capacity remains charged through
    /// every cloned Blob, including producer caches after registry/frame retirement.
    pub fn register_image(
        &mut self,
        version: ResourceVersion,
        pixels: OwnedBytes,
        size: [u32; 2],
        format: ImageFormat,
        alpha_type: ImageAlphaType,
        encoding: ColorEncoding,
    ) -> Result<ResourceHandle, ResourceError> {
        let image = ImageData {
            data: self.owned_blob(pixels)?,
            format,
            alpha_type,
            width: size[0],
            height: size[1],
        };
        self.register_inner(version, ResourceData::Image { image, encoding }, None)
    }
    /// Transfer validated prepared texture bytes into the existing image allocation ledger.
    pub fn register_texture(
        &mut self,
        version: ResourceVersion,
        payload: TexturePayload,
    ) -> Result<ResourceHandle, ResourceError> {
        let (descriptor, bytes) = payload.into_parts();
        let bytes = self.owned_blob(bytes)?;
        self.register_inner(version, ResourceData::Texture { descriptor, bytes }, None)
    }
    /// Transfer a complete platform shader into the ordinary immutable resource
    /// ledger. Every lease and cloned blob retains its original byte reservation.
    pub fn register_effect_shader(
        &mut self,
        version: ResourceVersion,
        descriptor: EffectShaderDescriptor,
        bytes: OwnedBytes,
    ) -> Result<ResourceHandle, ResourceError> {
        let bytes = self.owned_blob(bytes)?;
        self.register_inner(
            version,
            ResourceData::EffectShader { descriptor, bytes },
            None,
        )
    }
    /// Import producer-accounted planar pixels. The storage Arc retains the
    /// producer's budget reservation; the registry charges its own descriptors.
    pub fn register_yuv420(
        &mut self,
        version: ResourceVersion,
        image: reel_core::image::Yuv420Image,
    ) -> Result<ResourceHandle, ResourceError> {
        self.register_inner(version, ResourceData::Yuv420(image), None)
    }
    /// Prepare a complete immutable font file or collection. Glyph runs select
    /// their face independently, sharing these bytes and their allocation ticket.
    pub fn register_font(
        &mut self,
        version: ResourceVersion,
        bytes: OwnedBytes,
    ) -> Result<ResourceHandle, ResourceError> {
        let font = self.owned_blob(bytes)?;
        self.register_inner(version, ResourceData::Font(font), None)
    }
    fn owned_blob(&self, bytes: OwnedBytes) -> Result<Blob<u8>, ResourceError> {
        let storage = self
            .descriptors
            .reserve((size_of::<AccountedBlob>() + 2 * size_of::<usize>()) as u64)
            .map_err(|_| ResourceError::Bytes)?;
        Ok(Blob::new(Arc::new(AccountedBlob {
            bytes,
            _storage: storage,
        })))
    }
    fn register_inner(
        &mut self,
        version: ResourceVersion,
        data: ResourceData,
        ticket: Option<BudgetTicket>,
    ) -> Result<ResourceHandle, ResourceError> {
        if let ResourceData::EffectShader { descriptor, bytes } = &data
            && (!descriptor.matches_bytes(bytes.data())
                || (descriptor.format != EffectShaderFormat::Sgx543
                    && std::str::from_utf8(bytes.data()).is_err()))
        {
            return Err(ResourceError::InvalidData);
        }
        if let ResourceData::Texture { descriptor, bytes } = &data {
            descriptor
                .validate(bytes.len())
                .map_err(|_| ResourceError::InvalidData)?;
        }
        if ticket
            .as_ref()
            .is_some_and(|t| data.bytes() as u64 > t.bytes())
        {
            return Err(ResourceError::Bytes);
        }
        if let ResourceData::Image { image, .. } = &data
            && (image.width == 0
                || image.height == 0
                || image.format.size_in_bytes(image.width, image.height) != Some(image.data.len()))
        {
            return Err(ResourceError::InvalidData);
        }
        for existing in self.slots.iter().filter_map(|s| s.last.weak.upgrade()) {
            if existing.version == version {
                if existing.retired.load(Ordering::Acquire) {
                    return Err(ResourceError::RetiredOrForeign);
                }
                return if existing.data.same_source(&data) {
                    Ok(existing.handle)
                } else {
                    Err(ResourceError::ConflictingVersion)
                };
            }
        }
        let (index, slot) = self
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, s)| s.active.is_none() && s.last.is_released())
            .ok_or(ResourceError::Capacity)?;
        let generation = slot
            .generation
            .checked_add(1)
            .ok_or(ResourceError::IdentityExhausted)?;
        // The selected slot has no strong lease. Drop only this registry's expired
        // weak descriptor before reserving its replacement. External weak aliases
        // keep their own Arc allocation ticket until their actual last drop.
        slot.last = ResourceWeak {
            weak: Weak::new(),
            _descriptor: None,
        };
        let bytes = size_of::<Resource>()
            + size_of::<BudgetTicket>()
            + 4 * size_of::<usize>()
            + version.key.0.capacity()
            + match &data {
                ResourceData::EffectShader { descriptor, .. } => {
                    descriptor.parameters.capacity() * size_of::<crate::stage::effect::EffectType>()
                }
                _ => 0,
            };
        let descriptor = self
            .descriptors
            .reserve(bytes as u64)
            .map_err(|_| ResourceError::Bytes)?;
        let handle = ResourceHandle {
            registry: self.id,
            slot: index as u32,
            generation,
        };
        let lease = ResourceLease(Arc::new(Resource {
            handle,
            version,
            data,
            retired: AtomicBool::new(false),
            _reference_ticket: ticket,
            descriptor_ticket: Arc::new(descriptor),
        }));
        slot.generation = generation;
        slot.last = lease.downgrade();
        slot.active = Some(lease);
        Ok(handle)
    }
    pub fn acquire(&self, handle: ResourceHandle) -> Result<ResourceLease, ResourceError> {
        self.slots
            .get(handle.slot as usize)
            .and_then(|s| s.active.as_ref())
            .filter(|r| r.handle() == handle && !r.is_retired())
            .cloned()
            .ok_or(ResourceError::RetiredOrForeign)
    }
    pub fn retire(&mut self, handle: ResourceHandle) -> bool {
        let Some(slot) = self.slots.get_mut(handle.slot as usize) else {
            return false;
        };
        if slot.active.as_ref().is_none_or(|r| r.handle() != handle) {
            return false;
        }
        slot.active.as_ref().unwrap().prohibit_new_leases();
        slot.active = None;
        true
    }
    /// Release the producer's registration. Existing immutable leases can still
    /// enter frames, while new acquisitions fail. Unlike retirement this does
    /// not invalidate a captured stage that has not yet built its frame.
    pub fn release(&mut self, handle: ResourceHandle) -> bool {
        let Some(slot) = self.slots.get_mut(handle.slot as usize) else {
            return false;
        };
        if slot.active.as_ref().is_none_or(|r| r.handle() != handle) {
            return false;
        }
        slot.active = None;
        true
    }
    /// Freeze only the selected versions before a build can pause or retirement can occur.
    /// Snapshot copies and their capacity are reserved before acquisition/allocation.
    pub fn freeze(
        &self,
        handles: &[ResourceHandle],
        pool: &BudgetPool,
    ) -> Result<ResourceSnapshot, ResourceError> {
        if handles.len() > self.slots.len() {
            return Err(ResourceError::Capacity);
        }
        ResourceSnapshot::capture(handles.iter().map(|&handle| self.acquire(handle)), pool)
    }
}
#[derive(Debug)]
pub struct ResourceSnapshot {
    pub(crate) leases: Vec<ResourceLease>,
    _ticket: BudgetTicket,
}
impl ResourceSnapshot {
    /// Transfer existing accepted leases; retirement only forbids new admissions.
    pub(crate) fn retain(
        mut leases: Vec<ResourceLease>,
        pool: &BudgetPool,
    ) -> Result<Self, ResourceError> {
        let ticket = pool
            .reserve((leases.capacity() * size_of::<ResourceLease>()) as u64)
            .map_err(|_| ResourceError::Bytes)?;
        leases.sort_unstable_by_key(|r| {
            (
                r.data().blob().is_none(),
                r.data().blob().map_or(0, Blob::id),
                r.handle(),
            )
        });
        Ok(Self {
            leases,
            _ticket: ticket,
        })
    }

    /// Capture prepared leases from independent resource producers. Existing
    /// snapshots survive retirement; a new snapshot cannot admit retired data.
    pub fn capture(
        resources: impl ExactSizeIterator<Item = Result<ResourceLease, ResourceError>>,
        pool: &BudgetPool,
    ) -> Result<Self, ResourceError> {
        let bytes = resources
            .len()
            .checked_mul(size_of::<ResourceLease>())
            .ok_or(ResourceError::Bytes)?;
        let ticket = pool
            .reserve(bytes as u64)
            .map_err(|_| ResourceError::Bytes)?;
        let mut leases = Vec::with_capacity(resources.len());
        for lease in resources {
            let lease = lease?;
            if lease.is_retired() {
                return Err(ResourceError::RetiredOrForeign);
            }
            if leases.iter().any(|other: &ResourceLease| {
                other.handle() == lease.handle() || lookup_same_source(other.data(), lease.data())
            }) {
                return Err(ResourceError::ConflictingVersion);
            }
            leases.push(lease);
        }
        leases.sort_unstable_by_key(|r| {
            (
                r.data().blob().is_none(),
                r.data().blob().map_or(0, Blob::id),
                r.handle(),
            )
        });
        Ok(ResourceSnapshot {
            leases,
            _ticket: ticket,
        })
    }
    fn matching(
        &self,
        blob: &Blob<u8>,
        matches: impl Fn(&ResourceData) -> bool,
    ) -> Option<ResourceHandle> {
        let start = self
            .leases
            .partition_point(|r| r.data().blob().is_some_and(|b| b.id() < blob.id()));
        self.leases[start..]
            .iter()
            .take_while(|r| r.data().blob().is_some_and(|b| b.id() == blob.id()))
            .find(|r| matches(r.data()))
            .map(ResourceLease::handle)
    }
    pub(crate) fn image(&self, image: &ImageData) -> Option<ResourceHandle> {
        self.matching(
            &image.data,
            |data| matches!(data,ResourceData::Image {image:other,..} if same_image(image,other)),
        )
    }
    pub(crate) fn contains_image(&self, handle: ResourceHandle) -> bool {
        self.leases
            .iter()
            .any(|lease| lease.handle() == handle && lease.data().image_size().is_some())
    }
    pub(crate) fn font(&self, font: &FontData) -> Option<ResourceHandle> {
        self.matching(
            &font.data,
            |data| matches!(data, ResourceData::Font(other) if same_blob(&font.data, other)),
        )
    }
    pub(crate) fn external(&self, token: u64) -> Option<ResourceHandle> {
        self.leases
            .iter()
            .find(|r| matches!(r.data(),ResourceData::External {token:t} if *t==token))
            .map(ResourceLease::handle)
    }
    pub fn leases(&self) -> &[ResourceLease] {
        &self.leases
    }
}
