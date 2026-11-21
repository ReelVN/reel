//! Persistent widgets for the shared author stage. Property edits keep widget identities.
use reel_ui::{
    accesskit::{Node, Role},
    core::*,
    imaging::Painter,
    kurbo::{Axis, Insets, Point, Rect, Size, Vec2},
    layout::{LenDef, LenReq, Length, SizeDef},
    peniko::Color,
    properties::{
        Background, BorderColor, BorderWidth, ContentColor, CornerRadius, ThumbColor, ThumbRadius,
        TrackColor, TrackThickness,
    },
    resources::{EffectShaderFormat, ResourceLease},
    stage::{
        ControlValue, InteractionState, NodeChange, NodeClick, NodeEvent, NodeId, NodeInteraction,
        NodeKind, NodeProps, NodeRevision, NodeText, NodeVirtualList, Stage, StageIdentity,
        VirtualListNavigation, VirtualRowMeasurement,
    },
    widgets::{
        ButtonBehavior, Image, Label, ScrollAxis, ScrollState, Slider, SliderMoved, TextInput,
        TextInputEvent,
    },
};
#[cfg(test)]
use reel_ui::{peniko::ImageData, resources::ResourceData};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Selection authorized by the UI's accepted-frame group navigation. Deferred
/// navigation completes outside the synchronous keyboard/pointer dispatch span.
#[derive(Debug)]
pub(crate) struct NavigationSelection(pub NodeChange);
mod flow;
mod image_demand;
mod resource_index;
use resource_index::ResourceIndexCache;
pub(crate) use resource_index::{ImageRequests, ResourceIndex};
mod input;
pub(crate) mod popup;
#[cfg(test)]
mod projection_tests;
mod scrollbar;
mod selection;
#[cfg(test)]
mod tests;
mod virtual_images;
pub(crate) use virtual_images::OffscreenImage;
mod virtual_list;
use input::NodeInput;
use virtual_list::VirtualListState;

