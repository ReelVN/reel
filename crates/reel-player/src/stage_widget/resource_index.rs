//! Mounted resource identities shared by layout and viewport demand queries.
use super::*;
use std::borrow::Cow;
use std::sync::{OnceLock, Weak};

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(super) struct ResourceIndexCache {
    wgsl: OnceLock<Arc<ResourceIndex>>,
    gxp: OnceLock<Arc<ResourceIndex>>,
}

pub(crate) struct ResourceIndex {
    entries: Vec<(String, Entry)>,
    pub has_fixed_images: bool,
    pub has_skin_paths: bool,
}

#[derive(Default)]
struct Entry {
    uses: Vec<Usage>,
    encoded: Option<Arc<[u8]>>,
    shader: bool,
    preload: bool,
    mounted: bool,
}

struct Usage {
    origin: Option<String>,
    image: Option<(NodeId, bool)>,
    needs_intrinsic_size: bool,
    skin: bool,
}

enum Selection {
    All,
    Layout,
    Viewport(Vec<NodeId>),
}

pub(crate) struct ImageRequests {
    pub index: Arc<ResourceIndex>,
    selection: Selection,
    pub approaching: Vec<NodeId>,
}

pub(super) struct RowResources {
    projection: Weak<SceneNode>,
    format: EffectShaderFormat,
    requests: ImageRequests,
}

impl RowResources {
    pub(super) fn new(projection: &Arc<SceneNode>, format: EffectShaderFormat) -> Self {
        Self {
            projection: Arc::downgrade(projection),
            format,
            requests: ImageRequests::new(
                Arc::new(ResourceIndex::collect(Some(projection), format)),
                Selection::All,
            ),
        }
    }

    pub(super) fn matches(&self, projection: &Arc<SceneNode>, format: EffectShaderFormat) -> bool {
        self.format == format && self.projection.as_ptr() == Arc::as_ptr(projection)
    }

    pub(super) fn ready(&self, images: &BTreeMap<String, ResourceLease>) -> bool {
        self.requests
            .required()
            .all(|(source, _)| images.contains_key(source))
    }
}

impl StageView {
    fn resource_index(&self, format: EffectShaderFormat) -> Arc<ResourceIndex> {
        let slot = match format {
            EffectShaderFormat::Wgsl => &self.image_index.wgsl,
            EffectShaderFormat::Gxp => &self.image_index.gxp,
        };
        slot.get_or_init(|| Arc::new(ResourceIndex::collect(self.root.as_ref(), format)))
            .clone()
    }
    #[cfg(test)]
    pub fn image_requests(&self, format: EffectShaderFormat) -> ImageRequests {
        ImageRequests::new(self.resource_index(format), Selection::All)
    }
    pub(crate) fn layout_image_requests(&self, format: EffectShaderFormat) -> ImageRequests {
        ImageRequests::new(self.resource_index(format), Selection::Layout)
    }
    pub(crate) fn viewport_image_requests(
        &self,
        outside: Vec<NodeId>,
        approaching: Vec<NodeId>,
        format: EffectShaderFormat,
    ) -> ImageRequests {
        let mut requests =
            ImageRequests::new(self.resource_index(format), Selection::Viewport(outside));
        requests.approaching = approaching;
        requests
    }
}

