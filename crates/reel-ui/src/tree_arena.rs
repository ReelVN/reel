// Copyright 2024 the Xilem Authors
// SPDX-License-Identifier: Apache-2.0

//! Private safe widget tree with bounded, nonrecursive lookup.

use std::vec::Vec;

use hashbrown::HashMap;

type NodeId = u64;

// One independently leased workspace per arena. References never retain its
// contents. Mutex preserves Send/Sync and try_lock avoids blocking/reentrancy;
// An unavailable lease uses the bounded stack fallback.
#[derive(Debug, Default)]
struct LookupScratch {
    slots: std::sync::Mutex<Vec<NodeId>>,
    capacity: usize,
}

impl LookupScratch {
    fn bytes_for(capacity: usize) -> Option<usize> {
        capacity
            .checked_mul(size_of::<NodeId>())?
            .checked_add(size_of::<Self>())
    }
    fn configure(&mut self, capacity: usize) -> usize {
        let slots = self
            .slots
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slots = vec![0; capacity];
        self.capacity = slots.capacity();
        self.bytes()
    }
    fn bytes(&self) -> usize {
        Self::bytes_for(self.capacity).expect("allocated lookup capacity fits usize")
    }
}

/// Retain the ancestors closest to `boundary` in a circular bounded buffer.
/// Return the full number of parent links walked, or None for an absent/outside
/// node. The caller descends through that chunk and repeats from its new boundary.
/// A query of relative depth D and C slots visits D + (D-C) + ... parent entries,
/// and descends through D child entries. It neither allocates nor recurses.
fn lookup_chunk(
    parents: &HashMap<NodeId, Option<NodeId>>,
    id: NodeId,
    boundary: Option<NodeId>,
    slots: &mut [NodeId],
) -> Option<usize> {
    debug_assert!(
        !slots.is_empty(),
        "lookup selects a nonempty leased or stack buffer"
    );
    let mut current = Some(id);
    let mut walked = 0;
    while current != boundary {
        let id = current?;
        let parent = *parents.get(&id)?;
        slots[walked % slots.len()] = id;
        walked += 1;
        current = parent;
    }
    Some(walked)
}

#[derive(Debug)]
struct TreeNode<T> {
    id: NodeId,
    item: T,
    children: HashMap<NodeId, Self>,
}

/// A container type for a tree of items.
///
/// This type is used to store zero, one or many trees of a given item type. It
/// will keep track of parent-child relationships, lets you efficiently find
/// an item anywhere in the tree hierarchy, and give you mutable access to this item
/// and its children.
#[derive(Debug)]
pub struct TreeArena<T> {
    roots: HashMap<NodeId, TreeNode<T>>,
    parents_map: HashMap<NodeId, Option<NodeId>>,
    lookup_scratch: LookupScratch,
}

/// A reference type giving shared access to an arena item and its children.
///
/// When you borrow an item from a [`TreeArena`], it returns an `ArenaRef`.
/// You can iterate over its children to get access to child `ArenaRef` handles.
#[derive(Debug)]
pub struct ArenaRef<'arena, T> {
    /// The parent of this node
    pub parent_id: Option<NodeId>,
    /// The payload of the node
    pub item: &'arena T,
    /// Reference to the children of the node
    pub children: ArenaRefList<'arena, T>,
}

/// A handle giving shared access to a set of arena items.
///
/// See [`ArenaRef`] for more information.
#[derive(Debug)]
pub struct ArenaRefList<'arena, T> {
    parent_id: Option<NodeId>,
    children: &'arena HashMap<NodeId, TreeNode<T>>,
    parents_map: ArenaMapRef<'arena>,
}

/// A reference type giving mutable access to an arena item and its children.
///
/// When you borrow an item from a [`TreeArena`], it returns an `ArenaMut`.
/// This struct holds three fields:
///  - the id of its parent.
///  - a reference to the item itself.
///  - an [`ArenaMutList`] handle to access its children.
///
/// Because the latter two are disjoint references, you can mutate the node's value
/// and its children independently without invalidating the references.
///
/// You can iterate over its children to get access to child `ArenaMut` handles.
#[derive(Debug)]
pub struct ArenaMut<'arena, T> {
    /// The parent of the node
    pub parent_id: Option<NodeId>,
    /// The payload of the node
    pub item: &'arena mut T,
    /// Reference to the children of the node
    pub children: ArenaMutList<'arena, T>,
}