#[derive(Clone, Default)]
pub(crate) struct StageView {
    pub revision: u64,
    pub button_revision: u64,
    pub presentation_request: u64,
    pub feedback: bool,
    pub(crate) identity: Option<StageIdentity>,
    root: Option<Arc<SceneNode>>,
    popups: Option<Arc<[popup::PopupSource]>>,
    image_index: Arc<ResourceIndexCache>,
    layout_images: Option<(u64, EffectShaderFormat)>,
    pub resources: Arc<[ResourceLease]>,
}
#[derive(Clone, PartialEq)]
struct SceneProperties {
    id: NodeId,
    kind: NodeKind,
    props: NodeProps,
}
#[derive(Clone, Copy, Default)]
struct SceneSummary {
    image: bool,
    button: bool,
    input: bool,
    virtual_list: bool,
    popup: bool,
    modal: Option<NodeId>,
}
#[derive(Clone)]
struct SceneNode {
    node: Arc<SceneProperties>,
    revision: NodeRevision,
    children: Arc<[Arc<SceneNode>]>,
    children_summary: SceneSummary,
    image_branch: bool,
    button_branch: bool,
    input_branch: bool,
    virtual_list_branch: bool,
    modal: Option<NodeId>,
    images: SceneImages,
}
#[derive(Clone)]
struct ResolvedEffect {
    shader: ResourceLease,
    images: Arc<[ResourceLease]>,
}
impl ResolvedEffect {
    fn resolve(
        effect: &reel_ui::stage::effect::EffectInstance,
        images: &BTreeMap<String, ResourceLease>,
        format: EffectShaderFormat,
    ) -> Option<Self> {
        let shader = images.get(&format.asset_path(effect.definition()))?.clone();
        let images = effect
            .textures()
            .iter()
            .map(|source| images.get(source.as_ref()).cloned())
            .collect::<Option<Vec<_>>>()?
            .into();
        Some(Self { shader, images })
    }
    fn leases(&self) -> impl Iterator<Item = &ResourceLease> {
        std::iter::once(&self.shader).chain(self.images.iter())
    }
    fn paint(
        &self,
        instance: &Arc<reel_ui::stage::effect::EffectInstance>,
        bounds: Rect,
        coverage_invert: bool,
        resolution_scale: f64,
    ) -> PaintEffect {
        PaintEffect {
            instance: instance.clone(),
            shader: self.shader.clone(),
            images: self.images.to_vec(),
            bounds,
            coverage_invert,
            resolution_scale,
        }
    }
}
impl PartialEq for ResolvedEffect {
    fn eq(&self, other: &Self) -> bool {
        self.leases()
            .map(ResourceLease::handle)
            .eq(other.leases().map(ResourceLease::handle))
    }
}
#[derive(Clone, Default)]
struct SceneImages {
    effect: Option<ResolvedEffect>,
    mask_effect: Option<ResolvedEffect>,
    image_resource: Option<ResourceLease>,
    mask_images: Arc<[ResourceLease]>,
    layout_size: Option<Size>,
    tiled_size: Option<Size>,
    tiles: Vec<reel_ui::widgets::ImageTile>,
}
impl PartialEq for SceneImages {
    fn eq(&self, other: &Self) -> bool {
        self.effect == other.effect
            && self.mask_effect == other.mask_effect
            && self.image_resource.as_ref().map(ResourceLease::handle)
                == other.image_resource.as_ref().map(ResourceLease::handle)
            && self
                .mask_images
                .iter()
                .map(ResourceLease::handle)
                .eq(other.mask_images.iter().map(ResourceLease::handle))
            && self.layout_size == other.layout_size
            && self.tiled_size == other.tiled_size
            && self.tiles.len() == other.tiles.len()
            && self.tiles.iter().zip(&other.tiles).all(|(a, b)| {
                a.resource.handle() == b.resource.handle()
                    && a.transform == b.transform
                    && a.clip == b.clip
            })
    }
}
impl SceneNode {
    fn has_fixed_image_size(&self) -> bool {
        match self.node.kind {
            NodeKind::Image { source_rect, .. } => {
                source_rect.is_some()
                    || (self.node.props.width.is_some() && self.node.props.height.is_some())
            }
            _ => false,
        }
    }
    fn needs_intrinsic_image_size(&self) -> bool {
        matches!(self.node.kind, NodeKind::Image { .. }) && !self.has_fixed_image_size()
    }
    fn popup_branch(&self) -> bool {
        matches!(self.node.kind, NodeKind::Popup(_)) || self.children_summary.popup
    }
    fn image_visible(&self, parent_visible: bool, parent_enabled: bool, has_parent: bool) -> bool {
        parent_visible
            && self.node.props.visible
            && (!matches!(self.node.kind, NodeKind::Popup(_))
                || (has_parent && parent_enabled && self.node.props.enabled))
    }
    fn physical_children(&self) -> impl DoubleEndedIterator<Item = &Arc<SceneNode>> {
        self.children
            .iter()
            .filter(|child| !matches!(child.node.kind, NodeKind::Popup(_)))
    }
    fn image_size(&self) -> Size {
        if let NodeKind::Image {
            source_rect: Some(rect),
            ..
        } = &self.node.kind
        {
            return rect.size();
        }
        let size = self
            .images
            .image_resource
            .as_ref()
            .and_then(|r| r.image_size())
            .map(|[w, h]| Size::new(w.into(), h.into()))
            .or(self.images.tiled_size)
            .or(self.images.layout_size)
            .unwrap_or(Size::ZERO);
        match &self.node.kind {
            NodeKind::Image { alpha_layout, .. } => alpha_layout.size(size),
            _ => size,
        }
    }
}
impl StageView {
    #[cfg(test)]
    pub fn capture(stage: &Stage) -> Self {
        Self::update(stage, None)
    }
    pub fn same_stage(&self, other: &Self) -> bool {
        self.identity == other.identity
    }
    pub fn matches(&self, stage: &Stage) -> bool {
        self.identity.as_ref() == Some(stage.identity()) && self.revision == stage.revision()
    }
    /// Local feedback may change completed layout/appearance properties, while
    /// topology, control content and input ownership remain the committed ones.
    pub(crate) fn permits_feedback(&self, stage: &Stage) -> bool {
        fn visit(old: &SceneNode, stage: &Stage, id: NodeId) -> bool {
            let Some(node) = stage.node(id) else {
                return false;
            };
            let same_kind = match (&old.node.kind, &node.kind) {
                (
                    NodeKind::Scroll {
                        axis: a,
                        follow_end: b,
                        align: c,
                        ..
                    },
                    NodeKind::Scroll {
                        axis: x,
                        follow_end: y,
                        align: z,
                        ..
                    },
                ) => a == x && b == y && c == z,
                (a, b) => !SceneWidget::content_changed(a, b),
            };
            old.node.id == id
                && same_kind
                && old.node.props.visible == node.props.visible
                && old.node.props.enabled == node.props.enabled
                && old.node.props.modal == node.props.modal
                && old.children.len() == node.children.len()
                && old
                    .children
                    .iter()
                    .zip(&node.children)
                    .all(|(old, id)| visit(old, stage, *id))
        }
        self.identity.as_ref() == Some(stage.identity())
            && self
                .root
                .as_ref()
                .zip(stage.root())
                .is_some_and(|(old, id)| visit(old, stage, id))
    }
    pub fn update(stage: &Stage, previous: Option<&Self>) -> Self {
        fn capture_node(stage: &Stage, id: NodeId, old: Option<&Arc<SceneNode>>) -> Arc<SceneNode> {
            let revision = stage.node_revision(id).expect("mounted stage node");
            let old = old.filter(|old| old.node.id == id);
            if let Some(old) = old
                && old.revision == revision
            {
                return old.clone();
            }
            let node = if let Some(old) = old
                && old.revision.properties == revision.properties
            {
                old.node.clone()
            } else {
                let source = stage.node(id).expect("mounted stage node");
                Arc::new(SceneProperties {
                    id,
                    kind: source.kind.clone(),
                    props: source.props.clone(),
                })
            };
            let children = if let Some(old) = old
                && old.revision.children == revision.children
            {
                old.children.clone()
            } else {
                let source = stage.node(id).expect("mounted stage node");
                let reordered = old
                    .filter(|old| {
                        !old.children
                            .iter()
                            .map(|n| n.node.id)
                            .eq(source.children.iter().copied())
                    })
                    .map(|old| {
                        old.children
                            .iter()
                            .map(|n| (n.node.id, n))
                            .collect::<BTreeMap<_, _>>()
                    });
                source
                    .children
                    .iter()
                    .enumerate()
                    .map(|(index, &id)| {
                        let previous = match &reordered {
                            Some(children) => children.get(&id).copied(),
                            None => old.and_then(|old| old.children.get(index)),
                        };
                        capture_node(stage, id, previous)
                    })
                    .collect::<Arc<[_]>>()
            };
            let children_summary = if let Some(old) = old
                && Arc::ptr_eq(&old.children, &children)
            {
                old.children_summary
            } else {
                children
                    .iter()
                    .fold(SceneSummary::default(), |mut summary, child| {
                        summary.image |= child.image_branch;
                        summary.button |= child.button_branch;
                        summary.input |= child.input_branch;
                        summary.virtual_list |= child.virtual_list_branch;
                        summary.popup |= child.popup_branch();
                        if !matches!(child.node.kind, NodeKind::Popup(_)) {
                            summary.modal = child.modal.or(summary.modal);
                        }
                        summary
                    })
            };
            let image_branch = matches!(node.kind, NodeKind::Image { .. } | NodeKind::Particles(_))
                || node.props.mask.is_some()
                || node.props.effect.is_some()
                || node.props.mask_effect.is_some()
                || children_summary.image;
            let button_branch =
                matches!(node.kind, NodeKind::Button { .. }) || children_summary.button;
            let input_branch =
                matches!(node.kind, NodeKind::Input { .. }) || children_summary.input;
            let virtual_list_branch =
                matches!(node.kind, NodeKind::VirtualList(_)) || children_summary.virtual_list;
            let modal = node
                .props
                .visible
                .then(|| {
                    children_summary
                        .modal
                        .or_else(|| node.props.modal.then_some(id))
                })
                .flatten();
            let mut images = old.map(|old| old.images.clone()).unwrap_or_default();
            if !matches!(node.kind, NodeKind::Image { .. } | NodeKind::Particles(_)) {
                images.image_resource = None;
                images.tiled_size = None;
                images.tiles.clear();
            }
            if node.props.mask.is_none() {
                images.mask_images = Arc::from([]);
            }
            if node.props.effect.is_none() {
                images.effect = None;
            }
            if node.props.mask_effect.is_none() {
                images.mask_effect = None;
            }
            Arc::new(SceneNode {
                node,
                revision,
                children,
                children_summary,
                image_branch,
                button_branch,
                input_branch,
                virtual_list_branch,
                modal,
                images,
            })
        }
        let previous = previous.filter(|view| view.identity.as_ref() == Some(stage.identity()));
        let root = stage
            .root()
            .map(|id| capture_node(stage, id, previous.and_then(|v| v.root.as_ref())));
        let popups = popup::sources(root.as_ref(), previous, stage.has_popups());
        let image_index = previous
            .filter(|old| resource_index::same_inputs(root.as_ref(), old.root.as_ref()))
            .map_or_else(
                || Arc::new(ResourceIndexCache::default()),
                |old| old.image_index.clone(),
            );
        let layout_images = previous
            .filter(|old| Arc::ptr_eq(&old.image_index, &image_index))
            .and_then(|old| old.layout_images);
        Self {
            revision: stage.revision(),
            identity: Some(stage.identity().clone()),
            button_revision: 0,
            presentation_request: 0,
            feedback: false,
            root,
            popups,
            image_index,
            layout_images,
            resources: previous.map_or_else(|| Arc::from([]), |view| view.resources.clone()),
        }
    }
    pub fn input_scope(&self) -> (Option<NodeId>, Option<NodeId>) {
        (
            self.root.as_ref().map(|root| root.node.id),
            self.popups
                .as_deref()
                .unwrap_or_default()
                .iter()
                .rev()
                .filter(|popup| popup.popup().hint.is_none())
                .find_map(|popup| popup.root.modal)
                .or_else(|| {
                    self.root
                        .as_ref()
                        .filter(|root| !matches!(root.node.kind, NodeKind::Popup(_)))
                        .and_then(|root| root.modal)
                }),
        )
    }
    pub fn has_root(&self) -> bool {
        self.root.is_some()
    }
    /// Compare button content only. Animated geometry uses accepted hit testing;
    /// changes outside a control cannot invalidate its activation.
    /// `None` preserves the existing bindings when every button is unchanged.
    pub fn button_changes(
        &self,
        previous: Option<&Self>,
        feedback: bool,
    ) -> Option<Vec<(NodeId, bool)>> {
        fn buttons(view: &StageView) -> BTreeMap<NodeId, &Arc<SceneNode>> {
            let mut found = BTreeMap::new();
            let mut pending: Vec<_> = view.root.iter().collect();
            while let Some(node) = pending.pop() {
                if !node.button_branch {
                    continue;
                }
                if matches!(node.node.kind, NodeKind::Button { .. }) {
                    found.insert(node.node.id, node);
                }
                pending.extend(node.children.iter());
            }
            found
        }
        fn same_content(a: &Arc<SceneNode>, b: &Arc<SceneNode>) -> bool {
            let mut pending = smallvec::SmallVec::<[(_, _); 8]>::new();
            pending.push((a, b));
            while let Some((a, b)) = pending.pop() {
                if Arc::ptr_eq(a, b) {
                    continue;
                }
                if a.node.id != b.node.id
                    || SceneWidget::content_changed(&a.node.kind, &b.node.kind)
                    || a.node.props.visible != b.node.props.visible
                    || a.node.props.enabled != b.node.props.enabled
                {
                    return false;
                }
                if !Arc::ptr_eq(&a.children, &b.children) {
                    // Portals keep logical ownership but render outside this control.
                    let mut a = a.physical_children();
                    let mut b = b.physical_children();
                    loop {
                        match (a.next(), b.next()) {
                            (Some(a), Some(b)) => pending.push((a, b)),
                            (None, None) => break,
                            _ => return false,
                        }
                    }
                }
            }
            true
        }
        fn same_buttons(a: &Arc<SceneNode>, b: &Arc<SceneNode>, feedback: bool) -> bool {
            let mut pending = smallvec::SmallVec::<[(_, _); 8]>::new();
            pending.push((a, b));
            while let Some((a, b)) = pending.pop() {
                if Arc::ptr_eq(a, b) || (!a.button_branch && !b.button_branch) {
                    continue;
                }
                if a.node.id != b.node.id {
                    return false;
                }
                match (
                    matches!(a.node.kind, NodeKind::Button { .. }),
                    matches!(b.node.kind, NodeKind::Button { .. }),
                ) {
                    (true, true) if !feedback => {
                        if !same_content(a, b) {
                            return false;
                        }
                        // Buttons inside owned portals still need their own bindings.
                        if !a.popup_branch() && !b.popup_branch() {
                            continue;
                        }
                    }
                    (true, false) | (false, true) => return false,
                    _ => {}
                }
                if !Arc::ptr_eq(&a.children, &b.children) {
                    if a.children.len() != b.children.len() {
                        return false;
                    }
                    pending.extend(a.children.iter().zip(b.children.iter()));
                }
            }
            true
        }
        let previous = previous.filter(|old| old.identity == self.identity);
        if previous.is_some_and(|old| match (&old.root, &self.root) {
            (Some(a), Some(b)) => same_buttons(a, b, feedback),
            (None, None) => true,
            _ => false,
        }) {
            return None;
        }
        let previous = previous.map(buttons).unwrap_or_default();
        Some(
            buttons(self)
                .into_iter()
                .map(|(id, node)| {
                    let changed = previous
                        .get(&id)
                        .is_none_or(|old| !feedback && !same_content(old, node));
                    (id, changed)
                })
                .collect(),
        )
    }
    #[cfg(test)]
    pub fn resolve_images(
        &mut self,
        images: &BTreeMap<String, ResourceLease>,
        format: EffectShaderFormat,
    ) -> Result<(), String> {
        let mut leases = Vec::new();
        let mut handles = std::collections::BTreeSet::new();
        resolve_image_nodes(&mut self.root, |node, visible| {
            let mut resolved = SceneImages::default();
            if !visible {
                return resolved;
            }
            for (effect, destination) in [
                (&node.node.props.effect, &mut resolved.effect),
                (&node.node.props.mask_effect, &mut resolved.mask_effect),
            ] {
                *destination = effect
                    .as_ref()
                    .and_then(|effect| ResolvedEffect::resolve(effect, images, format));
                for lease in destination.iter().flat_map(ResolvedEffect::leases) {
                    if handles.insert(lease.handle()) {
                        leases.push(lease.clone());
                    }
                }
            }
            if let Some((source, _)) = image_source(&node.node.kind) {
                let lease = &images[source];
                resolved.image_resource = Some(lease.clone());
                if handles.insert(lease.handle()) {
                    leases.push(lease.clone());
                }
            }
            if let Some(lease) = image_resource(&node.node.kind) {
                resolved.image_resource = Some(lease.clone());
                if handles.insert(lease.handle()) {
                    leases.push(lease.clone());
                }
            }
            if let Some(mask) = &node.node.props.mask {
                resolved.mask_images = mask
                    .sources()
                    .iter()
                    .map(|source| {
                        let lease = &images[source.as_ref()];
                        if handles.insert(lease.handle()) {
                            leases.push(lease.clone());
                        }
                        lease.clone()
                    })
                    .collect();
            }
            resolved
        });
        self.resources = leases.into();
        self.refresh_popups();
        Ok(())
    }
    pub fn resolve_metadata(&mut self, images: &crate::images::Images) -> Result<(), String> {
        self.resolve_metadata_for(images, true)
    }
    pub(crate) fn resolve_layout_images(
        &mut self,
        images: &crate::images::Images,
    ) -> Result<(), String> {
        // Geometry edits preserve resolved whole-image sizes and leases. Viewport
        // demand is still recomputed below the layout pass; tiled images retain
        // their per-node compatibility checks and visible-region resolution.
        if images.manifests.is_empty()
            && self.layout_images == Some((images.content_revision, images.shader_format))
        {
            return Ok(());
        }
        self.resolve_metadata_for(images, false)
    }
    fn resolve_metadata_for(
        &mut self,
        images: &crate::images::Images,
        required_only: bool,
    ) -> Result<(), String> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::UiImageResolve);
        let mut leases = Vec::new();
        let mut resources = std::collections::BTreeSet::new();
        let mut failure = None;
        resolve_image_nodes(&mut self.root, |node, visible| {
            let mut resolved = SceneImages::default();
            if !visible || failure.is_some() {
                return resolved;
            }
            for (effect, destination) in [
                (&node.node.props.effect, &mut resolved.effect),
                (&node.node.props.mask_effect, &mut resolved.mask_effect),
            ] {
                let Some(effect) = effect else { continue };
                if let Some(source) = effect
                    .textures()
                    .iter()
                    .find(|source| images.manifests.contains_key(source.as_ref()))
                {
                    failure = Some(format!(
                        "{}: effect texture {source:?}: effects require a whole PNG, JPEG or .reel-texture image",
                        node.node
                            .props
                            .appearance_origin
                            .as_deref()
                            .unwrap_or("stage")
                    ));
                    break;
                }
                *destination =
                    ResolvedEffect::resolve(effect, &images.loaded, images.shader_format);
                for lease in destination.iter().flat_map(ResolvedEffect::leases) {
                    if resources.insert(lease.handle()) {
                        leases.push(lease.clone());
                    }
                }
            }
            if failure.is_some() {
                return resolved;
            }
            if let Some(lease) = image_resource(&node.node.kind) {
                resolved.image_resource = Some(lease.clone());
                if resources.insert(lease.handle()) {
                    leases.push(lease.clone());
                }
            }
            if let Some((source, origin)) = image_source(&node.node.kind)
                && (!required_only || images.is_required(source))
            {
                if let Some(manifest) = images.manifests.get(source) {
                    let restriction = match &node.node.kind {
                        NodeKind::Particles(_) => {
                            Some("particles require a whole PNG, JPEG or .reel-texture sprite")
                        }
                        NodeKind::Image {
                            alpha_layout: reel_ui::frame::ImageAlphaLayout::TopBottom,
                            ..
                        } => Some(
                            "alpha_layout top_bottom requires a whole PNG, JPEG or .reel-texture image",
                        ),
                        NodeKind::Image { slice: Some(_), .. } => Some(
                            "nine-slice images require a whole PNG, JPEG or .reel-texture image",
                        ),
                        NodeKind::Image { repeat, .. } if *repeat != [false; 2] => {
                            Some("repetition requires a whole PNG, JPEG or .reel-texture image")
                        }
                        _ => None,
                    };
                    if let Some(restriction) = restriction {
                        failure = Some(format!(
                            "{}: image {source:?}: {restriction}; prepared image manifests support ordinary image nodes and source_rect",
                            origin.as_deref().unwrap_or("stage")
                        ));
                        return resolved;
                    }
                    resolved.tiled_size =
                        Some(Size::new(manifest.width.into(), manifest.height.into()));
                } else if let Some(lease) = images.loaded.get(source) {
                    resolved.image_resource = Some(lease.clone());
                    if resources.insert(lease.handle()) {
                        leases.push(lease.clone());
                    }
                } else if !required_only
                    && node.needs_intrinsic_image_size()
                    && let Some([width, height]) = images.pending_image_size(source)
                {
                    resolved.layout_size = Some(Size::new(width.into(), height.into()));
                }
            }
            if let Some(mask) = &node.node.props.mask {
                if let Some(source) = mask
                    .sources()
                    .iter()
                    .find(|source| images.manifests.contains_key(source.as_ref()))
                {
                    failure = Some(format!(
                        "{}: mask image {source:?}: masks require a whole PNG, JPEG or .reel-texture image",
                        node.node
                            .props
                            .appearance_origin
                            .as_deref()
                            .unwrap_or("stage")
                    ));
                    return resolved;
                }
                resolved.mask_images = mask
                    .sources()
                    .iter()
                    .filter_map(|source| {
                        let lease = images.loaded.get(source.as_ref())?;
                        if resources.insert(lease.handle()) {
                            leases.push(lease.clone());
                        }
                        Some(lease.clone())
                    })
                    .collect();
            }
            resolved
        });
        if let Some(error) = failure {
            return Err(error);
        }
        self.resources = leases.into();
        self.refresh_popups();
        self.layout_images = Some((images.content_revision, images.shader_format));
        Ok(())
    }
    pub fn resolve_tiles(
        &mut self,
        images: &crate::images::Images,
        regions: &BTreeMap<NodeId, crate::images::ImageRegion>,
    ) {
        let mut resources = std::collections::BTreeSet::new();
        resolve_image_nodes(&mut self.root, |node, visible| {
            if !visible {
                return SceneImages::default();
            }
            let mut resolved = node.images.clone();
            if let Some(region) = regions.get(&node.node.id) {
                resolved.tiles = images.tile_paint(region);
                resources.extend(region.tiles.iter().cloned());
            }
            resolved
        });
        self.refresh_popups();
        let mut handles: std::collections::BTreeSet<_> =
            self.resources.iter().map(ResourceLease::handle).collect();
        self.resources = self
            .resources
            .iter()
            .cloned()
            .chain(
                resources
                    .iter()
                    .filter_map(|key| images.loaded.get(key))
                    .filter(|lease| handles.insert(lease.handle()))
                    .cloned(),
            )
            .collect();
    }
}

