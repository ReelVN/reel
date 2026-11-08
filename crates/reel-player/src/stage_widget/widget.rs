//! Scene control input, layout, composition and painting.
use super::*;
use reel_ui::widgets::ScrollAlign;

impl Widget for SceneWidget {
    type Action = NodeEvent;
    fn capture_included(&self) -> bool {
        self.node.props.capture_included
    }
    fn virtual_children(&self) -> Option<VirtualChildren<'_>> {
        let NodeKind::VirtualList(list) = &self.node.kind else {
            return None;
        };
        Some(VirtualChildren {
            revision: list.revision,
            total: list.total_rows,
            children: &self.virtual_list.as_ref()?.children,
            target: list
                .navigation
                .map(|n| (n.request, n.resolved_index, n.forward)),
            target_ready: list.navigation.is_some_and(|n| n.ready),
        })
    }
    fn request_virtual_child(
        &mut self,
        ctx: &mut UpdateCtx<'_>,
        _: &mut PropertiesMut<'_>,
        target: Option<(u64, usize, bool)>,
    ) {
        let target = target.map(|(request, index, forward)| VirtualListNavigation {
            request,
            ready: false,
            index,
            resolved_index: index,
            forward,
        });
        let state = self
            .virtual_list
            .as_mut()
            .expect("virtual navigation owner");
        state.navigation = target;
        if let Some(feedback) = &mut state.feedback {
            feedback.navigation = target;
            ctx.submit_action::<NodeEvent>(NodeEvent::VirtualList(feedback.clone()));
        }
    }
    fn on_pointer_preview(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        if matches!(self.node.kind, NodeKind::Scroll { .. }) {
            let before = self.scroll.translation(ScrollAlign::Start);
            self.scroll.preview(ctx, event);
            if before != self.scroll.translation(ScrollAlign::Start) {
                ctx.request_compose();
            }
        }
    }
    fn scroll_to_child(
        &mut self,
        ctx: &mut UpdateCtx<'_>,
        _: &mut PropertiesMut<'_>,
        rect: Rect,
    ) -> Rect {
        if !matches!(self.node.kind, NodeKind::Scroll { .. }) {
            return rect;
        }
        let delta = self
            .scroll
            .reveal(rect - Vec2::splat(self.node.props.padding));
        if delta != Vec2::ZERO {
            self.scroll_input = true;
            ctx.request_compose();
        }
        rect + delta
    }

    fn on_pointer_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        if matches!(self.node.kind, NodeKind::Scroll { .. }) {
            if self.scroll.pointer(ctx, event) {
                self.scroll_input = true;
                ctx.request_compose();
            }
            return;
        }
        if let Some(input) = &mut self.input {
            if input.wheel(self.node.id, event, ctx) {
                return;
            }
            input.drag(self.node.id, event, ctx);
            if input.pointer(self.node.id, event, |e| ctx.submit_action::<NodeEvent>(e)) {
                if matches!(event, PointerEvent::Down(_)) {
                    input.pointer_focus(ctx);
                    ctx.capture_pointer();
                }
                ctx.set_handled();
            }
            return;
        }
        if self
            .slider
            .as_ref()
            .is_some_and(|slider| slider.id() == ctx.target())
            && matches!(
                event,
                PointerEvent::Down(_) | PointerEvent::Move(_) | PointerEvent::Up(_)
            )
        {
            ctx.set_handled();
        }
        if self.is_button() {
            let consumes = matches!(event, PointerEvent::Down(e) if e.button.is_none_or(|b| b == PointerButton::Primary))
                || matches!(event, PointerEvent::Up(_)) && ctx.is_active();
            let focus_on_pointer = self.accepts_focus()
                && !matches!(
                    self.node.kind,
                    NodeKind::Button {
                        focus_on_pointer: false,
                        ..
                    }
                );
            if ButtonBehavior::pointer(ctx, event, focus_on_pointer).is_some() {
                ctx.submit_action::<Self::Action>(self.activate());
                ctx.request_render();
            }
            if consumes {
                ctx.set_handled();
            }
        }
    }
    fn on_text_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &TextEvent,
    ) {
        if matches!(event, TextEvent::WindowFocusChange(false)) {
            self.scroll.cancel();
        }
        if let Some(input) = &mut self.input {
            use reel_ui::input::{Control, InputDeviceId, InputEvent};
            // Event propagation has already given child controls first refusal.
            // Editors retain even unhandled keys; ordinary controls may bubble
            // unrelated keys and mapped controls to their ancestor input nodes.
            if !ctx.is_text_input_focused()
                && let TextEvent::Keyboard(key) = event
                && input.key(self.node.id, key, |e| ctx.submit_action::<NodeEvent>(e))
            {
                ctx.set_handled();
            }
            // Cleanup still reaches the previous owner after focus changed.
            let cleanup = matches!(
                event,
                TextEvent::WindowFocusChange(false)
                    | TextEvent::Input(InputEvent::Cancel | InputEvent::Disconnected(_))
            );
            let editing_key = ctx.is_text_input_focused()
                && matches!(
                    event,
                    TextEvent::Keyboard(_)
                        | TextEvent::Input(InputEvent::Value {
                            device: InputDeviceId::Keyboard,
                            ..
                        })
                );
            if editing_key && !cleanup {
                return;
            }
            let raw = match event {
                TextEvent::Input(e) => Some(e.clone()),
                TextEvent::Keyboard(e) if e.repeat => {
                    if input.binds(Control::Key(e.code)) {
                        ctx.set_handled();
                    }
                    None
                }
                TextEvent::Keyboard(e) => Some(InputEvent::Value {
                    device: InputDeviceId::Keyboard,
                    control: Control::Key(e.code),
                    value: if e.state == keyboard::KeyState::Down {
                        1.
                    } else {
                        0.
                    },
                }),
                TextEvent::WindowFocusChange(false) => Some(InputEvent::Cancel),
                _ => None,
            };
            if let Some(raw) = raw
                && input.update(self.node.id, &raw, |e| ctx.submit_action::<NodeEvent>(e))
            {
                ctx.set_handled();
            }
        }
        if self.is_button() && self.button.text(ctx, event).is_some() {
            ctx.set_handled();
            ctx.submit_action::<Self::Action>(self.activate());
            ctx.request_render();
        }
        publish_interaction!(self, ctx);
    }
    fn on_access_event(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &AccessEvent,
    ) {
        if self.is_button() && ButtonBehavior::access(event).is_some() {
            ctx.set_handled();
            ctx.submit_action::<Self::Action>(self.activate());
            ctx.request_render();
        }
    }
    fn on_action(
        &mut self,
        ctx: &mut ActionCtx<'_>,
        _: &mut PropertiesMut<'_>,
        action: &ErasedAction,
        source: WidgetId,
    ) {
        if matches!(self.node.kind, NodeKind::Scroll { .. })
            && matches!(
                action.downcast_ref::<NodeEvent>(),
                Some(NodeEvent::Interaction(_))
            )
        {
            ctx.request_compose();
        }
        // Control proposals bubble through the same widget ancestors as their actions.
        // Reconcile optimistic values even when the authoritative projection is unchanged.
        if action.downcast_ref::<NavigationSelection>().is_some()
            || matches!(
                action.downcast_ref::<NodeEvent>(),
                Some(NodeEvent::Change(_) | NodeEvent::Text(_))
            )
        {
            self.sync_dirty = true;
        }
        if self.label.as_ref().is_some_and(|pod| pod.id() == source)
            && let Some(link) = action.downcast_ref::<reel_ui::widgets::LinkActivated>()
        {
            ctx.submit_action::<NodeEvent>(NodeEvent::Link(reel_ui::stage::NodeLink {
                node: self.node.id,
                id: link.id.clone(),
                text: link.text.clone(),
            }));
            ctx.set_handled();
        }
        if self.slider.as_ref().is_some_and(|pod| pod.id() == source)
            && let Some(change) = action.downcast_ref::<SliderMoved>()
            && let NodeKind::Slider { range, value, .. } = &mut Arc::make_mut(&mut self.node).kind
        {
            *value = change.value;
            let range = *range;
            self.sync_dirty = true;
            ctx.submit_action::<NodeEvent>(NodeEvent::Change(NodeChange {
                node: self.node.id,
                value: ControlValue::Number(change.value),
                range: Some(range),
            }));
            ctx.set_handled();
        }
        if self.editor.as_ref().is_some_and(|pod| pod.id() == source)
            && let Some(event) = action.downcast_ref::<TextInputEvent>()
        {
            if let TextInputEvent::Changed { value, .. } = event {
                self.sync_dirty = true;
                if let NodeKind::TextInput { value: current, .. } =
                    &mut Arc::make_mut(&mut self.node).kind
                {
                    *current = value.clone();
                }
            }
            ctx.submit_action::<NodeEvent>(NodeEvent::Text(NodeText {
                node: self.node.id,
                event: event.clone(),
            }));
            ctx.set_handled();
        }
    }
    fn update(&mut self, ctx: &mut UpdateCtx<'_>, _: &mut PropertiesMut<'_>, event: &Update) {
        self.button.update(event);
        if matches!(
            event,
            Update::DisabledChanged(true)
                | Update::StashedChanged(true)
                | Update::WindowFocusChanged(false)
        ) {
            let before = self.scroll.translation(ScrollAlign::Start);
            self.scroll.cancel();
            if before != self.scroll.translation(ScrollAlign::Start) {
                ctx.request_compose();
            }
        }
        if self.is_button()
            && matches!(
                event,
                Update::FocusChanged(_)
                    | Update::FocusVisibilityChanged(_)
                    | Update::WindowFocusChanged(_)
                    | Update::DisabledChanged(_)
            )
        {
            ctx.request_render();
        }
        if matches!(
            event,
            Update::FocusChanged(false)
                | Update::ChildFocusChanged(false)
                | Update::DisabledChanged(true)
                | Update::StashedChanged(true)
        ) && let Some(input) = &mut self.input
        {
            input.cancel(self.node.id, |e| ctx.submit_action::<NodeEvent>(e));
        }
        if matches!(event, Update::WidgetAdded) {
            ctx.set_disabled(!self.node.props.enabled);
            ctx.set_modal(self.node.props.modal && !matches!(self.node.kind, NodeKind::Popup(_)));
        }
        if matches!(event, Update::DescendantFocusChanged(_))
            && let Some(input) = &mut self.input
        {
            // Includes sibling-to-sibling moves with unchanged ancestor focus state.
            input.cancel(self.node.id, |e| ctx.submit_action::<NodeEvent>(e));
        }
        // These notifications run before the widget state's flag is committed.
        let disabled = match event {
            Update::DisabledChanged(value) => *value,
            _ => ctx.is_disabled(),
        };
        let stashed = match event {
            Update::StashedChanged(value) => *value,
            _ => ctx.is_stashed(),
        };
        let focused = match event {
            Update::ChildFocusChanged(value) => *value,
            _ => ctx.has_focus_target(),
        };
        if self.interaction.is_some()
            && let Some(event) = self.interaction(
                InteractionState {
                    hover: ctx.has_hovered(),
                    pressed: ctx.has_active(),
                    focused,
                    focus_visible: ctx.focus_visible(),
                    disabled,
                },
                !disabled && !stashed && ctx.is_window_focused(),
            )
        {
            ctx.submit_action::<NodeEvent>(event);
        }
    }
    fn on_anim_frame(
        &mut self,
        ctx: &mut UpdateCtx<'_>,
        _: &mut PropertiesMut<'_>,
        elapsed_ns: u64,
    ) {
        if self.scroll.advance(elapsed_ns) {
            ctx.request_compose();
        }
        if self.scroll.animating() {
            ctx.request_anim_frame();
        }
    }
    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
        if let Some(editor) = &mut self.editor {
            ctx.register_child(editor);
        }
        if let Some(slider) = &mut self.slider {
            ctx.register_child(slider);
        }
        if let Some(label) = &mut self.label {
            ctx.register_child(label);
        }
        if let Some(image) = &mut self.image {
            ctx.register_child(image);
        }
        for (_, child) in &mut self.children {
            ctx.register_child(child);
        }
    }
    fn measure(
        &mut self,
        ctx: &mut MeasureCtx<'_>,
        _: &PropertiesRef<'_>,
        axis: Axis,
        request: LenReq,
        cross: Option<Length>,
    ) -> Length {
        let props = &self.node.props;
        let maximum = if axis == Axis::Horizontal {
            props.max_width
        } else {
            props.max_height
        };
        let bounded = |value: f64| maximum.map_or(value, |limit| value.min(limit));
        let request = match (request, maximum) {
            (LenReq::FitContent(space), Some(limit)) => {
                LenReq::FitContent(Length::px(space.get().min(limit)))
            }
            (LenReq::MaxContent, Some(limit)) => LenReq::FitContent(Length::px(limit)),
            _ => request,
        };
        if let Some(fixed) = if axis == Axis::Horizontal {
            props.width
        } else {
            props.height
        } {
            return Length::px(bounded(fixed));
        }
        if matches!(self.node.kind, NodeKind::Scroll { .. })
            && let LenReq::FitContent(space) = request
        {
            return space;
        }
        if let NodeKind::VirtualList(list) = &self.node.kind {
            if axis == Axis::Vertical {
                return Length::px(list.total_height + props.padding * 2.);
            }
            if let LenReq::FitContent(space) = request {
                return space;
            }
        }
        let padding = Length::px(props.padding * 2.);
        if reel_ui::layout::flow::is_flow(&self.node.kind) {
            return self.measure_flow(ctx, axis, request, cross);
        }
        if self.image.is_some() {
            let other = if props.stretch.axis(axis.cross()) {
                cross.map(Length::get)
            } else if axis == Axis::Horizontal {
                props
                    .height
                    .map(|height| props.max_height.map_or(height, |max| height.min(max)))
            } else {
                props
                    .width
                    .map(|width| props.max_width.map_or(width, |max| width.min(max)))
            };
            let native = self.image_size.get_coord(axis);
            let length = other.map_or(native, |other| {
                (other - padding.get()).max(0.) * (native / self.image_size.get_coord(axis.cross()))
            });
            return Length::px(bounded(length + padding.get()));
        }
        let available = LenDef::from(request).reduce(padding);
        let block = if self.label.is_some() && props.writing_mode.is_vertical() {
            Axis::Horizontal
        } else {
            Axis::Vertical
        };
        let cross = if axis == block && !props.stretch.axis(axis.cross()) {
            let (fixed, maximum) = if axis == Axis::Vertical {
                (props.width, props.max_width)
            } else {
                (props.height, props.max_height)
            };
            fixed
                .map(|value| Length::px(maximum.map_or(value, |max| value.min(max))))
                .or(cross)
        } else {
            cross
        }
        .map(|value| value.saturating_sub(padding));
        let context = ctx.context_size();
        let sequential = self.sequential(axis);
        let mut total: f64 = 0.;
        if let Some(editor) = &mut self.editor {
            total = ctx
                .compute_length(editor, available, context, axis, cross)
                .get();
        }
        if let Some(slider) = &mut self.slider {
            total = ctx
                .compute_length(slider, available, context, axis, cross)
                .get();
        }
        if Self::caption(&self.node.kind).is_some()
            && let Some(label) = &mut self.label
        {
            total = ctx
                .compute_length(label, available, context, axis, cross)
                .get();
        }
        if self.node.props.decorated && self.selection_state().is_some() {
            total = if axis == Axis::Horizontal {
                total + 32.
            } else {
                total.max(24.)
            };
        }
        let mut visible = 0;
        for (node, child) in &mut self.children {
            if !node.props.visible
                || node.props.stretch.axis(axis)
                || matches!(node.kind, NodeKind::Cursor)
            {
                continue;
            }
            let length = ctx
                .compute_length(child, available, context, axis, cross)
                .get();
            if sequential {
                total += length;
            } else {
                total = total.max(length);
            }
            visible += 1;
        }
        if sequential && visible > 1 {
            total += self.node.props.gap * f64::from(visible - 1);
        }
        Length::px(bounded(total + padding.get()))
    }
    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, size: Size) {
        #[cfg(test)]
        {
            self.layouts += 1;
        }
        self.size = size;
        if self.node.props.clip || matches!(self.node.kind, NodeKind::Scroll { .. }) {
            ctx.set_clip_path(size.to_rect());
        } else {
            ctx.clear_clip_path();
        }
        let padding = self.node.props.padding;
        let inner = Size::new(
            (size.width - padding * 2.).max(0.),
            (size.height - padding * 2.).max(0.),
        );
        if let Some(editor) = &mut self.editor {
            ctx.run_layout(editor, inner);
            ctx.place_child(editor, Point::new(padding, padding));
        }
        if let Some(slider) = &mut self.slider {
            ctx.run_layout(slider, inner);
            ctx.place_child(slider, Point::new(padding, padding));
        }
        if let Some(label) = &mut self.label {
            let present = Self::caption(&self.node.kind).is_some();
            ctx.set_stashed(label, !present);
            if present {
                let inset = if self.node.props.decorated
                    && matches!(
                        self.node.kind,
                        NodeKind::Toggle { .. } | NodeKind::Radio { .. }
                    ) {
                    32.
                } else {
                    0.
                };
                let label_area = Size::new((inner.width - inset).max(0.), inner.height);
                let chosen = if matches!(
                    self.node.kind,
                    NodeKind::Button { .. } | NodeKind::Toggle { .. } | NodeKind::Radio { .. }
                ) {
                    ctx.compute_size(label, SizeDef::fit(label_area), label_area.into())
                } else {
                    inner
                };
                ctx.run_layout(label, chosen);
                ctx.place_child(
                    label,
                    Point::new(
                        padding + inset + (label_area.width - chosen.width) * 0.5,
                        padding + (inner.height - chosen.height) * 0.5,
                    ),
                );
            }
        }
        if let (NodeKind::Particles(batch), Some(image)) = (&self.node.kind, &self.particle_image) {
            let [width, height] = image.image_size().expect("particle image resource");
            let insets = batch.batches.iter().fold(Insets::ZERO, |insets, emission| {
                let frame = emission.frame;
                let config = &emission.config;
                let max_size = (config.size + config.size_spread)
                    .max(config.size_end.unwrap_or(0.))
                    * batch.size_scale;
                let lifetime = config.max_lifetime();
                let radius = max_size * (1. + f64::from(height) / f64::from(width)) * 0.5;
                let reach: [f64; 2] = std::array::from_fn(|axis| {
                    radius
                        + (config.velocity[axis].abs() + config.spread[axis]) * lifetime
                        + 0.5 * config.acceleration[axis].abs() * lifetime * lifetime
                });
                // Birth dimensions belong to emit's snapshot, not the current
                // layout. Resizing must keep older sprites inside paint bounds.
                Insets::new(
                    insets.x0.max(reach[0] - frame.origin[0]),
                    insets.y0.max(reach[1] - frame.origin[1]),
                    insets
                        .x1
                        .max((frame.origin[0] + frame.extent[0] + reach[0] - size.width).max(0.)),
                    insets
                        .y1
                        .max((frame.origin[1] + frame.extent[1] + reach[1] - size.height).max(0.)),
                )
            });
            ctx.set_paint_insets(insets);
        }
        if let Some(image) = &mut self.image {
            ctx.run_layout(image, inner);
            ctx.place_child(image, Point::new(padding, padding));
        }
        if reel_ui::layout::flow::is_flow(&self.node.kind) {
            self.layout_flow(ctx, size);
            return;
        }
        let mut cursor = 0.;
        let mut extent = Size::ZERO;
        let virtual_list = match &self.node.kind {
            NodeKind::VirtualList(list) => Some(list),
            _ => None,
        };
        let mut measurements = Vec::new();
        if let Some(state) = &mut self.virtual_list {
            state.children.clear();
            state.placement_pending = true;
            state.index_rows(virtual_list.expect("virtual list state"));
            measurements.extend(virtual_list.unwrap().rows.iter().map(|row| {
                VirtualRowMeasurement {
                    node: row.node,
                    height: 0.,
                }
            }));
        }
        for (child_index, (node, child)) in self.children.iter_mut().enumerate() {
            let row_index = self.virtual_list.as_ref().map(|state| {
                state
                    .row(node.id)
                    .expect("virtual projection contains every mounted child")
            });
            if let Some(index) = row_index {
                self.virtual_list.as_mut().unwrap().child_indices[index] = child_index;
            }
            ctx.set_stashed(child, !node.props.visible);
            if !node.props.visible {
                continue;
            }
            if matches!(node.kind, NodeKind::Cursor) {
                let chosen = ctx.compute_size(child, SizeDef::MAX, ctx.window_size().into());
                ctx.run_layout(child, chosen);
                ctx.place_child(child, Point::ORIGIN);
                continue;
            }
            let virtual_row = row_index.map(|index| &virtual_list.unwrap().rows[index]);
            let definition = if virtual_list.is_some() {
                SizeDef::new(LenDef::Fixed(Length::px(inner.width)), LenDef::MaxContent)
            } else if let NodeKind::Scroll { axis, .. } = self.node.kind {
                SizeDef::new(
                    if axis.horizontal() {
                        LenDef::MaxContent
                    } else {
                        LenDef::FitContent(Length::px(inner.width))
                    },
                    if axis.vertical() {
                        LenDef::MaxContent
                    } else {
                        LenDef::FitContent(Length::px(inner.height))
                    },
                )
            } else {
                SizeDef::fit(inner)
            };
            let capped = |axis, maximum: Option<f64>| match (definition.dim(axis), maximum) {
                (LenDef::Fixed(value), Some(limit)) => {
                    LenDef::Fixed(Length::px(value.get().min(limit)))
                }
                (LenDef::FitContent(value), Some(limit)) => {
                    LenDef::FitContent(Length::px(value.get().min(limit)))
                }
                (LenDef::MaxContent, Some(limit)) => LenDef::FitContent(Length::px(limit)),
                (value, _) => value,
            };
            let definition = SizeDef::new(
                if node.props.stretch.axis(Axis::Horizontal) {
                    LenDef::Fixed(Length::px(inner.width))
                } else {
                    capped(Axis::Horizontal, node.props.max_width)
                },
                if node.props.stretch.axis(Axis::Vertical) {
                    LenDef::Fixed(Length::px(inner.height))
                } else {
                    capped(Axis::Vertical, node.props.max_height)
                },
            );
            let chosen = ctx.compute_size(child, definition, inner.into());
            let mut offset = if let Some(row) = virtual_row {
                measurements[row_index.unwrap()].height = chosen.height;
                self.virtual_list
                    .as_mut()
                    .unwrap()
                    .children
                    .push(VirtualChild {
                        index: row.index,
                        widget: child.id(),
                    });
                Point::new(0., row.offset)
            } else {
                match self.node.kind {
                    NodeKind::Scroll {
                        axis: ScrollAxis::Horizontal,
                        ..
                    } if !node.props.stretch.axis(Axis::Horizontal) => {
                        let offset = Point::new(cursor, 0.);
                        cursor += chosen.width + self.node.props.gap;
                        offset
                    }
                    NodeKind::Scroll {
                        axis: ScrollAxis::Vertical | ScrollAxis::Both,
                        ..
                    } if !node.props.stretch.axis(Axis::Vertical) => {
                        let offset = Point::new(0., cursor);
                        cursor += chosen.height + self.node.props.gap;
                        offset
                    }
                    _ => Point::ORIGIN,
                }
            };
            let (flow_x, flow_y) = match self.node.kind {
                NodeKind::VirtualList(_) => (false, true),
                NodeKind::Scroll { axis, .. } => (axis.horizontal(), axis.vertical()),
                _ => (false, false),
            };
            if !flow_x {
                offset.x += (inner.width - chosen.width).max(0.) * node.props.align_x;
            }
            if !flow_y {
                offset.y += (inner.height - chosen.height).max(0.) * node.props.align_y;
            }
            if !node.props.stretch.axis(Axis::Horizontal) {
                extent.width = extent.width.max(offset.x + chosen.width);
            }
            if !node.props.stretch.axis(Axis::Vertical) {
                extent.height = extent.height.max(offset.y + chosen.height);
            }
            ctx.run_layout(child, chosen);
            // Virtual offsets remain subpixel until composition. Rounding a large
            // prefix before subtracting the corrected scroll offset creates a
            // visible 1px jump even when their mathematical difference is fixed.
            let layout_y = if virtual_row.is_some() { 0. } else { offset.y };
            ctx.place_child(child, Point::new(padding + offset.x, padding + layout_y));
        }
        if let Some(state) = &mut self.virtual_list {
            if state.measurements.as_ref() != measurements.as_slice() {
                state.measurements = measurements.into();
            }
            state.children.sort_unstable_by_key(|row| row.index);
        }
        if let NodeKind::Scroll {
            axis,
            overscroll,
            follow_end,
            ..
        } = self.node.kind
        {
            self.scroll.set_overscroll(overscroll);
            self.scroll.layout(inner, extent, axis, follow_end);
            self.scroll_laid_out = true;
        }
    }
    fn compose(&mut self, ctx: &mut ComposeCtx<'_>) {
        #[cfg(test)]
        {
            self.paint_work[0] += 1;
        }
        if self.interaction.is_none() {
            publish_interaction!(self, ctx);
        }
        // A restored hidden scroll has not been laid out. Keep its pending
        // position until showing it supplies the actual viewport and content.
        if matches!(self.node.kind, NodeKind::Scroll { .. })
            && (ctx.is_stashed() || !self.scroll_laid_out)
        {
            return;
        }
        if let Some(offset) = self.scroll_request.take() {
            self.scroll.set_offset(offset);
        }
        let translation = if let NodeKind::Scroll { align, .. } = self.node.kind {
            let metrics = (
                self.scroll.offset(),
                self.scroll.viewport(),
                self.scroll.content(),
            );
            if self.scroll_input
                || self.scroll_feedback != Some(metrics)
                || self.scroll_baseline != metrics.0
            {
                ctx.submit_action::<NodeEvent>(NodeEvent::Scroll(reel_ui::stage::NodeScroll {
                    input: std::mem::take(&mut self.scroll_input),
                    node: self.node.id,
                    requested: self.scroll_baseline,
                    offset: metrics.0,
                    viewport: metrics.1,
                    content: metrics.2,
                }));
                self.scroll_feedback = Some(metrics);
                self.scroll_baseline = metrics.0;
            }
            self.scroll.translation(align)
        } else {
            Vec2::ZERO
        };
        if matches!(self.node.kind, NodeKind::Scroll { .. }) {
            for (node, child) in &mut self.children {
                let NodeKind::VirtualList(view) = &node.kind else {
                    continue;
                };
                let Some((list, mut list_ctx)) = ctx.get_raw_mut(child) else {
                    continue;
                };
                let state = list.virtual_list.as_mut().expect("virtual list child");
                state.retained.clear();
                for (row, pod) in &mut list.children {
                    if let Some((_, row_ctx)) = list_ctx.get_raw(pod)
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
                let align = match self.node.kind {
                    NodeKind::Scroll { align, .. } => align,
                    _ => unreachable!(),
                };
                let start = -self.scroll.logical_translation(align).y - node.props.padding;
                state.viewport_start = start;
                let feedback = NodeVirtualList {
                    node: node.id,
                    scroll: self.node.id,
                    revision: view.revision,
                    requested: self.scroll_baseline,
                    offset: self.scroll.offset(),
                    viewport: self.scroll.viewport(),
                    width: (list.size.width - node.props.padding * 2.).max(0.),
                    visible_start: start,
                    visible_end: start + self.scroll.viewport().height,
                    measurements: state.measurements.clone(),
                    retained,
                    navigation: state.navigation,
                };
                if state.feedback.as_ref() != Some(&feedback) {
                    state.feedback = Some(feedback.clone());
                    list_ctx.submit_action::<NodeEvent>(NodeEvent::VirtualList(feedback));
                }
            }
        }
        if let NodeKind::VirtualList(view) = &self.node.kind {
            self.virtual_list
                .as_mut()
                .unwrap()
                .place_measured(view, self.node.props.gap);
        }
        let view = if let NodeKind::Camera(camera) = self.node.kind {
            camera.transform(Size::new(
                (self.size.width - self.node.props.padding * 2.).max(0.),
                (self.size.height - self.node.props.padding * 2.).max(0.),
            ))
        } else {
            reel_ui::kurbo::Affine::IDENTITY
        };
        for (node, child) in &mut self.children {
            let transform = view * node.props.transform(ctx.child_size(child));
            ctx.set_child_transform(child, transform);
            let row_offset = self
                .virtual_list
                .as_ref()
                .and_then(|state| state.row(node.id).map(|index| state.placement[index]))
                .unwrap_or(0.);
            // Keep scroll and row prefixes continuous until their transforms
            // combine. Rounding each layer can move a stationary reading anchor.
            ctx.set_animated_child_scroll_translation(
                child,
                translation + Vec2::new(0., row_offset),
            );
        }
    }
    fn pre_paint(&mut self, ctx: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, _: &mut Painter<'_>) {
        #[cfg(test)]
        {
            self.paint_work[1] += 1;
        }
        ctx.set_subtree_effect(
            self.node
                .props
                .effect
                .as_ref()
                .zip(self.effect.as_ref())
                .map(|(instance, resolved)| {
                    resolved.paint(
                        instance,
                        self.size.to_rect(),
                        false,
                        self.node.props.effect_resolution_scale,
                    )
                }),
        );
        ctx.set_subtree_mask_effect(
            self.node
                .props
                .mask_effect
                .as_ref()
                .zip(self.mask_effect.as_ref())
                .map(|(instance, resolved)| {
                    resolved.paint(
                        instance,
                        self.size.to_rect(),
                        self.node.props.mask_invert,
                        1.,
                    )
                }),
        );
        let mask = self
            .node
            .props
            .mask
            .as_ref()
            .filter(|_| self.node.props.mask_effect.is_none())
            .filter(|mask| !mask.is_image_threshold() || self.node.props.mask_progress < 1.);
        ctx.set_subtree_composite(
            self.node.props.opacity as f32,
            self.node.props.invert as f32,
            self.node.props.blend,
            mask.map(|program| PaintImageMask {
                program: program.clone(),
                images: self.mask_images.clone(),
                bounds: self.size.to_rect(),
                parameters: [
                    self.node.props.mask_progress as f32,
                    self.node.props.mask_softness.min(f32::MAX as f64) as f32,
                    self.node.props.mask_x as f32,
                    self.node.props.mask_y as f32,
                    f32::from(self.node.props.mask_invert),
                ],
            }),
        );
    }
    fn paint(&mut self, ctx: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, painter: &mut Painter<'_>) {
        #[cfg(test)]
        {
            self.paint_work[2] += 1;
        }
        let bounds = self.size.to_rect();
        let border = BorderWidth::all(Length::px(
            self.node
                .props
                .border_width
                .min(self.size.width.min(self.size.height) * 0.5),
        ));
        let radius = CornerRadius::all(Length::px(self.node.props.corner_radius));
        if let Some(background) = self.node.props.background {
            paint_background(
                painter,
                bounds,
                &Background::Color(color(background)),
                &radius,
            );
        }
        paint_border(
            painter,
            bounds,
            &BorderColor {
                color: color(self.node.props.border_color),
            },
            &border,
            &radius,
        );
        if self.node.props.decorated
            && self.is_button()
            && ctx.is_focus_target()
            && ctx.focus_visible()
            && ctx.is_window_focused()
        {
            painter
                .stroke(
                    self.size.to_rect().inset(-2.).to_rounded_rect(3.),
                    &reel_ui::kurbo::Stroke::new(2.),
                    color(self.node.props.color),
                )
                .draw();
        }
        if let (NodeKind::Particles(batch), Some(image)) = (&self.node.kind, &self.particle_image) {
            use reel_ui::{kurbo::Affine, peniko::ImageSampler};
            let [width, height] = image.image_size().expect("particle image resource");
            let instances = self.particle_sampler.record(batch);
            ctx.draw_resource_sprites(
                painter,
                image,
                Rect::new(0., 0., f64::from(width), f64::from(height)),
                Affine::IDENTITY,
                ImageSampler::default(),
                instances,
            );
        } else {
            self.particle_sampler = Default::default();
        }
        self.paint_selection(ctx, painter);
    }
    fn accessibility_role(&self) -> Role {
        match self.node.kind {
            NodeKind::Toggle {
                appearance: reel_ui::stage::ToggleAppearance::Checkbox,
                ..
            } => Role::CheckBox,
            NodeKind::Toggle { .. } => Role::Switch,
            NodeKind::Radio { .. } => Role::RadioButton,
            NodeKind::Button { .. } => Role::Button,
            _ => Role::GenericContainer,
        }
    }
    fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, node: &mut Node) {
        if self.is_button() {
            node.add_action(reel_ui::accesskit::Action::Click);
            if let Some(label) = Self::caption(&self.node.kind) {
                node.set_label(label);
            }
            if let Some(value) = self.selection_state() {
                node.set_toggled(if value {
                    reel_ui::accesskit::Toggled::True
                } else {
                    reel_ui::accesskit::Toggled::False
                });
            }
        }
    }
    fn children_ids(&self) -> ChildrenIds {
        self.label
            .iter()
            .map(WidgetPod::id)
            .chain(self.image.iter().map(WidgetPod::id))
            .chain(self.slider.iter().map(WidgetPod::id))
            .chain(self.editor.iter().map(WidgetPod::id))
            .chain(self.children.iter().map(|(_, child)| child.id()))
            .collect()
    }
    fn accepts_pointer_interaction(&self) -> bool {
        self.is_button()
            || self.input.is_some()
            || matches!(self.node.kind, NodeKind::Scroll { .. })
            || self
                .children
                .iter()
                .any(|(node, _)| matches!(node.kind, NodeKind::Cursor))
    }
    fn is_cursor(&self) -> bool {
        matches!(self.node.kind, NodeKind::Cursor)
    }
    fn accepts_focus(&self) -> bool {
        match self.node.kind {
            NodeKind::Button { focusable, .. } => focusable,
            _ => self.is_button() || self.input.as_ref().is_some_and(NodeInput::accepts_focus),
        }
    }
    fn selection_group(&self) -> Option<reel_ui::core::SelectionGroup> {
        match self.node.kind {
            NodeKind::Radio {
                group: Some(id),
                selected,
                ..
            } => Some(reel_ui::core::SelectionGroup { id, selected }),
            _ => None,
        }
    }
    fn select_group_member(&mut self, ctx: &mut UpdateCtx<'_>, _: &mut PropertiesMut<'_>) {
        let NodeEvent::Change(change) = self.activate() else {
            unreachable!("selection group member is a radio");
        };
        ctx.submit_untyped_action(Box::new(NavigationSelection(change)));
    }
    fn accepts_multiple_pointers(&self) -> bool {
        self.input.is_some()
    }
    fn propagates_pointer_interaction(&self) -> bool {
        !self.is_button()
    }
}
