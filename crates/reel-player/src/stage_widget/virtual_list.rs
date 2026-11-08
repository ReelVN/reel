//! Window-local identity, measurement and placement shared by layout and feedback.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct VirtualScrollOwner {
    pub(super) node: NodeId,
    pub(super) requested: Vec2,
    pub(super) offset: Vec2,
    pub(super) viewport: Size,
    pub(super) limit: f64,
    /// Owner's positive vertical scroll axis expressed in this node's space.
    pub(super) axis: Vec2,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct VirtualViewport {
    pub(super) visible: Rect,
    paint_visible: Rect,
    pub(super) owner: Option<VirtualScrollOwner>,
}
impl VirtualViewport {
    pub(super) fn new(visible: Rect) -> Self {
        Self {
            visible,
            paint_visible: visible,
            owner: None,
        }
    }
    pub(super) fn clipped(mut self, clip: Rect) -> Self {
        self.visible = self.visible.intersect(clip);
        self.paint_visible = self.paint_visible.intersect(clip);
        if self.visible.width() <= 0. || self.visible.height() <= 0. {
            self.visible = Rect::from_origin_size(self.visible.origin(), Size::ZERO);
        }
        if self.paint_visible.width() <= 0. || self.paint_visible.height() <= 0. {
            self.paint_visible = Rect::from_origin_size(self.paint_visible.origin(), Size::ZERO);
        }
        self
    }
    pub(super) fn mapped(self, transform: reel_ui::kurbo::Affine) -> Self {
        self.mapped_scrolled(transform, Vec2::ZERO)
    }
    pub(super) fn mapped_scrolled(
        mut self,
        transform: reel_ui::kurbo::Affine,
        visual_offset: Vec2,
    ) -> Self {
        if transform.determinant() == 0. {
            self.visible = Rect::ZERO;
            self.paint_visible = Rect::ZERO;
            if let Some(owner) = &mut self.owner {
                owner.axis = Vec2::ZERO;
            }
            return self;
        }
        let inverse = transform.inverse();
        self.visible = inverse.transform_rect_bbox(self.visible);
        self.paint_visible = inverse.transform_rect_bbox(self.paint_visible - visual_offset);
        if let Some(owner) = &mut self.owner {
            let [a, b, c, d, _, _] = inverse.as_coeffs();
            owner.axis = Vec2::new(
                a * owner.axis.x + c * owner.axis.y,
                b * owner.axis.x + d * owner.axis.y,
            );
        }
        self
    }
}

impl SceneWidget {
    pub(super) fn set_virtual_viewport(&mut self, ctx: &mut RawCtx<'_>, viewport: VirtualViewport) {
        if self.virtual_viewport != Some(viewport) {
            self.virtual_viewport = Some(viewport);
            ctx.request_compose();
        }
    }
    pub(super) fn virtual_viewport(&self, ctx: &ComposeCtx<'_>) -> Option<VirtualViewport> {
        let mut viewport = self.virtual_viewport?;
        if let Some(clip) = ctx.clip_path() {
            viewport = viewport.clipped(clip);
        }
        if let NodeKind::Scroll { axis, .. } = self.node.kind {
            let padding = self.node.props.padding;
            viewport = viewport.clipped(Rect::from_origin_size(
                Point::new(padding, padding),
                self.scroll.viewport(),
            ));
            viewport.owner = Some(VirtualScrollOwner {
                node: self.node.id,
                requested: self.scroll_baseline,
                offset: self.scroll.offset(),
                viewport: self.scroll.viewport(),
                limit: (self.scroll.content().height - self.scroll.viewport().height).max(0.),
                axis: if axis.vertical() {
                    Vec2::new(0., 1.)
                } else {
                    Vec2::ZERO
                },
            });
        }
        Some(viewport)
    }
    pub(super) fn publish_virtual_viewport(&mut self, ctx: &mut ComposeCtx<'_>) {
        if ctx.is_stashed() {
            return;
        }
        let NodeKind::VirtualList(view) = &self.node.kind else {
            return;
        };
        let Some(viewport) = self.virtual_viewport(ctx) else {
            return;
        };
        let Some(owner) = viewport.owner else { return };
        let state = self.virtual_list.as_mut().unwrap();
        state.visible = viewport.paint_visible;
        state.retained.clear();
        for (row, pod) in &mut self.children {
            if let Some((_, row_ctx)) = ctx.get_raw(pod)
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
        let start = viewport.visible.y0 - self.node.props.padding;
        let overlap = viewport.visible.intersect(self.size.to_rect());
        let end = if overlap.width() > 0. && overlap.height() > 0. {
            viewport.visible.y1 - self.node.props.padding
        } else {
            start
        };
        state.viewport_start = start;
        let feedback = NodeVirtualList {
            node: self.node.id,
            scroll: owner.node,
            revision: view.revision,
            requested: owner.requested,
            offset: owner.offset,
            viewport: owner.viewport,
            scroll_scale: owner.axis.y,
            scroll_limit: owner.limit,
            width: (self.size.width - self.node.props.padding * 2.).max(0.),
            visible_start: start,
            visible_end: end,
            measurements: state.measurements.clone(),
            retained,
            navigation: state.navigation,
        };
        if state.feedback.as_ref() != Some(&feedback) {
            state.feedback = Some(feedback.clone());
            ctx.submit_action::<NodeEvent>(NodeEvent::VirtualList(feedback));
        }
    }
}

#[derive(Default)]
pub(super) struct VirtualListState {
    pub(super) measurements: Arc<[VirtualRowMeasurement]>,
    pub(super) feedback: Option<NodeVirtualList>,
    pub(super) retained: Vec<NodeId>,
    pub(super) children: Vec<VirtualChild>,
    pub(super) navigation: Option<VirtualListNavigation>,
    pub(super) placement_pending: bool,
    pub(super) viewport_start: f64,
    pub(super) visible: Rect,
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
