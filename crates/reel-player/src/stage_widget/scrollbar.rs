//! Native input emits ordinary numeric proposals; visual slots use common child widgets.
use super::*;
impl SceneWidget {
    pub(super) fn scrollbar_proposal(&mut self, ctx: &mut EventCtx<'_>, value: Option<f64>) {
        let Some(value) = value else {
            return;
        };
        let state = self.scrollbar.as_ref().unwrap();
        let range = state.view().range();
        if let NodeKind::ScrollBar(view) = &mut Arc::make_mut(&mut self.node).kind {
            view.value = value;
        }
        self.sync_dirty = true;
        ctx.submit_action::<NodeEvent>(NodeEvent::Change(NodeChange {
            node: self.node.id,
            value: ControlValue::Number(value),
            range,
        }));
        ctx.request_compose();
        ctx.request_accessibility_update();
    }
    pub(super) fn layout_scrollbar(&mut self, ctx: &mut LayoutCtx<'_>, rect: Rect) {
        let state = self.scrollbar.as_mut().unwrap();
        state.layout_rect(rect);
        let thumb = state.thumb_rect();
        for (index, (node, child)) in self.children.iter_mut().enumerate() {
            ctx.set_stashed(child, !node.props.visible);
            if !node.props.visible {
                continue;
            }
            let size = if index == 1 {
                thumb.size()
            } else {
                rect.size()
            };
            ctx.run_layout(child, size);
            ctx.place_child(
                child,
                if index == 1 {
                    Point::ORIGIN
                } else {
                    rect.origin()
                },
            );
        }
    }
    pub(super) fn scrollbar_accessibility(&self, node: &mut Node) {
        let Some(state) = &self.scrollbar else {
            return;
        };
        let view = state.view();
        node.set_orientation(if view.axis == ScrollAxis::Horizontal {
            reel_ui::accesskit::Orientation::Horizontal
        } else {
            reel_ui::accesskit::Orientation::Vertical
        });
        node.set_numeric_value(view.value);
        node.set_min_numeric_value(0.);
        node.set_max_numeric_value(view.maximum());
        node.set_numeric_value_step(view.step);
        if view.range().is_some() {
            for action in [
                reel_ui::accesskit::Action::SetValue,
                reel_ui::accesskit::Action::Increment,
                reel_ui::accesskit::Action::Decrement,
            ] {
                node.add_action(action);
            }
        }
    }
}
