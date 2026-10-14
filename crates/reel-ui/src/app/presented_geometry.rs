// Copyright 2026 the Xilem Authors and Reel contributors
// SPDX-License-Identifier: Apache-2.0

//! Compact immutable geometry. Identity, acceptance and resource budgets belong to the host.
use super::{RenderRoot, VisualLayerPlan};
use crate::core::{WidgetArenaNode, WidgetId, WidgetState};
use kurbo::{Affine, Insets, Point, Rect, Size, Vec2};
use std::sync::Arc;

/// Input geometry and scope policy. Presented(None) blocks interaction.
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
    /// Maximum export allocation peak: snapshot capacities plus temporary child IDs.
    pub max_bytes: usize,
}
/// A refused export does not change the input view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeometryError {
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
    /// Visible modal subtrees isolate input in painter order.
    pub is_modal: bool,
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
            is_modal: s.is_modal,
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
    modal_scope: Option<WidgetId>,
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
    /// Peak explicit traversal stack and temporary child-ID heap storage during export.
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
    /// Highest visible modal subtree captured by this frame.
    pub fn modal_scope(&self) -> Option<WidgetId> {
        self.modal_scope
    }
    pub(crate) fn contains_in_scope(&self, id: WidgetId, scope: WidgetId) -> bool {
        let mut index = self.index(id);
        while let Some(i) = index {
            let record = &self.records[i];
            if record.id == scope {
                return true;
            }
            index = record.parent;
        }
        false
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
    /// Export after compose/record, charging actual registered storage and traversal workspaces.
    pub fn capture_presented_geometry(
        &self,
        limits: GeometryLimits,
    ) -> Result<PresentedGeometryView, GeometryError> {
        struct Visit<'a> {
            node: crate::tree_arena::ArenaRef<'a, WidgetArenaNode>,
            parent: Option<(usize, usize)>,
            depth: usize,
        }
        let registered = self.widget_arena.nodes.len();
        let snapshot_bytes =
            PresentedGeometryView::bytes_for_nodes(registered).ok_or(GeometryError::Bytes)?;
        let stack_bytes = registered
            .checked_mul(size_of::<Visit<'_>>())
            .ok_or(GeometryError::Bytes)?;
        if snapshot_bytes
            .checked_add(stack_bytes)
            .is_none_or(|n| n > limits.max_bytes)
        {
            return Err(GeometryError::Bytes);
        }
        let mut view = PresentedGeometryView {
            records: Vec::with_capacity(registered),
            children: Vec::with_capacity(registered - 1),
            ids: Vec::with_capacity(registered),
            scale_factor: self.global_state.scale_factor,
            root: 0,
            modal_scope: None,
            max_depth: 0,
            retention_guard: None,
            scratch_peak_bytes: stack_bytes,
        };
        let root = self
            .widget_arena
            .nodes
            .roots()
            .item(self.root_id())
            .expect("registered root");
        let mut pending = Vec::with_capacity(registered);
        pending.push(Visit {
            node: root,
            parent: None,
            depth: 1,
        });
        while let Some(Visit {
            node,
            parent,
            depth,
        }) = pending.pop()
        {
            let children = node.item.widget.children_ids();
            let child_bytes = if children.spilled() {
                children.capacity() * size_of::<WidgetId>()
            } else {
                0
            };
            let scratch_bytes = stack_bytes
                .checked_add(child_bytes)
                .ok_or(GeometryError::Bytes)?;
            if snapshot_bytes
                .checked_add(scratch_bytes)
                .is_none_or(|n| n > limits.max_bytes)
            {
                return Err(GeometryError::Bytes);
            }
            view.scratch_peak_bytes = view.scratch_peak_bytes.max(scratch_bytes);
            view.max_depth = view.max_depth.max(depth);
            let index = view.records.len();
            if let Some((_, slot)) = parent {
                view.children[slot] = index;
            }
            let start = view.children.len();
            let end = start + children.len();
            view.children.resize(end, 0);
            view.records.push(GeometryRecord::project(
                &node.item.state,
                parent.map(|(index, _)| index),
                start..end,
            ));
            view.ids.push((node.item.state.id, index));
            if node.item.state.is_modal && !node.item.state.is_stashed {
                view.modal_scope = Some(node.item.state.id);
            }
            for (slot, child) in (start..end).zip(children).rev() {
                pending.push(Visit {
                    node: node.children.item(child).expect("registered direct child"),
                    parent: Some((index, slot)),
                    depth: depth + 1,
                });
            }
        }
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
        self.accept_input_scope();
        self.run_rewrite_passes();
    }
    /// Reserve reusable pointer routes before presentation; existing active paths are preserved.
    pub fn configure_input_path_storage(&mut self, limit: usize) -> usize {
        self.widget_arena.nodes.configure_lookup_path_storage(limit);
        self.global_state.max_input_path = limit;
        self.global_state.pointer_route = Vec::with_capacity(limit);
        self.global_state.pointer_hover_spare = Vec::with_capacity(limit);
        self.global_state.pointer_active_spare = Vec::with_capacity(limit);
        // Growing routes between presentations must preserve hover and active capture.
        // A standalone caller may also request less storage than an existing path needs.
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
        let hit = match view {
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
        };
        hit.filter(|&id| self.input_in_scope(id, view))
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
