use super::*;

fn stack(stage: &mut Stage, children: Vec<NodeId>) -> NodeId {
    stage
        .create(NodeKind::Stack, NodeProps::default(), children)
        .unwrap()
}

fn roots(stage: &Stage) -> Vec<NodeId> {
    stage.detached_roots().collect()
}

#[test]
fn detached_roots_follow_component_adoption_mounting_and_retirement() {
    let mut stage = Stage::default();
    let first = stack(&mut stage, vec![]);
    let second = stack(&mut stage, vec![]);
    let spare = stack(&mut stage, vec![]);
    let root = stack(&mut stage, vec![second, first]);
    assert_eq!(roots(&stage), [spare, root]);
    assert_eq!(
        stage.revision(),
        0,
        "detached construction stays unpublished"
    );

    assert!(stage.replace(root).unwrap().is_empty());
    assert_eq!(roots(&stage), [spare]);
    let fresh = stack(&mut stage, vec![]);
    stage.add(root, fresh, 0).unwrap();
    assert_eq!(roots(&stage), [spare]);

    assert_eq!(
        stage.reconcile_children(root, vec![first, spare]).unwrap(),
        [fresh, second]
    );
    assert!(roots(&stage).is_empty());
    let revision = stage.revision();
    assert!(
        stage
            .reconcile_children(root, vec![first, spare])
            .unwrap()
            .is_empty()
    );
    assert!(stage.replace(root).unwrap().is_empty());
    assert_eq!(stage.revision(), revision);
    let survivor = stack(&mut stage, vec![]);
    assert_eq!(stage.clear_children(root).unwrap(), [first, spare]);
    assert_eq!(roots(&stage), [survivor]);
    assert_eq!(stage.clear(), [root]);
    assert_eq!(roots(&stage), [survivor]);

    stage.replace(survivor).unwrap();
    let discarded = stack(&mut stage, vec![]);
    let removed = stack(&mut stage, vec![]);
    assert_eq!(stage.discard(discarded).unwrap(), [discarded]);
    assert_eq!(stage.remove(removed).unwrap(), [removed]);
    assert!(roots(&stage).is_empty());
    assert_eq!(stage.root(), Some(survivor));
}

#[test]
fn failed_structure_changes_preserve_component_roots_and_revision() {
    let mut stage = Stage::default();
    let root = stack(&mut stage, vec![]);
    stage.replace(root).unwrap();
    let child = stack(&mut stage, vec![]);
    let outer = stack(&mut stage, vec![child]);
    let spare = stack(&mut stage, vec![]);
    let expected = roots(&stage);
    let revision = stage.revision();

    assert!(matches!(
        stage.create(NodeKind::Stack, NodeProps::default(), vec![spare, spare]),
        Err(StageError::DuplicateChild)
    ));
    assert!(matches!(stage.add(child, outer, 0), Err(StageError::Cycle)));
    assert!(matches!(stage.add(root, spare, 1), Err(StageError::Index)));
    assert!(matches!(
        stage.reconcile_children(root, vec![spare, spare]),
        Err(StageError::DuplicateChild)
    ));
    assert!(matches!(
        stage.reconcile_children(root, vec![child]),
        Err(StageError::AlreadyParented)
    ));
    assert!(matches!(
        stage.replace(child),
        Err(StageError::AlreadyParented)
    ));
    assert!(matches!(
        stage.discard(root),
        Err(StageError::AlreadyParented)
    ));
    assert_eq!(roots(&stage), expected);
    assert_eq!(stage.revision(), revision);
    assert_eq!(stage.node(outer).unwrap().children, [child]);
    assert!(stage.node(root).unwrap().children.is_empty());
    stage.next_id = u64::MAX;
    assert!(matches!(
        stage.create(NodeKind::Stack, NodeProps::default(), vec![spare]),
        Err(StageError::IdentityExhausted)
    ));
    assert_eq!(roots(&stage), expected);
}

#[test]
fn restored_nodes_rebuild_sorted_component_roots_through_normal_adoption() {
    let mut stage = Stage::default();
    for id in [NodeId(3), NodeId(8), NodeId(20)] {
        stage
            .restore_node(id, NodeKind::Stack, NodeProps::default())
            .unwrap();
    }
    stage.add(NodeId(20), NodeId(8), 0).unwrap();
    assert_eq!(roots(&stage), [NodeId(3), NodeId(20)]);
    stage.replace(NodeId(20)).unwrap();
    assert_eq!(roots(&stage), [NodeId(3)]);
    assert!(matches!(
        stage.restore_node(NodeId(19), NodeKind::Stack, NodeProps::default()),
        Err(StageError::DuplicateChild)
    ));
    assert_eq!(roots(&stage), [NodeId(3)]);
    assert_eq!(stage.remove(NodeId(8)).unwrap(), [NodeId(8)]);
    assert_eq!(stage.clear(), [NodeId(20)]);
    assert_eq!(roots(&stage), [NodeId(3)]);
}

#[test]
fn large_component_adoption_preserves_root_capacity_and_sorted_survivors() {
    let mut stage = Stage::default();
    let children: Vec<_> = (0..10_000).map(|_| stack(&mut stage, vec![])).collect();
    let capacity = stage.detached_roots.capacity();
    eprintln!(
        "detached component index: roots=10000, capacity={capacity}, bytes={}",
        capacity * std::mem::size_of::<NodeId>()
    );
    let retained = [children[1], children[5000], children[9998]];
    let adopted = children
        .into_iter()
        .filter(|id| !retained.contains(id))
        .rev()
        .collect();
    let root = stack(&mut stage, adopted);
    assert_eq!(roots(&stage), [retained.as_slice(), &[root]].concat());
    assert_eq!(stage.detached_roots.capacity(), capacity);
    stage.replace(root).unwrap();
    assert_eq!(roots(&stage), retained);
    stage.clear();
    assert_eq!(roots(&stage), retained);
    assert_eq!(stage.detached_roots.capacity(), capacity);
}