impl ResourceIndex {
    fn collect<'a>(root: Option<&'a Arc<SceneNode>>, format: EffectShaderFormat) -> Self {
        let mut entries: BTreeMap<Cow<'a, str>, Entry> = BTreeMap::new();
        let mut has_fixed_images = false;
        let mut has_skin_paths = false;
        let mut pending: Vec<_> = root
            .into_iter()
            .map(|node| (node, true, true, false, false, false))
            .collect();
        while let Some((node, parent_visible, parent_enabled, has_parent, cursor, preparing_page)) =
            pending.pop()
        {
            if !node.image_branch {
                continue;
            }
            let visible = node.image_visible(parent_visible, parent_enabled, has_parent);
            let enabled = parent_enabled
                && node.node.props.enabled
                && !matches!(node.node.kind, NodeKind::Cursor);
            let cursor = cursor || matches!(node.node.kind, NodeKind::Cursor);
            let props = &node.node.props;
            // Opening the first hidden modal reveals only its visible descendants.
            // Hidden tabs and inactive layers inside it still wait for a selection.
            let preload = (preparing_page && props.visible)
                || (parent_visible && !props.visible && props.modal);
            pending.extend(
                node.children
                    .iter()
                    .map(|child| (child, visible, enabled, true, cursor, preload)),
            );
            let mut add = |source: Cow<'a, str>,
                           origin: Option<&str>,
                           image,
                           needs_intrinsic_size,
                           shader,
                           encoded: Option<Arc<[u8]>>| {
                let entry = entries.entry(source).or_default();
                entry.mounted = true;
                entry.preload |= preload;
                if visible || preload {
                    entry.shader |= shader;
                    if encoded.is_some() {
                        entry.encoded = encoded;
                    }
                }
                if visible {
                    has_fixed_images |= matches!(image, Some((_, true)));
                    entry.uses.push(Usage {
                        origin: origin.map(str::to_owned),
                        image,
                        needs_intrinsic_size,
                        skin: false,
                    });
                }
            };
            // Keep appearance dependencies before the image for diagnostic precedence.
            for effect in node
                .node
                .props
                .effect
                .iter()
                .chain(node.node.props.mask_effect.iter())
            {
                add(
                    Cow::Owned(format.asset_path(effect.definition())),
                    node.node.props.appearance_origin.as_deref(),
                    None,
                    false,
                    true,
                    None,
                );
                for source in effect.textures() {
                    add(
                        Cow::Borrowed(source),
                        node.node.props.appearance_origin.as_deref(),
                        None,
                        false,
                        false,
                        None,
                    );
                }
            }
            if let Some((source, origin)) = image_source(&node.node.kind) {
                let encoded = if let NodeKind::Image { source, .. } = &node.node.kind {
                    source.bytes().cloned()
                } else {
                    None
                };
                add(
                    Cow::Borrowed(source),
                    origin.as_deref(),
                    (!cursor).then_some((node.node.id, node.has_fixed_image_size())),
                    node.needs_intrinsic_image_size(),
                    false,
                    encoded,
                );
            }
            for source in node.node.props.mask.iter().flat_map(|mask| mask.sources()) {
                add(
                    Cow::Borrowed(source),
                    node.node.props.appearance_origin.as_deref(),
                    None,
                    false,
                    false,
                    None,
                );
            }
            if visible
                && enabled
                && let NodeKind::Image { source, .. } = &node.node.kind
                && let Some(paths) = &source.skin_paths
            {
                for path in paths.iter().filter(|path| path.as_str() != source.key()) {
                    has_skin_paths = true;
                    entries
                        .entry(Cow::Borrowed(path.as_str()))
                        .or_default()
                        .uses
                        .push(Usage {
                            origin: None,
                            image: Some((node.node.id, node.has_fixed_image_size())),
                            needs_intrinsic_size: false,
                            skin: true,
                        });
                }
            }
        }
        Self {
            has_fixed_images,
            has_skin_paths,
            entries: entries
                .into_iter()
                .map(|(source, mut entry)| {
                    entry.uses.shrink_to_fit();
                    (source.into_owned(), entry)
                })
                .collect(),
        }
    }

    pub fn mounted(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter(|(_, entry)| entry.mounted)
            .map(|(source, _)| source.as_str())
    }
    fn entry(&self, source: &str) -> Option<&Entry> {
        let index = self
            .entries
            .binary_search_by(|(key, _)| key.as_str().cmp(source))
            .ok()?;
        Some(&self.entries[index].1)
    }
    pub(crate) fn has_visible_use(&self, source: &str) -> bool {
        self.entry(source)
            .is_some_and(|entry| entry.uses.iter().any(|usage| !usage.skin))
    }
    pub fn encoded(&self) -> impl Iterator<Item = (&str, &Arc<[u8]>)> {
        self.entries.iter().filter_map(|(source, entry)| {
            entry.encoded.as_ref().map(|bytes| (source.as_str(), bytes))
        })
    }
    pub fn shaders(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter(|(_, entry)| entry.shader)
            .map(|(source, _)| source.as_str())
    }
}

