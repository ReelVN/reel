use super::*;

impl StageView {
    /// Missing textures may participate in layout only when both image dimensions
    /// are known. All missing resources must belong to virtual rows.
    pub(crate) fn can_layout_virtual_images(
        &self,
        images: &BTreeMap<String, ResourceLease>,
    ) -> bool {
        fn visit(
            node: &SceneNode,
            visible: bool,
            enabled: bool,
            has_parent: bool,
            in_list: bool,
            found: &mut bool,
            images: &BTreeMap<String, ResourceLease>,
        ) -> bool {
            let visible = node.image_visible(visible, enabled, has_parent);
            let enabled =
                enabled && node.node.props.enabled && !matches!(node.node.kind, NodeKind::Cursor);
            let in_list = in_list || matches!(node.node.kind, NodeKind::VirtualList(_));
            *found |= matches!(node.node.kind, NodeKind::VirtualList(_));
            if visible {
                if let Some((source, _)) = image_source(&node.node.kind)
                    && !images.contains_key(source)
                    && (!in_list
                        || !matches!(node.node.kind, NodeKind::Image { .. })
                        || node.node.props.width.is_none()
                        || node.node.props.height.is_none())
                {
                    return false;
                }
                if node.node.props.mask.as_ref().is_some_and(|mask| {
                    mask.sources()
                        .iter()
                        .any(|source| !images.contains_key(source.as_ref()))
                }) {
                    return false;
                }
            }
            node.children
                .iter()
                .all(|child| visit(child, visible, enabled, true, in_list, found, images))
        }
        let mut found = false;
        self.root
            .as_ref()
            .is_some_and(|root| visit(root, true, true, false, false, &mut found, images))
            && found
    }
}

impl StageWidget {
    /// Inspect composed live geometry before starting raster IO. Fixed-size
    /// images can participate in this layout without an image child or pixels.
    /// The physical tree includes popup placement, camera transforms and scroll clips.
    pub(crate) fn offscreen_images(
        root: &reel_ui::app::RenderRoot,
        root_id: WidgetId,
    ) -> Vec<NodeId> {
        fn visit(widget: WidgetRef<'_, dyn Widget>, viewport: Rect, outside: &mut Vec<NodeId>) {
            if widget.ctx().is_stashed() || widget.is_cursor() {
                return;
            }
            let viewport = crate::images::tiles::clipped_viewport(
                viewport,
                widget.ctx().window_transform(),
                widget.ctx().clip_path(),
            );
            if let Some(scene) = widget.downcast::<SceneWidget>() {
                // Virtual lists own overscan prefetch and partial presentation.
                // Effects may sample source content at a different position.
                if !scene.projection.image_branch
                    || scene.node.props.effect.is_some()
                    || matches!(scene.node.kind, NodeKind::VirtualList(_))
                {
                    return;
                }
                if scene.projection.has_fixed_image_size() {
                    let bounds = widget
                        .ctx()
                        .window_transform()
                        .transform_rect_bbox(widget.ctx().content_box());
                    // Keep one physical pixel for raster edge coverage.
                    let edge = 1. / widget.ctx().scale_factor();
                    let visible = bounds.inflate(edge, edge).intersect(viewport);
                    if bounds.is_finite() && (visible.width() <= 0. || visible.height() <= 0.) {
                        outside.push(scene.node.id);
                    }
                }
            }
            for child in widget.children() {
                visit(child, viewport, outside);
            }
        }
        let mut outside = Vec::new();
        let widget = root.get_widget(root_id).unwrap();
        let viewport = widget
            .ctx()
            .window_transform()
            .transform_rect_bbox(widget.ctx().content_box());
        visit(widget, viewport, &mut outside);
        outside.sort_unstable();
        outside
    }

    /// Inspect the one current layout tree. Ready consecutive rows must cover the
    /// viewport, and every row whose painted subtree reaches it must be ready.
    pub(crate) fn virtual_images_visible_ready(
        &self,
        root: &reel_ui::app::RenderRoot,
        candidate: &StageView,
        geometry: &reel_ui::app::PresentedGeometryView,
        images: &BTreeMap<String, ResourceLease>,
        format: EffectShaderFormat,
    ) -> bool {
        fn visit(
            stage: &StageWidget,
            root: &reel_ui::app::RenderRoot,
            node: &SceneNode,
            geometry: &reel_ui::app::PresentedGeometryView,
            images: &BTreeMap<String, ResourceLease>,
            format: EffectShaderFormat,
        ) -> bool {
            if !node.node.props.visible {
                return true;
            }
            if let NodeKind::VirtualList(view) = &node.node.kind {
                let widget = stage
                    .nodes
                    .get(&node.node.id)
                    .and_then(|id| root.get_widget(*id))
                    .and_then(|w| w.downcast::<SceneWidget>())
                    .unwrap();
                let state = widget.virtual_list.as_ref().unwrap();
                let Some(viewport) = &state.feedback else {
                    return false;
                };
                let list = stage
                    .nodes
                    .get(&viewport.node)
                    .and_then(|id| geometry.get(*id))
                    .unwrap();
                let bounds = list.window_transform.transform_rect_bbox(state.visible);
                let mut segment: Option<(usize, f64)> = None;
                let mut covered =
                    view.total_rows == 0 || viewport.visible_end <= viewport.visible_start;
                for (index, row) in view.rows.iter().enumerate() {
                    let child = &node.children[state.child_indices[index]];
                    let ready =
                        StageView::image_requests_for(Some(child), format).is_ok_and(|requests| {
                            requests
                                .required()
                                .all(|(source, _)| images.contains_key(source))
                        });
                    let actual = stage
                        .nodes
                        .get(&row.node)
                        .and_then(|id| geometry.get(*id))
                        .unwrap();
                    // Compose bounds include row transforms, overflowing descendants
                    // and their clips. This also checks new and pinned rows.
                    if !ready
                        && !actual.is_stashed
                        && actual.bounding_box.intersect(bounds).area() > 0.
                    {
                        return false;
                    }
                    let top = state.placement[index];
                    let height = state.measurements[index].height;
                    if !ready {
                        segment = None;
                        continue;
                    }
                    let start = match segment {
                        Some((last, start)) if last + 1 == row.index => start,
                        _ => {
                            if row.index == 0 {
                                f64::NEG_INFINITY
                            } else {
                                top
                            }
                        }
                    };
                    segment = Some((row.index, start));
                    covered |= start <= viewport.visible_start
                        && (row.index + 1 == view.total_rows
                            || top + height >= viewport.visible_end);
                }
                if !covered {
                    return false;
                }
            }
            node.physical_children()
                .all(|child| visit(stage, root, child, geometry, images, format))
        }
        candidate
            .root
            .as_ref()
            .is_some_and(|node| visit(self, root, node, geometry, images, format))
    }
}
