//! Connect the shared layout algorithms to persistent stage widgets.
use super::*;
use reel_ui::layout::flow::{self, FlowChildren};

type Children = [(Arc<SceneProperties>, WidgetPod<SceneWidget>)];
struct MeasureChildren<'a, 'ctx> {
    children: &'a mut Children,
    ctx: &'a mut MeasureCtx<'ctx>,
}
impl FlowChildren for MeasureChildren<'_, '_> {
    fn len(&self) -> usize {
        self.children.len()
    }
    fn node(&self, index: usize) -> (&NodeKind, &NodeProps) {
        let node = &self.children[index].0;
        (&node.kind, &node.props)
    }
    fn measure(
        &mut self,
        index: usize,
        size: SizeDef,
        context: reel_ui::layout::LayoutSize,
    ) -> Size {
        self.ctx
            .compute_size(&mut self.children[index].1, size, context)
    }
    fn place(&mut self, _: usize, _: Size, _: Point) {
        unreachable!("measurement does not place children")
    }
}
struct LayoutChildren<'a, 'ctx> {
    children: &'a mut Children,
    ctx: &'a mut LayoutCtx<'ctx>,
}
impl FlowChildren for LayoutChildren<'_, '_> {
    fn len(&self) -> usize {
        self.children.len()
    }
    fn node(&self, index: usize) -> (&NodeKind, &NodeProps) {
        let node = &self.children[index].0;
        (&node.kind, &node.props)
    }
    fn measure(
        &mut self,
        index: usize,
        size: SizeDef,
        context: reel_ui::layout::LayoutSize,
    ) -> Size {
        self.ctx
            .compute_size(&mut self.children[index].1, size, context)
    }
    fn place(&mut self, index: usize, size: Size, origin: Point) {
        let child = &mut self.children[index].1;
        self.ctx.run_layout(child, size);
        self.ctx.place_child(child, origin);
    }
}
impl SceneWidget {
    pub(super) fn measure_flow(
        &mut self,
        ctx: &mut MeasureCtx<'_>,
        axis: Axis,
        request: LenReq,
        cross: Option<Length>,
    ) -> Length {
        flow::measure_flow(
            &self.node.kind,
            &self.node.props,
            &mut MeasureChildren {
                children: &mut self.children,
                ctx,
            },
            axis,
            request,
            cross,
        )
    }
    pub(super) fn layout_flow(&mut self, ctx: &mut LayoutCtx<'_>, size: Size) {
        let kind = &self.node.kind;
        let row = matches!(kind, NodeKind::Row(_));
        let flow_axis = if row {
            Axis::Horizontal
        } else {
            Axis::Vertical
        };
        let in_flow = |node: &SceneProperties| {
            node.props.visible
                && !matches!(node.kind, NodeKind::Cursor)
                && (matches!(kind, NodeKind::Grid(_)) || !node.props.stretch.axis(flow_axis))
        };
        for (node, pod) in &mut self.children {
            ctx.set_stashed(pod, !node.props.visible);
        }
        flow::layout_flow(
            kind,
            &self.node.props,
            &mut LayoutChildren {
                children: &mut self.children,
                ctx,
            },
            size,
        );
        let padding = self.node.props.padding;
        let inner = Size::new(
            (size.width - 2. * padding).max(0.),
            (size.height - 2. * padding).max(0.),
        );
        // Align within each line using already stored widget boxes; no positions array is retained.
        let cross_axis = flow_axis.cross();
        let wrapped = match kind {
            NodeKind::Row(v) | NodeKind::Column(v) => v.wrap,
            _ => true,
        };
        let line_axis = if matches!(kind, NodeKind::Grid(_)) {
            Axis::Vertical
        } else {
            cross_axis
        };
        let mut start = 0;
        while start < self.children.len() {
            if !in_flow(&self.children[start].0) {
                start += 1;
                continue;
            }
            let origin = ctx
                .child_origin(&self.children[start].1)
                .get_coord(line_axis);
            let mut end = start;
            let mut extent: f64 = 0.;
            while end < self.children.len() {
                let (node, child) = &self.children[end];
                if in_flow(node) {
                    if wrapped && ctx.child_origin(child).get_coord(line_axis) != origin {
                        break;
                    }
                    extent = extent.max(ctx.child_size(child).get_coord(line_axis));
                }
                end += 1;
            }
            if !wrapped {
                extent = inner.get_coord(line_axis);
            }
            if let NodeKind::Grid(grid) = kind {
                extent = grid.row_height.map_or(extent, f64::from);
            }
            for (node, child) in &mut self.children[start..end] {
                if !in_flow(node) {
                    continue;
                }
                let chosen = ctx.child_size(child);
                let mut offset = ctx.child_origin(child);
                if let NodeKind::Grid(grid) = kind {
                    let gap = grid.column_gap.map_or(self.node.props.gap, f64::from);
                    let width = grid.column_width.map_or(
                        (inner.width - gap * f64::from(grid.columns - 1)).max(0.)
                            / f64::from(grid.columns),
                        f64::from,
                    );
                    offset.x += (width - chosen.width).max(0.) * node.props.align_x;
                    offset.y += (extent - chosen.height).max(0.) * node.props.align_y;
                } else if row {
                    offset.y += (extent - chosen.height).max(0.) * node.props.align_y;
                } else {
                    offset.x += (extent - chosen.width).max(0.) * node.props.align_x;
                }
                ctx.place_child(child, offset);
            }
            start = end;
        }
        // Cursors and flow-axis stretch remain overlays and do not consume tracks or gaps.
        for (node, child) in &mut self.children {
            if !node.props.visible || in_flow(node) {
                continue;
            }
            if matches!(node.kind, NodeKind::Cursor) {
                let chosen = ctx.compute_size(child, SizeDef::MAX, ctx.window_size().into());
                ctx.run_layout(child, chosen);
                ctx.place_child(child, Point::ORIGIN);
                continue;
            }
            let dimension = |axis, maximum: Option<f64>| {
                let length = inner.get_coord(axis);
                if node.props.stretch.axis(axis) {
                    LenDef::Fixed(Length::px(length))
                } else {
                    LenDef::FitContent(Length::px(maximum.map_or(length, |max| max.min(length))))
                }
            };
            let chosen = ctx.compute_size(
                child,
                SizeDef::new(
                    dimension(Axis::Horizontal, node.props.max_width),
                    dimension(Axis::Vertical, node.props.max_height),
                ),
                inner.into(),
            );
            let offset = Point::new(
                padding + (inner.width - chosen.width).max(0.) * node.props.align_x,
                padding + (inner.height - chosen.height).max(0.) * node.props.align_y,
            );
            ctx.run_layout(child, chosen);
            ctx.place_child(child, offset);
        }
    }
}