/// A handle giving mutable access to a set of arena items.
///
/// See [`ArenaMut`] for more information.
#[derive(Debug)]
pub struct ArenaMutList<'arena, T> {
    parent_id: Option<NodeId>,
    children: &'arena mut HashMap<NodeId, TreeNode<T>>,
    parents_map: ArenaMapMut<'arena>,
}

/// A shared reference to the parent father map
#[derive(Clone, Copy, Debug)]
struct ArenaMapRef<'arena> {
    parents_map: &'arena HashMap<NodeId, Option<NodeId>>,
    lookup: &'arena LookupScratch,
}

/// A mutable reference to the parent father map
#[derive(Debug)]
struct ArenaMapMut<'arena> {
    parents_map: &'arena mut HashMap<NodeId, Option<NodeId>>,
    lookup: &'arena LookupScratch,
}

// -- MARK: IMPLS

impl<T> Clone for ArenaRef<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for ArenaRef<'_, T> {}

impl<T> Clone for ArenaRefList<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for ArenaRefList<'_, T> {}

impl<T> Default for TreeArena<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> TreeArena<T> {
    /// Creates an empty tree.
    pub fn new() -> Self {
        Self {
            roots: HashMap::new(),
            parents_map: HashMap::new(),
            lookup_scratch: LookupScratch::default(),
        }
    }

    /// Returns the number of items in the arena.
    pub fn len(&self) -> usize {
        self.parents_map.len()
    }

    /// Preallocate an independent lookup workspace outside event dispatch.
    /// Queries never grow it. Longer paths use repeated bounded chunks, without
    /// truncation or treating a present node as absent. An unconfigured or busy workspace uses 32 stack slots.
    pub fn configure_lookup_path_storage(&mut self, capacity: usize) -> usize {
        self.lookup_scratch.configure(capacity)
    }

    /// Retained workspace header and actual heap capacity, excluding stack slots.
    pub fn lookup_path_storage_bytes(&self) -> usize {
        self.lookup_scratch.bytes()
    }

    /// Planned storage for a configured capacity, before allocating it.
    pub fn lookup_path_storage_bytes_for(capacity: usize) -> Option<usize> {
        LookupScratch::bytes_for(capacity)
    }

    /// Returns a handle giving access to the roots of the tree.
    pub fn roots(&self) -> ArenaRefList<'_, T> {
        ArenaRefList {
            parent_id: None,
            children: &self.roots,
            parents_map: ArenaMapRef {
                parents_map: &self.parents_map,
                lookup: &self.lookup_scratch,
            },
        }
    }

    /// Returns a handle giving access to the roots of the tree.
    ///
    /// Using [`insert`](ArenaMutList::insert) on this handle
    /// will add a new root to the tree.
    pub fn roots_mut(&mut self) -> ArenaMutList<'_, T> {
        ArenaMutList {
            parent_id: None,
            children: &mut self.roots,
            parents_map: ArenaMapMut {
                parents_map: &mut self.parents_map,
                lookup: &self.lookup_scratch,
            },
        }
    }

    /// Finds an item in the tree.
    ///
    /// Returns a shared reference to the item if present.
    ///
    /// # Complexity
    ///
    /// - O(Depth + Depth²/Capacity) in the safe implementation (linear when the
    ///   path fits its configured workspace; 32 slots when no lease is available).
    pub fn find(&self, id: impl Into<NodeId>) -> Option<ArenaRef<'_, T>> {
        self.roots().find_inner(id.into())
    }

    /// Finds an item in the tree.
    ///
    /// Returns a mutable reference to the item if present.
    ///
    /// # Complexity
    ///
    /// - O(Depth + Depth²/Capacity) in the safe implementation (linear when the
    ///   path fits its configured workspace; 32 slots when no lease is available).
    pub fn find_mut(&mut self, id: impl Into<NodeId>) -> Option<ArenaMut<'_, T>> {
        self.roots_mut().find_mut_inner(id.into())
    }

    /// Returns the path of items from the given item to the root of the tree.
    ///
    /// The path is in order from the bottom to the top, starting at the given item and ending at
    /// the root.
    ///
    /// If the id is not in the tree, returns an empty vector.
    pub fn get_id_path(&self, id: impl Into<NodeId>) -> Vec<NodeId> {
        self.roots().parents_map.get_id_path(id.into(), None)
    }

    /// Moves the given child (along with all its children) to the new parent.
    ///
    /// # Panics
    ///
    /// Panics if the parent is actually a child of the to-be-reparented node, or
    /// if either node id cannot be found, or
    /// if both given ids are equal.
    #[cfg(test)]
    pub fn reparent(&mut self, child: impl Into<NodeId>, new_parent: impl Into<NodeId>) {
        let child_id = child.into();
        let new_parent_id = new_parent.into();

        assert_ne!(
            child_id, new_parent_id,
            "expected child to be different from new_parent but both have id #{child_id}"
        );
        assert!(
            !self.get_id_path(new_parent_id).contains(&child_id),
            "cannot reparent because new_parent #{new_parent_id} is a child of the to-be-reparented node #{child_id}"
        );

        // Ensure new parent exists.
        let _ = self
            .find(new_parent_id)
            .unwrap_or_else(|| panic!("no node found for new_parent id #{new_parent_id}"));

        // Detach the child from its current location (root or non-root).
        let was_root = self.roots.contains_key(&child_id);
        let child_node = if was_root {
            self.roots
                .remove(&child_id)
                .unwrap_or_else(|| panic!("no node found for child id #{child_id}"))
        } else {
            let old_parent_id = self
                .parents_map
                .get(&child_id)
                .unwrap_or_else(|| panic!("no node found for child id #{child_id}"))
                .unwrap();
            self.find_mut(old_parent_id)
                .unwrap()
                .children
                .children
                .remove(&child_id)
                .unwrap()
        };

        self.parents_map.insert(child_id, Some(new_parent_id));
        self.find_mut(new_parent_id)
            .unwrap_or_else(|| panic!("no node found for new_parent id #{new_parent_id}"))
            .children
            .children
            .insert(child_id, child_node);
    }
}

