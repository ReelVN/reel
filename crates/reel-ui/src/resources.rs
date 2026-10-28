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
type PixelStorage = dyn AsRef<[u8]> + Send + Sync;

#[derive(Debug)]
struct PixelState {
    // A weak reference preserves producer identity without retaining its bytes.
    storage: Weak<PixelStorage>,
    resident: Option<Blob<u8>>,
    metadata: BudgetPool,
    weak_storage: Option<BudgetTicket>,
    reloadable: bool,
}
impl PixelState {
    fn new(blob: Blob<u8>, metadata: BudgetPool) -> Self {
        let (storage, _) = blob.clone().into_raw_parts();
        Self {
            storage: Arc::downgrade(&storage),
            resident: Some(blob),
            metadata,
            weak_storage: None,
            reloadable: false,
        }
    }
    fn matches(&self, blob: &Blob<u8>) -> bool {
        let (storage, _) = blob.clone().into_raw_parts();
        Weak::ptr_eq(&self.storage, &Arc::downgrade(&storage))
    }
}

#[derive(Debug)]
enum StoredResource {
    Image(ImageDescriptor),
    Texture(TextureDescriptor),
    Other(ResourceData),
}

#[derive(Debug)]
struct Resource {
    handle: ResourceHandle,
    version: ResourceVersion,
    data: StoredResource,
    pixels: Option<Mutex<PixelState>>,
    // Remains stable when pixel residency changes; Blob IDs are lookup hints.
    source_blob_id: Option<u64>,
    pixel_bytes: Option<usize>,
    image_coverage: Option<reel_core::image_coverage::ImageCoverage>,
    retired: AtomicBool,
    // Opaque imported producer storage has its own accounting domain. This reservation
    // covers the frozen reference's declared extent; owned blobs carry their own ticket.
    _reference_ticket: Option<BudgetTicket>,
    descriptor_ticket: Arc<BudgetTicket>,
}
#[derive(Clone, Debug)]
pub struct ResourceLease(Arc<Resource>);

/// An owned reader for immutable image pixels. Clones share the producer's
/// allocation and its budget until the last reader drops.
#[derive(Clone, Debug)]
pub struct ResourcePixels(Blob<u8>);
impl ResourcePixels {
    /// Pixel or block bytes retained for this reader's lifetime.
    pub fn bytes(&self) -> &[u8] {
        self.0.data()
    }
    /// Share this reader's immutable pixel backing with a retained snapshot.
    pub fn blob(&self) -> Blob<u8> {
        self.0.clone()
    }
}

/// Immutable decoded image metadata, independent of pixel residency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageDescriptor {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Pixel channel format.
    pub format: ImageFormat,
    /// Alpha representation of the decoded pixels.
    pub alpha_type: ImageAlphaType,
    /// Color encoding of the decoded pixels.
    pub encoding: ColorEncoding,
}