/// Resource changes use the same persistent tree but remain independent of script revisions.
fn resolve_image_nodes(
    root: &mut Option<Arc<SceneNode>>,
    mut resolve: impl FnMut(&SceneNode, bool) -> SceneImages,
) {
    fn visit(
        node: &Arc<SceneNode>,
        parent_visible: bool,
        parent_enabled: bool,
        has_parent: bool,
        resolve: &mut impl FnMut(&SceneNode, bool) -> SceneImages,
    ) -> Arc<SceneNode> {
        if !node.image_branch {
            return node.clone();
        }
        let visible = node.image_visible(parent_visible, parent_enabled, has_parent);
        let enabled = parent_enabled
            && node.node.props.enabled
            && !matches!(node.node.kind, NodeKind::Cursor);
        let images = resolve(node, visible);
        let mut children = None;
        for (index, child) in node.children.iter().enumerate() {
            if !child.image_branch {
                continue;
            }
            let next = visit(child, visible, enabled, true, resolve);
            if !Arc::ptr_eq(child, &next) {
                children.get_or_insert_with(|| node.children.to_vec())[index] = next;
            }
        }
        if images == node.images && children.is_none() {
            return node.clone();
        }
        Arc::new(SceneNode {
            node: node.node.clone(),
            revision: node.revision,
            children: children
                .map(Arc::from)
                .unwrap_or_else(|| node.children.clone()),
            children_summary: node.children_summary,
            image_branch: node.image_branch,
            button_branch: node.button_branch,
            input_branch: node.input_branch,
            virtual_list_branch: node.virtual_list_branch,
            modal: node.modal,
            images,
        })
    }
    if let Some(node) = root {
        *node = visit(node, true, true, false, &mut resolve);
    }
}

