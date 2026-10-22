//! Persistent widgets for the shared author stage. Property edits keep widget identities.
use reel_ui::{
    accesskit::{Node, Role},
    core::*,
    imaging::Painter,
    kurbo::{Axis, Insets, Point, Rect, Size, Vec2},
    layout::{LenDef, LenReq, Length, SizeDef},
    peniko::{Color, ImageData},
    properties::{
        Background, BorderColor, BorderWidth, ContentColor, CornerRadius, ThumbColor, ThumbRadius,
        TrackColor, TrackThickness,
    },
    resources::{ResourceData, ResourceLease},
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
use std::collections::BTreeMap;
use std::sync::Arc;

/// Selection authorized by the UI's accepted-frame group navigation. Deferred
/// navigation completes outside the synchronous keyboard/pointer dispatch span.
#[derive(Debug)]
pub(crate) struct NavigationSelection(pub NodeChange);
mod flow;
mod input;
#[cfg(test)]
mod projection_tests;
mod selection;
#[cfg(test)]
mod tests;
mod virtual_images;
use input::NodeInput;

#[derive(Clone, Default)]
pub(crate) struct StageView {
    pub revision: u64,
    pub button_revision: u64,
    pub presentation_request: u64,
    pub feedback: bool,
    pub(crate) identity: Option<StageIdentity>,
    root: Option<Arc<SceneNode>>,
    pub resources: Vec<ResourceLease>,
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
    modal: Option<NodeId>,
    images: SceneImages,
}
#[derive(Clone, Default)]
struct SceneImages {
    image: Option<ImageData>,
    image_resource: Option<ResourceLease>,
    mask_images: Arc<[ImageData]>,
    tiled_size: Option<Size>,
    tiles: Vec<reel_ui::widgets::ImageTile>,
}
impl PartialEq for SceneImages {
    fn eq(&self, other: &Self) -> bool {
        self.image.as_ref().map(|i| i.data.id()) == other.image.as_ref().map(|i| i.data.id())
            && self.image_resource.as_ref().map(ResourceLease::handle)
                == other.image_resource.as_ref().map(ResourceLease::handle)
            && self
                .mask_images
                .iter()
                .map(|i| i.data.id())
                .eq(other.mask_images.iter().map(|i| i.data.id()))
            && self.tiled_size == other.tiled_size
            && self.tiles.len() == other.tiles.len()
            && self
                .tiles
                .iter()
                .zip(&other.tiles)
                .all(|(a, b)| a.image == b.image && a.transform == b.transform && a.clip == b.clip)
    }
}
impl SceneNode {
    fn image_size(&self) -> Size {
        self.images
            .image_resource
            .as_ref()
            .and_then(|r| r.data().image_size())
            .map(|[w, h]| Size::new(w.into(), h.into()))
            .or_else(|| {
                self.images
                    .image
                    .as_ref()
                    .map(|i| Size::new(i.width.into(), i.height.into()))
            })
            .or(self.images.tiled_size)
            .unwrap_or(Size::ZERO)
    }
}
pub(crate) struct ImageRequests {
    pub required: BTreeMap<String, Option<String>>,
    pub mounted: std::collections::BTreeSet<String>,
    pub encoded: BTreeMap<String, std::sync::Arc<[u8]>>,
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
                (a, b) => a == b,
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
                        summary.modal = child.modal.or(summary.modal);
                        summary
                    })
            };
            let image_branch = matches!(node.kind, NodeKind::Image { .. } | NodeKind::Particles(_))
                || node.props.mask.is_some()
                || children_summary.image;
            let button_branch =
                matches!(node.kind, NodeKind::Button { .. }) || children_summary.button;
            let input_branch =
                matches!(node.kind, NodeKind::Input { .. }) || children_summary.input;
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
                images.image = None;
                images.image_resource = None;
                images.tiled_size = None;
                images.tiles.clear();
            }
            if node.props.mask.is_none() {
                images.mask_images = Arc::from([]);
            }
            Arc::new(SceneNode {
                node,
                revision,
                children,
                children_summary,
                image_branch,
                button_branch,
                input_branch,
                modal,
                images,
            })
        }
        let previous = previous.filter(|view| view.identity.as_ref() == Some(stage.identity()));
        Self {
            revision: stage.revision(),
            identity: Some(stage.identity().clone()),
            button_revision: 0,
            presentation_request: 0,
            feedback: false,
            root: stage
                .root()
                .map(|id| capture_node(stage, id, previous.and_then(|v| v.root.as_ref()))),
            resources: Vec::new(),
        }
    }
    pub fn input_scope(&self) -> (Option<NodeId>, Option<NodeId>) {
        (
            self.root.as_ref().map(|root| root.node.id),
            self.root.as_ref().and_then(|root| root.modal),
        )
    }
    #[cfg(feature = "window")]
    pub fn has_root(&self) -> bool {
        self.root.is_some()
    }
    /// Compare button content only. Animated geometry uses accepted hit testing;
    /// changes outside a control cannot invalidate its activation.
    pub fn button_changes(&self, previous: Option<&Self>, feedback: bool) -> Vec<(NodeId, bool)> {
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
            if Arc::ptr_eq(a, b) {
                return true;
            }
            let mut pending = vec![(a, b)];
            while let Some((a, b)) = pending.pop() {
                if Arc::ptr_eq(a, b) {
                    continue;
                }
                if a.node.id != b.node.id
                    || a.node.kind != b.node.kind
                    || a.node.props.visible != b.node.props.visible
                    || a.node.props.enabled != b.node.props.enabled
                    || a.children.len() != b.children.len()
                {
                    return false;
                }
                pending.extend(a.children.iter().zip(b.children.iter()));
            }
            true
        }
        let previous = previous
            .filter(|old| old.identity == self.identity)
            .map(buttons)
            .unwrap_or_default();
        buttons(self)
            .into_iter()
            .map(|(id, node)| {
                let changed = previous
                    .get(&id)
                    .is_none_or(|old| !feedback && !same_content(old, node));
                (id, changed)
            })
            .collect()
    }
    pub fn image_requests(&self) -> Result<ImageRequests, String> {
        Self::image_requests_for(self.root.as_ref())
    }
    fn image_requests_for(root: Option<&Arc<SceneNode>>) -> Result<ImageRequests, String> {
        let mut requests = BTreeMap::new();
        let mut mounted = std::collections::BTreeSet::new();
        let mut encoded = BTreeMap::new();
        let mut pending: Vec<_> = root.into_iter().map(|node| (node, true)).collect();
        while let Some((node, parent_visible)) = pending.pop() {
            if !node.image_branch {
                continue;
            }
            let visible = parent_visible && node.node.props.visible;
            pending.extend(node.children.iter().map(|child| (child, visible)));
            if let Some((source, _)) = image_source(&node.node.kind) {
                mounted.insert(source.to_owned());
            }
            if let Some(mask) = &node.node.props.mask {
                mounted.extend(mask.sources().iter().map(|source| source.to_string()));
            }
            if !visible {
                continue;
            }
            if let Some((source, origin)) = image_source(&node.node.kind) {
                if matches!(node.node.kind, NodeKind::Particles(_))
                    && source.ends_with(".reel-image")
                {
                    return Err(format!(
                        "{}: particle image {source:?}: particles require a whole PNG or JPEG sprite; prepared .reel-image sources are supported by image nodes",
                        origin.as_deref().unwrap_or("stage")
                    ));
                }
                if matches!(&node.node.kind, NodeKind::Image { slice: Some(_), .. })
                    && source.ends_with(".reel-image")
                {
                    return Err(format!(
                        "{}: sliced image {source:?}: nine-slice images require a whole PNG or JPEG image",
                        origin.as_deref().unwrap_or("stage")
                    ));
                }
                if matches!(&node.node.kind, NodeKind::Image { repeat, .. } if *repeat != [false; 2])
                    && source.ends_with(".reel-image")
                {
                    return Err(format!(
                        "{}: repeating image {source:?}: repetition requires a whole PNG or JPEG image",
                        origin.as_deref().unwrap_or("stage")
                    ));
                }
                requests
                    .entry(source.to_owned())
                    .or_insert_with(|| origin.clone());
            }
            if let NodeKind::Image { source, .. } = &node.node.kind
                && let Some(bytes) = source.bytes()
            {
                encoded.insert(source.key().to_owned(), bytes.clone());
            }
            for source in node.node.props.mask.iter().flat_map(|mask| mask.sources()) {
                let origin = node.node.props.mask_origin.as_deref();
                if source.ends_with(".reel-image") {
                    return Err(format!(
                        "{}: mask image {source:?}: masks require a whole PNG or JPEG image; prepared .reel-image sources are supported by image nodes",
                        origin.unwrap_or("stage")
                    ));
                }
                requests
                    .entry(source.to_string())
                    .or_insert_with(|| origin.map(str::to_owned));
            }
        }
        Ok(ImageRequests {
            required: requests,
            mounted,
            encoded,
        })
    }
    pub fn resolve_images(&mut self, images: &BTreeMap<String, ResourceLease>) {
        self.resources.clear();
        let mut handles = std::collections::BTreeSet::new();
        resolve_image_nodes(&mut self.root, |node, visible| {
            let mut resolved = SceneImages::default();
            if !visible {
                return resolved;
            }
            if let Some((source, _)) = image_source(&node.node.kind) {
                let lease = &images[source];
                let ResourceData::Image { image, .. } = lease.data() else {
                    unreachable!()
                };
                resolved.image = Some(image.clone());
                if handles.insert(lease.handle()) {
                    self.resources.push(lease.clone());
                }
            }
            if let Some(lease) = image_resource(&node.node.kind) {
                resolved.image_resource = Some(lease.clone());
                if handles.insert(lease.handle()) {
                    self.resources.push(lease.clone());
                }
            }
            if let Some(mask) = &node.node.props.mask {
                resolved.mask_images = mask
                    .sources()
                    .iter()
                    .map(|source| {
                        let lease = &images[source.as_ref()];
                        let ResourceData::Image { image, .. } = lease.data() else {
                            unreachable!()
                        };
                        if handles.insert(lease.handle()) {
                            self.resources.push(lease.clone());
                        }
                        image.clone()
                    })
                    .collect();
            }
            resolved
        });
    }
    pub fn resolve_metadata(&mut self, images: &crate::images::Images) {
        self.resources.clear();
        let mut resources = std::collections::BTreeSet::new();
        resolve_image_nodes(&mut self.root, |node, visible| {
            let mut resolved = SceneImages::default();
            if !visible {
                return resolved;
            }
            if let Some(lease) = image_resource(&node.node.kind) {
                resolved.image_resource = Some(lease.clone());
                if resources.insert(lease.handle()) {
                    self.resources.push(lease.clone());
                }
            }
            if let Some((source, _)) = image_source(&node.node.kind) {
                if let Some(manifest) = images.manifests.get(source) {
                    resolved.tiled_size =
                        Some(Size::new(manifest.width.into(), manifest.height.into()));
                } else if let Some(lease) = images.loaded.get(source) {
                    let ResourceData::Image { image, .. } = lease.data() else {
                        unreachable!()
                    };
                    resolved.image = Some(image.clone());
                    if resources.insert(lease.handle()) {
                        self.resources.push(lease.clone());
                    }
                }
            }
            if let Some(mask) = &node.node.props.mask {
                resolved.mask_images = mask
                    .sources()
                    .iter()
                    .filter_map(|source| {
                        let lease = images.loaded.get(source.as_ref())?;
                        let ResourceData::Image { image, .. } = lease.data() else {
                            unreachable!()
                        };
                        if resources.insert(lease.handle()) {
                            self.resources.push(lease.clone());
                        }
                        Some(image.clone())
                    })
                    .collect();
            }
            resolved
        });
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
        let mut handles: std::collections::BTreeSet<_> =
            self.resources.iter().map(ResourceLease::handle).collect();
        self.resources.extend(
            resources
                .iter()
                .filter_map(|key| images.loaded.get(key))
                .filter(|lease| handles.insert(lease.handle()))
                .cloned(),
        );
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
        resolve: &mut impl FnMut(&SceneNode, bool) -> SceneImages,
    ) -> Arc<SceneNode> {
        if !node.image_branch {
            return node.clone();
        }
        let visible = parent_visible && node.node.props.visible;
        let images = resolve(node, visible);
        let mut children = None;
        for (index, child) in node.children.iter().enumerate() {
            if !child.image_branch {
                continue;
            }
            let next = visit(child, visible, resolve);
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
            modal: node.modal,
            images,
        })
    }
    if let Some(node) = root {
        *node = visit(node, true, &mut resolve);
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
}
impl StageWidget {
    pub(crate) fn image_widgets(
        this: &mut WidgetMut<'_, Self>,
    ) -> Vec<(NodeId, crate::images::tiles::ImageGeometry)> {
        fn visit(
            this: &mut WidgetMut<'_, SceneWidget>,
            out: &mut Vec<(NodeId, crate::images::tiles::ImageGeometry)>,
        ) {
            if !this.widget.node.props.visible || !this.widget.projection.image_branch {
                return;
            }
            if let (Some(image), NodeKind::Image { source, fit, .. }) =
                (&this.widget.image, &this.widget.node.kind)
            {
                out.push((
                    this.widget.node.id,
                    crate::images::tiles::ImageGeometry {
                        source: source.key().to_owned(),
                        widget: image.id(),
                        fit: *fit,
                    },
                ));
            }
            for (_, pod) in &mut this.widget.children {
                if let Some(mut child) = this.ctx.get_mut(pod) {
                    visit(&mut child, out);
                }
            }
        }
        let mut out = Vec::new();
        if let Some(root) = &mut this.widget.root
            && let Some(mut root) = this.ctx.get_mut(root)
        {
            visit(&mut root, &mut out);
        }
        out
    }
    pub fn new(view: Option<&StageView>) -> Self {
        let root = view.and_then(|view| view.root.as_ref());
        let mut nodes = BTreeMap::new();
        let root_pod = root.map(|node| SceneWidget::new_pod(node, &mut nodes));
        Self {
            nodes,
            root: root_pod,
            root_id: root.map(|node| node.node.id),
            identity: view.and_then(|view| view.identity.clone()),
            root_props: root.map(|node| node.node.props.clone()).unwrap_or_default(),
        }
    }
    pub(crate) fn node_widget(&self, node: NodeId) -> Option<WidgetId> {
        self.nodes.get(&node).copied()
    }
    pub(crate) fn text_end(
        &self,
        root: &reel_ui::app::RenderRoot,
        node: NodeId,
    ) -> Option<(WidgetId, Point)> {
        let widget = root
            .get_widget(self.node_widget(node)?)?
            .downcast::<SceneWidget>()?;
        if !matches!(widget.node.kind, NodeKind::Text(_)) {
            return None;
        }
        let label_id = widget.label.as_ref()?.id();
        let label = root.get_widget(label_id)?.downcast::<Label>()?;
        Some((label_id, label.text_end()?))
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
        let id = view.root.as_ref().map(|node| node.node.id);
        if id != this.widget.root_id || this.widget.identity != view.identity {
            if let Some(root) = this.widget.root.take() {
                this.ctx.remove_child(root);
            }
            this.widget.nodes.clear();
            this.widget.root = view
                .root
                .as_ref()
                .map(|node| SceneWidget::new_pod(node, &mut this.widget.nodes));
            this.widget.root_id = id;
            this.widget.identity = view.identity.clone();
            this.ctx.children_changed();
        } else if let (Some(pod), Some(node)) = (&mut this.widget.root, &view.root) {
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
        let props = view
            .root
            .as_ref()
            .map(|node| node.node.props.clone())
            .unwrap_or_default();
        if this.widget.root_props.visible != props.visible {
            this.ctx.request_layout();
        }
        this.widget.root_props = props;
        this.ctx.request_compose();
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
        _: &mut MeasureCtx<'_>,
        _: &PropertiesRef<'_>,
        _: Axis,
        request: LenReq,
        _: Option<Length>,
    ) -> Length {
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
            let transform = self.root_props.transform(ctx.child_size(root));
            ctx.set_child_transform(root, transform);
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

#[derive(Default)]
struct VirtualListState {
    measurements: Arc<[VirtualRowMeasurement]>,
    feedback: Option<NodeVirtualList>,
    retained: Vec<NodeId>,
    children: Vec<VirtualChild>,
    navigation: Option<VirtualListNavigation>,
    placement_pending: bool,
    viewport_start: f64,
    placement: Vec<(NodeId, f64)>,
}
impl VirtualListState {
    // Keep one projected row fixed while packing its contiguous logical segment
    // with this layout's real measurements. This also runs for a distant jump:
    // the first painted window never exposes estimated gaps/overlapping rows.
    fn place_measured(&mut self, view: &reel_ui::stage::VirtualListView, gap: f64) {
        if !self.placement_pending {
            return;
        }
        self.placement_pending = false;
        self.placement.clear();
        self.placement
            .extend(view.rows.iter().map(|row| (row.node, row.offset)));
        let height = |i: usize| {
            self.measurements
                .iter()
                .find(|m| m.node == view.rows[i].node)
                .expect("every mounted row was measured")
                .height
        };
        let mut start = 0;
        while start < view.rows.len() {
            let mut end = start + 1;
            while end < view.rows.len() && view.rows[end].index == view.rows[end - 1].index + 1 {
                end += 1;
            }
            let anchor = (start..end)
                .rfind(|&i| view.rows[i].offset <= self.viewport_start)
                .unwrap_or(start);
            for i in anchor + 1..end {
                self.placement[i].1 = self.placement[i - 1].1 + height(i - 1) + gap;
            }
            for i in (start..anchor).rev() {
                self.placement[i].1 = self.placement[i + 1].1 - height(i) - gap;
            }
            start = end;
        }
    }
}

struct SceneWidget {
    node: Arc<SceneProperties>,
    projection: Arc<SceneNode>,
    sync_dirty: bool,
    button: ButtonBehavior,
    interaction: InteractionState,
    input: Option<NodeInput>,
    slider: Option<WidgetPod<Slider>>,
    label: Option<WidgetPod<Label>>,
    editor: Option<WidgetPod<TextInput>>,
    image: Option<WidgetPod<Image>>,
    image_size: Size,
    particle_image: Option<ImageData>,
    mask_images: Arc<[ImageData]>,
    children: Vec<(Arc<SceneProperties>, WidgetPod<SceneWidget>)>,
    size: Size,
    scroll: ScrollState,
    scroll_laid_out: bool,
    scroll_request: Option<Vec2>,
    scroll_feedback: Option<(Vec2, Size, Size)>,
    scroll_baseline: Vec2,
    scroll_input: bool,
    virtual_list: Option<Box<VirtualListState>>,
    #[cfg(test)]
    layouts: usize,
    #[cfg(test)]
    syncs: usize,
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
impl SceneWidget {
    fn image_widget(view: &SceneNode) -> Option<WidgetPod<Image>> {
        let NodeKind::Image {
            fit,
            slice,
            slice_scale,
            repeat,
            tile_scale,
            corner_colors,
            slice_center,
            ..
        } = &view.node.kind
        else {
            return None;
        };
        view.images
            .image
            .as_ref()
            .map(|image| Image::new(image.clone()))
            .or_else(|| {
                view.images
                    .image_resource
                    .as_ref()
                    .map(|lease| Image::from_resource(lease.clone()))
            })
            .or_else(|| {
                view.images
                    .tiled_size
                    .map(|size| Image::tiled(size, view.images.tiles.clone()))
            })
            .map(|image| {
                NewWidget::new(
                    image
                        .with_color(color(view.node.props.color))
                        .with_slice(*slice, *slice_scale, *slice_center)
                        .with_pattern(*repeat, *tile_scale, *corner_colors),
                )
                .with_props(*fit)
                .to_pod()
            })
    }
    fn caption(kind: &NodeKind) -> Option<&str> {
        match kind {
            NodeKind::Text(text) => Some(text),
            NodeKind::Button { label, .. }
            | NodeKind::Toggle { label, .. }
            | NodeKind::Radio { label, .. } => label.as_deref(),
            _ => None,
        }
    }
    fn is_button(&self) -> bool {
        matches!(
            self.node.kind,
            NodeKind::Button { .. } | NodeKind::Toggle { .. } | NodeKind::Radio { .. }
        )
    }
    fn reveal_fade(node: &SceneProperties) -> Option<reel_ui::core::RevealFade> {
        node.props
            .reveal_fade
            .map(|progress| reel_ui::core::RevealFade {
                from: node.props.reveal_fade_from,
                progress,
                step: node.props.reveal_fade_step,
            })
    }
    fn reveal_range(node: &SceneProperties) -> std::ops::Range<usize> {
        let text = Self::caption(&node.kind).unwrap_or("");
        let mut end = node.props.reveal.unwrap_or(text.len()).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        0..end
    }
    fn new_pod(view: &Arc<SceneNode>, nodes: &mut BTreeMap<NodeId, WidgetId>) -> WidgetPod<Self> {
        let pod = WidgetPod::new(Self::new(view, nodes));
        nodes.insert(view.node.id, pod.id());
        pod
    }
    fn forget(view: &Arc<SceneNode>, nodes: &mut BTreeMap<NodeId, WidgetId>) {
        nodes.remove(&view.node.id);
        for child in view.children.iter() {
            Self::forget(child, nodes);
        }
    }
    fn new(view: &Arc<SceneNode>, nodes: &mut BTreeMap<NodeId, WidgetId>) -> Self {
        let label = match &view.node.kind {
            NodeKind::Text(_)
            | NodeKind::Button { .. }
            | NodeKind::Toggle { .. }
            | NodeKind::Radio { .. } => Some(
                NewWidget::new(
                    Label::new("")
                        .with_rich_text(match &view.node.kind {
                            NodeKind::Text(text) => text.clone(),
                            kind => Self::caption(kind).unwrap_or("").into(),
                        })
                        .with_visible_bytes(Self::reveal_range(&view.node))
                        .with_reveal_fade(Self::reveal_fade(&view.node))
                        .with_style(StyleProperty::FontSize(view.node.props.font_size as f32))
                        .with_style(StyleProperty::FontWeight(
                            reel_ui::reel_text::FontWeight::new(view.node.props.font_weight as f32),
                        ))
                        .with_style(StyleProperty::LetterSpacing(
                            view.node.props.letter_spacing as f32,
                        ))
                        .with_style(StyleProperty::WordSpacing(
                            view.node.props.word_spacing as f32,
                        ))
                        .with_text_alignment(view.node.props.text_align)
                        .with_text_fit(view.node.props.text_fit)
                        .with_outline(text_outline(&view.node.props))
                        .with_shadow(text_shadow(&view.node.props))
                        .with_style(StyleProperty::LineHeight(
                            reel_ui::reel_text::LineHeight::FontSizeRelative(
                                view.node.props.line_height as f32,
                            ),
                        ))
                        .with_style(font_style(&view.node.props.font)),
                )
                .with_props(text_line_breaking(&view.node))
                .with_props(ContentColor::new(color(view.node.props.color)))
                .to_pod(),
            ),
            _ => None,
        };
        Self {
            node: view.node.clone(),
            projection: view.clone(),
            sync_dirty: false,
            button: ButtonBehavior::default(),
            interaction: InteractionState::default(),
            input: match &view.node.kind {
                NodeKind::Input {
                    bindings,
                    keyboard,
                    pointer,
                    wheel,
                    ..
                } => Some(NodeInput::new(
                    bindings.clone(),
                    *keyboard,
                    *pointer,
                    *wheel,
                )),
                _ => None,
            },
            label,
            editor: if let NodeKind::TextInput { value, multiline } = &view.node.kind {
                Some(
                    NewWidget::new(
                        TextInput::with_mode(value.clone(), *multiline)
                            .with_text_alignment(view.node.props.text_align)
                            .with_text_wrap(view.node.props.text_wrap)
                            .with_style(StyleProperty::LineHeight(
                                reel_ui::reel_text::LineHeight::FontSizeRelative(
                                    view.node.props.line_height as f32,
                                ),
                            ))
                            .with_style(StyleProperty::FontSize(view.node.props.font_size as f32))
                            .with_style(StyleProperty::FontWeight(
                                reel_ui::reel_text::FontWeight::new(
                                    view.node.props.font_weight as f32,
                                ),
                            ))
                            .with_style(StyleProperty::LetterSpacing(
                                view.node.props.letter_spacing as f32,
                            ))
                            .with_style(StyleProperty::WordSpacing(
                                view.node.props.word_spacing as f32,
                            ))
                            .with_style(font_style(&view.node.props.font)),
                    )
                    .with_props(ContentColor::new(color(view.node.props.color)))
                    .to_pod(),
                )
            } else {
                None
            },
            slider: if let NodeKind::Slider { range, value, .. } = view.node.kind {
                let mut slider = Slider::new(range.min(), range.max(), value)
                    .with_decorated(view.node.props.decorated);
                if let Some(step) = range.step() {
                    slider = slider.with_step(step);
                }
                let tint = color(view.node.props.color);
                Some(
                    NewWidget::new(slider)
                        .with_props(ThumbRadius(Length::px(if view.node.props.decorated {
                            10.
                        } else {
                            0.
                        })))
                        .with_props(TrackThickness(Length::px(4.)))
                        .with_props(ThumbColor(tint))
                        .with_props(TrackColor {
                            active: tint,
                            inactive: tint.with_alpha(tint.components[3] * 0.3),
                        })
                        .to_pod(),
                )
            } else {
                None
            },
            particle_image: matches!(view.node.kind, NodeKind::Particles(_))
                .then(|| view.images.image.clone())
                .flatten(),
            mask_images: view.images.mask_images.clone(),
            image: Self::image_widget(view),
            image_size: view.image_size(),
            children: view
                .children
                .iter()
                .map(|child| (child.node.clone(), Self::new_pod(child, nodes)))
                .collect(),
            size: Size::ZERO,
            scroll: ScrollState::default(),
            scroll_laid_out: false,
            scroll_request: match view.node.kind {
                NodeKind::Scroll { offset, .. } if offset != Vec2::ZERO => Some(offset),
                _ => None,
            },
            scroll_feedback: None,
            scroll_input: false,
            scroll_baseline: match view.node.kind {
                NodeKind::Scroll { offset, .. } => offset,
                _ => Vec2::ZERO,
            },
            virtual_list: matches!(view.node.kind, NodeKind::VirtualList(_)).then(Default::default),
            #[cfg(test)]
            layouts: 0,
            #[cfg(test)]
            syncs: 0,
        }
    }
    fn sync(
        this: &mut WidgetMut<'_, Self>,
        view: &Arc<SceneNode>,
        feedback: bool,
        nodes: &mut BTreeMap<NodeId, WidgetId>,
    ) -> bool {
        if !this.widget.sync_dirty && Arc::ptr_eq(&this.widget.projection, view) {
            return false;
        }
        #[cfg(test)]
        {
            this.widget.syncs += 1;
        }
        if !this
            .widget
            .mask_images
            .iter()
            .map(|image| image.data.id())
            .eq(view.images.mask_images.iter().map(|image| image.data.id()))
        {
            this.widget.mask_images = view.images.mask_images.clone();
            this.ctx.request_render();
        }
        if matches!(view.node.kind, NodeKind::Particles(_)) {
            if this
                .widget
                .particle_image
                .as_ref()
                .map(|i| (i.width, i.height))
                != view.images.image.as_ref().map(|i| (i.width, i.height))
            {
                this.ctx.request_layout();
            }
            if this
                .widget
                .particle_image
                .as_ref()
                .map(|image| image.data.id())
                != view.images.image.as_ref().map(|image| image.data.id())
            {
                this.ctx.request_render();
            }
            this.widget.particle_image = view.images.image.clone();
        }
        if this.widget.node.props.capture_included != view.node.props.capture_included {
            this.ctx.request_render();
        }
        let children_changed = this.widget.sync_dirty
            || !Arc::ptr_eq(&this.widget.projection.children, &view.children);
        let order_changed = children_changed
            && !this
                .widget
                .children
                .iter()
                .map(|(node, _)| node.id)
                .eq(view.children.iter().map(|child| child.node.id));
        let mut content_changed = this.widget.node.kind != view.node.kind
            || order_changed
            || this.widget.node.props.visible != view.node.props.visible
            || this.widget.node.props.enabled != view.node.props.enabled;
        if let (Some(pod), NodeKind::TextInput { value, multiline }) =
            (&mut this.widget.editor, &view.node.kind)
            && let Some(mut editor) = this.ctx.get_mut(pod)
        {
            TextInput::set_multiline(&mut editor, *multiline);
            TextInput::set_value(&mut editor, value.clone());
            TextInput::set_text_alignment(&mut editor, view.node.props.text_align);
            TextInput::set_text_wrap(&mut editor, view.node.props.text_wrap);
            if this.widget.node.props.line_height != view.node.props.line_height {
                TextInput::insert_style(
                    &mut editor,
                    StyleProperty::LineHeight(reel_ui::reel_text::LineHeight::FontSizeRelative(
                        view.node.props.line_height as f32,
                    )),
                );
            }
            if this.widget.node.props.font_size != view.node.props.font_size {
                TextInput::insert_style(
                    &mut editor,
                    StyleProperty::FontSize(view.node.props.font_size as f32),
                );
            }
            if this.widget.node.props.font_weight != view.node.props.font_weight {
                TextInput::insert_style(
                    &mut editor,
                    StyleProperty::FontWeight(reel_ui::reel_text::FontWeight::new(
                        view.node.props.font_weight as f32,
                    )),
                );
            }
            if this.widget.node.props.letter_spacing != view.node.props.letter_spacing {
                TextInput::insert_style(
                    &mut editor,
                    StyleProperty::LetterSpacing(view.node.props.letter_spacing as f32),
                );
            }
            if this.widget.node.props.word_spacing != view.node.props.word_spacing {
                TextInput::insert_style(
                    &mut editor,
                    StyleProperty::WordSpacing(view.node.props.word_spacing as f32),
                );
            }
            if this.widget.node.props.font != view.node.props.font {
                TextInput::insert_style(&mut editor, font_style(&view.node.props.font));
            }
            if this.widget.node.props.color != view.node.props.color {
                editor.insert_prop(ContentColor::new(color(view.node.props.color)));
            }
        }
        if let (Some(pod), NodeKind::Slider { range, value, .. }) =
            (&mut this.widget.slider, &view.node.kind)
            && let Some(mut slider) = this.ctx.get_mut(pod)
        {
            Slider::configure(&mut slider, *range, *value);
            if this.widget.node.props.decorated != view.node.props.decorated {
                Slider::set_decorated(&mut slider, view.node.props.decorated);
                slider.insert_prop(ThumbRadius(Length::px(if view.node.props.decorated {
                    10.
                } else {
                    0.
                })));
            }
            if this.widget.node.props.color != view.node.props.color {
                let tint = color(view.node.props.color);
                slider.insert_prop(ThumbColor(tint));
                slider.insert_prop(TrackColor {
                    active: tint,
                    inactive: tint.with_alpha(tint.components[3] * 0.3),
                });
            }
        }
        if let (
            Some(input),
            NodeKind::Input {
                bindings,
                keyboard,
                pointer,
                wheel,
                ..
            },
        ) = (&mut this.widget.input, &view.node.kind)
            && (input.bindings != *bindings
                || input.keyboard != *keyboard
                || input.pointer_enabled != *pointer
                || input.wheel_enabled != *wheel)
        {
            input.cancel(this.widget.node.id, |event| {
                this.ctx.submit_action::<NodeEvent>(event)
            });
            *input = NodeInput::new(bindings.clone(), *keyboard, *pointer, *wheel);
            this.ctx.set_interactive(
                this.widget.accepts_pointer_interaction(),
                this.widget.accepts_focus(),
            );
        }
        if this.widget.node.props.enabled != view.node.props.enabled {
            this.ctx.set_disabled(!view.node.props.enabled);
        }
        if this.widget.node.props.modal != view.node.props.modal {
            this.ctx.set_modal(view.node.props.modal);
        }
        let has_image = matches!(view.node.kind, NodeKind::Image { .. })
            && (view.images.image.is_some()
                || view.images.image_resource.is_some()
                || view.images.tiled_size.is_some());
        if has_image && this.widget.image.is_none() {
            this.widget.image = Self::image_widget(view);
            this.widget.image_size = view.image_size();
            this.ctx.children_changed();
        } else if !has_image && let Some(image) = this.widget.image.take() {
            this.ctx.remove_child(image);
            this.widget.image_size = Size::ZERO;
            this.ctx.children_changed();
        }
        if this.widget.node.props.color != view.node.props.color
            && let Some(pod) = &mut this.widget.image
            && let Some(mut image) = this.ctx.get_mut(pod)
        {
            Image::set_color(&mut image, color(view.node.props.color));
        }
        let image_source_changed = matches!(
            (&this.widget.node.kind, &view.node.kind),
            (NodeKind::Image { source: old, .. }, NodeKind::Image { source, .. }) if old != source
        );
        if (image_source_changed
            || this
                .widget
                .projection
                .images
                .image_resource
                .as_ref()
                .map(ResourceLease::handle)
                != view
                    .images
                    .image_resource
                    .as_ref()
                    .map(ResourceLease::handle))
            && let (Some(pod), Some(lease)) = (&mut this.widget.image, &view.images.image_resource)
            && let Some(mut image) = this.ctx.get_mut(pod)
        {
            Image::set_resource(&mut image, lease.clone());
            this.widget.image_size = view.image_size();
        }
        if let (Some(pod), Some(data)) = (&mut this.widget.image, &view.images.image)
            && (image_source_changed
                || this
                    .widget
                    .projection
                    .images
                    .image
                    .as_ref()
                    .map(|image| image.data.id())
                    != Some(data.data.id())
                || this.widget.image_size != Size::new(data.width.into(), data.height.into()))
            && let Some(mut image) = this.ctx.get_mut(pod)
        {
            Image::set_image_data(&mut image, data.clone());
            this.widget.image_size = Size::new(data.width.into(), data.height.into());
        }
        if let (Some(pod), Some(size)) = (&mut this.widget.image, view.images.tiled_size)
            && this.widget.projection.images != view.images
            && let Some(mut image) = this.ctx.get_mut(pod)
        {
            Image::set_tiles(&mut image, size, view.images.tiles.clone());
            this.widget.image_size = size;
        }
        if this.widget.node.kind != view.node.kind
            && let (
                Some(pod),
                NodeKind::Image {
                    fit,
                    slice,
                    slice_scale,
                    repeat,
                    tile_scale,
                    corner_colors,
                    slice_center,
                    ..
                },
            ) = (&mut this.widget.image, &view.node.kind)
            && let Some(mut image) = this.ctx.get_mut(pod)
        {
            if !matches!(&this.widget.node.kind, NodeKind::Image { fit: old, .. } if old == fit) {
                image.insert_prop(*fit);
            }
            // This also clears slicing when a whole image switches to tiles.
            Image::set_slice(&mut image, *slice, *slice_scale, *slice_center);
            Image::set_pattern(&mut image, *repeat, *tile_scale, *corner_colors);
        }
        let caption_changed = match (&this.widget.node.kind, &view.node.kind) {
            (NodeKind::Text(a), NodeKind::Text(b)) => a != b,
            _ => Self::caption(&this.widget.node.kind) != Self::caption(&view.node.kind),
        };
        if caption_changed {
            this.ctx.request_layout();
        }
        if let Some(pod) = &mut this.widget.label
            && let Some(mut label) = this.ctx.get_mut(pod)
        {
            if caption_changed {
                Label::set_rich_text(
                    &mut label,
                    match &view.node.kind {
                        NodeKind::Text(text) => text.clone(),
                        kind => Self::caption(kind).unwrap_or("").into(),
                    },
                );
            }
            if caption_changed || this.widget.node.props.reveal != view.node.props.reveal {
                Label::set_visible_bytes(&mut label, Self::reveal_range(&view.node));
            }
            if caption_changed
                || Self::reveal_fade(&this.widget.node) != Self::reveal_fade(&view.node)
            {
                Label::set_reveal_fade(&mut label, Self::reveal_fade(&view.node));
            }
            if this.widget.node.props.font_size != view.node.props.font_size {
                Label::insert_style(
                    &mut label,
                    StyleProperty::FontSize(view.node.props.font_size as f32),
                );
            }
            if this.widget.node.props.font_weight != view.node.props.font_weight {
                Label::insert_style(
                    &mut label,
                    StyleProperty::FontWeight(reel_ui::reel_text::FontWeight::new(
                        view.node.props.font_weight as f32,
                    )),
                );
            }
            if this.widget.node.props.letter_spacing != view.node.props.letter_spacing {
                Label::insert_style(
                    &mut label,
                    StyleProperty::LetterSpacing(view.node.props.letter_spacing as f32),
                );
            }
            if this.widget.node.props.word_spacing != view.node.props.word_spacing {
                Label::insert_style(
                    &mut label,
                    StyleProperty::WordSpacing(view.node.props.word_spacing as f32),
                );
            }
            if this.widget.node.props.font != view.node.props.font {
                Label::insert_style(&mut label, font_style(&view.node.props.font));
            }
            if this.widget.node.props.text_align != view.node.props.text_align {
                Label::set_text_alignment(&mut label, view.node.props.text_align);
            }
            if text_line_breaking(&this.widget.node) != text_line_breaking(&view.node) {
                label.insert_prop(text_line_breaking(&view.node));
            }
            Label::set_text_fit(&mut label, view.node.props.text_fit);
            Label::set_outline(&mut label, text_outline(&view.node.props));
            Label::set_shadow(&mut label, text_shadow(&view.node.props));
            if this.widget.node.props.line_height != view.node.props.line_height {
                Label::insert_style(
                    &mut label,
                    StyleProperty::LineHeight(reel_ui::reel_text::LineHeight::FontSizeRelative(
                        view.node.props.line_height as f32,
                    )),
                );
            }
            if this.widget.node.props.color != view.node.props.color {
                label.insert_prop(ContentColor::new(color(view.node.props.color)));
            }
        }
        if order_changed {
            let mut retained: BTreeMap<_, _> = std::mem::take(&mut this.widget.children)
                .into_iter()
                .map(|(node, pod)| (node.id, (node, pod)))
                .collect();
            this.widget.children = view
                .children
                .iter()
                .map(|next| {
                    retained
                        .remove(&next.node.id)
                        .unwrap_or_else(|| (next.node.clone(), Self::new_pod(next, nodes)))
                })
                .collect();
            for (id, (_, pod)) in retained {
                if let Some(old) = this
                    .widget
                    .projection
                    .children
                    .iter()
                    .find(|old| old.node.id == id)
                {
                    Self::forget(old, nodes);
                }
                this.ctx.remove_child(pod);
            }
            this.ctx.children_changed();
            this.ctx.set_interactive(
                this.widget.accepts_pointer_interaction(),
                this.widget.accepts_focus(),
            );
            this.ctx.request_cursor_icon_change();
        }
        if children_changed {
            for ((node, pod), next) in this.widget.children.iter_mut().zip(view.children.iter()) {
                if node.props.visible != next.node.props.visible
                    || node.props.flex != next.node.props.flex
                    || node.props.stretch != next.node.props.stretch
                    || node.props.align_x != next.node.props.align_x
                    || node.props.align_y != next.node.props.align_y
                {
                    this.ctx.request_layout();
                }
                if *node != next.node {
                    *node = next.node.clone();
                    this.ctx.request_compose();
                }
                let registered = if let Some(mut child) = this.ctx.get_mut(pod) {
                    content_changed |= Self::sync(&mut child, next, feedback, nodes);
                    true
                } else {
                    false
                };
                if registered {
                    this.ctx.set_stashed(pod, !next.node.props.visible);
                }
            }
        }
        if let (
            NodeKind::Scroll {
                offset: previous, ..
            },
            NodeKind::Scroll { offset, .. },
        ) = (&this.widget.node.kind, &view.node.kind)
            && previous != offset
        {
            this.widget.scroll_baseline = *offset;
            this.widget.scroll_request =
                (*offset != this.widget.scroll.offset()).then_some(*offset);
            this.ctx.request_compose();
        }
        if this.widget.node != view.node {
            let old = &this.widget.node.props;
            let new = &view.node.props;
            if old.width != new.width
                || old.flex != new.flex
                || old.height != new.height
                || old.max_width != new.max_width
                || old.max_height != new.max_height
                || old.stretch != new.stretch
                || old.gap != new.gap
                || old.padding != new.padding
                || old.font_size != new.font_size
                || old.font_weight != new.font_weight
                || old.letter_spacing != new.letter_spacing
                || old.word_spacing != new.word_spacing
                || old.font != new.font
                || old.text_fit != new.text_fit
                || text_line_breaking(&this.widget.node) != text_line_breaking(&view.node)
                || old.clip != new.clip
                || old.decorated != new.decorated
                || match (&this.widget.node.kind, &view.node.kind) {
                    (NodeKind::Row(a), NodeKind::Row(b))
                    | (NodeKind::Column(a), NodeKind::Column(b)) => a != b,
                    (NodeKind::Grid(a), NodeKind::Grid(b)) => a != b,
                    (NodeKind::VirtualList(a), NodeKind::VirtualList(b)) => a != b,
                    (NodeKind::Particles(a), NodeKind::Particles(b)) => a.batches != b.batches,
                    (
                        NodeKind::Scroll {
                            axis: a,
                            follow_end: b,
                            align: c,
                            ..
                        },
                        NodeKind::Scroll {
                            axis: d,
                            follow_end: e,
                            align: f,
                            ..
                        },
                    ) => (a, b, c) != (d, e, f),
                    _ => false,
                }
            {
                this.ctx.request_layout();
            }
            if matches!(view.node.kind, NodeKind::Camera(_)) {
                this.ctx.request_compose();
            }
            this.widget.node = view.node.clone();
            this.ctx.request_render();
        }
        if content_changed && !feedback && this.widget.is_button() {
            this.widget.button.cancel();
            this.ctx.invalidate_input_configuration();
        }
        this.widget.projection = view.clone();
        if matches!(this.widget.node.kind, NodeKind::VirtualList(_)) {
            this.widget
                .virtual_list
                .get_or_insert_with(Default::default);
        }
        this.widget.sync_dirty = false;
        content_changed
    }
    fn interaction(&mut self, mut next: InteractionState, available: bool) -> Option<NodeEvent> {
        next.hover &= available;
        next.pressed = available && (next.pressed || self.button.is_pressed());
        next.focused &= available;
        next.focus_visible &= next.focused;
        if next == self.interaction {
            return None;
        }
        self.interaction = next;
        Some(NodeEvent::Interaction(NodeInteraction {
            node: self.node.id,
            state: next,
        }))
    }
    fn activate(&mut self) -> NodeEvent {
        if let NodeKind::Radio { revision, .. } = self.node.kind {
            NodeEvent::Change(NodeChange {
                node: self.node.id,
                value: ControlValue::Selection(revision),
                range: None,
            })
        } else if let NodeKind::Toggle { value, .. } = &mut Arc::make_mut(&mut self.node).kind {
            *value = !*value;
            let value = *value;
            self.sync_dirty = true;
            NodeEvent::Change(NodeChange {
                node: self.node.id,
                value: ControlValue::Bool(value),
                range: None,
            })
        } else {
            NodeEvent::Click(NodeClick { node: self.node.id })
        }
    }
    fn sequential(&self, axis: Axis) -> bool {
        matches!(
            (&self.node.kind, axis),
            (NodeKind::Row(_), Axis::Horizontal)
                | (NodeKind::Column(_), Axis::Vertical)
                | (
                    NodeKind::Scroll {
                        axis: ScrollAxis::Horizontal,
                        ..
                    },
                    Axis::Horizontal
                )
                | (
                    NodeKind::Scroll {
                        axis: ScrollAxis::Vertical | ScrollAxis::Both,
                        ..
                    },
                    Axis::Vertical
                )
        )
    }
}
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
impl Widget for SceneWidget {
    type Action = NodeEvent;
    fn capture_included(&self) -> bool {
        self.node.props.capture_included
    }
    fn virtual_children(&self) -> Option<VirtualChildren<'_>> {
        let NodeKind::VirtualList(list) = &self.node.kind else {
            return None;
        };
        Some(VirtualChildren {
            revision: list.revision,
            total: list.total_rows,
            children: &self.virtual_list.as_ref()?.children,
            target: list
                .navigation
                .map(|n| (n.request, n.resolved_index, n.forward)),
            target_ready: list.navigation.is_some_and(|n| n.ready),
        })
    }
    fn request_virtual_child(
        &mut self,
        ctx: &mut UpdateCtx<'_>,
        _: &mut PropertiesMut<'_>,
        target: Option<(u64, usize, bool)>,
    ) {
        let target = target.map(|(request, index, forward)| VirtualListNavigation {
            request,
            ready: false,
            index,
            resolved_index: index,
            forward,
        });
        let state = self
            .virtual_list
            .as_mut()
            .expect("virtual navigation owner");
        state.navigation = target;
        if let Some(feedback) = &mut state.feedback {
            feedback.navigation = target;
            ctx.submit_action::<NodeEvent>(NodeEvent::VirtualList(feedback.clone()));
        }
    }
    fn on_pointer_preview(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        if matches!(self.node.kind, NodeKind::Scroll { .. }) {
            self.scroll.preview(ctx, event);
        }
    }
    fn scroll_to_child(
        &mut self,
        ctx: &mut UpdateCtx<'_>,
        _: &mut PropertiesMut<'_>,
        rect: Rect,
    ) -> Rect {
        if !matches!(self.node.kind, NodeKind::Scroll { .. }) {
            return rect;
        }
        let delta = self
            .scroll
            .reveal(rect - Vec2::splat(self.node.props.padding));
        if delta != Vec2::ZERO {
            self.scroll_input = true;
            ctx.request_compose();
        }
        rect + delta
    }

    fn on_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        if matches!(self.node.kind, NodeKind::Scroll { .. }) {
            if self.scroll.pointer(ctx, event) {
                self.scroll_input = true;
                ctx.request_compose();
            }
            return;
        }
        if let Some(input) = &mut self.input {
            if input.wheel(self.node.id, event, ctx) {
                return;
            }
            input.drag(self.node.id, event, ctx);
            if input.pointer(self.node.id, event, |e| ctx.submit_action::<NodeEvent>(e)) {
                if matches!(event, PointerEvent::Down(_)) {
                    input.pointer_focus(ctx);
                    ctx.capture_pointer();
                }
                ctx.set_handled();
            }
            return;
        }
        if self
            .slider
            .as_ref()
            .is_some_and(|slider| slider.id() == ctx.target())
            && matches!(
                event,
                PointerEvent::Down(_) | PointerEvent::Move(_) | PointerEvent::Up(_)
            )
        {
            ctx.set_handled();
        }
        if self.is_button() {
            let consumes = matches!(event, PointerEvent::Down(e) if e.button.is_none_or(|b| b == PointerButton::Primary))
                || matches!(event, PointerEvent::Up(_)) && ctx.is_active();
            if ButtonBehavior::pointer(ctx, event).is_some() {
                ctx.submit_action::<Self::Action>(self.activate());
                ctx.request_render();
            }
            if consumes {
                ctx.set_handled();
            }
        }
    }
    fn on_text_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &TextEvent,
    ) {
        if matches!(event, TextEvent::WindowFocusChange(false)) {
            self.scroll.cancel();
        }
        if let Some(input) = &mut self.input {
            use reel_ui::input::{Control, InputDeviceId, InputEvent};
            // Event propagation has already given child controls first refusal.
            // Editors retain even unhandled keys; ordinary controls may bubble
            // unrelated keys and mapped controls to their ancestor input nodes.
            if !ctx.is_text_input_focused()
                && let TextEvent::Keyboard(key) = event
                && input.key(self.node.id, key, |e| ctx.submit_action::<NodeEvent>(e))
            {
                ctx.set_handled();
            }
            // Cleanup still reaches the previous owner after focus changed.
            let cleanup = matches!(
                event,
                TextEvent::WindowFocusChange(false)
                    | TextEvent::Input(InputEvent::Cancel | InputEvent::Disconnected(_))
            );
            let editing_key = ctx.is_text_input_focused()
                && matches!(
                    event,
                    TextEvent::Keyboard(_)
                        | TextEvent::Input(InputEvent::Value {
                            device: InputDeviceId::Keyboard,
                            ..
                        })
                );
            if editing_key && !cleanup {
                return;
            }
            let raw = match event {
                TextEvent::Input(e) => Some(e.clone()),
                TextEvent::Keyboard(e) if e.repeat => {
                    if input.binds(Control::Key(e.code)) {
                        ctx.set_handled();
                    }
                    None
                }
                TextEvent::Keyboard(e) => Some(InputEvent::Value {
                    device: InputDeviceId::Keyboard,
                    control: Control::Key(e.code),
                    value: if e.state == keyboard::KeyState::Down {
                        1.
                    } else {
                        0.
                    },
                }),
                TextEvent::WindowFocusChange(false) => Some(InputEvent::Cancel),
                _ => None,
            };
            if let Some(raw) = raw
                && input.update(self.node.id, &raw, |e| ctx.submit_action::<NodeEvent>(e))
            {
                ctx.set_handled();
            }
        }
        if self.is_button() && self.button.text(ctx, event).is_some() {
            ctx.set_handled();
            ctx.submit_action::<Self::Action>(self.activate());
            ctx.request_render();
        }
        publish_interaction!(self, ctx);
    }
    fn on_access_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &AccessEvent,
    ) {
        if self.is_button() && ButtonBehavior::access(event).is_some() {
            ctx.set_handled();
            ctx.submit_action::<Self::Action>(self.activate());
            ctx.request_render();
        }
    }
    fn on_action(
        &mut self,
        ctx: &mut ActionCtx<'_>,
        _: &mut PropertiesMut<'_>,
        action: &ErasedAction,
        source: WidgetId,
    ) {
        if matches!(self.node.kind, NodeKind::Scroll { .. })
            && matches!(
                action.downcast_ref::<NodeEvent>(),
                Some(NodeEvent::Interaction(_))
            )
        {
            ctx.request_compose();
        }
        // Control proposals bubble through the same widget ancestors as their actions.
        // Reconcile optimistic values even when the authoritative projection is unchanged.
        if action.downcast_ref::<NavigationSelection>().is_some()
            || matches!(
                action.downcast_ref::<NodeEvent>(),
                Some(NodeEvent::Change(_) | NodeEvent::Text(_))
            )
        {
            self.sync_dirty = true;
        }
        if self.label.as_ref().is_some_and(|pod| pod.id() == source)
            && let Some(link) = action.downcast_ref::<reel_ui::widgets::LinkActivated>()
        {
            ctx.submit_action::<NodeEvent>(NodeEvent::Link(reel_ui::stage::NodeLink {
                node: self.node.id,
                id: link.id.clone(),
                text: link.text.clone(),
            }));
            ctx.set_handled();
        }
        if self.slider.as_ref().is_some_and(|pod| pod.id() == source)
            && let Some(change) = action.downcast_ref::<SliderMoved>()
            && let NodeKind::Slider { range, value, .. } = &mut Arc::make_mut(&mut self.node).kind
        {
            *value = change.value;
            let range = *range;
            self.sync_dirty = true;
            ctx.submit_action::<NodeEvent>(NodeEvent::Change(NodeChange {
                node: self.node.id,
                value: ControlValue::Number(change.value),
                range: Some(range),
            }));
            ctx.set_handled();
        }
        if self.editor.as_ref().is_some_and(|pod| pod.id() == source)
            && let Some(event) = action.downcast_ref::<TextInputEvent>()
        {
            if let TextInputEvent::Changed { value, .. } = event {
                self.sync_dirty = true;
                if let NodeKind::TextInput { value: current, .. } =
                    &mut Arc::make_mut(&mut self.node).kind
                {
                    *current = value.clone();
                }
            }
            ctx.submit_action::<NodeEvent>(NodeEvent::Text(NodeText {
                node: self.node.id,
                event: event.clone(),
            }));
            ctx.set_handled();
        }
    }
    fn update(&mut self, ctx: &mut UpdateCtx<'_>, _: &mut PropertiesMut<'_>, event: &Update) {
        self.button.update(event);
        if matches!(
            event,
            Update::DisabledChanged(true) | Update::StashedChanged(true)
        ) {
            self.scroll.cancel();
        }
        if self.is_button()
            && matches!(
                event,
                Update::FocusChanged(_)
                    | Update::FocusVisibilityChanged(_)
                    | Update::WindowFocusChanged(_)
                    | Update::DisabledChanged(_)
            )
        {
            ctx.request_render();
        }
        if matches!(
            event,
            Update::FocusChanged(false)
                | Update::ChildFocusChanged(false)
                | Update::DisabledChanged(true)
                | Update::StashedChanged(true)
        ) && let Some(input) = &mut self.input
        {
            input.cancel(self.node.id, |e| ctx.submit_action::<NodeEvent>(e));
        }
        if matches!(event, Update::WidgetAdded) {
            ctx.set_disabled(!self.node.props.enabled);
            ctx.set_modal(self.node.props.modal);
        }
        if matches!(event, Update::DescendantFocusChanged(_))
            && let Some(input) = &mut self.input
        {
            // Includes sibling-to-sibling moves with unchanged ancestor focus state.
            input.cancel(self.node.id, |e| ctx.submit_action::<NodeEvent>(e));
        }
        // These notifications run before the widget state's flag is committed.
        let disabled = match event {
            Update::DisabledChanged(value) => *value,
            _ => ctx.is_disabled(),
        };
        let stashed = match event {
            Update::StashedChanged(value) => *value,
            _ => ctx.is_stashed(),
        };
        let focused = match event {
            Update::ChildFocusChanged(value) => *value,
            _ => ctx.has_focus_target(),
        };
        if let Some(event) = self.interaction(
            InteractionState {
                hover: ctx.has_hovered(),
                pressed: ctx.has_active(),
                focused,
                focus_visible: ctx.focus_visible(),
                disabled,
            },
            !disabled && !stashed && ctx.is_window_focused(),
        ) {
            ctx.submit_action::<NodeEvent>(event);
        }
    }
    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
        if let Some(editor) = &mut self.editor {
            ctx.register_child(editor);
        }
        if let Some(slider) = &mut self.slider {
            ctx.register_child(slider);
        }
        if let Some(label) = &mut self.label {
            ctx.register_child(label);
        }
        if let Some(image) = &mut self.image {
            ctx.register_child(image);
        }
        for (_, child) in &mut self.children {
            ctx.register_child(child);
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
        let props = &self.node.props;
        let maximum = if axis == Axis::Horizontal {
            props.max_width
        } else {
            props.max_height
        };
        let bounded = |value: f64| maximum.map_or(value, |limit| value.min(limit));
        let request = match (request, maximum) {
            (LenReq::FitContent(space), Some(limit)) => {
                LenReq::FitContent(Length::px(space.get().min(limit)))
            }
            (LenReq::MaxContent, Some(limit)) => LenReq::FitContent(Length::px(limit)),
            _ => request,
        };
        if let Some(fixed) = if axis == Axis::Horizontal {
            props.width
        } else {
            props.height
        } {
            return Length::px(bounded(fixed));
        }
        if matches!(self.node.kind, NodeKind::Scroll { .. })
            && let LenReq::FitContent(space) = request
        {
            return space;
        }
        if let NodeKind::VirtualList(list) = &self.node.kind {
            if axis == Axis::Vertical {
                return Length::px(list.total_height + props.padding * 2.);
            }
            if let LenReq::FitContent(space) = request {
                return space;
            }
        }
        let padding = Length::px(props.padding * 2.);
        if reel_ui::layout::flow::is_flow(&self.node.kind) {
            return self.measure_flow(ctx, axis, request, cross);
        }
        if self.image.is_some() {
            let other = if props.stretch.axis(axis.cross()) {
                cross.map(Length::get)
            } else if axis == Axis::Horizontal {
                props
                    .height
                    .map(|height| props.max_height.map_or(height, |max| height.min(max)))
            } else {
                props
                    .width
                    .map(|width| props.max_width.map_or(width, |max| width.min(max)))
            };
            let native = self.image_size.get_coord(axis);
            let length = other.map_or(native, |other| {
                (other - padding.get()).max(0.) * (native / self.image_size.get_coord(axis.cross()))
            });
            return Length::px(bounded(length + padding.get()));
        }
        let available = LenDef::from(request).reduce(padding);
        let cross = if axis == Axis::Vertical && !props.stretch.axis(Axis::Horizontal) {
            props
                .width
                .map(|width| Length::px(props.max_width.map_or(width, |max| width.min(max))))
                .or(cross)
        } else {
            cross
        }
        .map(|value| value.saturating_sub(padding));
        let context = ctx.context_size();
        let sequential = self.sequential(axis);
        let mut total: f64 = 0.;
        if let Some(editor) = &mut self.editor {
            total = ctx
                .compute_length(editor, available, context, axis, cross)
                .get();
        }
        if let Some(slider) = &mut self.slider {
            total = ctx
                .compute_length(slider, available, context, axis, cross)
                .get();
        }
        if Self::caption(&self.node.kind).is_some()
            && let Some(label) = &mut self.label
        {
            total = ctx
                .compute_length(label, available, context, axis, cross)
                .get();
        }
        if self.node.props.decorated && self.selection_state().is_some() {
            total = if axis == Axis::Horizontal {
                total + 32.
            } else {
                total.max(24.)
            };
        }
        let mut visible = 0;
        for (node, child) in &mut self.children {
            if !node.props.visible
                || node.props.stretch.axis(axis)
                || matches!(node.kind, NodeKind::Cursor)
            {
                continue;
            }
            let length = ctx
                .compute_length(child, available, context, axis, cross)
                .get();
            if sequential {
                total += length;
            } else {
                total = total.max(length);
            }
            visible += 1;
        }
        if sequential && visible > 1 {
            total += self.node.props.gap * f64::from(visible - 1);
        }
        Length::px(bounded(total + padding.get()))
    }
    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, size: Size) {
        #[cfg(test)]
        {
            self.layouts += 1;
        }
        self.size = size;
        if self.node.props.clip || matches!(self.node.kind, NodeKind::Scroll { .. }) {
            ctx.set_clip_path(size.to_rect());
        } else {
            ctx.clear_clip_path();
        }
        let padding = self.node.props.padding;
        let inner = Size::new(
            (size.width - padding * 2.).max(0.),
            (size.height - padding * 2.).max(0.),
        );
        if let Some(editor) = &mut self.editor {
            ctx.run_layout(editor, inner);
            ctx.place_child(editor, Point::new(padding, padding));
        }
        if let Some(slider) = &mut self.slider {
            ctx.run_layout(slider, inner);
            ctx.place_child(slider, Point::new(padding, padding));
        }
        if let Some(label) = &mut self.label {
            let present = Self::caption(&self.node.kind).is_some();
            ctx.set_stashed(label, !present);
            if present {
                let inset = if self.node.props.decorated
                    && matches!(
                        self.node.kind,
                        NodeKind::Toggle { .. } | NodeKind::Radio { .. }
                    ) {
                    32.
                } else {
                    0.
                };
                let label_area = Size::new((inner.width - inset).max(0.), inner.height);
                let chosen = if matches!(
                    self.node.kind,
                    NodeKind::Button { .. } | NodeKind::Toggle { .. } | NodeKind::Radio { .. }
                ) {
                    ctx.compute_size(label, SizeDef::fit(label_area), label_area.into())
                } else {
                    inner
                };
                ctx.run_layout(label, chosen);
                ctx.place_child(
                    label,
                    Point::new(
                        padding + inset + (label_area.width - chosen.width) * 0.5,
                        padding + (inner.height - chosen.height) * 0.5,
                    ),
                );
            }
        }
        if let (NodeKind::Particles(batch), Some(image)) = (&self.node.kind, &self.particle_image) {
            let insets = batch.batches.iter().fold(Insets::ZERO, |insets, emission| {
                let frame = emission.frame;
                let config = &emission.config;
                let max_size =
                    (config.size + config.size_spread).max(config.size_end.unwrap_or(0.));
                let lifetime = config.max_lifetime();
                let radius =
                    max_size * (1. + f64::from(image.height) / f64::from(image.width)) * 0.5;
                let reach: [f64; 2] = std::array::from_fn(|axis| {
                    radius
                        + (config.velocity[axis].abs() + config.spread[axis]) * lifetime
                        + 0.5 * config.acceleration[axis].abs() * lifetime * lifetime
                });
                // Birth dimensions belong to emit's snapshot, not the current
                // layout. Resizing must keep older sprites inside paint bounds.
                Insets::new(
                    insets.x0.max(reach[0] - frame.origin[0]),
                    insets.y0.max(reach[1] - frame.origin[1]),
                    insets
                        .x1
                        .max((frame.origin[0] + frame.extent[0] + reach[0] - size.width).max(0.)),
                    insets
                        .y1
                        .max((frame.origin[1] + frame.extent[1] + reach[1] - size.height).max(0.)),
                )
            });
            ctx.set_paint_insets(insets);
        }
        if let Some(image) = &mut self.image {
            ctx.run_layout(image, inner);
            ctx.place_child(image, Point::new(padding, padding));
        }
        if reel_ui::layout::flow::is_flow(&self.node.kind) {
            self.layout_flow(ctx, size);
            return;
        }
        let mut cursor = 0.;
        let mut extent = Size::ZERO;
        let virtual_list = match &self.node.kind {
            NodeKind::VirtualList(list) => Some(list),
            _ => None,
        };
        let mut measurements = Vec::new();
        if let Some(state) = &mut self.virtual_list {
            state.children.clear();
            state.placement_pending = true;
        }
        for (node, child) in &mut self.children {
            ctx.set_stashed(child, !node.props.visible);
            if !node.props.visible {
                if virtual_list.is_some() {
                    measurements.push(VirtualRowMeasurement {
                        node: node.id,
                        height: 0.,
                    });
                }
                continue;
            }
            if matches!(node.kind, NodeKind::Cursor) {
                let chosen = ctx.compute_size(child, SizeDef::MAX, ctx.window_size().into());
                ctx.run_layout(child, chosen);
                ctx.place_child(child, Point::ORIGIN);
                continue;
            }
            let virtual_row = virtual_list.map(|list| {
                list.rows
                    .iter()
                    .find(|row| row.node == node.id)
                    .expect("virtual projection contains every mounted child")
            });
            let definition = if virtual_list.is_some() {
                SizeDef::new(LenDef::Fixed(Length::px(inner.width)), LenDef::MaxContent)
            } else if let NodeKind::Scroll { axis, .. } = self.node.kind {
                SizeDef::new(
                    if axis.horizontal() {
                        LenDef::MaxContent
                    } else {
                        LenDef::FitContent(Length::px(inner.width))
                    },
                    if axis.vertical() {
                        LenDef::MaxContent
                    } else {
                        LenDef::FitContent(Length::px(inner.height))
                    },
                )
            } else {
                SizeDef::fit(inner)
            };
            let capped = |axis, maximum: Option<f64>| match (definition.dim(axis), maximum) {
                (LenDef::Fixed(value), Some(limit)) => {
                    LenDef::Fixed(Length::px(value.get().min(limit)))
                }
                (LenDef::FitContent(value), Some(limit)) => {
                    LenDef::FitContent(Length::px(value.get().min(limit)))
                }
                (LenDef::MaxContent, Some(limit)) => LenDef::FitContent(Length::px(limit)),
                (value, _) => value,
            };
            let definition = SizeDef::new(
                if node.props.stretch.axis(Axis::Horizontal) {
                    LenDef::Fixed(Length::px(inner.width))
                } else {
                    capped(Axis::Horizontal, node.props.max_width)
                },
                if node.props.stretch.axis(Axis::Vertical) {
                    LenDef::Fixed(Length::px(inner.height))
                } else {
                    capped(Axis::Vertical, node.props.max_height)
                },
            );
            let chosen = ctx.compute_size(child, definition, inner.into());
            let mut offset = if let Some(row) = virtual_row {
                measurements.push(VirtualRowMeasurement {
                    node: node.id,
                    height: chosen.height,
                });
                self.virtual_list
                    .as_mut()
                    .unwrap()
                    .children
                    .push(VirtualChild {
                        index: row.index,
                        widget: child.id(),
                    });
                Point::new(0., row.offset)
            } else {
                match self.node.kind {
                    NodeKind::Row(_)
                    | NodeKind::Scroll {
                        axis: ScrollAxis::Horizontal,
                        ..
                    } if !node.props.stretch.axis(Axis::Horizontal) => {
                        let offset = Point::new(cursor, 0.);
                        cursor += chosen.width + self.node.props.gap;
                        offset
                    }
                    NodeKind::Column(_)
                    | NodeKind::Scroll {
                        axis: ScrollAxis::Vertical | ScrollAxis::Both,
                        ..
                    } if !node.props.stretch.axis(Axis::Vertical) => {
                        let offset = Point::new(0., cursor);
                        cursor += chosen.height + self.node.props.gap;
                        offset
                    }
                    _ => Point::ORIGIN,
                }
            };
            let (flow_x, flow_y) = match self.node.kind {
                NodeKind::Row(_) => (true, false),
                NodeKind::Column(_) | NodeKind::VirtualList(_) => (false, true),
                NodeKind::Scroll { axis, .. } => (axis.horizontal(), axis.vertical()),
                _ => (false, false),
            };
            if !flow_x {
                offset.x += (inner.width - chosen.width).max(0.) * node.props.align_x;
            }
            if !flow_y {
                offset.y += (inner.height - chosen.height).max(0.) * node.props.align_y;
            }
            if !node.props.stretch.axis(Axis::Horizontal) {
                extent.width = extent.width.max(offset.x + chosen.width);
            }
            if !node.props.stretch.axis(Axis::Vertical) {
                extent.height = extent.height.max(offset.y + chosen.height);
            }
            ctx.run_layout(child, chosen);
            // Virtual offsets remain subpixel until composition. Rounding a large
            // prefix before subtracting the corrected scroll offset creates a
            // visible 1px jump even when their mathematical difference is fixed.
            let layout_y = if virtual_row.is_some() { 0. } else { offset.y };
            ctx.place_child(child, Point::new(padding + offset.x, padding + layout_y));
        }
        if let Some(state) = &mut self.virtual_list {
            if state.measurements.as_ref() != measurements.as_slice() {
                state.measurements = measurements.into();
            }
            state.children.sort_unstable_by_key(|row| row.index);
        }
        if let NodeKind::Scroll {
            axis, follow_end, ..
        } = self.node.kind
        {
            self.scroll.layout(inner, extent, axis, follow_end);
            self.scroll_laid_out = true;
        }
    }
    fn compose(&mut self, ctx: &mut ComposeCtx<'_>) {
        // A restored hidden scroll has not been laid out. Keep its pending
        // position until showing it supplies the actual viewport and content.
        if matches!(self.node.kind, NodeKind::Scroll { .. })
            && (ctx.is_stashed() || !self.scroll_laid_out)
        {
            return;
        }
        if let Some(offset) = self.scroll_request.take() {
            self.scroll.set_offset(offset);
        }
        let translation = if let NodeKind::Scroll { align, .. } = self.node.kind {
            let metrics = (
                self.scroll.offset(),
                self.scroll.viewport(),
                self.scroll.content(),
            );
            if self.scroll_feedback != Some(metrics) || self.scroll_baseline != metrics.0 {
                ctx.submit_action::<NodeEvent>(NodeEvent::Scroll(reel_ui::stage::NodeScroll {
                    input: std::mem::take(&mut self.scroll_input),
                    node: self.node.id,
                    requested: self.scroll_baseline,
                    offset: metrics.0,
                    viewport: metrics.1,
                    content: metrics.2,
                }));
                self.scroll_feedback = Some(metrics);
                self.scroll_baseline = metrics.0;
            }
            self.scroll.translation(align)
        } else {
            Vec2::ZERO
        };
        if matches!(self.node.kind, NodeKind::Scroll { .. }) {
            for (node, child) in &mut self.children {
                let NodeKind::VirtualList(view) = &node.kind else {
                    continue;
                };
                let Some((list, mut list_ctx)) = ctx.get_raw_mut(child) else {
                    continue;
                };
                let state = list.virtual_list.as_mut().expect("virtual list child");
                state.retained.clear();
                for (row, pod) in &mut list.children {
                    if let Some((_, row_ctx)) = list_ctx.get_raw(pod)
                        && (row_ctx.has_focus_target() || row_ctx.has_active())
                    {
                        state.retained.push(row.id);
                    }
                }
                let retained = state
                    .feedback
                    .as_ref()
                    .filter(|old| old.retained.as_ref() == state.retained.as_slice())
                    .map(|old| old.retained.clone())
                    .unwrap_or_else(|| Arc::from(state.retained.as_slice()));
                let start = -translation.y - node.props.padding;
                state.viewport_start = start;
                let feedback = NodeVirtualList {
                    node: node.id,
                    scroll: self.node.id,
                    revision: view.revision,
                    requested: self.scroll_baseline,
                    offset: self.scroll.offset(),
                    viewport: self.scroll.viewport(),
                    width: (list.size.width - node.props.padding * 2.).max(0.),
                    visible_start: start,
                    visible_end: start + self.scroll.viewport().height,
                    measurements: state.measurements.clone(),
                    retained,
                    navigation: state.navigation,
                };
                if state.feedback.as_ref() != Some(&feedback) {
                    state.feedback = Some(feedback.clone());
                    list_ctx.submit_action::<NodeEvent>(NodeEvent::VirtualList(feedback));
                }
            }
        }
        if let NodeKind::VirtualList(view) = &self.node.kind {
            self.virtual_list
                .as_mut()
                .unwrap()
                .place_measured(view, self.node.props.gap);
        }
        let view = if let NodeKind::Camera(camera) = self.node.kind {
            camera.transform(Size::new(
                (self.size.width - self.node.props.padding * 2.).max(0.),
                (self.size.height - self.node.props.padding * 2.).max(0.),
            ))
        } else {
            reel_ui::kurbo::Affine::IDENTITY
        };
        for (node, child) in &mut self.children {
            let transform = view * node.props.transform(ctx.child_size(child));
            ctx.set_child_transform(child, transform);
            let row_offset = self
                .virtual_list
                .as_ref()
                .and_then(|state| state.placement.iter().find(|(id, _)| *id == node.id))
                .map_or(0., |(_, y)| *y);
            // Keep scroll and row prefixes continuous until their transforms
            // combine. Rounding each layer can move a stationary reading anchor.
            ctx.set_animated_child_scroll_translation(
                child,
                translation + Vec2::new(0., row_offset),
            );
        }
    }
    fn pre_paint(&mut self, ctx: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, _: &mut Painter<'_>) {
        let mask = self
            .node
            .props
            .mask
            .as_ref()
            .filter(|mask| !mask.is_image_threshold() || self.node.props.mask_progress < 1.);
        ctx.set_subtree_composite(
            self.node.props.opacity as f32,
            self.node.props.invert as f32,
            self.node.props.blend,
            mask.map(|program| PaintImageMask {
                program: program.clone(),
                images: self.mask_images.clone(),
                bounds: self.size.to_rect(),
                parameters: [
                    self.node.props.mask_progress as f32,
                    self.node.props.mask_softness.min(f32::MAX as f64) as f32,
                    self.node.props.mask_x as f32,
                    self.node.props.mask_y as f32,
                    f32::from(self.node.props.mask_invert),
                ],
            }),
        );
    }
    fn paint(&mut self, ctx: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, painter: &mut Painter<'_>) {
        let bounds = self.size.to_rect();
        let border = BorderWidth::all(Length::px(
            self.node
                .props
                .border_width
                .min(self.size.width.min(self.size.height) * 0.5),
        ));
        let radius = CornerRadius::all(Length::px(self.node.props.corner_radius));
        if let Some(background) = self.node.props.background {
            paint_background(
                painter,
                bounds,
                &Background::Color(color(background)),
                &radius,
            );
        }
        paint_border(
            painter,
            bounds,
            &BorderColor {
                color: color(self.node.props.border_color),
            },
            &border,
            &radius,
        );
        if self.node.props.decorated
            && self.is_button()
            && ctx.is_focus_target()
            && ctx.focus_visible()
            && ctx.is_window_focused()
        {
            painter
                .stroke(
                    self.size.to_rect().inset(-2.).to_rounded_rect(3.),
                    &reel_ui::kurbo::Stroke::new(2.),
                    color(self.node.props.color),
                )
                .draw();
        }
        if let (NodeKind::Particles(batch), Some(image)) = (&self.node.kind, &self.particle_image) {
            use reel_ui::{kurbo::Affine, peniko::ImageBrushRef};
            // A single widget, image resource and painter pass. Image alpha is
            // carried in the brush, avoiding one compositing group per sprite.
            for sprite in batch.samples() {
                if sprite.opacity <= 0. || sprite.size <= 0. {
                    continue;
                }
                let transform = Affine::translate((sprite.position[0], sprite.position[1]))
                    * Affine::rotate(sprite.rotation.to_radians())
                    * Affine::scale(sprite.size / f64::from(image.width))
                    * Affine::translate((
                        -f64::from(image.width) * 0.5,
                        -f64::from(image.height) * 0.5,
                    ));
                painter.draw_image(
                    ImageBrushRef::from(image).with_alpha(sprite.opacity as f32),
                    transform,
                );
            }
        }
        self.paint_selection(ctx, painter);
    }
    fn accessibility_role(&self) -> Role {
        match self.node.kind {
            NodeKind::Toggle {
                appearance: reel_ui::stage::ToggleAppearance::Checkbox,
                ..
            } => Role::CheckBox,
            NodeKind::Toggle { .. } => Role::Switch,
            NodeKind::Radio { .. } => Role::RadioButton,
            NodeKind::Button { .. } => Role::Button,
            _ => Role::GenericContainer,
        }
    }
    fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, node: &mut Node) {
        if self.is_button() {
            node.add_action(reel_ui::accesskit::Action::Click);
            if let Some(label) = Self::caption(&self.node.kind) {
                node.set_label(label);
            }
            if let Some(value) = self.selection_state() {
                node.set_toggled(if value {
                    reel_ui::accesskit::Toggled::True
                } else {
                    reel_ui::accesskit::Toggled::False
                });
            }
        }
    }
    fn children_ids(&self) -> ChildrenIds {
        self.label
            .iter()
            .map(WidgetPod::id)
            .chain(self.image.iter().map(WidgetPod::id))
            .chain(self.slider.iter().map(WidgetPod::id))
            .chain(self.editor.iter().map(WidgetPod::id))
            .chain(self.children.iter().map(|(_, child)| child.id()))
            .collect()
    }
    fn accepts_pointer_interaction(&self) -> bool {
        self.is_button()
            || self.input.is_some()
            || matches!(self.node.kind, NodeKind::Scroll { .. })
            || self
                .children
                .iter()
                .any(|(node, _)| matches!(node.kind, NodeKind::Cursor))
    }
    fn is_cursor(&self) -> bool {
        matches!(self.node.kind, NodeKind::Cursor)
    }
    fn accepts_focus(&self) -> bool {
        self.is_button() || self.input.as_ref().is_some_and(NodeInput::accepts_focus)
    }
    fn selection_group(&self) -> Option<reel_ui::core::SelectionGroup> {
        match self.node.kind {
            NodeKind::Radio {
                group: Some(id),
                selected,
                ..
            } => Some(reel_ui::core::SelectionGroup { id, selected }),
            _ => None,
        }
    }
    fn select_group_member(&mut self, ctx: &mut UpdateCtx<'_>, _: &mut PropertiesMut<'_>) {
        let NodeEvent::Change(change) = self.activate() else {
            unreachable!("selection group member is a radio");
        };
        ctx.submit_untyped_action(Box::new(NavigationSelection(change)));
    }
    fn accepts_multiple_pointers(&self) -> bool {
        self.input.is_some()
    }
    fn propagates_pointer_interaction(&self) -> bool {
        !self.is_button()
    }
}