impl<T> TreeNode<T> {
    fn arena_ref<'arena>(
        &'arena self,
        parent_id: Option<NodeId>,
        parents_map: ArenaMapRef<'arena>,
    ) -> ArenaRef<'arena, T> {
        ArenaRef {
            parent_id,
            item: &self.item,
            children: ArenaRefList {
                parent_id: Some(self.id),
                children: &self.children,
                parents_map,
            },
        }
    }

    fn arena_mut<'arena>(
        &'arena mut self,
        parent_id: Option<NodeId>,
        parents_map: ArenaMapMut<'arena>,
    ) -> ArenaMut<'arena, T> {
        ArenaMut {
            parent_id,
            item: &mut self.item,
            children: ArenaMutList {
                parent_id: Some(self.id),
                children: &mut self.children,
                parents_map,
            },
        }
    }
}

impl<T> ArenaRef<'_, T> {}

impl<'arena, T> ArenaRefList<'arena, T> {
    /// Returns a handle to the element of the list with the given id.
    pub fn item(self, id: impl Into<NodeId>) -> Option<ArenaRef<'arena, T>> {
        let id = id.into();
        self.children
            .get(&id)
            .map(|child| child.arena_ref(self.parent_id, self.parents_map))
    }

    /// Finds an arena item among the list's items and their descendants.
    ///
    /// Returns a shared reference to the item if present.
    ///
    /// # Complexity
    ///
    /// - O(Depth + Depth²/Capacity) in the safe implementation.
    pub fn find(self, id: impl Into<NodeId>) -> Option<ArenaRef<'arena, T>> {
        self.find_inner(id.into())
    }

    fn find_inner(self, id: NodeId) -> Option<ArenaRef<'arena, T>> {
        if let Ok(mut slots) = self.parents_map.lookup.slots.try_lock()
            && !slots.is_empty()
        {
            return self.find_with_storage(id, &mut slots);
        }
        self.find_with_storage(id, &mut [0; 32])
    }

    fn find_with_storage(self, id: NodeId, slots: &mut [NodeId]) -> Option<ArenaRef<'arena, T>> {
        let parent_id = *self.parents_map.parents_map.get(&id)?;
        let mut boundary = self.parent_id;
        let mut children = self.children;
        loop {
            let walked = lookup_chunk(self.parents_map.parents_map, id, boundary, slots)?;
            if walked == 0 {
                return None;
            }
            for offset in 0..walked.min(slots.len()) {
                let next = slots[(walked - 1 - offset) % slots.len()];
                let node = children.get(&next)?;
                if next == id {
                    return Some(node.arena_ref(parent_id, self.parents_map));
                }
                boundary = Some(next);
                children = &node.children;
            }
        }
    }
}

