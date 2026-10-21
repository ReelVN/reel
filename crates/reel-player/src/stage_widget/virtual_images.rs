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
            in_list: bool,
            found: &mut bool,
            images: &BTreeMap<String, ResourceLease>,
        ) -> bool {
            let visible = visible && node.node.props.visible;
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
                .all(|child| visit(child, visible, in_list, found, images))
        }
        let mut found = false;
        self.root
            .as_ref()
            .is_some_and(|root| visit(root, true, false, &mut found, images))
            && found
    }
}

impl StageWidget {
    /// Inspect the one current layout tree. Ready consecutive rows must cover the
    /// viewport, and every row whose painted subtree reaches it must be ready.
    pub(crate) fn virtual_images_visible_ready(
        &self,
        root: &reel_ui::app::RenderRoot,
        candidate: &StageView,
        geometry: &reel_ui::app::PresentedGeometryView,
        images: &BTreeMap<String, ResourceLease>,
    ) -> bool {
        fn visit(
            stage: &StageWidget,
            root: &reel_ui::app::RenderRoot,
            node: &SceneNode,
            geometry: &reel_ui::app::PresentedGeometryView,
            images: &BTreeMap<String, ResourceLease>,
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
                let scroll = stage
                    .nodes
                    .get(&viewport.scroll)
                    .and_then(|id| geometry.get(*id))
                    .unwrap();
                let bounds = scroll
                    .window_transform
                    .transform_rect_bbox(scroll.size.to_rect());
                let mut segment: Option<(usize, f64)> = None;
                let mut covered = view.total_rows == 0;
                for row in view.rows.iter() {
                    let child = node
                        .children
                        .iter()
                        .find(|child| child.node.id == row.node)
                        .unwrap();
                    let ready = StageView::image_requests_for(Some(child)).is_ok_and(|requests| {
                        requests
                            .required
                            .keys()
                            .all(|source| images.contains_key(source))
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
                    let top = state
                        .placement
                        .iter()
                        .find(|(id, _)| *id == row.node)
                        .map(|(_, y)| *y)
                        .unwrap();
                    let height = state
                        .measurements
                        .iter()
                        .find(|r| r.node == row.node)
                        .map(|r| r.height)
                        .unwrap();
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
            node.children
                .iter()
                .all(|child| visit(stage, root, child, geometry, images))
        }
        candidate
            .root
            .as_ref()
            .is_some_and(|node| visit(self, root, node, geometry, images))
    }
}