fn image_source(kind: &NodeKind) -> Option<(&str, &Option<String>)> {
    match kind {
        NodeKind::Image { source, origin, .. }
            if !source.is_empty() && source.lease().is_none() =>
        {
            Some((source.key(), origin))
        }
        NodeKind::Particles(batch) => Some((&batch.source, &batch.origin)),
        _ => None,
    }
}

fn image_resource(kind: &NodeKind) -> Option<&ResourceLease> {
    if let NodeKind::Image { source, .. } = kind {
        source.lease()
    } else {
        None
    }
}

pub(crate) struct StageWidget {
    nodes: BTreeMap<NodeId, WidgetId>,
    root: Option<WidgetPod<SceneWidget>>,
    root_id: Option<NodeId>,
    pub(crate) identity: Option<StageIdentity>,
    root_props: reel_ui::stage::NodeProps,
    portal: bool,
}
impl StageWidget {
    pub(crate) fn image_widgets(
        this: &mut WidgetMut<'_, Self>,
    ) -> Vec<(NodeId, crate::images::tiles::ImageGeometry)> {
        fn visit(
            this: &mut WidgetMut<'_, SceneWidget>,
            mut effect_domain: Option<(WidgetId, Rect)>,
            out: &mut Vec<(NodeId, crate::images::tiles::ImageGeometry)>,
        ) {
            if !this.widget.node.props.visible || !this.widget.projection.image_branch {
                return;
            }
            if this.widget.node.props.effect.is_some()
                || this.widget.node.props.mask_effect.is_some()
            {
                effect_domain = Some((
                    this.ctx.widget_id(),
                    this.ctx
                        .window_transform()
                        .transform_rect_bbox(this.widget.size.to_rect()),
                ));
            }
            if let (
                Some(image),
                NodeKind::Image {
                    source,
                    fit,
                    source_rect,
                    ..
                },
            ) = (&this.widget.image, &this.widget.node.kind)
            {
                out.push((
                    this.widget.node.id,
                    crate::images::tiles::ImageGeometry {
                        source: source.key().to_owned(),
                        widget: image.id(),
                        fit: *fit,
                        source_rect: *source_rect,
                        effect_domain,
                    },
                ));
            }
            for (_, pod) in &mut this.widget.children {
                if let Some(mut child) = this.ctx.get_mut(pod) {
                    visit(&mut child, effect_domain, out);
                }
            }
        }
        let mut out = Vec::new();
        if let Some(root) = &mut this.widget.root
            && let Some(mut root) = this.ctx.get_mut(root)
        {
            visit(&mut root, None, &mut out);
        }
        out
    }
    pub fn new(view: Option<&StageView>) -> Self {
        Self::project(view, false)
    }
    pub(crate) fn new_portal(view: &StageView) -> Self {
        Self::project(Some(view), true)
    }
    fn project(view: Option<&StageView>, portal: bool) -> Self {
        let root = view
            .and_then(|view| view.root.as_ref())
            .filter(|root| portal || !matches!(root.node.kind, NodeKind::Popup(_)));
        let mut nodes = BTreeMap::new();
        let root_pod = root.map(|node| SceneWidget::new_pod(node, &mut nodes));
        Self {
            nodes,
            root: root_pod,
            root_id: root.map(|node| node.node.id),
            identity: view.and_then(|view| view.identity.clone()),
            root_props: root.map(|node| node.node.props.clone()).unwrap_or_default(),
            portal,
        }
    }
    pub(crate) fn node_widget(&self, node: NodeId) -> Option<WidgetId> {
        self.nodes.get(&node).copied()
    }
    pub(crate) fn scroll_reveal(
        root: &reel_ui::app::RenderRoot,
        viewport: WidgetId,
        [x, y, width, height]: [f64; 4],
        margins: [f64; 2],
    ) -> Option<reel_engine::aside::RevealProjection> {
        let widget = root.get_widget(viewport)?.downcast::<SceneWidget>()?;
        let NodeKind::Scroll { offset, .. } = widget.node.kind else {
            return None;
        };
        let bounds =
            Rect::new(x, y, x + width, y + height) - Vec2::splat(widget.node.props.padding);
        let rendered = widget.scroll.offset();
        let target = widget.scroll.reveal_offset(bounds, margins);
        Some(reel_engine::aside::RevealProjection {
            requested: [offset.x, offset.y],
            rendered: [rendered.x, rendered.y],
            target: [target.x, target.y],
        })
    }
    pub(crate) fn text_caret(
        &self,
        root: &reel_ui::app::RenderRoot,
        node: NodeId,
    ) -> Option<(WidgetId, reel_ui::kurbo::Line)> {
        let widget = root
            .get_widget(self.node_widget(node)?)?
            .downcast::<SceneWidget>()?;
        if !matches!(widget.node.kind, NodeKind::Text(_)) {
            return None;
        }
        let label_id = widget.label.as_ref()?.id();
        let label = root.get_widget(label_id)?.downcast::<Label>()?;
        Some((label_id, label.text_caret()?))
    }
    pub fn input_fallback(&self, root: &reel_ui::app::RenderRoot) -> Option<WidgetId> {
        fn visit(root: &reel_ui::app::RenderRoot, id: WidgetId) -> Option<WidgetId> {
            let node = root.get_widget(id)?.downcast::<SceneWidget>()?;
            if !node.projection.input_branch || !node.node.props.visible || !node.node.props.enabled
            {
                return None;
            }
            node.children
                .iter()
                .rev()
                .find_map(|(_, child)| visit(root, child.id()))
                .or_else(|| {
                    node.input
                        .as_ref()
                        .filter(|input| input.accepts_focus())
                        .map(|_| id)
                })
        }
        visit(root, self.root.as_ref()?.id())
    }
    pub fn sync(this: &mut WidgetMut<'_, Self>, view: &StageView) {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::UiWidgetSync);
        let root = view
            .root
            .as_ref()
            .filter(|root| this.widget.portal || !matches!(root.node.kind, NodeKind::Popup(_)));
        let id = root.map(|node| node.node.id);
        if id != this.widget.root_id || this.widget.identity != view.identity {
            if let Some(root) = this.widget.root.take() {
                this.ctx.remove_child(root);
            }
            this.widget.nodes.clear();
            this.widget.root = root.map(|node| SceneWidget::new_pod(node, &mut this.widget.nodes));
            this.widget.root_id = id;
            this.widget.identity = view.identity.clone();
            this.ctx.children_changed();
        } else if let (Some(pod), Some(node)) = (&mut this.widget.root, root) {
            let registered = if let Some(mut child) = this.ctx.get_mut(pod) {
                SceneWidget::sync(&mut child, node, view.feedback, &mut this.widget.nodes);
                true
            } else {
                false
            };
            if registered {
                this.ctx.set_stashed(pod, !node.node.props.visible);
            }
        }
        let props = root.map(|node| node.node.props.clone()).unwrap_or_default();
        if this.widget.root_props.visible != props.visible {
            this.ctx.request_layout();
        }
        if !this.widget.portal && transform_changed(&this.widget.root_props, &props) {
            this.ctx.request_compose();
        }
        this.widget.root_props = props;
    }
}
impl Widget for StageWidget {
    type Action = NoAction;
    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
        if let Some(root) = &mut self.root {
            ctx.register_child(root);
        }
    }
    fn measure(
        &mut self,
        ctx: &mut MeasureCtx<'_>,
        _: &PropertiesRef<'_>,
        axis: Axis,
        request: LenReq,
        cross: Option<Length>,
    ) -> Length {
        if self.portal
            && let Some(root) = &mut self.root
        {
            return ctx.redirect_measurement(root, axis, cross);
        }
        match request {
            LenReq::FitContent(space) => space,
            _ => Length::ZERO,
        }
    }
    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, size: Size) {
        if let Some(root) = &mut self.root {
            ctx.set_stashed(root, !self.root_props.visible);
            if self.root_props.visible {
                let chosen = ctx.compute_size(root, SizeDef::fit(size), size.into());
                ctx.run_layout(root, chosen);
                ctx.place_child(root, Point::ORIGIN);
            }
        }
    }
    fn compose(&mut self, ctx: &mut ComposeCtx<'_>) {
        if let Some(root) = &mut self.root {
            let transform = if self.portal {
                reel_ui::kurbo::Affine::IDENTITY
            } else {
                self.root_props.transform(ctx.child_size(root))
            };
            ctx.set_child_transform(root, transform);
            let viewport = virtual_list::VirtualViewport::new(ctx.content_box())
                .mapped(ctx.child_layout_transform(root));
            if let Some((root, mut root_ctx)) = ctx.get_raw_mut(root)
                && root.projection.virtual_list_branch
            {
                root.set_virtual_viewport(&mut root_ctx, viewport);
            }
        }
    }
    fn paint(&mut self, _: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, _: &mut Painter<'_>) {}
    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }
    fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, _: &mut Node) {}
    fn children_ids(&self) -> ChildrenIds {
        self.root.iter().map(WidgetPod::id).collect()
    }
    fn accepts_pointer_interaction(&self) -> bool {
        false
    }
}