/// Immutable resource metadata. Image and texture pixel bytes require an owned reader.
#[derive(Debug)]
pub enum ResourceView<'a> {
    /// Portable shader descriptor and immutable program bytes.
    EffectShader {
        /// Shader format and parameter contract.
        descriptor: &'a EffectShaderDescriptor,
        /// Immutable shader program bytes.
        bytes: &'a Blob<u8>,
    },
    /// Prepared texture metadata.
    Texture {
        /// Validated texture storage layout.
        descriptor: TextureDescriptor,
    },
    /// Decoded image metadata.
    Image {
        /// Decoded size, channel and color information.
        image: ImageDescriptor,
    },
    /// Producer-owned planar video pixels.
    Yuv420(&'a reel_core::image::Yuv420Image),
    /// Immutable font bytes.
    Font(&'a Blob<u8>),
    /// External producer identity.
    External {
        /// Producer-provided identity.
        token: u64,
    },
}
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
    /// Return the resource metadata without taking ownership of image pixels.
    pub fn view(&self) -> ResourceView<'_> {
        match &self.0.data {
            StoredResource::Image(image) => ResourceView::Image { image: *image },
            StoredResource::Texture(descriptor) => ResourceView::Texture {
                descriptor: *descriptor,
            },
            StoredResource::Other(ResourceData::EffectShader { descriptor, bytes }) => {
                ResourceView::EffectShader { descriptor, bytes }
            }
            StoredResource::Other(ResourceData::Yuv420(image)) => ResourceView::Yuv420(image),
            StoredResource::Other(ResourceData::Font(font)) => ResourceView::Font(font),
            StoredResource::Other(ResourceData::External { token }) => {
                ResourceView::External { token: *token }
            }
            StoredResource::Other(ResourceData::Image { .. } | ResourceData::Texture { .. }) => {
                unreachable!()
            }
        }
    }
    /// Return decoded image metadata without retaining its pixel allocation.
    pub fn image_descriptor(&self) -> Option<ImageDescriptor> {
        match self.0.data {
            StoredResource::Image(image) => Some(image),
            _ => None,
        }
    }
    /// Return prepared texture metadata without retaining its block allocation.
    pub fn texture_descriptor(&self) -> Option<TextureDescriptor> {
        match self.0.data {
            StoredResource::Texture(descriptor) => Some(descriptor),
            _ => None,
        }
    }
    /// Logical dimensions of an image, texture, or video resource.
    pub fn image_size(&self) -> Option<[u32; 2]> {
        match self.view() {
            ResourceView::Image { image } => Some([image.width, image.height]),
            ResourceView::Texture { descriptor } => Some(descriptor.layout.logical_size),
            ResourceView::Yuv420(image) => Some(image.size()),
            _ => None,
        }
    }
    /// Declared image or texture payload length, independent of residency.
    pub fn pixel_byte_len(&self) -> Option<usize> {
        self.0.pixel_bytes
    }
    /// Acquire an owning CPU reader for decoded image or texture pixels.
    /// Other resource kinds have their own data and return `None`.
    pub fn read_pixels(&self) -> Option<ResourcePixels> {
        self.0
            .pixels
            .as_ref()?
            .lock()
            .expect("image pixels poisoned")
            .resident
            .clone()
            .map(ResourcePixels)
    }
    fn source_blob_id(&self) -> Option<u64> {
        self.0.source_blob_id
    }
    fn snapshot_source_key(&self) -> (u8, u64) {
        match self.view() {
            ResourceView::Yuv420(image) => {
                let [width, height] = image.size();
                (1, (u64::from(width) << 32) | u64::from(height))
            }
            ResourceView::External { token } => (2, token),
            _ => (0, self.source_blob_id().expect("blob-backed resource")),
        }
    }
    fn lookup_same_source(&self, other: &Self) -> bool {
        match (self.view(), other.view()) {
            (ResourceView::Image { image: a }, ResourceView::Image { image: b }) => {
                a.width == b.width
                    && a.height == b.height
                    && a.format == b.format
                    && a.alpha_type == b.alpha_type
                    && self.same_pixels(other)
            }
            (ResourceView::Texture { descriptor: a }, ResourceView::Texture { descriptor: b }) => {
                a == b && self.same_pixels(other)
            }
            _ => match (&self.0.data, &other.0.data) {
                (StoredResource::Other(a), StoredResource::Other(b)) => lookup_same_source(a, b),
                _ => false,
            },
        }
    }
    fn same_pixels(&self, other: &Self) -> bool {
        if self.source_blob_id() != other.source_blob_id() {
            return false;
        }
        let Some(a) = &self.0.pixels else {
            return false;
        };
        let Some(b) = &other.0.pixels else {
            return false;
        };
        let a = a.lock().expect("image pixels poisoned").storage.clone();
        let b = b.lock().expect("image pixels poisoned").storage.clone();
        Weak::ptr_eq(&a, &b)
    }
    fn same_input_pixels(&self, blob: &Blob<u8>) -> bool {
        self.source_blob_id() == Some(blob.id())
            && self.0.pixels.as_ref().is_some_and(|identity| {
                identity
                    .lock()
                    .expect("image pixels poisoned")
                    .matches(blob)
            })
    }
    fn same_input_source(&self, data: &ResourceData) -> bool {
        match (self.view(), data) {
            (ResourceView::Image { image: a }, ResourceData::Image { image: b, encoding }) => {
                a.width == b.width
                    && a.height == b.height
                    && a.format == b.format
                    && a.alpha_type == b.alpha_type
                    && a.encoding == *encoding
                    && self.same_input_pixels(&b.data)
            }
            (
                ResourceView::Texture { descriptor: a },
                ResourceData::Texture {
                    descriptor: b,
                    bytes,
                },
            ) => a == *b && self.same_input_pixels(bytes),
            _ => match &self.0.data {
                StoredResource::Other(existing) => existing.same_source(data),
                _ => false,
            },
        }
    }
    /// Mark pixels as reconstructible from this resource's fixed asset version.
    /// The asset owner must retain a path that can asynchronously restore them.
    pub fn permit_pixel_reload(&self) {
        if let Some(pixels) = &self.0.pixels {
            pixels.lock().expect("image pixels poisoned").reloadable = true;
        }
    }
    /// Release reconstructible pixels after the same version is resident on the
    /// consumer. Returns false while a CPU reader or producer alias holds them.
    pub fn discard_uploaded_pixels(&self) -> bool {
        let Some(pixels) = &self.0.pixels else {
            return false;
        };
        let mut pixels = pixels.lock().expect("image pixels poisoned");
        if !pixels.reloadable
            || pixels
                .resident
                .as_ref()
                .is_none_or(|blob| blob.strong_count() != 1)
        {
            return false;
        }
        // The producer's blob ticket ends with the last strong reference; its
        // Arc control block remains until our source-identity weak reference ends.
        let Ok(storage) = pixels.metadata.reserve((2 * size_of::<usize>()) as u64) else {
            return false;
        };
        pixels.weak_storage = Some(storage);
        pixels.resident = None;
        true
    }
    /// Restore the same fixed version and descriptor into this resource identity.
    /// Existing readers keep their own backing until they are dropped.
    pub fn restore_pixels(&self, source: &ResourceLease) -> Result<(), ResourceError> {
        if self.version() != source.version()
            || self.image_descriptor() != source.image_descriptor()
            || self.texture_descriptor() != source.texture_descriptor()
            || self.pixel_byte_len() != source.pixel_byte_len()
        {
            return Err(ResourceError::ConflictingVersion);
        }
        let replacement = source.read_pixels().ok_or(ResourceError::InvalidData)?;
        let Some(state) = &self.0.pixels else {
            return Err(ResourceError::InvalidData);
        };
        let mut state = state.lock().expect("image pixels poisoned");
        if !state.reloadable {
            return Err(ResourceError::InvalidData);
        }
        if state.resident.is_none() {
            let (backing, _) = replacement.0.into_raw_parts();
            let blob =
                Blob::from_raw_parts(backing, self.source_blob_id().expect("pixel identity"));
            let (backing, _) = blob.clone().into_raw_parts();
            state.storage = Arc::downgrade(&backing);
            state.resident = Some(blob);
            state.weak_storage = None;
        }
        Ok(())
    }
    /// Cached conservative alpha support in visible texels, including the
    /// filtering border. Registration computes this once from immutable RGBA
    /// pixels or BC3 blocks. Unsupported storage retains ordinary geometry.
    pub fn image_coverage(&self) -> Option<&reel_core::image_coverage::ImageCoverage> {
        self.0.image_coverage.as_ref()
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
                || (descriptor.format == EffectShaderFormat::Wgsl
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
                return if ResourceLease(existing.clone()).same_input_source(&data) {
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
        let image_coverage = match &data {
            ResourceData::Image { image, .. } if image.format == ImageFormat::Rgba8 => {
                reel_core::image_coverage::ImageCoverage::rgba8(
                    [image.width, image.height],
                    image.data.data(),
                )
            }
            ResourceData::Texture { descriptor, bytes } => {
                reel_core::image_coverage::ImageCoverage::texture(*descriptor, bytes.data())
            }
            _ => None,
        };
        let source_blob_id = data.blob().map(Blob::id);
        let pixel_bytes = match &data {
            ResourceData::Image { image, .. } => Some(image.data.len()),
            ResourceData::Texture { bytes, .. } => Some(bytes.len()),
            _ => None,
        };
        let (data, pixels) = match data {
            ResourceData::Image { image, encoding } => (
                StoredResource::Image(ImageDescriptor {
                    width: image.width,
                    height: image.height,
                    format: image.format,
                    alpha_type: image.alpha_type,
                    encoding,
                }),
                Some(Mutex::new(PixelState::new(
                    image.data,
                    self.descriptors.clone(),
                ))),
            ),
            ResourceData::Texture { descriptor, bytes } => (
                StoredResource::Texture(descriptor),
                Some(Mutex::new(PixelState::new(bytes, self.descriptors.clone()))),
            ),
            other => (StoredResource::Other(other), None),
        };
        let lease = ResourceLease(Arc::new(Resource {
            handle,
            version,
            data,
            pixels,
            source_blob_id,
            pixel_bytes,
            image_coverage,
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
#[cfg(test)]
mod tests;

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
                r.source_blob_id().is_none(),
                r.source_blob_id().unwrap_or(0),
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
        let indexed = resources.len() >= 16;
        let mut leases = Vec::with_capacity(resources.len());
        for lease in resources {
            let lease = lease?;
            if lease.is_retired() {
                return Err(ResourceError::RetiredOrForeign);
            }
            if indexed {
                let key = lease.snapshot_source_key();
                let start = leases
                    .partition_point(|other: &ResourceLease| other.snapshot_source_key() < key);
                let end = start
                    + leases[start..].partition_point(|other| other.snapshot_source_key() == key);
                // A handle belongs to one immutable Resource, so the same handle
                // necessarily has the same source key. Blob IDs remain hints.
                if leases[start..end].iter().any(|other| {
                    other.handle() == lease.handle() || other.lookup_same_source(&lease)
                }) {
                    return Err(ResourceError::ConflictingVersion);
                }
                leases.insert(end, lease);
            } else {
                if leases.iter().any(|other: &ResourceLease| {
                    other.handle() == lease.handle() || other.lookup_same_source(&lease)
                }) {
                    return Err(ResourceError::ConflictingVersion);
                }
                leases.push(lease);
            }
        }
        leases.sort_unstable_by_key(|r| {
            (
                r.source_blob_id().is_none(),
                r.source_blob_id().unwrap_or(0),
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
        matches: impl Fn(&ResourceLease) -> bool,
    ) -> Option<ResourceHandle> {
        let start = self
            .leases
            .partition_point(|r| r.source_blob_id().is_some_and(|id| id < blob.id()));
        self.leases[start..]
            .iter()
            .take_while(|r| r.source_blob_id().is_some_and(|id| id == blob.id()))
            .find(|r| matches(r))
            .map(ResourceLease::handle)
    }
    pub(crate) fn image(&self, image: &ImageData) -> Option<ResourceHandle> {
        self.matching(&image.data, |lease| {
            lease.image_descriptor().is_some_and(|other| {
                image.width == other.width
                    && image.height == other.height
                    && image.format == other.format
                    && image.alpha_type == other.alpha_type
                    && lease
                        .read_pixels()
                        .is_some_and(|pixels| same_blob(&image.data, &pixels.0))
            })
        })
    }
    pub(crate) fn contains_image(&self, handle: ResourceHandle) -> bool {
        self.leases
            .iter()
            .any(|lease| lease.handle() == handle && lease.image_size().is_some())
    }
    pub(crate) fn font(&self, font: &FontData) -> Option<ResourceHandle> {
        self.matching(
            &font.data,
            |lease| matches!(lease.view(), ResourceView::Font(other) if same_blob(&font.data, other)),
        )
    }
    pub(crate) fn external(&self, token: u64) -> Option<ResourceHandle> {
        self.leases
            .iter()
            .find(|r| matches!(r.view(),ResourceView::External {token:t} if t==token))
            .map(ResourceLease::handle)
    }
    pub fn leases(&self) -> &[ResourceLease] {
        &self.leases
    }
}
