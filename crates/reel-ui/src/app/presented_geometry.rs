// Copyright 2026 the Xilem Authors and Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Compact immutable geometry. Identity, acceptance and resource budgets belong to the host.
use super::{RenderRoot, VisualLayerPlan};
use crate::core::{WidgetArenaNode, WidgetId, WidgetState};
use kurbo::{Affine, Insets, Point, Rect, Size, Vec2};
use std::sync::Arc;

/// Explicit pointer input policy. Presented(None) intentionally has no hits.
#[derive(Clone, Debug)]
pub enum InputGeometrySource {
    /// Ordinary Masonry geometry and widget hit callbacks.
    Live,
    /// Last accepted host frame, or no accepted frame yet.
    Presented(Option<Arc<PresentedGeometryView>>),
}
impl InputGeometrySource {
    pub(crate) fn borrow(&self) -> GeometryView<'_> {
        match self {
            Self::Live => GeometryView::Live,
            Self::Presented(v) => GeometryView::Presented(v.as_deref()),
        }
    }
}
/// Bounds applied before snapshot allocation/publication.
#[derive(Clone, Copy, Debug)]
pub struct GeometryLimits {
    /// Maximum registered nodes, including stashed nodes.
    pub max_nodes: usize,
    /// Maximum export allocation peak: snapshot capacities plus temporary child IDs.
    pub max_bytes: usize,
    /// Maximum root-to-leaf path length.
    pub max_path: usize,
}
/// A refused export does not change the input view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeometryError {
    /// Registered node capacity exhausted.
    Nodes,
    /// Snapshot bytes exhausted or arithmetic overflow.
    Bytes,
    /// Path capacity exhausted.
    Path,
}
/// All geometry for one registered node, in border-box coordinates unless named otherwise.
#[derive(Clone, Debug)]
pub struct GeometryRecord {
    /// Runtime widget identity.
    pub id: WidgetId,
    /// Parent record index, absent for root.
    pub parent: Option<usize>,
    /// Range in the frame's child index array, in painter order.
    pub children: std::ops::Range<usize>,
    /// Aligned border-box dimensions.
    pub size: Size,
    /// Border and padding insets.
    pub border_box_insets: Insets,
    /// Paint overflow insets.
    pub paint_box_insets: Insets,
    /// Widget's explicit local affine transform.
    pub transform: Affine,
    /// Border-box to window affine transform.
    pub window_transform: Affine,
    /// Window-space subtree bounds.
    pub bounding_box: Rect,
    /// Resolved first content baseline (no NaN default remains).
    pub first_baseline: f64,
    /// Resolved last content baseline.
    pub last_baseline: f64,
    /// Border-box clip.
    pub clip_path: Option<Rect>,
    /// Frozen default hit policy.
    pub accepts_pointer_interaction: bool,
    /// Frozen child traversal policy.
    pub propagates_pointer_interaction: bool,
    /// Visibility at record time. Current stashed state is checked separately.
    pub is_stashed: bool,
}
impl GeometryRecord {
    fn project(s: &WidgetState, parent: Option<usize>, children: std::ops::Range<usize>) -> Self {
        Self {
            id: s.id,
            parent,
            children,
            size: s.border_box().size(),
            border_box_insets: s.border_box_insets,
            paint_box_insets: s.paint_box_insets,
            transform: s.transform,
            window_transform: s.window_transform,
            bounding_box: s.bounding_box,
            first_baseline: s.aligned_first_baseline() - s.border_box_insets.y0,
            last_baseline: s.aligned_last_baseline() - s.border_box_insets.y0,
            clip_path: s.clip_path,
            accepts_pointer_interaction: s.accepts_pointer_interaction,
            propagates_pointer_interaction: s.propagates_pointer_interaction,
            is_stashed: s.is_stashed,
        }
    }
}
/// Complete immutable compact projection. No `WidgetState` or per-node path is retained.
#[derive(Debug)]
pub struct PresentedGeometryView {
    records: Vec<GeometryRecord>,
    children: Vec<usize>,
    ids: Vec<(WidgetId, usize)>,
    scale_factor: f64,
    root: usize,
    max_depth: usize,
    retention_guard: Option<Arc<dyn std::fmt::Debug + Send + Sync>>,
    scratch_peak_bytes: usize,
}
impl PresentedGeometryView {
    /// Attach host accounting to the view itself, so every Arc lease retains its charge.
    /// Masonry does not interpret this guard or depend on the host budget implementation.
    pub fn with_retention_guard(mut self, guard: Arc<dyn std::fmt::Debug + Send + Sync>) -> Self {
        if self.retention_guard.is_none() {
            self.retention_guard = Some(guard);
        }
        self
    }
    /// Registered node count.
    pub fn len(&self) -> usize {
        self.records.len()
    }
    /// Whether this frame has no records.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
    /// Retained allocation capacities plus view header (Arc allocation overhead is host-owned).
    pub fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            + self.records.capacity() * size_of::<GeometryRecord>()
            + self.children.capacity() * size_of::<usize>()
            + self.ids.capacity() * size_of::<(WidgetId, usize)>()
    }
    /// Peak temporary child-ID heap storage during export (native call stack is separate).
    pub fn scratch_peak_bytes(&self) -> usize {
        self.scratch_peak_bytes
    }
    /// DPI used by this record.
    pub fn scale_factor(&self) -> f64 {
        self.scale_factor
    }
    /// Root identity.
    pub fn root_id(&self) -> WidgetId {
        self.records[self.root].id
    }
    /// Maximum retained path depth.
    pub fn max_depth(&self) -> usize {
        self.max_depth
    }
    /// Complete contiguous records, including stashed nodes.
    pub fn records(&self) -> &[GeometryRecord] {
        &self.records
    }
    /// Resolve an identity without falling back to live geometry.
    pub fn get(&self, id: WidgetId) -> Option<&GeometryRecord> {
        self.index(id).map(|i| &self.records[i])
    }
    fn index(&self, id: WidgetId) -> Option<usize> {
        self.ids
            .binary_search_by_key(&id.to_raw(), |e| e.0.to_raw())
            .ok()
            .map(|i| self.ids[i].1)
    }
    pub(crate) fn child_ids(&self, index: usize) -> impl Iterator<Item = WidgetId> + '_ {
        self.children[self.records[index].children.clone()]
            .iter()
            .map(|i| self.records[*i].id)
    }
    /// Fill one caller-owned route in target-to-root order. Never allocate or partially route.
    pub fn path_into(
        &self,
        id: WidgetId,
        path: &mut Vec<WidgetId>,
        limit: usize,
    ) -> Result<(), GeometryError> {
        path.clear();
        let mut index = self.index(id);
        while let Some(i) = index {
            if path.len() >= limit || path.len() >= path.capacity() {
                path.clear();
                return Err(GeometryError::Path);
            }
            path.push(self.records[i].id);
            index = self.records[i].parent;
        }
        Ok(())
    }
    /// Rectangular/affine frozen hit policy. No current geometry is consulted.
    pub fn hit_test(&self, pos: Point) -> Option<WidgetId> {
        self.hit_test_filtered(pos, |_| true)
    }
    pub(crate) fn hit_test_filtered(
        &self,
        pos: Point,
        eligible: impl Fn(WidgetId) -> bool,
    ) -> Option<WidgetId> {
        fn hit(
            v: &PresentedGeometryView,
            i: usize,
            p: Point,
            eligible: &impl Fn(WidgetId) -> bool,
        ) -> Option<WidgetId> {
            let s = &v.records[i];
            let t = s.window_transform;
            if s.is_stashed
                || !s.bounding_box.contains(p)
                || !t.as_coeffs().iter().all(|v| v.is_finite())
                || !t.determinant().is_finite()
                || t.determinant() == 0.
            {
                return None;
            }
            let local = t.inverse() * p;
            if !local.x.is_finite()
                || !local.y.is_finite()
                || s.clip_path.is_some_and(|c| !c.contains(local))
            {
                return None;
            }
            // Frozen bounds/clip reject misses before any current-state query.
            if !eligible(s.id) {
                return None;
            }
            if s.propagates_pointer_interaction {
                for child in v.children[s.children.clone()].iter().rev() {
                    if let Some(id) = hit(v, *child, p, eligible) {
                        return Some(id);
                    }
                }
            }
            (s.accepts_pointer_interaction && s.size.to_rect().contains(local)).then_some(s.id)
        }
        hit(self, self.root, pos, &eligible)
    }
}
/// Fixed borrowed selection for a dispatch (never stored in `RenderRootState`).
#[derive(Clone, Copy)]
pub(crate) enum GeometryView<'a> {
    Live,
    Presented(Option<&'a PresentedGeometryView>),
}
/// A validated own-record binding. Missing records cannot construct a context.
#[derive(Clone, Copy)]
pub(crate) enum ContextGeometry<'a> {
    Live,
    Presented {
        view: &'a PresentedGeometryView,
        index: usize,
    },
}
impl<'a> GeometryView<'a> {
    pub(crate) fn bind(self, id: WidgetId) -> Option<ContextGeometry<'a>> {
        match self {
            Self::Live => Some(ContextGeometry::Live),
            Self::Presented(v) => {
                let v = v?;
                Some(ContextGeometry::Presented {
                    view: v,
                    index: v.index(id)?,
                })
            }
        }
    }
    /// Bind behavior only if the selected record and its ancestors are visible.
    /// Current eligibility is checked separately. Cleanup uses `bind` so hidden
    /// records can still supply the old geometry for terminal/exit callbacks.
    pub(crate) fn bind_visible(self, id: WidgetId) -> Option<ContextGeometry<'a>> {
        let geometry = self.bind(id)?;
        if let ContextGeometry::Presented { view, index } = geometry {
            let mut ancestor = Some(index);
            while let Some(index) = ancestor {
                let record = &view.records[index];
                if record.is_stashed {
                    return None;
                }
                ancestor = record.parent;
            }
        }
        Some(geometry)
    }
    pub(crate) fn scale(self, live: f64) -> f64 {
        match self {
            Self::Presented(Some(v)) => v.scale_factor,
            _ => live,
        }
    }
}
impl<'a> ContextGeometry<'a> {
    pub(crate) fn child(self, id: WidgetId) -> Option<Self> {
        match self {
            Self::Live => Some(Self::Live),
            Self::Presented { view, index } => {
                let child = view.index(id)?;
                view.children[view.records[index].children.clone()]
                    .contains(&child)
                    .then_some(Self::Presented { view, index: child })
            }
        }
    }
    pub(crate) fn resolve<'b>(self, live: &'b WidgetState) -> GeometryRef<'b>
    where
        'a: 'b,
    {
        match self {
            Self::Live => GeometryRef::Live(live),
            Self::Presented { view, index } => GeometryRef::Presented(&view.records[index]),
        }
    }
    pub(crate) fn scale(self, live: f64) -> f64 {
        match self {
            Self::Live => live,
            Self::Presented { view, .. } => view.scale_factor,
        }
    }
}
pub(crate) enum GeometryRef<'a> {
    Live(&'a WidgetState),
    Presented(&'a GeometryRecord),
}
macro_rules! field {
    ($name:ident,$ty:ty) => {
        pub(crate) fn $name(&self) -> $ty {
            match self {
                Self::Live(s) => s.$name,
                Self::Presented(s) => s.$name,
            }
        }
    };
}
impl GeometryRef<'_> {
    field!(accepts_pointer_interaction, bool);
    field!(propagates_pointer_interaction, bool);
    field!(transform, Affine);
    field!(window_transform, Affine);
    field!(bounding_box, Rect);
    field!(clip_path, Option<Rect>);
    field!(border_box_insets, Insets);
    pub(crate) fn border_box(&self) -> Rect {
        match self {
            Self::Live(s) => s.border_box(),
            Self::Presented(s) => s.size.to_rect(),
        }
    }
    pub(crate) fn content_box(&self) -> Rect {
        if let Self::Live(s) = self {
            return s.content_box();
        }
        let r = self.border_box();
        let i = self.border_box_insets();
        let x = r.x0 + i.x0;
        let y = r.y0 + i.y0;
        Rect::new(x, y, (r.x1 - i.x1).max(x), (r.y1 - i.y1).max(y))
    }
    pub(crate) fn paint_box(&self) -> Rect {
        self.border_box()
            + match self {
                Self::Live(s) => s.paint_box_insets,
                Self::Presented(s) => s.paint_box_insets,
            }
    }
    pub(crate) fn border_box_translation(&self) -> Vec2 {
        let i = self.border_box_insets();
        Vec2::new(i.x0, i.y0)
    }
    pub(crate) fn first_baseline(&self) -> f64 {
        match self {
            Self::Live(s) => s.aligned_first_baseline() - s.border_box_insets.y0,
            Self::Presented(s) => s.first_baseline,
        }
    }
    pub(crate) fn last_baseline(&self) -> f64 {
        match self {
            Self::Live(s) => s.aligned_last_baseline() - s.border_box_insets.y0,
            Self::Presented(s) => s.last_baseline,
        }
    }
}
impl RenderRoot {
    /// Export after compose/record. Limits are checked before allocation, including stashed nodes.
    pub fn capture_presented_geometry(
        &self,
        limits: GeometryLimits,
    ) -> Result<PresentedGeometryView, GeometryError> {
        // Reject an oversized registered tree before invoking children_ids(), which
        // may itself create temporary SmallVec heap storage for a wide container.
        let registered = self.widget_arena.nodes.len();
        if registered > limits.max_nodes {
            return Err(GeometryError::Nodes);
        }
        if PresentedGeometryView::bytes_for_nodes(registered)
            .is_none_or(|bytes| bytes > limits.max_bytes)
        {
            return Err(GeometryError::Bytes);
        }
        fn count(
            node: crate::tree_arena::ArenaRef<'_, WidgetArenaNode>,
            depth: usize,
            n: &mut usize,
            max: &mut usize,
            l: GeometryLimits,
            heap_live: usize,
            peak: &mut usize,
        ) -> Result<(), GeometryError> {
            if depth > l.max_path {
                return Err(GeometryError::Path);
            }
            *n += 1;
            *max = (*max).max(depth);
            if *n > l.max_nodes {
                return Err(GeometryError::Nodes);
            }
            if PresentedGeometryView::bytes_for_nodes(*n).is_none_or(|bytes| bytes > l.max_bytes) {
                return Err(GeometryError::Bytes);
            }
            let children = node.item.widget.children_ids();
            let heap = if children.spilled() {
                children.capacity() * size_of::<WidgetId>()
            } else {
                0
            };
            let live = heap_live.checked_add(heap).ok_or(GeometryError::Bytes)?;
            *peak = (*peak).max(live);
            if live > l.max_bytes {
                return Err(GeometryError::Bytes);
            }
            for child in children {
                count(
                    node.children.item(child).expect("registered direct child"),
                    depth + 1,
                    n,
                    max,
                    l,
                    live,
                    peak,
                )?;
            }
            Ok(())
        }
        let mut n = 0;
        let mut max = 0;
        let mut peak = 0;
        let root_node = self
            .widget_arena
            .nodes
            .roots()
            .item(self.root_id())
            .expect("registered root");
        // Traverse borrowed direct children, avoiding TreeArena::find's temporary
        // ancestor Vec at every node in the upstream safe-tree implementation.
        count(root_node, 1, &mut n, &mut max, limits, 0, &mut peak)?;
        if PresentedGeometryView::bytes_for_nodes(n)
            .and_then(|b| b.checked_add(peak))
            .is_none_or(|b| b > limits.max_bytes)
        {
            return Err(GeometryError::Bytes);
        }
        let mut view = PresentedGeometryView {
            records: Vec::with_capacity(n),
            children: Vec::with_capacity(n - 1),
            ids: Vec::with_capacity(n),
            scale_factor: self.global_state.scale_factor,
            root: 0,
            max_depth: max,
            retention_guard: None,
            scratch_peak_bytes: peak,
        };
        fn visit(
            node: crate::tree_arena::ArenaRef<'_, WidgetArenaNode>,
            parent: Option<usize>,
            v: &mut PresentedGeometryView,
        ) -> usize {
            let id = node.item.state.id;
            let children = node.item.widget.children_ids();
            let index = v.records.len();
            let start = v.children.len();
            let end = start + children.len();
            v.children.resize(end, 0);
            v.records.push(GeometryRecord::project(
                &node.item.state,
                parent,
                start..end,
            ));
            v.ids.push((id, index));
            for (slot, child) in (start..end).zip(children) {
                let i = visit(
                    node.children.item(child).expect("registered direct child"),
                    Some(index),
                    v,
                );
                v.children[slot] = i;
            }
            index
        }
        visit(root_node, None, &mut view);
        view.ids.sort_unstable_by_key(|e| e.0.to_raw());
        Ok(view)
    }
    /// Actual rewrite/layout/compose/paint and matching geometry in a single operation.
    pub fn record_presented_geometry(
        &mut self,
        limits: GeometryLimits,
    ) -> Result<(VisualLayerPlan, PresentedGeometryView), GeometryError> {
        let (plan, _accessibility) = self.redraw();
        let geometry = self.capture_presented_geometry(limits)?;
        Ok((plan, geometry))
    }
    /// Current explicit input source.
    pub fn input_geometry_source(&self) -> &InputGeometrySource {
        &self.global_state.input_geometry_source
    }
    /// Switch views with old-view exits and then new-view entries. Host controls acceptance.
    pub fn set_input_geometry_source(&mut self, source: InputGeometrySource) {
        let old = self.global_state.input_geometry_source.clone();
        crate::passes::update::transition_pointer_geometry(self, old.borrow(), source.borrow());
        self.global_state.input_geometry_source = source;
        self.run_rewrite_passes();
    }
    /// Configure bounded reusable pointer routes. Existing over-depth input is suppressed.
    pub fn configure_input_path_storage(&mut self, limit: usize) -> usize {
        self.widget_arena.nodes.configure_lookup_path_storage(limit);
        self.global_state.max_input_path = limit;
        self.global_state.pointer_route = Vec::with_capacity(limit);
        self.global_state.pointer_hover_spare = Vec::with_capacity(limit);
        self.global_state.pointer_active_spare = Vec::with_capacity(limit);
        // This setup is used after clearing pointer state at the bridge boundary.
        // Preserve any standalone caller's existing paths, with bounded requested capacity.
        for path in [
            &mut self.global_state.hovered_path,
            &mut self.global_state.active_path,
        ] {
            let mut replacement = Vec::with_capacity(limit.max(path.len()));
            replacement.extend_from_slice(path);
            *path = replacement;
        }
        self.input_path_storage_bytes()
    }
    /// Actual retained capacities of the five reusable pointer route arrays.
    pub fn input_path_storage_bytes(&self) -> usize {
        let s = &self.global_state;
        (s.pointer_route.capacity()
            + s.pointer_active_spare.capacity()
            + s.pointer_hover_spare.capacity()
            + s.active_path.capacity()
            + s.hovered_path.capacity())
            * size_of::<WidgetId>()
            + 5 * size_of::<Vec<WidgetId>>()
            + self.widget_arena.nodes.lookup_path_storage_bytes()
    }

