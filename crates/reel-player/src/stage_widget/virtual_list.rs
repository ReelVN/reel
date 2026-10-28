//! Window-local identity, measurement and placement shared by layout and feedback.
use super::*;

#[derive(Default)]
pub(super) struct VirtualListState {
    pub(super) measurements: Arc<[VirtualRowMeasurement]>,
    pub(super) feedback: Option<NodeVirtualList>,
    pub(super) retained: Vec<NodeId>,
    pub(super) children: Vec<VirtualChild>,
    pub(super) navigation: Option<VirtualListNavigation>,
    pub(super) placement_pending: bool,
    pub(super) viewport_start: f64,
    pub(super) placement: Vec<f64>,
    row_index: Vec<(NodeId, usize)>,
    pub(super) child_indices: Vec<usize>,
}
impl VirtualListState {
    pub(super) fn index_rows(&mut self, view: &reel_ui::stage::VirtualListView) {
        self.row_index.clear();
        self.row_index
            .extend(view.rows.iter().enumerate().map(|(i, row)| (row.node, i)));
        self.row_index.sort_unstable_by_key(|&(node, _)| node);
        self.child_indices.resize(view.rows.len(), 0);
    }
    pub(super) fn row(&self, node: NodeId) -> Option<usize> {
        self.row_index
            .binary_search_by_key(&node, |&(id, _)| id)
            .ok()
            .map(|i| self.row_index[i].1)
    }
    // Keep one projected row fixed while packing its contiguous logical segment
    // with this layout's real measurements. This also runs for a distant jump:
    // the first painted window never exposes estimated gaps/overlapping rows.
    pub(super) fn place_measured(&mut self, view: &reel_ui::stage::VirtualListView, gap: f64) {
        if !self.placement_pending {
            return;
        }
        self.placement_pending = false;
        self.placement.clear();
        self.placement
            .extend(view.rows.iter().map(|row| row.offset));
        let height = |i: usize| self.measurements[i].height;
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
                self.placement[i] = self.placement[i - 1] + height(i - 1) + gap;
            }
            for i in (start..anchor).rev() {
                self.placement[i] = self.placement[i + 1] - height(i) - gap;
            }
            start = end;
        }
    }
}