impl<T> ArenaMut<'_, T> {
    /// Returns a mutable reference equivalent to this one.
    ///
    /// This is sometimes useful to work with the borrow checker.
    pub fn reborrow_mut(&mut self) -> ArenaMut<'_, T> {
        ArenaMut {
            parent_id: self.parent_id,
            item: self.item,
            children: self.children.reborrow_mut(),
        }
    }
}

impl<'arena, T> ArenaMutList<'arena, T> {
    /// Returns `true` if the list has an element with the given id.
    pub fn has(&self, id: impl Into<NodeId>) -> bool {
        let id = id.into();
        self.children.contains_key(&id)
    }

    /// Returns a shared handle to the element of the list with the given id.
    pub fn item(&self, id: impl Into<NodeId>) -> Option<ArenaRef<'_, T>> {
        let id = id.into();
        self.children
            .get(&id)
            .map(|child| child.arena_ref(self.parent_id, self.parents_map.reborrow()))
    }

    /// Returns a mutable handle to the element of the list with the given id.
    pub fn item_mut(&mut self, id: impl Into<NodeId>) -> Option<ArenaMut<'_, T>> {
        let id = id.into();
        self.children
            .get_mut(&id)
            .map(|child| child.arena_mut(self.parent_id, self.parents_map.reborrow_mut()))
    }

    /// Returns a mutable handle to the element of the list with the given id.
    ///
    /// This is the same as [`item_mut`](Self::item_mut), except it consumes
    /// self. This is sometimes necessary to accommodate the borrow checker.
    pub fn into_item_mut(self, id: impl Into<NodeId>) -> Option<ArenaMut<'arena, T>> {
        let id = id.into();
        self.children
            .get_mut(&id)
            .map(|child| child.arena_mut(self.parent_id, self.parents_map))
    }

    // TODO - Remove the child_id argument once creation of widgets is figured out.
    // Return the id instead.
    // TODO - Add #[must_use]
    /// Inserts a child into the tree under the common parent of this list's items.
    ///
    /// If this list was returned from [`TreeArena::roots_mut()`], create a new tree root.
    ///
    /// The new child will have the given id.
    ///
    /// Returns a handle to the new child.
    ///
    /// # Panics
    ///
    /// If the arena already contains an item with the given id.
    pub fn insert(&mut self, child_id: impl Into<NodeId>, value: T) -> ArenaMut<'_, T> {
        let child_id = child_id.into();
        assert!(
            !self.parents_map.parents_map.contains_key(&child_id),
            "Key already present"
        );
        self.parents_map
            .parents_map
            .insert(child_id, self.parent_id);

        self.children.insert(
            child_id,
            TreeNode {
                id: child_id,
                item: value,
                children: HashMap::new(),
            },
        );

        self.children
            .get_mut(&child_id)
            .unwrap()
            .arena_mut(self.parent_id, self.parents_map.reborrow_mut())
    }

    // TODO - How to handle when a subtree is removed?
    // Move children to the root?
    /// Removes the item with the given id from the arena.
    ///
    /// If the id isn't in the list (even if it's e.g. a descendant), does nothing
    /// and returns `None`.
    ///
    /// Else, returns the removed item.
    ///
    /// This will also silently remove any recursive grandchildren of this item.
    #[must_use]
    pub fn remove(&mut self, child_id: impl Into<NodeId>) -> Option<T> {
        let child_id = child_id.into();
        let child = self.children.remove(&child_id)?;

        fn remove_children_from_map<I>(
            node: &TreeNode<I>,
            parents_map: &mut HashMap<NodeId, Option<NodeId>>,
        ) {
            for child in &node.children {
                remove_children_from_map(child.1, parents_map);
            }
            parents_map.remove(&node.id);
        }

        remove_children_from_map(&child, self.parents_map.parents_map);

        Some(child.item)
    }

    /// Returns a shared handle equivalent to this one.
    pub fn reborrow(&self) -> ArenaRefList<'_, T> {
        ArenaRefList {
            parent_id: self.parent_id,
            children: &*self.children,
            parents_map: self.parents_map.reborrow(),
        }
    }

    /// Returns a mutable handle equivalent to this one.
    ///
    /// This is sometimes useful to work with the borrow checker.
    pub fn reborrow_mut(&mut self) -> ArenaMutList<'_, T> {
        ArenaMutList {
            parent_id: self.parent_id,
            children: &mut *self.children,
            parents_map: self.parents_map.reborrow_mut(),
        }
    }

    /// Finds an arena item among the list's items and their descendants.
    ///
    /// Returns a shared reference to the item if present.
    ///
    /// # Complexity
    ///
    /// - O(Depth + Depth²/Capacity) in the safe implementation.
    pub fn find(&self, id: impl Into<NodeId>) -> Option<ArenaRef<'_, T>> {
        self.reborrow().find(id)
    }

    /// Finds an arena item among the list's items and their descendants.
    ///
    /// Returns a mutable reference to the item if present.
    ///
    /// # Complexity
    ///
    /// - O(Depth + Depth²/Capacity) in the safe implementation.
    #[cfg(test)]
    pub fn find_mut(self, id: impl Into<NodeId>) -> Option<ArenaMut<'arena, T>> {
        self.find_mut_inner(id.into())
    }

    fn find_mut_inner(self, id: NodeId) -> Option<ArenaMut<'arena, T>> {
        if let Ok(mut slots) = self.parents_map.lookup.slots.try_lock()
            && !slots.is_empty()
        {
            return self.find_mut_with_storage(id, &mut slots);
        }
        self.find_mut_with_storage(id, &mut [0; 32])
    }

    fn find_mut_with_storage(
        self,
        id: NodeId,
        slots: &mut [NodeId],
    ) -> Option<ArenaMut<'arena, T>> {
        let parent_id = *self.parents_map.parents_map.get(&id)?;
        let mut boundary = self.parent_id;
        let mut children = self.children;
        loop {
            let walked = lookup_chunk(self.parents_map.parents_map, id, boundary, slots)?;
            if walked == 0 {
                return None;
            }
            for offset in 0..walked.min(slots.len()) {
                let next = slots[(walked - 1 - offset) % slots.len()];
                let node = children.get_mut(&next)?;
                if next == id {
                    return Some(node.arena_mut(parent_id, self.parents_map));
                }
                boundary = Some(next);
                children = &mut node.children;
            }
        }
    }
}

