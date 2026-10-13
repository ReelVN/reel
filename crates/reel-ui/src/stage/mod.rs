//! Author-owned scene composition, shared by the script host and presentation.
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u64);

#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    Text(String),
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
        }
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
