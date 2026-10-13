// Copyright 2026 Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Author-owned scene composition, shared by the script host and presentation.
use crate::kurbo::{Affine, Size};
use crate::properties::ObjectFit;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u64);

#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    Text(String),
    Image {
        source: String,
        fit: ObjectFit,
        /// Author constructor location, retained through later source changes.
        origin: Option<String>,
    },
    Row,
    Column,
    Stack,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NodeProps {
    pub x: f64,
    pub y: f64,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub opacity: f64,
    pub visible: bool,
    pub clip: bool,
    pub gap: f64,
    pub padding: f64,
    pub font_size: f64,
    pub color: [u8; 4],
    pub background: Option<[u8; 4]>,
    pub rotation: f64,
    pub scale_x: f64,
    pub scale_y: f64,
    pub anchor_x: f64,
    pub anchor_y: f64,
}
impl Default for NodeProps {
    fn default() -> Self {
        Self {
            x: 0.,
            y: 0.,
            width: None,
            height: None,
            opacity: 1.,
            visible: true,
            clip: false,
            gap: 0.,
            padding: 0.,
            font_size: 24.,
            color: [255; 4],
            background: None,
            rotation: 0.,
            scale_x: 1.,
            scale_y: 1.,
            anchor_x: 0.,
            anchor_y: 0.,
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

#[derive(Default, Debug)]
pub struct Stage {
    root: Option<NodeId>,
    nodes: BTreeMap<NodeId, StageNode>,
    next_id: u64,
    revision: u64,
}
impl Stage {
    pub fn root(&self) -> Option<NodeId> {
        self.root
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn node(&self, id: NodeId) -> Option<&StageNode> {
        self.nodes.get(&id)
    }
    pub fn nodes(&self) -> impl Iterator<Item = &StageNode> {
        self.nodes.values()
    }
    pub fn next_identity(&self) -> Result<NodeId, StageError> {
        self.next_id
            .checked_add(1)
            .map(NodeId)
            .ok_or(StageError::IdentityExhausted)
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
            },
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
            self.revision = self.revision.wrapping_add(1);
        }
        Ok(())
    }

    /// Insert one detached component, keeping every existing node identity.
    pub fn add(&mut self, parent: NodeId, child: NodeId, index: usize) -> Result<(), StageError> {
        let container = self.nodes.get(&parent).ok_or(StageError::RetiredNode)?;
        if !matches!(
            container.kind,
            NodeKind::Row | NodeKind::Column | NodeKind::Stack
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
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    /// Remove a component from its parent or the stage and retire its full tree.
    pub fn remove(&mut self, node: NodeId) -> Result<Vec<NodeId>, StageError> {
        let parent = self.nodes.get(&node).ok_or(StageError::RetiredNode)?.parent;
        if let Some(parent) = parent {
            let children = &mut self.nodes.get_mut(&parent).unwrap().children;
            children.remove(children.iter().position(|&child| child == node).unwrap());
        } else if self.root == Some(node) {
            self.root = None;
        }
        self.revision = self.revision.wrapping_add(1);
        Ok(self.remove_tree(node))
    }

    /// Keep the container, retiring only the children it currently owns.
    pub fn clear_children(&mut self, parent: NodeId) -> Result<Vec<NodeId>, StageError> {
        let node = self.nodes.get_mut(&parent).ok_or(StageError::RetiredNode)?;
        if !matches!(
            node.kind,
            NodeKind::Row | NodeKind::Column | NodeKind::Stack
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
        self.revision = self.revision.wrapping_add(1);
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
                pending.extend(node.children);
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