impl ArenaMapRef<'_> {
    /// Returns the path of items from the given item to the root of the tree.
    ///
    /// The path is in order from the bottom to the top, starting at the given item and ending at
    /// the root.
    ///
    /// If `start_id` is Some, the path ends just before that id instead; `start_id` is not included.
    ///
    /// If there is no path from `start_id` to id, returns an empty vector.
    fn get_id_path(self, id: NodeId, start_id: Option<NodeId>) -> Vec<NodeId> {
        let mut path = Vec::new();

        if !self.parents_map.contains_key(&id) {
            return path;
        }

        let mut current_id = Some(id);
        while let Some(current) = current_id {
            path.push(current);
            current_id = *self
                .parents_map
                .get(&current)
                .expect("All ids in the tree should have a parent in the parent map");
            if current_id == start_id {
                break;
            }
        }

        if current_id != start_id {
            path.clear();
        }

        path
    }
}

impl<'arena> ArenaMapMut<'arena> {
    /// Returns a shared handle equivalent to this one.
    fn reborrow(&self) -> ArenaMapRef<'_> {
        ArenaMapRef {
            parents_map: self.parents_map,
            lookup: self.lookup,
        }
    }

    /// Returns a mutable handle equivalent to this one.
    ///
    /// This is sometimes useful to work with the borrow checker.
    fn reborrow_mut(&mut self) -> ArenaMapMut<'_> {
        ArenaMapMut {
            parents_map: self.parents_map,
            lookup: self.lookup,
        }
    }
}

#[cfg(test)]
mod read_lookup_tests {
    use super::*;
    #[test]
    fn read_lookup_preserves_scope_after_reparent_and_removal() {
        let mut tree = TreeArena::new();
        {
            let mut roots = tree.roots_mut();
            let mut root = roots.insert(1_u64, "root");
            root.children
                .insert(2_u64, "branch")
                .children
                .insert(3_u64, "leaf");
            root.children.insert(4_u64, "sibling");
        }
        tree.roots_mut().insert(9_u64, "other root");
        assert_eq!(tree.find(3_u64).unwrap().item, &"leaf");
        let branch = tree.find(2_u64).unwrap().children;
        assert!(branch.find(3_u64).is_some());
        for outside in [1_u64, 2, 4, 9, 99] {
            assert!(
                branch.find(outside).is_none(),
                "outside child scope: {outside}"
            );
        }
        for outside in [1_u64, 2, 4, 9, 99] {
            assert!(
                tree.find_mut(2_u64)
                    .unwrap()
                    .children
                    .find_mut(outside)
                    .is_none(),
                "outside mutable child scope: {outside}"
            );
        }
        *tree
            .find_mut(2_u64)
            .unwrap()
            .children
            .find_mut(3_u64)
            .unwrap()
            .item = "changed leaf";
        tree.reparent(3_u64, 4_u64);
        assert!(tree.find(2_u64).unwrap().children.find(3_u64).is_none());
        assert_eq!(
            tree.find(4_u64).unwrap().children.find(3_u64).unwrap().item,
            &"changed leaf"
        );
        assert_eq!(
            tree.find_mut(4_u64).unwrap().children.remove(3_u64),
            Some("changed leaf")
        );
        assert!(tree.find(3_u64).is_none());
    }