impl Selection {
    fn includes(&self, usage: &Usage) -> bool {
        match (self, usage.image) {
            (Self::Layout, Some((_, fixed))) => !fixed,
            (Self::Viewport(outside), Some((id, _))) => outside.binary_search(&id).is_err(),
            _ => true,
        }
    }
}

impl ImageRequests {
    fn new(index: Arc<ResourceIndex>, selection: Selection) -> Self {
        Self {
            index,
            selection,
            approaching: Vec::new(),
        }
    }
    pub(crate) fn is_provisional_layout(&self) -> bool {
        self.index.has_fixed_images && matches!(self.selection, Selection::Layout)
    }

    pub fn preloaded(&self) -> impl Iterator<Item = &str> {
        self.index.entries.iter().filter_map(|(source, entry)| {
            (entry.preload
                || entry
                    .uses
                    .iter()
                    .any(|usage| usage.skin && self.selection.includes(usage))
                || (!self.approaching.is_empty()
                    && entry.uses.iter().any(|usage| {
                        !usage.skin
                            && usage
                                .image
                                .is_some_and(|(id, _)| self.approaching.binary_search(&id).is_ok())
                    })))
            .then_some(source.as_str())
        })
    }
    pub fn required(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        self.index.entries.iter().filter_map(|(source, entry)| {
            entry
                .uses
                .iter()
                .find(|usage| !usage.skin && self.selection.includes(usage))
                .map(|usage| (source.as_str(), usage.origin.as_deref()))
        })
    }
    /// Only a size-dependent image can block the provisional viewport layout.
    pub fn intrinsic_sources(&self) -> impl Iterator<Item = &str> {
        self.index.entries.iter().filter_map(|(source, entry)| {
            entry
                .uses
                .iter()
                .any(|usage| usage.needs_intrinsic_size)
                .then_some(source.as_str())
        })
    }
    pub fn origin(&self, source: &str) -> Option<Option<&str>> {
        self.index
            .entry(source)?
            .uses
            .iter()
            .find(|usage| !usage.skin && self.selection.includes(usage))
            .map(|usage| usage.origin.as_deref())
    }
}

pub(super) fn same_inputs(a: Option<&Arc<SceneNode>>, b: Option<&Arc<SceneNode>>) -> bool {
    fn visit(a: &Arc<SceneNode>, b: &Arc<SceneNode>) -> bool {
        if Arc::ptr_eq(a, b) || (!a.image_branch && !b.image_branch) {
            return true;
        }
        if a.node.id != b.node.id || a.image_branch != b.image_branch {
            return false;
        }
        if !Arc::ptr_eq(&a.node, &b.node) {
            let ap = &a.node.props;
            let bp = &b.node.props;
            if (ap.visible, ap.enabled, ap.modal, a.has_fixed_image_size())
                != (bp.visible, bp.enabled, bp.modal, b.has_fixed_image_size())
                || a.needs_intrinsic_image_size() != b.needs_intrinsic_image_size()
                || matches!(a.node.kind, NodeKind::Popup(_))
                    != matches!(b.node.kind, NodeKind::Popup(_))
                || matches!(a.node.kind, NodeKind::Cursor)
                    != matches!(b.node.kind, NodeKind::Cursor)
                || image_source(&a.node.kind) != image_source(&b.node.kind)
                || skin_paths(&a.node.kind) != skin_paths(&b.node.kind)
                || image_resource(&a.node.kind).map(ResourceLease::handle)
                    != image_resource(&b.node.kind).map(ResourceLease::handle)
                || ap.appearance_origin != bp.appearance_origin
                || ap.mask.as_ref().map(|mask| mask.sources())
                    != bp.mask.as_ref().map(|mask| mask.sources())
                || !super::image_demand::effect_matches(&ap.effect, &bp.effect)
                || !super::image_demand::effect_matches(&ap.mask_effect, &bp.mask_effect)
            {
                return false;
            }
        }
        Arc::ptr_eq(&a.children, &b.children)
            || (a.children.len() == b.children.len()
                && a.children
                    .iter()
                    .zip(b.children.iter())
                    .all(|(a, b)| visit(a, b)))
    }
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => visit(a, b),
        _ => false,
    }
}

fn skin_paths(kind: &NodeKind) -> Option<&[String]> {
    match kind {
        NodeKind::Image { source, .. } => source.skin_paths.as_deref(),
        _ => None,
    }
}