    /// Planned five route arrays plus the independent arena lookup workspace.
    /// The host reserves this storage before configuring it.
    pub fn input_path_storage_bytes_for(limit: usize) -> Option<usize> {
        limit
            .checked_mul(5 * size_of::<WidgetId>())?
            .checked_add(5 * size_of::<Vec<WidgetId>>())?
            .checked_add(
                crate::tree_arena::TreeArena::<WidgetArenaNode>::lookup_path_storage_bytes_for(
                    limit,
                )?,
            )
    }
    pub(crate) fn pointer_hit_test(&self, pos: Point, view: GeometryView<'_>) -> Option<WidgetId> {
        match view {
            GeometryView::Presented(Some(v)) => v.hit_test_filtered(pos, |id| {
                self.widget_arena
                    .nodes
                    .find(id)
                    .is_some_and(|n| !n.item.state.is_stashed)
            }),
            GeometryView::Presented(None) => None,
            GeometryView::Live => self
                .get_widget(self.root_id())
                .and_then(|w| w.find_widget_under_pointer(pos))
                .map(|w| w.id()),
        }
    }
    pub(crate) fn input_path_into(
        &self,
        id: WidgetId,
        view: GeometryView<'_>,
        path: &mut Vec<WidgetId>,
    ) -> Result<(), GeometryError> {
        match view {
            GeometryView::Presented(Some(v)) => {
                v.path_into(id, path, self.global_state.max_input_path)
            }
            GeometryView::Presented(None) => {
                path.clear();
                Ok(())
            }
            GeometryView::Live => {
                path.clear();
                let mut current = Some(id);
                while let Some(i) = current {
                    if path.len() >= self.global_state.max_input_path
                        || path.len() >= path.capacity()
                    {
                        path.clear();
                        return Err(GeometryError::Path);
                    }
                    path.push(i);
                    current = self.widget_arena.parent_of(i);
                }
                Ok(())
            }
        }
    }
}
impl PresentedGeometryView {
    /// Exact requested vector storage plus header for n nodes.
    pub fn bytes_for_nodes(n: usize) -> Option<usize> {
        n.checked_mul(size_of::<GeometryRecord>() + size_of::<(WidgetId, usize)>())?
            .checked_add(n.saturating_sub(1).checked_mul(size_of::<usize>())?)?
            .checked_add(size_of::<Self>())
    }
}
