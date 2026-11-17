use reel_ui::stage::{NodeId, NodeVirtualList, VirtualRowMeasurement};
use std::{collections::BTreeMap, sync::Arc};

/// Rebuilt from feedback and restored VM state; node identities need no GC roots.
#[derive(Default)]
pub(in crate::aside) struct ScrollOwners {
    owners: BTreeMap<NodeId, Arc<[NodeId]>>,
    lists: BTreeMap<NodeId, NodeId>,
    layout: BTreeMap<NodeId, ScrollLayout>,
}

struct ScrollLayout {
    geometry: ScrollGeometry,
    measured: Option<MeasuredWindow>,
}

struct MeasuredWindow {
    revision: u64,
    epoch: i64,
    rows: Arc<[VirtualRowMeasurement]>,
    retained: Arc<[NodeId]>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_reuses_owner_rows_and_retirement_removes_both_directions() {
        let mut index = ScrollOwners::default();
        let a = ScrollGeometry {
            owner: NodeId(1),
            scale: 0.5,
            limit: 100.,
        };
        index.feedback(NodeId(10), a);
        index.feedback(NodeId(11), a);
        let rows = index.get(NodeId(1)).unwrap();
        for _ in 0..100 {
            index.feedback(NodeId(10), a);
        }
        assert!(Arc::ptr_eq(&rows, &index.get(NodeId(1)).unwrap()));
        assert_eq!(rows.as_ref(), &[NodeId(10), NodeId(11)]);
        index.feedback(
            NodeId(10),
            ScrollGeometry {
                owner: NodeId(2),
                ..a
            },
        );
        assert_eq!(index.get(NodeId(1)).unwrap().as_ref(), &[NodeId(11)]);
        index.retire(NodeId(11));
        assert!(index.get(NodeId(1)).is_none());
        index.retire(NodeId(2));
        assert!(index.get(NodeId(2)).is_none());
        assert!(index.geometry(NodeId(10)).is_none());
    }

    #[test]
    fn restored_ownership_waits_for_current_geometry() {
        let mut index = ScrollOwners::default();
        index.bind(NodeId(10), NodeId(1));
        assert_eq!(index.get(NodeId(1)).unwrap().as_ref(), &[NodeId(10)]);
        assert!(index.geometry(NodeId(10)).is_none());
    }

    #[test]
    fn applied_measurements_follow_the_layout_batch_and_its_owner() {
        let mut index = ScrollOwners::default();
        let mut event = NodeVirtualList {
            node: NodeId(10),
            scroll: NodeId(1),
            revision: 3,
            requested: Default::default(),
            offset: Default::default(),
            viewport: Default::default(),
            scroll_scale: 1.,
            scroll_limit: 100.,
            width: 320.,
            visible_start: 0.,
            visible_end: 100.,
            measurements: Arc::from([VirtualRowMeasurement {
                node: NodeId(11),
                height: 24.,
            }]),
            retained: Arc::from([]),
            navigation: None,
        };
        let geometry = ScrollGeometry {
            owner: event.scroll,
            scale: 1.,
            limit: 100.,
        };
        index.feedback(event.node, geometry);
        index.remember_measurements(&event, 4);
        event.visible_start = 20.;
        assert!(index.measurements_applied(&event, 4));
        assert!(!index.measurements_applied(&event, 5));
        event.revision += 1;
        assert!(!index.measurements_applied(&event, 4));
        index.remember_measurements(&event, 4);
        event.retained = Arc::from([NodeId(11)]);
        assert!(!index.measurements_applied(&event, 4));
        index.remember_measurements(&event, 4);
        index.retire(event.scroll);
        assert!(!index.measurements_applied(&event, 4));
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::aside) struct ScrollGeometry {
    pub(in crate::aside) owner: NodeId,
    pub(in crate::aside) scale: f64,
    pub(in crate::aside) limit: f64,
}
impl ScrollOwners {
    pub(in crate::aside) fn bind(&mut self, list: NodeId, owner: NodeId) {
        if self.lists.get(&list) == Some(&owner) {
            return;
        }
        self.remove_list(list);
        self.lists.insert(list, owner);
        let mut lists = self
            .owners
            .get(&owner)
            .map_or_else(Vec::new, |ids| ids.to_vec());
        let index = lists.binary_search(&list).unwrap_err();
        lists.insert(index, list);
        self.owners.insert(owner, lists.into());
    }
    pub(in crate::aside) fn get(&self, owner: NodeId) -> Option<Arc<[NodeId]>> {
        self.owners.get(&owner).cloned()
    }
    pub(in crate::aside) fn geometry(&self, list: NodeId) -> Option<ScrollGeometry> {
        self.layout.get(&list).map(|layout| layout.geometry)
    }
    pub(in crate::aside) fn feedback(&mut self, list: NodeId, geometry: ScrollGeometry) {
        self.bind(list, geometry.owner);
        self.layout
            .entry(list)
            .and_modify(|layout| layout.geometry = geometry)
            .or_insert(ScrollLayout {
                geometry,
                measured: None,
            });
    }
    pub(super) fn measurements_applied(&self, event: &NodeVirtualList, epoch: i64) -> bool {
        self.layout
            .get(&event.node)
            .and_then(|layout| layout.measured.as_ref())
            .is_some_and(|window| {
                window.revision == event.revision
                    && window.epoch == epoch
                    && Arc::ptr_eq(&window.rows, &event.measurements)
                    && Arc::ptr_eq(&window.retained, &event.retained)
            })
    }
    pub(super) fn remember_measurements(&mut self, event: &NodeVirtualList, epoch: i64) {
        self.layout.get_mut(&event.node).unwrap().measured = Some(MeasuredWindow {
            revision: event.revision,
            epoch,
            rows: event.measurements.clone(),
            retained: event.retained.clone(),
        });
    }
    fn remove_list(&mut self, list: NodeId) {
        self.layout.remove(&list);
        let Some(owner) = self.lists.remove(&list) else {
            return;
        };
        let Some(ids) = self.owners.get(&owner) else {
            return;
        };
        if ids.len() == 1 {
            self.owners.remove(&owner);
        } else {
            let mut ids = ids.to_vec();
            ids.remove(ids.binary_search(&list).unwrap());
            self.owners.insert(owner, ids.into());
        }
    }
    pub(in crate::aside) fn retire(&mut self, node: NodeId) {
        self.remove_list(node);
        if let Some(lists) = self.owners.remove(&node) {
            for list in lists.iter() {
                self.lists.remove(list);
                self.layout.remove(list);
            }
        }
    }
}