    fn chain(capacity: usize) -> TreeArena<u64> {
        let mut tree = TreeArena::new();
        tree.configure_lookup_path_storage(capacity);
        tree.roots_mut().insert(1_u64, 1);
        for id in 2_u64..=65 {
            tree.find_mut(id - 1).unwrap().children.insert(id, id);
        }
        tree.roots_mut().insert(100_u64, 100);
        tree
    }

    #[test]
    fn segmented_lookup_keeps_deep_scope_and_reparent_remove_correct() {
        for capacity in [0, 1, 2, 8, 32, 128] {
            let mut tree = chain(capacity);
            assert_eq!(tree.find(65_u64).unwrap().item, &65);
            let earlier = tree.find(17_u64).unwrap();
            assert_eq!(earlier.children.find(65_u64).unwrap().item, &65);
            assert_eq!(
                earlier.item, &17,
                "a later lookup cannot invalidate a handle"
            );
            let branch = tree.find(33_u64).unwrap().children;
            for outside in [1_u64, 17, 33, 100, 101] {
                assert!(branch.find(outside).is_none());
            }
            for outside in [1_u64, 17, 33, 100, 101] {
                assert!(
                    tree.find_mut(33_u64)
                        .unwrap()
                        .children
                        .find_mut(outside)
                        .is_none()
                );
            }
            *tree.find_mut(65_u64).unwrap().item = 999;
            tree.reparent(33_u64, 100_u64);
            assert!(tree.find(17_u64).unwrap().children.find(65_u64).is_none());
            assert_eq!(
                tree.find(100_u64)
                    .unwrap()
                    .children
                    .find(65_u64)
                    .unwrap()
                    .item,
                &999
            );
            assert_eq!(
                tree.find_mut(100_u64).unwrap().children.remove(33_u64),
                Some(33)
            );
            assert!(tree.find(65_u64).is_none());
        }
    }

    #[test]
    fn busy_lookup_workspace_is_never_overwritten_and_traits_are_preserved() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<TreeArena<u64>>();
        send_sync::<ArenaRef<'static, u64>>();
        let mut tree = chain(8);
        {
            let mut guard = tree.lookup_scratch.slots.try_lock().unwrap();
            guard.fill(u64::MAX);
            assert_eq!(tree.find(65_u64).unwrap().item, &65);
            assert!(guard.iter().all(|id| *id == u64::MAX));
        }
        {
            let roots = tree.roots_mut();
            let lookup = roots.parents_map.lookup;
            let mut guard = lookup.slots.try_lock().unwrap();
            guard.fill(u64::MAX);
            *roots.find_mut(65_u64).unwrap().item = 123;
            assert!(guard.iter().all(|id| *id == u64::MAX));
        }
        assert_eq!(tree.find(65_u64).unwrap().item, &123);
        assert_eq!(
            tree.lookup_path_storage_bytes(),
            TreeArena::<u64>::lookup_path_storage_bytes_for(8).unwrap()
        );
    }

    #[test]
    fn repeated_parent_work_matches_the_configuration_bound() {
        for depth in [1_usize, 32, 33, 128, 512, 2048] {
            let mut parents = HashMap::new();
            parents.insert(1, None);
            for id in 2..=depth as u64 {
                parents.insert(id, Some(id - 1));
            }
            for capacity in [1, 32, 128, depth] {
                let mut slots = vec![0; capacity];
                let mut boundary = None;
                let mut visits = 0;
                loop {
                    let walked =
                        lookup_chunk(&parents, depth as u64, boundary, &mut slots).unwrap();
                    visits += walked;
                    if walked <= capacity {
                        break;
                    }
                    boundary = Some(slots[(walked - capacity) % capacity]);
                }
                let chunks = depth.div_ceil(capacity);
                assert_eq!(
                    visits,
                    chunks * depth - capacity * chunks * (chunks - 1) / 2
                );
            }
        }
    }
}
