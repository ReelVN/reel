//! Mounted resource identities shared by layout and viewport demand queries.
use super::*;
use std::sync::OnceLock;

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(super) struct ResourceIndexCache {
    wgsl: OnceLock<Arc<ResourceIndex>>,
    gxp: OnceLock<Arc<ResourceIndex>>,
}

pub(crate) struct ResourceIndex {
    entries: Vec<Entry>,
}

struct Entry {
    source: String,
    uses: Vec<Usage>,
    encoded: Option<Arc<[u8]>>,
    shader: bool,
    preload: bool,
}

struct Usage {
    origin: Option<String>,
    image: Option<(NodeId, bool)>,
}

enum Selection {
    All,
    Layout,
    Viewport(Vec<NodeId>),
}

pub(crate) struct ImageRequests {
    pub index: Arc<ResourceIndex>,
    selection: Selection,
    pub deferred_images: bool,
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
    pub fn image_requests(&self, format: EffectShaderFormat) -> Result<ImageRequests, String> {
        Ok(ImageRequests::new(
            self.resource_index(format),
            Selection::All,
        ))
    }
    pub(super) fn image_requests_for(
        root: Option<&Arc<SceneNode>>,
        format: EffectShaderFormat,
    ) -> Result<ImageRequests, String> {
        Ok(ImageRequests::new(
            Arc::new(ResourceIndex::collect(root, format)),
            Selection::All,
        ))
    }
    pub(crate) fn layout_image_requests(
        &self,
        format: EffectShaderFormat,
    ) -> Result<ImageRequests, String> {
        Ok(ImageRequests::new(
            self.resource_index(format),
            Selection::Layout,
        ))
    }
    pub(crate) fn viewport_image_requests(
        &self,
        outside: Vec<NodeId>,
        format: EffectShaderFormat,
    ) -> Result<ImageRequests, String> {
        Ok(ImageRequests::new(
            self.resource_index(format),
            Selection::Viewport(outside),
        ))
    }
}

impl ResourceIndex {
    fn collect(root: Option<&Arc<SceneNode>>, format: EffectShaderFormat) -> Self {
        let mut entries: BTreeMap<String, Entry> = BTreeMap::new();
        let mut pending: Vec<_> = root
            .into_iter()
            .map(|node| (node, true, true, false, false, false))
            .collect();
        while let Some((node, parent_visible, parent_enabled, has_parent, cursor, preload)) =
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
            let preload = preload || node.node.props.preload;
            pending.extend(
                node.children
                    .iter()
                    .map(|child| (child, visible, enabled, true, cursor, preload)),
            );
            let mut add =
                |source: &str, origin: Option<&str>, image, shader, encoded: Option<Arc<[u8]>>| {
                    let entry = entries.entry(source.to_owned()).or_insert_with(|| Entry {
                        source: source.to_owned(),
                        uses: Vec::new(),
                        encoded: None,
                        shader: false,
                        preload: false,
                    });
                    entry.preload |= preload;
                    if visible || preload {
                        entry.shader |= shader;
                        if encoded.is_some() {
                            entry.encoded = encoded.clone();
                        }
                    }
                    if visible {
                        entry.uses.push(Usage {
                            origin: origin.map(str::to_owned),
                            image,
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
                    &format.asset_path(effect.definition()),
                    node.node.props.appearance_origin.as_deref(),
                    None,
                    true,
                    None,
                );
                for source in effect.textures() {
                    add(
                        source,
                        node.node.props.appearance_origin.as_deref(),
                        None,
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
                    source,
                    origin.as_deref(),
                    (!cursor).then_some((node.node.id, node.has_fixed_image_size())),
                    false,
                    encoded,
                );
            }
            for source in node.node.props.mask.iter().flat_map(|mask| mask.sources()) {
                add(
                    source,
                    node.node.props.appearance_origin.as_deref(),
                    None,
                    false,
                    None,
                );
            }
        }
        Self {
            entries: entries
                .into_values()
                .map(|mut entry| {
                    entry.uses.shrink_to_fit();
                    entry
                })
                .collect(),
        }
    }

    pub fn mounted(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.source.as_str())
    }
    pub fn preloaded(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter(|entry| entry.preload)
            .map(|entry| entry.source.as_str())
    }
    pub fn encoded(&self) -> impl Iterator<Item = (&str, &Arc<[u8]>)> {
        self.entries.iter().filter_map(|entry| {
            entry
                .encoded
                .as_ref()
                .map(|bytes| (entry.source.as_str(), bytes))
        })
    }
    pub fn shaders(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter(|entry| entry.shader)
            .map(|entry| entry.source.as_str())
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
        let deferred_images = index
            .entries
            .iter()
            .any(|entry| entry.uses.iter().any(|usage| !selection.includes(usage)));
        Self {
            index,
            selection,
            deferred_images,
        }
    }
    pub fn required(&self) -> impl Iterator<Item = (&str, Option<&str>)> {
        self.index.entries.iter().filter_map(|entry| {
            entry
                .uses
                .iter()
                .find(|usage| self.selection.includes(usage))
                .map(|usage| (entry.source.as_str(), usage.origin.as_deref()))
        })
    }
    pub fn origin(&self, source: &str) -> Option<Option<&str>> {
        let index = self
            .index
            .entries
            .binary_search_by(|entry| entry.source.as_str().cmp(source))
            .ok()?;
        self.index.entries[index]
            .uses
            .iter()
            .find(|usage| self.selection.includes(usage))
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
            if (ap.visible, ap.enabled, ap.preload, a.has_fixed_image_size())
                != (bp.visible, bp.enabled, bp.preload, b.has_fixed_image_size())
                || matches!(a.node.kind, NodeKind::Popup(_))
                    != matches!(b.node.kind, NodeKind::Popup(_))
                || matches!(a.node.kind, NodeKind::Cursor)
                    != matches!(b.node.kind, NodeKind::Cursor)
                || image_source(&a.node.kind) != image_source(&b.node.kind)
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
