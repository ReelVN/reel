//! Portable retained pictures. Asset references remain versioned; dynamic pixels
//! share their immutable producer backing until the save worker serializes them.
use super::*;
use crate::resources::{
    ColorEncoding, ResourceData, ResourceLease, ResourceRegistry, ResourceVersion,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
#[cfg(test)]
mod tests;
mod validate;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetainedAssetKind {
    Image,
    Font,
    EffectShader,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetainedAsset {
    #[serde(with = "wire::version")]
    pub version: ResourceVersion,
    pub kind: RetainedAssetKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum InlineResource {
    EffectShader {
        descriptor: crate::resources::EffectShaderDescriptor,
        bytes: peniko::Blob<u8>,
    },
    Texture {
        descriptor: reel_core::texture::TextureDescriptor,
        bytes: peniko::Blob<u8>,
    },
    Image {
        image: peniko::ImageData,
        encoding: ColorEncoding,
    },
    Font(peniko::Blob<u8>),
    Yuv(#[serde(with = "wire::yuv")] reel_core::image::Yuv420Image),
}
impl InlineResource {
    fn metadata_bytes(&self) -> usize {
        match self {
            Self::EffectShader { descriptor, .. } => {
                descriptor.parameters.capacity() * size_of::<crate::stage::effect::EffectType>()
            }
            _ => 0,
        }
    }
    fn bytes(&self) -> usize {
        match self {
            Self::EffectShader { bytes, .. } => bytes.len(),
            Self::Texture { bytes, .. } => bytes.len(),
            Self::Image { image, .. } => image.data.len(),
            Self::Font(font) => font.len(),
            Self::Yuv(image) => image.byte_len(),
        }
    }
    fn data(&self) -> ResourceData {
        match self {
            Self::EffectShader { descriptor, bytes } => ResourceData::EffectShader {
                descriptor: descriptor.clone(),
                bytes: bytes.clone(),
            },
            Self::Texture { descriptor, bytes } => ResourceData::Texture {
                descriptor: *descriptor,
                bytes: bytes.clone(),
            },
            Self::Image { image, encoding } => ResourceData::Image {
                image: image.clone(),
                encoding: *encoding,
            },
            Self::Font(font) => ResourceData::Font(font.clone()),
            Self::Yuv(image) => ResourceData::Yuv420(image.clone()),
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
enum SavedResourceKind {
    Asset(RetainedAsset),
    Inline(InlineResource),
}
#[derive(Debug, Serialize, Deserialize)]
struct SavedResource {
    handle: ResourceHandle,
    kind: SavedResourceKind,
    // Ordinary assets in a live archive are still held by Drawing::Live.
    // A decoded asset reference has no resident pixels until the host rebinds it.
    #[serde(skip)]
    live_bytes: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredDrawing {
    commands: Vec<FrameCommand>,
    capture_exclusions: Vec<Range<usize>>,
    masks: BTreeMap<u32, Arc<FrameMask>>,
}
#[derive(Debug)]
enum Drawing {
    Live(Arc<FrameDrawing>),
    Stored(StoredDrawing),
}
impl Drawing {
    fn capture_exclusions(&self) -> &[Range<usize>] {
        match self {
            Self::Live(v) => &v.capture_exclusions,
            Self::Stored(v) => &v.capture_exclusions,
        }
    }
    fn commands(&self) -> &[FrameCommand] {
        match self {
            Self::Live(v) => &v.commands,
            Self::Stored(v) => &v.commands,
        }
    }
    fn masks(&self) -> &BTreeMap<u32, Arc<FrameMask>> {
        match self {
            Self::Live(v) => &v.masks,
            Self::Stored(v) => &v.masks,
        }
    }
}
impl Serialize for Drawing {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut record = serializer.serialize_struct("StoredDrawing", 3)?;
        record.serialize_field("commands", self.commands())?;
        record.serialize_field("capture_exclusions", self.capture_exclusions())?;
        record.serialize_field("masks", self.masks())?;
        record.end()
    }
}
impl<'de> Deserialize<'de> for Drawing {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        StoredDrawing::deserialize(deserializer).map(Self::Stored)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct SavedTransition {
    commands: Range<usize>,
    capture_included: bool,
    previous: usize,
    to_parent: Affine,
    progress: f32,
    mask: Option<FrameImageMask>,
    parent_to_window: Affine,
    mask_to_window: Affine,
}
#[derive(Debug, Serialize, Deserialize)]
struct SavedNode {
    drawing: Drawing,
    resources: Vec<u32>,
    transitions: Vec<SavedTransition>,
}
#[derive(Debug, Serialize, Deserialize)]
struct Archive {
    // Postorder: every transition references an earlier node.
    nodes: Vec<SavedNode>,
    resources: Vec<SavedResource>,
    to_parent: Affine,
    #[serde(skip)]
    _ticket: Option<BudgetTicket>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedRetainedScene(Arc<Archive>);

fn snapshot_charge(ticket: &mut BudgetTicket, bytes: usize) -> Result<(), RetainError> {
    let bytes = ticket
        .bytes()
        .checked_add(bytes as u64)
        .ok_or_else(|| retained::error(BuildErrorKind::Bytes))?;
    ticket
        .resize(bytes)
        .map_err(|_| retained::error(BuildErrorKind::Bytes))
}

fn snapshot_reserve<T>(
    values: &mut Vec<T>,
    additional: usize,
    ticket: &mut BudgetTicket,
) -> Result<(), RetainError> {
    let error = || retained::error(BuildErrorKind::Bytes);
    let required = values.len().checked_add(additional).ok_or_else(error)?;
    if required <= values.capacity() {
        return Ok(());
    }
    let capacity = required.max(values.capacity().checked_mul(2).ok_or_else(error)?);
    let old_bytes = values
        .capacity()
        .checked_mul(size_of::<T>())
        .ok_or_else(error)?;
    let new_bytes = capacity.checked_mul(size_of::<T>()).ok_or_else(error)?;
    // Growth can temporarily own both allocations. Admit that overlap before
    // reserve, then release the old capacity after the allocator has replaced it.
    snapshot_charge(ticket, new_bytes)?;
    values
        .try_reserve_exact(capacity - values.len())
        .map_err(|_| error())?;
    ticket
        .resize(ticket.bytes() - old_bytes as u64)
        .map_err(|_| error())
}

impl RetainedScene {
    pub fn snapshot(
        &self,
        mut describe: impl FnMut(&ResourceLease) -> Option<RetainedAsset>,
    ) -> Result<SavedRetainedScene, RetainError> {
        let mut ticket = self
            .pool
            .reserve((size_of::<Archive>() + 2 * size_of::<usize>()) as u64)
            .map_err(|_| retained::error(BuildErrorKind::Bytes))?;
        let mut workspace = self
            .pool
            .reserve(0)
            .map_err(|_| retained::error(BuildErrorKind::Bytes))?;
        let mut nodes = Vec::new();
        let mut resources = Vec::new();
        let mut resource_ids = BTreeMap::new();
        let mut node_ids = BTreeMap::new();
        let mut pending = Vec::new();
        snapshot_reserve(&mut pending, 1, &mut workspace)?;
        pending.push((self.drawing.clone(), false));
        while let Some((drawing, visited)) = pending.pop() {
            let key = Arc::as_ptr(&drawing) as usize;
            if node_ids.contains_key(&key) {
                continue;
            }
            if !visited {
                snapshot_reserve(&mut pending, drawing.transitions.len() + 1, &mut workspace)?;
                pending.push((drawing.clone(), true));
                for transition in drawing.transitions.iter().rev() {
                    pending.push((transition.previous.drawing.clone(), false));
                }
                continue;
            }
            snapshot_reserve(&mut nodes, 1, &mut ticket)?;
            // Match the retained builder's conservative B-tree node allowance.
            // Both identity maps are temporary and drop before publication.
            snapshot_charge(&mut workspace, 512)?;
            let mut slots = Vec::new();
            snapshot_reserve(&mut slots, drawing.resources.leases().len(), &mut ticket)?;
            for lease in drawing.resources.leases() {
                let slot = if let Some(slot) = resource_ids.get(&lease.handle()) {
                    *slot
                } else {
                    snapshot_reserve(&mut resources, 1, &mut ticket)?;
                    snapshot_charge(&mut workspace, 512)?;
                    // Describe may clone the producer's asset name. Its allocation
                    // is admitted before calling into that producer.
                    let key_bytes = lease.version().key.0.len();
                    snapshot_charge(&mut ticket, key_bytes)?;
                    let kind = if let Some(asset) = describe(lease) {
                        if asset.version != *lease.version() || !asset_matches(&asset, lease) {
                            return Err(retained::error(BuildErrorKind::InvalidRecord));
                        }
                        snapshot_charge(
                            &mut ticket,
                            asset.version.key.0.capacity().saturating_sub(key_bytes),
                        )?;
                        SavedResourceKind::Asset(asset)
                    } else {
                        ticket
                            .resize(ticket.bytes() - key_bytes as u64)
                            .map_err(|_| retained::error(BuildErrorKind::Bytes))?;
                        SavedResourceKind::Inline(if let Some(image) = lease.image_descriptor() {
                            let pixels = lease
                                .read_pixels()
                                .ok_or_else(|| retained::error(BuildErrorKind::InvalidRecord))?;
                            InlineResource::Image {
                                image: peniko::ImageData {
                                    data: pixels.blob(),
                                    format: image.format,
                                    alpha_type: image.alpha_type,
                                    width: image.width,
                                    height: image.height,
                                },
                                encoding: image.encoding,
                            }
                        } else if let Some(descriptor) = lease.texture_descriptor() {
                            let pixels = lease
                                .read_pixels()
                                .ok_or_else(|| retained::error(BuildErrorKind::InvalidRecord))?;
                            InlineResource::Texture {
                                descriptor,
                                bytes: pixels.blob(),
                            }
                        } else {
                            match lease.data() {
                                ResourceData::EffectShader { descriptor, bytes } => {
                                    snapshot_charge(
                                        &mut ticket,
                                        descriptor.parameters.len()
                                            * size_of::<crate::stage::effect::EffectType>(),
                                    )?;
                                    InlineResource::EffectShader {
                                        descriptor: descriptor.clone(),
                                        bytes: bytes.clone(),
                                    }
                                }
                                ResourceData::Texture { .. } | ResourceData::Image { .. } => {
                                    unreachable!()
                                }
                                ResourceData::Font(font) => InlineResource::Font(font.clone()),
                                ResourceData::Yuv420(image) => InlineResource::Yuv(image.clone()),
                                ResourceData::External { .. } => {
                                    return Err(retained::error(BuildErrorKind::UnsupportedRecord));
                                }
                            }
                        })
                    };
                    let slot = u32::try_from(resources.len())
                        .map_err(|_| retained::error(BuildErrorKind::Capacity))?;
                    resources.push(SavedResource {
                        handle: lease.handle(),
                        kind,
                        live_bytes: 512
                            + lease.version().key.0.capacity() as u64
                            + if let Some(bytes) = lease.pixel_byte_len() {
                                bytes as u64
                            } else {
                                match lease.data() {
                                    ResourceData::EffectShader { bytes, descriptor } => {
                                        bytes.len() as u64
                                            + (descriptor.parameters.capacity()
                                                * size_of::<crate::stage::effect::EffectType>())
                                                as u64
                                    }
                                    ResourceData::Font(font) => font.len() as u64,
                                    ResourceData::Yuv420(image) => image.byte_len() as u64,
                                    _ => unreachable!(),
                                }
                            },
                    });
                    resource_ids.insert(lease.handle(), slot);
                    slot
                };
                slots.push(slot);
            }
            let mut transitions = Vec::new();
            snapshot_reserve(&mut transitions, drawing.transitions.len(), &mut ticket)?;
            for transition in &drawing.transitions {
                if let Some(mask) = &transition.mask {
                    snapshot_charge(&mut ticket, mask.images.len() * size_of::<ResourceHandle>())?;
                }
                transitions.push(SavedTransition {
                    commands: transition.commands.clone(),
                    capture_included: transition.capture_included,
                    previous: node_ids[&(Arc::as_ptr(&transition.previous.drawing) as usize)],
                    to_parent: transition.previous.to_parent,
                    progress: transition.progress,
                    mask: transition.mask.clone(),
                    parent_to_window: transition.parent_to_window,
                    mask_to_window: transition.mask_to_window,
                });
            }
            node_ids.insert(key, nodes.len());
            nodes.push(SavedNode {
                drawing: Drawing::Live(drawing),
                resources: slots,
                transitions,
            });
        }
        drop((pending, resource_ids, node_ids));
        drop(workspace);
        Ok(SavedRetainedScene(Arc::new(Archive {
            nodes,
            resources,
            to_parent: self.to_parent,
            _ticket: Some(ticket),
        })))
    }
}

fn asset_matches(asset: &RetainedAsset, lease: &ResourceLease) -> bool {
    match asset.kind {
        RetainedAssetKind::Image => {
            lease.image_descriptor().is_some() || lease.texture_descriptor().is_some()
        }
        RetainedAssetKind::Font => matches!(lease.data(), ResourceData::Font(_)),
        RetainedAssetKind::EffectShader => {
            matches!(lease.data(), ResourceData::EffectShader { .. })
        }
    }
}

impl SavedRetainedScene {
    /// Save-queue admission in bounded units. Drawings and resources are unique
    /// within this archive; summing several archives conservatively counts any
    /// shared pictures again. Each item visits one node, command or resource.
    pub fn retained_size_steps(&self) -> impl Iterator<Item = u64> + '_ {
        let archive = (size_of::<Archive>()
            + 2 * size_of::<usize>()
            + self.0.nodes.capacity() * size_of::<SavedNode>()
            + self.0.resources.capacity() * size_of::<SavedResource>())
            as u64;
        let nodes = self.0.nodes.iter().flat_map(|node| {
            let stored = match &node.drawing {
                Drawing::Stored(drawing) => Some(drawing),
                Drawing::Live(_) => None,
            };
            let drawing = match &node.drawing {
                Drawing::Live(drawing) => drawing.accounted_bytes,
                Drawing::Stored(drawing) => {
                    (drawing.commands.capacity() * size_of::<FrameCommand>()
                        + drawing.capture_exclusions.capacity() * size_of::<Range<usize>>())
                        as u64
                }
            };
            let metadata = drawing
                + (node.resources.capacity() * size_of::<u32>()
                    + node.transitions.capacity() * size_of::<SavedTransition>())
                    as u64;
            let commands = stored
                .into_iter()
                .flat_map(|drawing| &drawing.commands)
                .map(retained::command_payload_bytes);
            let masks = stored
                .into_iter()
                .flat_map(|drawing| drawing.masks.values())
                .flat_map(|mask| {
                    std::iter::once(
                        (512 + size_of::<FrameMask>()
                            + 2 * size_of::<usize>()
                            + mask.commands.capacity() * size_of::<FrameCommand>())
                            as u64,
                    )
                    .chain(mask.commands.iter().map(retained::command_payload_bytes))
                });
            let transitions = node.transitions.iter().map(move |transition| {
                transition.mask.as_ref().map_or(0, |mask| {
                    (mask.images.capacity() * size_of::<ResourceHandle>()
                        + if stored.is_some() {
                            mask.program.storage_bytes()
                        } else {
                            0
                        }) as u64
                })
            });
            std::iter::once(metadata)
                .chain(commands)
                .chain(masks)
                .chain(transitions)
        });
        let resources = self
            .0
            .resources
            .iter()
            .map(|resource| match &resource.kind {
                SavedResourceKind::Asset(asset) => {
                    resource.live_bytes + asset.version.key.0.capacity() as u64
                }
                SavedResourceKind::Inline(data) => {
                    resource.live_bytes.max(data.bytes() as u64 + 512)
                        + data.metadata_bytes() as u64
                }
            });
        std::iter::once(archive).chain(nodes).chain(resources)
    }
    pub fn assets(&self) -> impl Iterator<Item = (u32, &RetainedAsset)> {
        self.0
            .resources
            .iter()
            .enumerate()
            .filter_map(|(index, resource)| {
                if let SavedResourceKind::Asset(asset) = &resource.kind {
                    Some((index as u32, asset))
                } else {
                    None
                }
            })
    }
    pub fn restore_begin(
        &self,
        limits: BuildLimits,
        pool: &BudgetPool,
    ) -> Result<RetainedRestore, RetainError> {
        if self.0.nodes.is_empty() || !finite(self.0.to_parent) {
            return Err(retained::error(BuildErrorKind::InvalidRecord));
        }
        let count = u32::try_from(self.0.resources.len())
            .map_err(|_| retained::error(BuildErrorKind::Capacity))?;
        let bytes = size_of::<RetainedRestore>() as u64
            + self.0.resources.len() as u64 * (size_of::<Option<ResourceLease>>() as u64 + 128)
            + self.0.nodes.len() as u64 * size_of::<Arc<FrameDrawing>>() as u64;
        if bytes > limits.max_bytes {
            return Err(retained::error(BuildErrorKind::Bytes));
        }
        let ticket = pool
            .reserve(bytes)
            .map_err(|_| retained::error(BuildErrorKind::Bytes))?;
        let registry = ResourceRegistry::new(count, limits.max_bytes)
            .map_err(|_| retained::error(BuildErrorKind::Bytes))?;
        Ok(RetainedRestore {
            source: self.clone(),
            pool: pool.clone(),
            limits,
            bytes,
            _ticket: ticket,
            registry,
            leases: vec![None; self.0.resources.len()],
            next_resource: 0,
            handles: BTreeMap::new(),
            drawings: Vec::with_capacity(self.0.nodes.len()),
            building: None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RestoreProgress {
    pub build: BuildProgress,
    pub missing_asset: Option<u32>,
}

struct Building {
    commands: Vec<FrameCommand>,
    capture_exclusions: Vec<Range<usize>>,
    masks: BTreeMap<u32, Arc<FrameMask>>,
    current_mask: Option<u32>,
    last_mask: Option<u32>,
    masks_complete: bool,
    transitions: Vec<SceneTransition>,
    resources: Vec<ResourceLease>,
    handles: BTreeMap<ResourceHandle, ReboundResource>,
    ticket: BudgetTicket,
    balance: validate::Balance,
    commands_validated: bool,
    exclusion_balance: validate::Balance,
    exclusion_next: usize,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ResourceKind {
    Image,
    Video,
    Font,
    EffectShader,
}
#[derive(Clone, Copy)]
struct ReboundResource {
    handle: ResourceHandle,
    kind: ResourceKind,
}
pub struct RetainedRestore {
    source: SavedRetainedScene,
    pool: BudgetPool,
    limits: BuildLimits,
    bytes: u64,
    _ticket: BudgetTicket,
    registry: ResourceRegistry,
    leases: Vec<Option<ResourceLease>>,
    next_resource: usize,
    handles: BTreeMap<ResourceHandle, ReboundResource>,
    drawings: Vec<Arc<FrameDrawing>>,
    building: Option<Building>,
}
impl RetainedRestore {
    pub fn bind_asset(&mut self, index: u32, lease: ResourceLease) -> Result<(), RetainError> {
        let resource = self
            .source
            .0
            .resources
            .get(index as usize)
            .ok_or_else(|| retained::error(BuildErrorKind::InvalidRecord))?;
        let SavedResourceKind::Asset(asset) = &resource.kind else {
            return Err(retained::error(BuildErrorKind::InvalidRecord));
        };
        if *lease.version() != asset.version || !asset_matches(asset, &lease) || lease.is_retired()
        {
            return Err(retained::error(BuildErrorKind::InvalidRecord));
        }
        self.leases[index as usize] = Some(lease);
        Ok(())
    }
    fn charge(&mut self, bytes: u64) -> Result<(), RetainError> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|v| *v <= self.limits.max_bytes)
            .ok_or_else(|| retained::error(BuildErrorKind::Bytes))?;
        Ok(())
    }
    pub fn step(&mut self, budget: StepBudget) -> Result<RestoreProgress, RetainError> {
        let mut progress = RestoreProgress::default();
        while progress.build.commands < budget.max_commands {
            if self.next_resource < self.leases.len() {
                let index = self.next_resource;
                let resource = &self.source.0.resources[index];
                if let SavedResourceKind::Asset(_) = resource.kind
                    && self.leases[index].is_none()
                {
                    progress.missing_asset = Some(index as u32);
                    break;
                }
                if !admit(&mut progress.build, budget, 1, 128) {
                    break;
                }
                if let SavedResourceKind::Inline(data) = &resource.kind {
                    let ticket = self
                        .pool
                        .reserve(data.bytes() as u64)
                        .map_err(|_| retained::error(BuildErrorKind::Bytes))?;
                    let version = ResourceVersion {
                        key: reel_core::identity::AssetKey(format!("retained/{index}")),
                        version: reel_core::identity::ContentVersion([0; 32]),
                    };
                    let handle = self
                        .registry
                        .register(version, data.data(), ticket)
                        .map_err(|_| retained::error(BuildErrorKind::InvalidRecord))?;
                    self.leases[index] = Some(
                        self.registry
                            .acquire(handle)
                            .map_err(|_| retained::error(BuildErrorKind::InvalidRecord))?,
                    );
                    self.registry.release(handle);
                }
                let lease = self.leases[index].as_ref().unwrap();
                let kind =
                    if lease.image_descriptor().is_some() || lease.texture_descriptor().is_some() {
                        ResourceKind::Image
                    } else {
                        match lease.data() {
                            ResourceData::EffectShader { .. } => ResourceKind::EffectShader,
                            ResourceData::Yuv420(_) => ResourceKind::Video,
                            ResourceData::Font(_) => ResourceKind::Font,
                            _ => return Err(retained::error(BuildErrorKind::UnsupportedRecord)),
                        }
                    };
                if self
                    .handles
                    .insert(
                        resource.handle,
                        ReboundResource {
                            handle: lease.handle(),
                            kind,
                        },
                    )
                    .is_some()
                {
                    return Err(retained::error(BuildErrorKind::InvalidRecord));
                }
                self.next_resource += 1;
                continue;
            }
            if self.drawings.len() == self.source.0.nodes.len() {
                progress.build.complete = true;
                break;
            }
            if self.building.is_none() {
                let drawing = &self.source.0.nodes[self.drawings.len()].drawing;
                let commands = drawing.commands();
                let count = commands.len();
                let bytes = (size_of::<FrameDrawing>()
                    + std::mem::size_of_val(commands)
                    + std::mem::size_of_val(drawing.capture_exclusions()))
                    as u64;
                if !admit(
                    &mut progress.build,
                    budget,
                    1 + drawing.capture_exclusions().len() as u64,
                    bytes,
                ) {
                    break;
                }
                validate::capture_exclusions(commands.len(), drawing.capture_exclusions())?;
                self.charge(bytes)?;
                let capture_exclusions = self.source.0.nodes[self.drawings.len()]
                    .drawing
                    .capture_exclusions()
                    .to_vec();
                self.building = Some(Building {
                    commands: Vec::with_capacity(count),
                    capture_exclusions,
                    masks: BTreeMap::new(),
                    current_mask: None,
                    last_mask: None,
                    masks_complete: false,
                    transitions: Vec::new(),
                    resources: Vec::new(),
                    handles: BTreeMap::new(),
                    ticket: self
                        .pool
                        .reserve(bytes)
                        .map_err(|_| retained::error(BuildErrorKind::Bytes))?,
                    balance: validate::Balance::default(),
                    commands_validated: false,
                    exclusion_balance: validate::Balance::default(),
                    exclusion_next: 0,
                });
                continue;
            }
            let node = &self.source.0.nodes[self.drawings.len()];
            let build = self.building.as_mut().unwrap();
            if build.resources.len() < node.resources.len() {
                let bytes = size_of::<ResourceLease>() as u64 + 128;
                if !admit(&mut progress.build, budget, 1, bytes) {
                    break;
                }
                let slot = node.resources[build.resources.len()] as usize;
                let lease = self
                    .leases
                    .get(slot)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| retained::error(BuildErrorKind::InvalidRecord))?;
                let old = self.source.0.resources[slot].handle;
                let rebound = self.handles[&old];
                if build.handles.insert(old, rebound).is_some() {
                    return Err(retained::error(BuildErrorKind::InvalidRecord));
                }
                build
                    .ticket
                    .resize(build.ticket.bytes() + bytes)
                    .map_err(|_| retained::error(BuildErrorKind::Bytes))?;
                build.resources.push(lease.clone());
                self.charge(bytes)?;
                continue;
            }
            if build.commands.len() == node.drawing.commands().len() && !build.commands_validated {
                build.balance.finish()?;
                build.commands_validated = true;
            }
            let next = if build.commands.len() < node.drawing.commands().len() {
                Some((None, &node.drawing.commands()[build.commands.len()]))
            } else if !build.masks_complete {
                if build.current_mask.is_none() {
                    let next = match build.last_mask {
                        None => node.drawing.masks().first_key_value(),
                        Some(last) => node
                            .drawing
                            .masks()
                            .range((std::ops::Bound::Excluded(last), std::ops::Bound::Unbounded))
                            .next(),
                    };
                    if let Some((&id, mask)) = next {
                        let bytes = (size_of::<FrameMask>()
                            + mask.commands.len() * size_of::<FrameCommand>())
                            as u64
                            + 128;
                        if !admit(&mut progress.build, budget, 1, bytes) {
                            break;
                        }
                        self.bytes = self
                            .bytes
                            .checked_add(bytes)
                            .filter(|n| *n <= self.limits.max_bytes)
                            .ok_or_else(|| retained::error(BuildErrorKind::Bytes))?;
                        build
                            .ticket
                            .resize(build.ticket.bytes() + bytes)
                            .map_err(|_| retained::error(BuildErrorKind::Bytes))?;
                        build.current_mask = Some(id);
                        build.masks.insert(
                            id,
                            Arc::new(FrameMask {
                                mode: mask.mode,
                                commands: Vec::with_capacity(mask.commands.len()),
                            }),
                        );
                        continue;
                    } else {
                        build.masks_complete = true;
                    }
                }
                if let Some(id) = build.current_mask {
                    let index = build.masks[&id].commands.len();
                    if let Some(command) = node.drawing.masks()[&id].commands.get(index) {
                        Some((Some(id), command))
                    } else {
                        build.balance.finish()?;
                        build.last_mask = build.current_mask.take();
                        continue;
                    }
                } else {
                    None
                }
            } else {
                None
            };
            if let Some((mask, command)) = next {
                let bytes = retained::command_payload_bytes(command) + 64;
                let units = retained::command_bytes(command).div_ceil(8);
                if !admit(&mut progress.build, budget, units, bytes) {
                    break;
                }
                if self
                    .bytes
                    .checked_add(bytes)
                    .is_none_or(|n| n > self.limits.max_bytes)
                {
                    return Err(retained::error(BuildErrorKind::Bytes));
                }
                build
                    .ticket
                    .resize(build.ticket.bytes() + bytes)
                    .map_err(|_| retained::error(BuildErrorKind::Bytes))?;
                self.bytes += bytes;
                validate::command(command, mask, node.drawing.masks())?;
                build.balance.command(&command.op)?;
                if mask.is_none()
                    && let Some(range) = build.capture_exclusions.get(build.exclusion_next)
                    && range.contains(&build.commands.len())
                {
                    build.exclusion_balance.command(&command.op)?;
                    if build.commands.len() + 1 == range.end {
                        build.exclusion_balance.finish()?;
                        build.exclusion_next += 1;
                    }
                }
                let mut command = command.clone();
                remap_command(&mut command, &build.handles)?;
                if let FrameOp::PushGroup(group) = &command.op {
                    for (effect, coverage) in group
                        .effect
                        .iter()
                        .map(|effect| (effect, false))
                        .chain(group.mask_effect.iter().map(|effect| (effect, true)))
                    {
                        if !build.resources.iter().any(|lease| lease.handle() == effect.shader
                            && matches!(lease.data(), ResourceData::EffectShader { descriptor, .. }
                                if descriptor.definition == effect.definition
                                    && descriptor.accepts(&effect.values)
                                    && descriptor.textures as usize == effect.images.len()
                                    && (!coverage || descriptor.output == crate::stage::effect::EffectType::Float))) {
                            return Err(retained::error(BuildErrorKind::InvalidRecord));
                        }
                    }
                }
                if let Some(id) = mask {
                    Arc::get_mut(build.masks.get_mut(&id).unwrap())
                        .unwrap()
                        .commands
                        .push(command);
                } else {
                    build.commands.push(command);
                }
                continue;
            }
            if !build.masks_complete {
                continue;
            }
            if build.transitions.len() < node.transitions.len() {
                let transition = &node.transitions[build.transitions.len()];
                let bytes = size_of::<SceneTransition>() as u64
                    + 64
                    + transition
                        .mask
                        .as_ref()
                        .map_or(0, |mask| mask.images.len() * size_of::<ResourceHandle>())
                        as u64;
                let units = bytes.div_ceil(8)
                    + transition
                        .commands
                        .end
                        .saturating_sub(transition.commands.start) as u64;
                if !admit(&mut progress.build, budget, units, bytes) {
                    break;
                }
                validate::transition(
                    transition,
                    self.drawings.len(),
                    build
                        .transitions
                        .len()
                        .checked_sub(1)
                        .map(|i| &node.transitions[i]),
                    &build.commands,
                )?;
                build
                    .ticket
                    .resize(build.ticket.bytes() + bytes)
                    .map_err(|_| retained::error(BuildErrorKind::Bytes))?;
                let mut mask = transition.mask.clone();
                if let Some(mask) = &mut mask {
                    remap_mask(mask, &build.handles)?;
                }
                build.transitions.push(SceneTransition {
                    commands: transition.commands.clone(),
                    capture_included: transition.capture_included,
                    previous: RetainedScene {
                        drawing: self.drawings[transition.previous].clone(),
                        to_parent: transition.to_parent,
                        pool: self.pool.clone(),
                    },
                    progress: transition.progress,
                    mask,
                    parent_to_window: transition.parent_to_window,
                    mask_to_window: transition.mask_to_window,
                });
                self.charge(bytes)?;
                continue;
            }
            if !admit(&mut progress.build, budget, 1, 0) {
                break;
            }
            let build = self.building.take().unwrap();
            let resources = ResourceSnapshot::retain(build.resources, &self.pool)
                .map_err(|_| retained::error(BuildErrorKind::Bytes))?;
            self.drawings.push(Arc::new(FrameDrawing {
                commands: build.commands,
                recordings: Vec::new(),
                capture_exclusions: build.capture_exclusions,
                masks: build.masks,
                transitions: build.transitions,
                resources,
                accounted_bytes: build.ticket.bytes(),
                _ticket: build.ticket,
            }));
        }
        progress.build.complete = self.next_resource == self.leases.len()
            && self.drawings.len() == self.source.0.nodes.len();
        Ok(progress)
    }
    pub fn finish(mut self) -> Result<RetainedScene, RetainError> {
        if self.drawings.len() != self.source.0.nodes.len() {
            return Err(retained::error(BuildErrorKind::Incomplete));
        }
        Ok(RetainedScene {
            drawing: self.drawings.pop().unwrap(),
            to_parent: self.source.0.to_parent,
            pool: self.pool,
        })
    }
}

fn admit(progress: &mut BuildProgress, budget: StepBudget, units: u64, bytes: u64) -> bool {
    if progress.units + units > budget.max_units || progress.bytes + bytes > budget.max_bytes {
        progress.minimum_units = units;
        progress.minimum_bytes = bytes;
        return false;
    }
    progress.commands += 1;
    progress.units += units;
    progress.bytes += bytes;
    true
}
fn remap(
    handle: &mut ResourceHandle,
    handles: &BTreeMap<ResourceHandle, ReboundResource>,
    accepts: impl FnOnce(ResourceKind) -> bool,
) -> Result<(), RetainError> {
    let resource = handles
        .get(handle)
        .filter(|resource| accepts(resource.kind))
        .ok_or_else(|| retained::error(BuildErrorKind::InvalidRecord))?;
    *handle = resource.handle;
    Ok(())
}
fn remap_mask(
    mask: &mut FrameImageMask,
    handles: &BTreeMap<ResourceHandle, ReboundResource>,
) -> Result<(), RetainError> {
    for image in &mut mask.images {
        remap(image, handles, |kind| kind == ResourceKind::Image)?;
    }
    Ok(())
}
fn remap_command(
    command: &mut FrameCommand,
    handles: &BTreeMap<ResourceHandle, ReboundResource>,
) -> Result<(), RetainError> {
    let brush = match &mut command.op {
        FrameOp::Draw(FrameDraw::Fill { brush, .. } | FrameDraw::Stroke { brush, .. }) => {
            Some(brush)
        }
        FrameOp::Draw(FrameDraw::Sprites(s)) => {
            remap(&mut s.brush.image.resource, handles, |kind| {
                kind == ResourceKind::Image
            })?;
            None
        }
        FrameOp::Draw(FrameDraw::GlyphRun(glyphs)) => {
            remap(&mut glyphs.font, handles, |kind| kind == ResourceKind::Font)?;
            Some(&mut glyphs.brush)
        }
        FrameOp::PushGroup(group) => {
            for effect in group.effect.iter_mut().chain(group.mask_effect.iter_mut()) {
                remap(&mut effect.shader, handles, |kind| {
                    kind == ResourceKind::EffectShader
                })?;
                for image in &mut effect.images {
                    remap(image, handles, |kind| kind == ResourceKind::Image)?;
                }
            }
            if let Some(mask) = &mut group.image_mask {
                remap_mask(mask, handles)?;
            }
            None
        }
        FrameOp::External(_) => return Err(retained::error(BuildErrorKind::UnsupportedRecord)),
        _ => None,
    };
    if let Some(Brush::Image(image)) = brush {
        remap(&mut image.image.resource, handles, |kind| {
            matches!(kind, ResourceKind::Image | ResourceKind::Video)
        })?;
    }
    Ok(())
}