struct SceneWidget {
    effect: Option<ResolvedEffect>,
    mask_effect: Option<ResolvedEffect>,
    node: Arc<SceneProperties>,
    projection: Arc<SceneNode>,
    sync_dirty: bool,
    button: ButtonBehavior,
    /// The first publication follows inherited state propagation in compose.
    interaction: Option<InteractionState>,
    input: Option<NodeInput>,
    slider: Option<WidgetPod<Slider>>,
    scrollbar: Option<reel_ui::widgets::ScrollBarState>,
    label: Option<WidgetPod<Label>>,
    editor: Option<WidgetPod<TextInput>>,
    image: Option<WidgetPod<Image>>,
    image_size: Size,
    particle_image: Option<ResourceLease>,
    particle_sampler: reel_ui::stage::particles::ParticleSampler,
    mask_images: Arc<[ResourceLease]>,
    children: Vec<(Arc<SceneProperties>, WidgetPod<SceneWidget>)>,
    size: Size,
    scroll: ScrollState,
    scroll_laid_out: bool,
    scroll_request: Option<Vec2>,
    scroll_feedback: Option<(Size, Size, ScrollAxis)>,
    scroll_baseline: Vec2,
    scroll_input: bool,
    virtual_list: Option<Box<VirtualListState>>,
    virtual_viewport: Option<virtual_list::VirtualViewport>,
    #[cfg(test)]
    layouts: usize,
    #[cfg(test)]
    syncs: usize,
    #[cfg(test)]
    paint_work: [usize; 3],
}
fn transform_changed(old: &reel_ui::stage::NodeProps, new: &reel_ui::stage::NodeProps) -> bool {
    [
        old.x,
        old.y,
        old.rotation,
        old.scale_x,
        old.scale_y,
        old.anchor_x,
        old.anchor_y,
    ] != [
        new.x,
        new.y,
        new.rotation,
        new.scale_x,
        new.scale_y,
        new.anchor_x,
        new.anchor_y,
    ]
}
fn composite_changed(old: &reel_ui::stage::NodeProps, new: &reel_ui::stage::NodeProps) -> bool {
    old.opacity != new.opacity
        || old.invert != new.invert
        || old.blend != new.blend
        || old.mask != new.mask
        || old.effect != new.effect
        || old.effect_resolution_scale != new.effect_resolution_scale
        || old.mask_effect != new.mask_effect
        || old.mask_progress != new.mask_progress
        || old.mask_softness != new.mask_softness
        || old.mask_invert != new.mask_invert
        || old.mask_x != new.mask_x
        || old.mask_y != new.mask_y
}
fn decoration_changed(old: &SceneProperties, new: &SceneProperties) -> bool {
    // Child widgets own text, image, editor and slider recording invalidation.
    // These are the fields consumed directly by SceneWidget::paint.
    let props = &old.props;
    let next = &new.props;
    if std::mem::discriminant(&old.kind) != std::mem::discriminant(&new.kind)
        || props.background != next.background
        || props.border_width != next.border_width
        || props.border_color != next.border_color
        || props.corner_radius != next.corner_radius
        || props.decorated != next.decorated
        || (props.color != next.color
            && matches!(
                new.kind,
                NodeKind::Button { .. }
                    | NodeKind::Toggle { .. }
                    | NodeKind::Radio { .. }
                    | NodeKind::ProgressBar { .. }
            ))
    {
        return true;
    }
    match &new.kind {
        NodeKind::Particles(particles) => {
            !matches!(&old.kind, NodeKind::Particles(previous) if previous == particles)
        }
        NodeKind::Toggle {
            value, appearance, ..
        } => {
            !matches!(&old.kind, NodeKind::Toggle { value: previous, appearance: previous_appearance, .. } if previous == value && previous_appearance == appearance)
        }
        NodeKind::Radio { selected, .. } => {
            !matches!(&old.kind, NodeKind::Radio { selected: previous, .. } if previous == selected)
        }
        NodeKind::ProgressBar { value, max } => {
            !matches!(&old.kind, NodeKind::ProgressBar { value: previous, max: previous_max } if previous / previous_max == value / max)
        }
        NodeKind::Text(_)
        | NodeKind::TextInput { .. }
        | NodeKind::Button { .. }
        | NodeKind::Slider { .. }
        | NodeKind::ScrollBar(_)
        | NodeKind::Image { .. }
        | NodeKind::Input { .. }
        | NodeKind::Scroll { .. }
        | NodeKind::Camera(_)
        | NodeKind::CameraPlane { .. }
        | NodeKind::Popup(_)
        | NodeKind::Row(_)
        | NodeKind::Column(_)
        | NodeKind::Grid(_)
        | NodeKind::VirtualList(_)
        | NodeKind::Stack
        | NodeKind::Cursor => false,
    }
}
fn color(value: [u8; 4]) -> Color {
    Color::from_rgba8(value[0], value[1], value[2], value[3])
}
fn font_style(font: &str) -> StyleProperty {
    StyleProperty::FontFamily(reel_ui::reel_text::FontFamily::Source(
        std::borrow::Cow::Owned(font.to_owned()),
    ))
}
fn text_outline(props: &reel_ui::stage::NodeProps) -> Option<reel_ui::core::TextOutline> {
    (props.text_outline > 0.).then(|| reel_ui::core::TextOutline {
        width: props.text_outline,
        color: color(props.text_outline_color),
    })
}
fn text_line_breaking(node: &SceneProperties) -> reel_ui::properties::LineBreaking {
    if matches!(node.kind, NodeKind::Text(_)) && node.props.text_wrap {
        reel_ui::properties::LineBreaking::WordWrap
    } else {
        reel_ui::properties::LineBreaking::Overflow
    }
}
fn text_shadow(props: &reel_ui::stage::NodeProps) -> Option<reel_ui::core::TextShadow> {
    (props.text_shadow_x != 0. || props.text_shadow_y != 0.).then(|| reel_ui::core::TextShadow {
        offset: Vec2::new(props.text_shadow_x, props.text_shadow_y),
        color: color(props.text_shadow_color),
    })
}
mod properties;

impl SceneWidget {}
macro_rules! publish_interaction {
    ($widget:expr, $ctx:expr) => {
        if let Some(event) = $widget.interaction(
            InteractionState {
                hover: $ctx.has_hovered(),
                pressed: $ctx.has_active(),
                focused: $ctx.has_focus_target(),
                focus_visible: $ctx.focus_visible(),
                disabled: $ctx.is_disabled(),
            },
            !$ctx.is_disabled() && !$ctx.is_stashed() && $ctx.is_window_focused(),
        ) {
            $ctx.submit_action::<NodeEvent>(event);
        }
    };
}
impl AllowRawMut for SceneWidget {}

mod widget;
