// Copyright 2024 the Xilem Authors
// SPDX-License-Identifier: Apache-2.0

//! Thread-owned widget storage using the upstream flat tree implementation.

pub use tree_arena::{ArenaMut, ArenaMutList, ArenaRef, ArenaRefList, TreeArena};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_handles_survive_insertion_reparenting_and_removal() {
        let mut tree = TreeArena::new();
        tree.roots_mut().insert(1_u64, 1);
        for id in 2_u64..=65 {
            tree.find_mut(id - 1).unwrap().children.insert(id, id);
        }
        tree.roots_mut().insert(100_u64, 100);
        let earlier = tree.find(17_u64).unwrap();
        assert_eq!(earlier.children.find(65_u64).unwrap().item, &65);
        assert_eq!(earlier.item, &17);
        for outside in [1_u64, 17, 33, 100, 101] {
            assert!(tree.find(33_u64).unwrap().children.find(outside).is_none());
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

    #[test]
    fn disjoint_parent_payload_remains_live_while_children_grow() {
        let mut tree = TreeArena::new();
        let mut roots = tree.roots_mut();
        let root = roots.insert(1_u64, 10);
        let item = root.item;
        let mut children = root.children;
        for id in 2..1000_u64 {
            children.insert(id, id);
            *item += 1;
        }
        assert_eq!(*item, 1008);
        assert_eq!(*children.item(999_u64).unwrap().item, 999);
        assert_eq!(children.remove(2_u64), Some(2));
        assert_eq!(*item, 1008);
    }
}
