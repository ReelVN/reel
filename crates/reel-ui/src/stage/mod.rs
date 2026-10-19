// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Author-owned scene composition, shared by the script host and presentation.
mod camera;
mod image;
mod mask;
pub use camera::CameraView;
pub use image::ImageSource;
pub use mask::{MaskChannel, MaskOp, MaskProgram};
pub mod particles;

use crate::input::{ActionPhase, Binding};
use crate::kurbo::{Affine, Insets, Point, Size, Vec2};
use crate::peniko::Color;
use crate::properties::ObjectFit;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub struct ActionDefinition {
    pub name: Arc<str>,
    pub bindings: Vec<Binding>,
}

/// A validated immutable binding set. Replacing it starts a fresh input lifetime.
#[derive(Clone, Debug)]
pub struct InputBindings(Arc<[ActionDefinition]>);
impl PartialEq for InputBindings {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl std::ops::Deref for InputBindings {
    type Target = [ActionDefinition];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl InputBindings {
    pub fn new(actions: Vec<ActionDefinition>) -> Result<Self, crate::input::InvalidInput> {
        for (i, a) in actions.iter().enumerate() {
            if a.name.is_empty() || actions[..i].iter().any(|b| a.name == b.name) {
                return Err(crate::input::InvalidInput);
            }
        }
        crate::input::ActionMap::new(
            actions
                .iter()
                .enumerate()
                .map(|(i, a)| (i, a.bindings.clone())),
        )?;
        Ok(Self(actions.into()))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NodeAction {
    pub node: NodeId,
    pub action: Arc<str>,
    pub value: f32,
    pub phase: ActionPhase,
    /// Shared immutable mapping identifies events from a retired binding set.
    pub bindings: InputBindings,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NodeEvent {
    Link(NodeLink),
    Click(NodeClick),
    Action(NodeAction),
    Change(NodeChange),
    Interaction(NodeInteraction),
    Scroll(NodeScroll),
    VirtualList(NodeVirtualList),
    Pointer(NodePointer),
    Text(NodeText),
    Key(NodeKey),
}
#[derive(Clone, Debug, PartialEq)]
pub struct NodeLink {
    pub node: NodeId,
    pub id: Arc<str>,
    pub text: reel_core::text::Text,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeScroll {
    /// True only for a newer user scroll or focus-navigation gesture.
    pub input: bool,
    pub node: NodeId,
    pub requested: Vec2,
    pub offset: Vec2,
    pub viewport: Size,
    pub content: Size,
}
/// A mounted row's place in the complete logical list. Offsets exclude list padding.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VirtualRow {
    pub node: NodeId,
    pub index: usize,
    pub offset: f64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VirtualListNavigation {
    pub request: u64,
    pub ready: bool,
    /// Requested index identifies the UI gesture; resolved_index follows its key.
    pub index: usize,
    pub resolved_index: usize,
    pub forward: bool,
}
/// Compact author-stage projection; only these rows own mounted component trees.
#[derive(Clone, Debug, PartialEq)]
pub struct VirtualListView {
    pub revision: u64,
    pub total_height: f64,
    pub total_rows: usize,
    pub rows: Arc<[VirtualRow]>,
    pub navigation: Option<VirtualListNavigation>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VirtualRowMeasurement {
    pub node: NodeId,
    pub height: f64,
}
/// Latest layout/input demand. Measurements and offsets use list content coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeVirtualList {
    pub node: NodeId,
    pub scroll: NodeId,
    pub revision: u64,
    pub requested: Vec2,
    pub offset: Vec2,
    pub viewport: Size,
    pub width: f64,
    pub visible_start: f64,
    pub visible_end: f64,
    pub measurements: Arc<[VirtualRowMeasurement]>,
    pub retained: Arc<[NodeId]>,
    pub navigation: Option<VirtualListNavigation>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerPhase {
    Pressed,
    Moved,
    Released,
    Cancelled,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodePointer {
    pub node: NodeId,
    pub position: Point,
    pub phase: PointerPhase,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NodeKey {
    pub node: NodeId,
    pub event: crate::core::keyboard::KeyboardEvent,
    pub phase: ActionPhase,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NodeText {
    pub node: NodeId,
    pub event: crate::widgets::TextInputEvent,
}

/// Transient interaction shared by native input and author-owned visuals.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InteractionState {
    pub hover: bool,
    pub pressed: bool,
    pub focused: bool,
    pub focus_visible: bool,
    pub disabled: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeInteraction {
    pub node: NodeId,
    pub state: InteractionState,
}

/// A user change to one stateful control. Range identifies the input configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeChange {
    pub node: NodeId,
    pub value: ControlValue,
    pub range: Option<crate::widgets::SliderRange>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ControlValue {
    Number(f64),
    Bool(bool),
    /// Activate the option from this accepted presentation revision.
    Selection(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToggleAppearance {
    Switch,
    Checkbox,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct NodeId(pub u64);

/// One activation routed by the UI through accepted presentation geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeClick {
    pub node: NodeId,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    Particles(particles::ParticleNode),
    Text(reel_core::text::Text),
    TextInput {
        value: Arc<str>,
    },
    Button {
        label: Option<String>,
        listening: bool,
    },
    Slider {
        value: f64,
        range: crate::widgets::SliderRange,
        listening: bool,
    },
    Toggle {
        label: Option<String>,
        value: bool,
        listening: bool,
        appearance: ToggleAppearance,
    },
    Radio {
        label: Option<String>,
        selected: bool,
        group: Option<crate::core::SelectionGroupId>,
        /// Changes only when the option's identity changes, not for hover/value updates.
        revision: u64,
    },
    Image {
        source: ImageSource,
        fit: ObjectFit,
        /// Nine-slice borders in source pixels, or ordinary object-fit drawing.
        slice: Option<Insets>,
        /// Logical border width per source pixel when nine-slice drawing is enabled.
        slice_scale: f64,
        repeat: [bool; 2],
        tile_scale: f64,
        corner_colors: Option<[Color; 4]>,
        slice_center: bool,
        /// Author constructor location, retained through later source changes.
        origin: Option<String>,
    },
    Input {
        bindings: InputBindings,
        listening: bool,
        keyboard: bool,
        pointer: bool,
    },
    Scroll {
        axis: crate::widgets::ScrollAxis,
        follow_end: bool,
        align: crate::widgets::ScrollAlign,
        offset: Vec2,
    },
    Camera(CameraView),
    Row,
    Column,
    VirtualList(VirtualListView),
    Stack,
}

/// Axes filled from the parent's content box after its size has been measured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stretch {
    #[default]
    None,
    X,
    Y,
    Both,
}
impl Stretch {
    pub fn axis(self, axis: crate::kurbo::Axis) -> bool {
        matches!(
            (self, axis),
            (Self::Both, _)
                | (Self::X, crate::kurbo::Axis::Horizontal)
                | (Self::Y, crate::kurbo::Axis::Vertical)
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NodeProps {
    pub x: f64,
    pub y: f64,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub max_width: Option<f64>,
    pub max_height: Option<f64>,
    pub align_x: f64,
    pub align_y: f64,
    pub stretch: Stretch,
    pub opacity: f64,
    /// Subtree inversion in encoded sRGB, preserving alpha.
    pub invert: f64,
    /// Blend the completed subtree with its backdrop.
    pub blend: crate::peniko::BlendMode,
    pub visible: bool,
    pub enabled: bool,
    /// A visible subtree owns input until hidden, removed, or superseded in paint order.
    pub modal: bool,
    pub clip: bool,
    /// Draw built-in control decorations; authored children are unaffected.
    pub decorated: bool,
    /// Visible UTF-8 byte prefix; full text is always used for layout.
    pub reveal: Option<usize>,
    pub gap: f64,
    pub padding: f64,
    pub font_size: f64,
    pub font: String,
    pub text_align: crate::TextAlign,
    /// Automatic wrapping for text nodes; explicit newlines are always retained.
    pub text_wrap: bool,
    pub line_height: f64,
    pub text_fit: crate::core::TextFit,
    pub text_outline: f64,
    pub text_outline_color: [u8; 4],
    pub color: [u8; 4],
    pub background: Option<[u8; 4]>,
    pub rotation: f64,
    pub scale_x: f64,
    pub scale_y: f64,
    pub anchor_x: f64,
    pub anchor_y: f64,
    /// Whole-image threshold mask in the node's local layout rectangle.
    pub mask: Option<Arc<MaskProgram>>,
    pub mask_origin: Option<Arc<str>>,
    pub mask_progress: f64,
    pub mask_softness: f64,
    pub mask_invert: bool,
    pub mask_x: f64,
    pub mask_y: f64,
}
impl Default for NodeProps {
    fn default() -> Self {
        Self {
            x: 0.,
            y: 0.,
            width: None,
            height: None,
            max_width: None,
            max_height: None,
            align_x: 0.,
            align_y: 0.,
            stretch: Stretch::None,
            opacity: 1.,
            invert: 0.,
            blend: Default::default(),
            visible: true,
            enabled: true,
            modal: false,
            clip: false,
            decorated: true,
            reveal: None,
            gap: 0.,
            padding: 0.,
            font_size: 24.,
            font: "system-ui".into(),
            text_align: crate::TextAlign::Start,
            text_wrap: true,
            line_height: 1.2,
            text_fit: crate::core::TextFit::None,
            text_outline: 0.,
            text_outline_color: [0, 0, 0, 255],
            color: [255; 4],
            background: None,
            rotation: 0.,
            scale_x: 1.,
            scale_y: 1.,
            anchor_x: 0.,
            anchor_y: 0.,
            mask: None,
            mask_origin: None,
            mask_progress: 1.,
            mask_softness: 0.06,
            mask_invert: false,
            mask_x: 0.,
            mask_y: 0.,
        }
    }
}

impl NodeProps {
    /// Map the layout box's anchor to x/y in its parent's layout coordinates.
    /// The same affine is consumed by painting, clipping and presented geometry.
    pub fn transform(&self, size: Size) -> Affine {
        Affine::translate((self.x, self.y))
            * Affine::rotate(self.rotation.to_radians())
            * Affine::scale_non_uniform(self.scale_x, self.scale_y)
            * Affine::translate((-self.anchor_x * size.width, -self.anchor_y * size.height))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct StageNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub props: NodeProps,
    pub children: Vec<NodeId>,
    pub parent: Option<NodeId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageError {
    RetiredNode,
    AlreadyParented,
    DuplicateChild,
    IdentityExhausted,
    NotContainer,
    Cycle,
    Index,
}
impl std::fmt::Display for StageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for StageError {}

/// Process-local projection identity; restoring a stage starts a new identity.
#[derive(Clone, Default, Debug)]
pub struct StageIdentity(Arc<()>);
impl PartialEq for StageIdentity {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for StageIdentity {}

/// Revisions of the node itself and of its child order or descendants.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct NodeRevision {
    pub properties: u64,
    pub children: u64,
}

#[derive(Debug)]
struct StoredNode {
    node: StageNode,
    revision: NodeRevision,
}
impl std::ops::Deref for StoredNode {
    type Target = StageNode;
    fn deref(&self) -> &StageNode {
        &self.node
    }
}
impl std::ops::DerefMut for StoredNode {
    fn deref_mut(&mut self) -> &mut StageNode {
        &mut self.node
    }
}
impl From<StageNode> for StoredNode {
    fn from(node: StageNode) -> Self {
        Self {
            node,
            revision: NodeRevision::default(),
        }
    }
}

#[derive(Default, Debug)]
pub struct Stage {
    root: Option<NodeId>,
    nodes: BTreeMap<NodeId, StoredNode>,
    next_id: u64,
    revision: u64,
    identity: StageIdentity,
}
impl Stage {
    pub fn root(&self) -> Option<NodeId> {
        self.root
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn identity(&self) -> &StageIdentity {
        &self.identity
    }
    pub fn node_revision(&self, id: NodeId) -> Option<NodeRevision> {
        self.nodes.get(&id).map(|node| node.revision)
    }
    pub fn node(&self, id: NodeId) -> Option<&StageNode> {
        self.nodes.get(&id).map(|node| &node.node)
    }
    pub fn nodes(&self) -> impl Iterator<Item = &StageNode> {
        self.nodes.values().map(|node| &node.node)
    }
    fn changed(&mut self, id: NodeId, children: bool) {
        self.revision = self.revision.wrapping_add(1);
        let node = self.nodes.get_mut(&id).unwrap();
        node.revision.properties = self.revision;
        if children {
            node.revision.children = self.revision;
        }
        let mut parent = node.parent;
        while let Some(id) = parent {
            let node = self.nodes.get_mut(&id).unwrap();
            node.revision.children = self.revision;
            parent = node.parent;
        }
    }
    /// Clock changes do not clone the immutable emission collection.
    pub fn set_particle_time(&mut self, id: NodeId, time: f64) -> Result<(), StageError> {
        let node = self.nodes.get_mut(&id).ok_or(StageError::RetiredNode)?;
        let NodeKind::Particles(particles) = &mut node.kind else {
            return Err(StageError::NotContainer);
        };
        if particles.time != time {
            particles.time = time;
            self.changed(id, false);
        }
        Ok(())
    }
    pub fn set_particle_batches(
        &mut self,
        id: NodeId,
        batches: particles::ParticleBatches,
    ) -> Result<(), StageError> {
        let node = self.nodes.get_mut(&id).ok_or(StageError::RetiredNode)?;
        let NodeKind::Particles(particles) = &mut node.kind else {
            return Err(StageError::NotContainer);
        };
        if particles.batches != batches {
            particles.batches = batches;
            self.changed(id, false);
        }
        Ok(())
    }

    pub fn next_identity(&self) -> Result<NodeId, StageError> {
        self.next_id
            .checked_add(1)
            .map(NodeId)
            .ok_or(StageError::IdentityExhausted)
    }

    /// Reconstruct detached nodes in increasing identity order. Attach children
    /// with `add` and publish the root only after the candidate is complete.
    pub fn restore_node(
        &mut self,
        id: NodeId,
        kind: NodeKind,
        props: NodeProps,
    ) -> Result<(), StageError> {
        if id.0 <= self.next_id {
            return Err(StageError::DuplicateChild);
        }
        self.next_id = id.0;
        self.nodes.insert(
            id,
            StageNode {
                id,
                kind,
                props,
                children: Vec::new(),
                parent: None,
            }
            .into(),
        );
        Ok(())
    }

    /// Keep explicitly supplied component roots in authored order. An expression
    /// may also yield an already attached descendant of one of those roots.
    /// This does not detach, adopt, or retire anything.
    pub fn composition_roots(&self, candidates: &[NodeId]) -> Result<Vec<NodeId>, StageError> {
        let mut supplied = BTreeSet::new();
        for &id in candidates {
            if !self.nodes.contains_key(&id) {
                return Err(StageError::RetiredNode);
            }
            if !supplied.insert(id) {
                return Err(StageError::DuplicateChild);
            }
        }
        let mut roots = Vec::new();
        let mut resolved = BTreeMap::new();
        let mut path = Vec::new();
        for &id in candidates {
            path.clear();
            let mut current = id;
            let root = loop {
                if let Some(&root) = resolved.get(&current) {
                    break root;
                }
                if self.root == Some(current) {
                    return Err(StageError::AlreadyParented);
                }
                path.push(current);
                match self.nodes[&current].parent {
                    Some(parent) => current = parent,
                    None => break current,
                }
            };
            if !supplied.contains(&root) {
                return Err(StageError::AlreadyParented);
            }
            for &ancestor in &path {
                resolved.insert(ancestor, root);
            }
            if id == root {
                roots.push(id);
            }
        }
        Ok(roots)
    }

    /// Adopts each child exactly once. Constructing a detached tree does not
    /// change the displayed stage revision.
    pub fn create(
        &mut self,
        kind: NodeKind,
        props: NodeProps,
        children: Vec<NodeId>,
    ) -> Result<NodeId, StageError> {
        let mut seen = BTreeSet::new();
        for child in &children {
            let node = self.nodes.get(child).ok_or(StageError::RetiredNode)?;
            if node.parent.is_some() || self.root == Some(*child) {
                return Err(StageError::AlreadyParented);
            }
            if !seen.insert(*child) {
                return Err(StageError::DuplicateChild);
            }
        }
        let id = self.next_identity()?;
        self.next_id = id.0;
        for child in &children {
            self.nodes.get_mut(child).unwrap().parent = Some(id);
        }
        self.nodes.insert(
            id,
            StageNode {
                id,
                kind,
                props,
                children,
                parent: None,
            }
            .into(),
        );
        Ok(id)
    }

    pub fn update(
        &mut self,
        id: NodeId,
        kind: NodeKind,
        props: NodeProps,
    ) -> Result<(), StageError> {
        let node = self.nodes.get_mut(&id).ok_or(StageError::RetiredNode)?;
        if node.kind != kind || node.props != props {
            node.kind = kind;
            node.props = props;
            self.changed(id, false);
        }
        Ok(())
    }

    /// Insert one detached component, keeping every existing node identity.
    pub fn add(&mut self, parent: NodeId, child: NodeId, index: usize) -> Result<(), StageError> {
        let container = self.nodes.get(&parent).ok_or(StageError::RetiredNode)?;
        if !matches!(
            container.kind,
            NodeKind::Camera(_)
                | NodeKind::Row
                | NodeKind::Column
                | NodeKind::VirtualList(_)
                | NodeKind::Scroll { .. }
                | NodeKind::Stack
                | NodeKind::Button { .. }
                | NodeKind::Slider { .. }
                | NodeKind::Toggle { .. }
                | NodeKind::Radio { .. }
                | NodeKind::Input { .. }
        ) {
            return Err(StageError::NotContainer);
        }
        if index > container.children.len() {
            return Err(StageError::Index);
        }
        let node = self.nodes.get(&child).ok_or(StageError::RetiredNode)?;
        if node.parent.is_some() || self.root == Some(child) {
            return Err(StageError::AlreadyParented);
        }
        let mut ancestor = Some(parent);
        while let Some(id) = ancestor {
            if id == child {
                return Err(StageError::Cycle);
            }
            ancestor = self.nodes[&id].parent;
        }
        self.nodes
            .get_mut(&parent)
            .unwrap()
            .children
            .insert(index, child);
        self.nodes.get_mut(&child).unwrap().parent = Some(parent);
        self.changed(parent, true);
        self.nodes.get_mut(&child).unwrap().revision.properties = self.revision;
        Ok(())
    }

    /// Validate a complete child order, accepting retained children and detached roots.
    pub fn validate_children(&self, parent: NodeId, children: &[NodeId]) -> Result<(), StageError> {
        let container = self.nodes.get(&parent).ok_or(StageError::RetiredNode)?;
        if !matches!(
            container.kind,
            NodeKind::Camera(_)
                | NodeKind::Row
                | NodeKind::Column
                | NodeKind::VirtualList(_)
                | NodeKind::Scroll { .. }
                | NodeKind::Stack
                | NodeKind::Button { .. }
                | NodeKind::Slider { .. }
                | NodeKind::Toggle { .. }
                | NodeKind::Radio { .. }
                | NodeKind::Input { .. }
        ) {
            return Err(StageError::NotContainer);
        }
        let mut ancestors = BTreeSet::new();
        let mut current = Some(parent);
        while let Some(id) = current {
            ancestors.insert(id);
            current = self.nodes[&id].parent;
        }
        let mut seen = BTreeSet::new();
        for &child in children {
            let node = self.nodes.get(&child).ok_or(StageError::RetiredNode)?;
            if ancestors.contains(&child) {
                return Err(StageError::Cycle);
            }
            if node.parent.is_some_and(|p| p != parent) || self.root == Some(child) {
                return Err(StageError::AlreadyParented);
            }
            if !seen.insert(child) {
                return Err(StageError::DuplicateChild);
            }
        }
        Ok(())
    }

    /// Reorder/adopt children atomically; only absent subtrees are retired.
    pub fn reconcile_children(
        &mut self,
        parent: NodeId,
        children: Vec<NodeId>,
    ) -> Result<Vec<NodeId>, StageError> {
        self.validate_children(parent, &children)?;
        if self.nodes[&parent].children == children {
            return Ok(Vec::new());
        }
        let retained: BTreeSet<_> = children.iter().copied().collect();
        for &child in &children {
            let node = self.nodes.get_mut(&child).unwrap();
            if node.parent != Some(parent) {
                node.parent = Some(parent);
                node.revision.properties = self.revision.wrapping_add(1);
            }
        }
        let old = std::mem::replace(&mut self.nodes.get_mut(&parent).unwrap().children, children);
        let mut retired = Vec::new();
        for child in old {
            if !retained.contains(&child) {
                retired.extend(self.remove_tree(child));
            }
        }
        self.changed(parent, true);
        Ok(retired)
    }

    /// Remove a component from its parent or the stage and retire its full tree.
    pub fn remove(&mut self, node: NodeId) -> Result<Vec<NodeId>, StageError> {
        let parent = self.nodes.get(&node).ok_or(StageError::RetiredNode)?.parent;
        if let Some(parent) = parent {
            let children = &mut self.nodes.get_mut(&parent).unwrap().children;
            children.remove(children.iter().position(|&child| child == node).unwrap());
            self.changed(parent, true);
        } else if self.root == Some(node) {
            self.root = None;
        }
        if parent.is_none() {
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(self.remove_tree(node))
    }

    /// Keep the container, retiring only the children it currently owns.
    pub fn clear_children(&mut self, parent: NodeId) -> Result<Vec<NodeId>, StageError> {
        let node = self.nodes.get_mut(&parent).ok_or(StageError::RetiredNode)?;
        if !matches!(
            node.kind,
            NodeKind::Camera(_)
                | NodeKind::Row
                | NodeKind::Column
                | NodeKind::VirtualList(_)
                | NodeKind::Scroll { .. }
                | NodeKind::Stack
                | NodeKind::Button { .. }
                | NodeKind::Slider { .. }
                | NodeKind::Toggle { .. }
                | NodeKind::Radio { .. }
                | NodeKind::Input { .. }
        ) {
            return Err(StageError::NotContainer);
        }
        if node.children.is_empty() {
            return Ok(Vec::new());
        }
        let children = std::mem::take(&mut node.children);
        let mut retired = Vec::new();
        for child in children {
            retired.extend(self.remove_tree(child));
        }
        self.changed(parent, true);
        Ok(retired)
    }

    /// Commit a complete new root and return retired identities to its owner.
    pub fn replace(&mut self, root: NodeId) -> Result<Vec<NodeId>, StageError> {
        if self.root == Some(root) {
            return Ok(Vec::new());
        }
        if self
            .nodes
            .get(&root)
            .ok_or(StageError::RetiredNode)?
            .parent
            .is_some()
        {
            return Err(StageError::AlreadyParented);
        }
        let retired = self.clear();
        self.root = Some(root);
        self.revision = self.revision.wrapping_add(1);
        Ok(retired)
    }

    /// Retire a detached subtree whose script owner has disappeared.
    pub fn discard(&mut self, root: NodeId) -> Result<Vec<NodeId>, StageError> {
        if self
            .nodes
            .get(&root)
            .ok_or(StageError::RetiredNode)?
            .parent
            .is_some()
            || self.root == Some(root)
        {
            return Err(StageError::AlreadyParented);
        }
        Ok(self.remove_tree(root))
    }
    fn remove_tree(&mut self, root: NodeId) -> Vec<NodeId> {
        let mut pending = vec![root];
        let mut retired = Vec::new();
        while let Some(id) = pending.pop() {
            if let Some(node) = self.nodes.remove(&id) {
                pending.extend(node.node.children);
                retired.push(id);
            }
        }
        retired
    }
    pub fn clear(&mut self) -> Vec<NodeId> {
        match self.root.take() {
            Some(root) => {
                self.revision = self.revision.wrapping_add(1);
                self.remove_tree(root)
            }
            None => Vec::new(),
        }
    }
}
