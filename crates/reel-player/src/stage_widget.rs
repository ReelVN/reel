//! Persistent widgets for the shared author stage. Property edits keep widget identities.
use reel_ui::{
    accesskit::{Node, Role},
    core::*,
    imaging::{Composite, GroupRef, Painter},
    kurbo::{Axis, Point, Rect, Size, Vec2},
    layout::{LenDef, LenReq, Length, SizeDef},
    peniko::{Color, ImageData},
    properties::{ContentColor, ThumbColor, ThumbRadius, TrackColor, TrackThickness},
    resources::{ResourceData, ResourceLease},
    stage::{
        ControlValue, InteractionState, NodeChange, NodeClick, NodeEvent, NodeId, NodeInteraction,
        NodeKind, Stage, StageNode,
    },
    widgets::{ButtonBehavior, Image, Label, ScrollAxis, ScrollState, Slider, SliderMoved},
};
use std::collections::BTreeMap;
mod input;
#[cfg(test)]
mod tests;
use input::NodeInput;

#[derive(Clone, Default)]
pub(crate) struct StageView {
    pub revision: u64,
    root: Option<SceneNode>,
    pub resources: Vec<ResourceLease>,
}
#[derive(Clone)]
struct SceneNode {
    node: StageNode,
    children: Vec<SceneNode>,
    image: Option<ImageData>,
}
impl StageView {
    pub fn capture(stage: &Stage) -> Self {
        fn capture_node(stage: &Stage, id: NodeId) -> SceneNode {
            let node = stage.node(id).expect("mounted stage node").clone();
            let children = node
                .children
                .iter()
                .map(|id| capture_node(stage, *id))
                .collect();
            SceneNode {
                node,
                children,
                image: None,
            }
        }
        Self {
            revision: stage.revision(),
            root: stage.root().map(|id| capture_node(stage, id)),
            resources: Vec::new(),
        }
    }
    pub fn has_modal(&self) -> bool {
        fn visit(node: &SceneNode) -> bool {
            node.node.props.visible && (node.node.props.modal || node.children.iter().any(visit))
        }
        self.root.as_ref().is_some_and(visit)
    }
    pub fn image_requests(&self) -> BTreeMap<String, Option<String>> {
        let mut requests = BTreeMap::new();
        let mut pending: Vec<_> = self.root.iter().collect();
        while let Some(node) = pending.pop() {
            if let NodeKind::Image { source, origin, .. } = &node.node.kind {
                requests
                    .entry(source.clone())
                    .or_insert_with(|| origin.clone());
            }
            pending.extend(node.children.iter());
        }
        requests
    }
    pub fn resolve_images(&mut self, images: &BTreeMap<String, ResourceLease>) {
        self.resources = self
            .image_requests()
            .keys()
            .map(|source| images[source].clone())
            .collect();
        let mut pending: Vec<_> = self.root.iter_mut().collect();
        while let Some(node) = pending.pop() {
            if let NodeKind::Image { source, .. } = &node.node.kind {
                let ResourceData::Image { image, .. } = images[source].data() else {
                    unreachable!()
                };
                node.image = Some(image.clone());
            }
            pending.extend(node.children.iter_mut());
        }
    }
}

pub(crate) struct StageWidget {
    root: Option<WidgetPod<SceneWidget>>,
    root_id: Option<NodeId>,
    root_input: bool,
    root_props: reel_ui::stage::NodeProps,
}
impl StageWidget {
    pub fn new(view: Option<&StageView>) -> Self {
        let root = view.and_then(|view| view.root.as_ref());
        Self {
            root: root.map(|node| WidgetPod::new(SceneWidget::new(node))),
            root_id: root.map(|node| node.node.id),
            root_input: root.is_some_and(|node| matches!(node.node.kind, NodeKind::Input { .. })),
            root_props: root.map(|node| node.node.props.clone()).unwrap_or_default(),
        }
    }
    pub fn input_fallback(&self) -> Option<WidgetId> {
        (self.root_input && self.root_props.visible)
            .then(|| self.root.as_ref().map(WidgetPod::id))
            .flatten()
    }
    pub fn sync(this: &mut WidgetMut<'_, Self>, view: &StageView) {
        let id = view.root.as_ref().map(|node| node.node.id);
        this.widget.root_input = view
            .root
            .as_ref()
            .is_some_and(|node| matches!(node.node.kind, NodeKind::Input { .. }));
        if id != this.widget.root_id {
            if let Some(root) = this.widget.root.take() {
                this.ctx.remove_child(root);
            }
            this.widget.root = view
                .root
                .as_ref()
                .map(|node| WidgetPod::new(SceneWidget::new(node)));
            this.widget.root_id = id;
            this.ctx.children_changed();
        } else if let (Some(pod), Some(node)) = (&mut this.widget.root, &view.root)
            && let Some(mut child) = this.ctx.get_mut(pod)
        {
            SceneWidget::sync(&mut child, node);
        }
        let props = view
            .root
            .as_ref()
            .map(|node| node.node.props.clone())
            .unwrap_or_default();
        if this.widget.root_props.visible != props.visible {
            this.ctx.request_layout();
        }
        this.widget.root_props = props;
        this.ctx.request_compose();
    }
}
impl Widget for StageWidget {
    type Action = NoAction;
    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
        if let Some(root) = &mut self.root {
            ctx.register_child(root);
        }
    }
    fn measure(
        &mut self,
        _: &mut MeasureCtx<'_>,
        _: &PropertiesRef<'_>,
        _: Axis,
        request: LenReq,
        _: Option<Length>,
    ) -> Length {
        match request {
            LenReq::FitContent(space) => space,
            _ => Length::ZERO,
        }
    }
    fn layout(&mut self, ctx: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, size: Size) {
        if let Some(root) = &mut self.root {
            ctx.set_stashed(root, !self.root_props.visible);
            if self.root_props.visible {
                let chosen = ctx.compute_size(root, SizeDef::fit(size), size.into());
                ctx.run_layout(root, chosen);
                ctx.place_child(root, Point::ORIGIN);
            }
        }
    }
    fn compose(&mut self, ctx: &mut ComposeCtx<'_>) {
        if let Some(root) = &mut self.root {
            let transform = self.root_props.transform(ctx.child_size(root));
            ctx.set_child_transform(root, transform);
        }
    }
    fn paint(&mut self, _: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, _: &mut Painter<'_>) {}
    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }
    fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, _: &mut Node) {}
    fn children_ids(&self) -> ChildrenIds {
        self.root.iter().map(WidgetPod::id).collect()
    }
    fn accepts_pointer_interaction(&self) -> bool {
        false
    }
}

struct SceneWidget {
    node: StageNode,
    button: ButtonBehavior,
    interaction: InteractionState,
    input: Option<NodeInput>,
    slider: Option<WidgetPod<Slider>>,
    label: Option<WidgetPod<Label>>,
    image: Option<WidgetPod<Image>>,
    image_size: Size,
    children: Vec<(StageNode, WidgetPod<SceneWidget>)>,
    size: Size,
    scroll: ScrollState,
    #[cfg(test)]
    layouts: usize,
}
fn color(value: [u8; 4]) -> Color {
    Color::from_rgba8(value[0], value[1], value[2], value[3])
}
fn font_style(font: &str) -> StyleProperty {
    StyleProperty::FontFamily(reel_ui::reel_text::FontFamily::Source(
        std::borrow::Cow::Owned(font.to_owned()),
    ))
}
impl SceneWidget {
    fn caption(kind: &NodeKind) -> Option<&str> {
        match kind {
            NodeKind::Text(text) => Some(text),
            NodeKind::Button { label, .. } | NodeKind::Toggle { label, .. } => label.as_deref(),
            _ => None,
        }
    }
    fn is_button(&self) -> bool {
        matches!(
            self.node.kind,
            NodeKind::Button { .. } | NodeKind::Toggle { .. }
        )
    }
    fn reveal_range(node: &StageNode) -> std::ops::Range<usize> {
        let text = Self::caption(&node.kind).unwrap_or("");
        let mut end = node.props.reveal.unwrap_or(text.len()).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        0..end
    }
    fn new(view: &SceneNode) -> Self {
        let label = match &view.node.kind {
            NodeKind::Text(_) | NodeKind::Button { .. } | NodeKind::Toggle { .. } => Some(
                NewWidget::new(
                    Label::new(Self::caption(&view.node.kind).unwrap_or("").to_owned())
                        .with_visible_bytes(Self::reveal_range(&view.node))
                        .with_style(StyleProperty::FontSize(view.node.props.font_size as f32))
                        .with_style(font_style(&view.node.props.font)),
                )
                .with_props(ContentColor::new(color(view.node.props.color)))
                .to_pod(),
            ),
            _ => None,
        };
        Self {
            node: view.node.clone(),
            button: ButtonBehavior::default(),
            interaction: InteractionState::default(),
            input: match &view.node.kind {
                NodeKind::Input { bindings, .. } => Some(NodeInput::new(bindings.clone())),
                _ => None,
            },
            label,
            slider: if let NodeKind::Slider { range, value, .. } = view.node.kind {
                let mut slider = Slider::new(range.min(), range.max(), value);
                if let Some(step) = range.step() {
                    slider = slider.with_step(step);
                }
                let tint = color(view.node.props.color);
                Some(
                    NewWidget::new(slider)
                        .with_props(ThumbRadius(Length::px(10.)))
                        .with_props(TrackThickness(Length::px(4.)))
                        .with_props(ThumbColor(tint))
                        .with_props(TrackColor {
                            active: tint,
                            inactive: tint.with_alpha(tint.components[3] * 0.3),
                        })
                        .to_pod(),
                )
            } else {
                None
            },
            image: view.image.as_ref().map(|image| {
                let NodeKind::Image { fit, .. } = view.node.kind else {
                    unreachable!()
                };
                NewWidget::new(Image::new(image.clone()))
                    .with_props(fit)
                    .to_pod()
            }),
            image_size: view
                .image
                .as_ref()
                .map(|image| Size::new(image.width.into(), image.height.into()))
                .unwrap_or(Size::ZERO),
            children: view
                .children
                .iter()
                .map(|child| (child.node.clone(), WidgetPod::new(Self::new(child))))
                .collect(),
            size: Size::ZERO,
            scroll: ScrollState::default(),
            #[cfg(test)]
            layouts: 0,
        }
    }
    fn sync(this: &mut WidgetMut<'_, Self>, view: &SceneNode) {
        if let (Some(pod), NodeKind::Slider { range, value, .. }) =
            (&mut this.widget.slider, &view.node.kind)
            && let Some(mut slider) = this.ctx.get_mut(pod)
        {
            Slider::configure(&mut slider, *range, *value);
            if this.widget.node.props.color != view.node.props.color {
                let tint = color(view.node.props.color);
                slider.insert_prop(ThumbColor(tint));
                slider.insert_prop(TrackColor {
                    active: tint,
                    inactive: tint.with_alpha(tint.components[3] * 0.3),
                });
            }
        }
        if let (Some(input), NodeKind::Input { bindings, .. }) =
            (&mut this.widget.input, &view.node.kind)
            && input.bindings != *bindings
        {
            input.cancel(this.widget.node.id, |event| {
                this.ctx.submit_action::<NodeEvent>(event)
            });
            *input = NodeInput::new(bindings.clone());
        }
        if this.widget.node.props.enabled != view.node.props.enabled {
            this.ctx.set_disabled(!view.node.props.enabled);
        }
        if this.widget.node.props.modal != view.node.props.modal {
            this.ctx.set_modal(view.node.props.modal);
        }
        if let (Some(pod), Some(data)) = (&mut this.widget.image, &view.image)
            && this.widget.node.kind != view.node.kind
            && let Some(mut image) = this.ctx.get_mut(pod)
        {
            Image::set_image_data(&mut image, data.clone());
            let NodeKind::Image { fit, .. } = view.node.kind else {
                unreachable!()
            };
            image.insert_prop(fit);
            this.widget.image_size = Size::new(data.width.into(), data.height.into());
        }
        let caption_changed =
            Self::caption(&this.widget.node.kind) != Self::caption(&view.node.kind);
        if caption_changed {
            this.ctx.request_layout();
        }
        if let Some(pod) = &mut this.widget.label
            && let Some(mut label) = this.ctx.get_mut(pod)
        {
            if caption_changed {
                Label::set_text(
                    &mut label,
                    Self::caption(&view.node.kind).unwrap_or("").to_owned(),
                );
            }
            if caption_changed || this.widget.node.props.reveal != view.node.props.reveal {
                Label::set_visible_bytes(&mut label, Self::reveal_range(&view.node));
            }
            if this.widget.node.props.font_size != view.node.props.font_size {
                Label::insert_style(
                    &mut label,
                    StyleProperty::FontSize(view.node.props.font_size as f32),
                );
            }
            if this.widget.node.props.font != view.node.props.font {
                Label::insert_style(&mut label, font_style(&view.node.props.font));
            }
            if this.widget.node.props.color != view.node.props.color {
                label.insert_prop(ContentColor::new(color(view.node.props.color)));
            }
        }
        if this.widget.node.children != view.node.children {
            let mut retained: BTreeMap<_, _> = std::mem::take(&mut this.widget.children)
                .into_iter()
                .map(|(node, pod)| (node.id, (node, pod)))
                .collect();
            this.widget.children = view
                .children
                .iter()
                .map(|next| {
                    retained
                        .remove(&next.node.id)
                        .unwrap_or_else(|| (next.node.clone(), WidgetPod::new(Self::new(next))))
                })
                .collect();
            for (_, (_, pod)) in retained {
                this.ctx.remove_child(pod);
            }
            this.ctx.children_changed();
        }
        for ((node, pod), next) in this.widget.children.iter_mut().zip(&view.children) {
            if node.props.visible != next.node.props.visible {
                this.ctx.request_layout();
            }
            if *node != next.node {
                *node = next.node.clone();
                this.ctx.request_compose();
            }
            if let Some(mut child) = this.ctx.get_mut(pod) {
                Self::sync(&mut child, next);
            }
        }
        if this.widget.node != view.node {
            let old = &this.widget.node.props;
            let new = &view.node.props;
            if old.width != new.width
                || old.height != new.height
                || old.gap != new.gap
                || old.padding != new.padding
                || old.font_size != new.font_size
                || old.font != new.font
                || old.clip != new.clip
                || old.decorated != new.decorated
                || matches!(view.node.kind, NodeKind::Scroll { .. })
                    && this.widget.node.kind != view.node.kind
            {
                this.ctx.request_layout();
            }
            this.widget.node = view.node.clone();
            this.ctx.request_render();
        }
    }
    fn interaction(
        &mut self,
        hover: bool,
        active: bool,
        focused: bool,
        disabled: bool,
        stashed: bool,
        window_focused: bool,
    ) -> Option<NodeEvent> {
        let available = !disabled && !stashed && window_focused;
        let next = InteractionState {
            hover: available && hover,
            pressed: available && (active || self.button.is_pressed()),
            focused: available && focused,
            disabled,
        };
        if next == self.interaction {
            return None;
        }
        self.interaction = next;
        Some(NodeEvent::Interaction(NodeInteraction {
            node: self.node.id,
            state: next,
        }))
    }
    fn activate(&mut self) -> NodeEvent {
        if let NodeKind::Toggle { value, .. } = &mut self.node.kind {
            *value = !*value;
            NodeEvent::Change(NodeChange {
                node: self.node.id,
                value: ControlValue::Bool(*value),
                range: None,
            })
        } else {
            NodeEvent::Click(NodeClick { node: self.node.id })
        }
    }
    fn sequential(&self, axis: Axis) -> bool {
        matches!(
            (&self.node.kind, axis),
            (NodeKind::Row, Axis::Horizontal)
                | (NodeKind::Column, Axis::Vertical)
                | (
                    NodeKind::Scroll {
                        axis: ScrollAxis::Horizontal,
                        ..
                    },
                    Axis::Horizontal
                )
                | (
                    NodeKind::Scroll {
                        axis: ScrollAxis::Vertical | ScrollAxis::Both,
                        ..
                    },
                    Axis::Vertical
                )
        )
    }
}
macro_rules! publish_interaction {
    ($widget:expr, $ctx:expr) => {
        if let Some(event) = $widget.interaction(
            $ctx.has_hovered(),
            $ctx.has_active(),
            $ctx.has_focus_target(),
            $ctx.is_disabled(),
            $ctx.is_stashed(),
            $ctx.is_window_focused(),
        ) {
            $ctx.submit_action::<NodeEvent>(event);
        }
    };
}
impl Widget for SceneWidget {
    type Action = NodeEvent;
    fn on_pointer_preview(
        &mut self,
        ctx: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        event: &PointerEvent,
    ) {
        if matches!(self.node.kind, NodeKind::Scroll { .. }) {
            self.scroll.preview(ctx, event);
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
                ctx.request_compose();
            }
            return;
        }
        if let Some(input) = &mut self.input
            && input.pointer(self.node.id, event, |e| ctx.submit_action::<NodeEvent>(e))
        {
            if matches!(event, PointerEvent::Down(_)) {
                ctx.request_focus();
                ctx.capture_pointer();
            }
            ctx.set_handled();
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
            if ButtonBehavior::pointer(ctx, event).is_some() {
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
            // Focused child controls own both keyboard and controller input.
            // Cleanup still reaches the previous input owner after focus changed.
            let cleanup = matches!(
                event,
                TextEvent::WindowFocusChange(false)
                    | TextEvent::Input(InputEvent::Cancel | InputEvent::Disconnected(_))
            );
            if ctx.focus_target_id().is_some() && !ctx.is_focus_target() && !cleanup {
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
        if self.slider.as_ref().is_some_and(|pod| pod.id() == source)
            && let Some(change) = action.downcast_ref::<SliderMoved>()
            && let NodeKind::Slider { range, value, .. } = &mut self.node.kind
        {
            *value = change.value;
            ctx.submit_action::<NodeEvent>(NodeEvent::Change(NodeChange {
                node: self.node.id,
                value: ControlValue::Number(change.value),
                range: Some(*range),
            }));
            ctx.set_handled();
        }
    }
    fn update(&mut self, ctx: &mut UpdateCtx<'_>, _: &mut PropertiesMut<'_>, event: &Update) {
        self.button.update(event);
        if matches!(
            event,
            Update::DisabledChanged(true) | Update::StashedChanged(true)
        ) {
            self.scroll.cancel();
        }
        if self.is_button() && matches!(event, Update::FocusChanged(_) | Update::DisabledChanged(_))
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
            ctx.set_modal(self.node.props.modal);
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
        if let Some(event) = self.interaction(
            ctx.has_hovered(),
            ctx.has_active(),
            focused,
            disabled,
            stashed,
            ctx.is_window_focused(),
        ) {
            ctx.submit_action::<NodeEvent>(event);
        }
    }
    fn register_children(&mut self, ctx: &mut RegisterCtx<'_>) {
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
        if let Some(fixed) = if axis == Axis::Horizontal {
            props.width
        } else {
            props.height
        } {
            return Length::px(fixed);
        }
        if matches!(self.node.kind, NodeKind::Scroll { .. })
            && let LenReq::FitContent(space) = request
        {
            return space;
        }
        let padding = Length::px(props.padding * 2.);
        if self.image.is_some() {
            let other = if axis == Axis::Horizontal {
                props.height
            } else {
                props.width
            };
            let native = self.image_size.get_coord(axis);
            let length = other.map_or(native, |other| {
                (other - padding.get()).max(0.) * (native / self.image_size.get_coord(axis.cross()))
            });
            return Length::px(length + padding.get());
        }
        let available = LenDef::from(request).reduce(padding);
        let cross = cross.map(|value| value.saturating_sub(padding));
        let context = ctx.context_size();
        let sequential = self.sequential(axis);
        let mut total: f64 = 0.;
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
        if self.node.props.decorated && matches!(self.node.kind, NodeKind::Toggle { .. }) {
            total = if axis == Axis::Horizontal {
                total + 32.
            } else {
                total.max(24.)
            };
        }
        let mut visible = 0;
        for (node, child) in &mut self.children {
            if !node.props.visible {
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
        Length::px(total + padding.get())
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
        if let Some(slider) = &mut self.slider {
            ctx.run_layout(slider, inner);
            ctx.place_child(slider, Point::new(padding, padding));
        }
        if let Some(label) = &mut self.label {
            let present = Self::caption(&self.node.kind).is_some();
            ctx.set_stashed(label, !present);
            if present {
                let inset = if self.node.props.decorated
                    && matches!(self.node.kind, NodeKind::Toggle { .. })
                {
                    32.
                } else {
                    0.
                };
                let label_area = Size::new((inner.width - inset).max(0.), inner.height);
                let chosen = if matches!(
                    self.node.kind,
                    NodeKind::Button { .. } | NodeKind::Toggle { .. }
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
        if let Some(image) = &mut self.image {
            ctx.run_layout(image, inner);
            ctx.place_child(image, Point::new(padding, padding));
        }
        let mut cursor = 0.;
        let mut extent = Size::ZERO;
        for (node, child) in &mut self.children {
            ctx.set_stashed(child, !node.props.visible);
            if !node.props.visible {
                continue;
            }
            let definition = if let NodeKind::Scroll { axis, .. } = self.node.kind {
                SizeDef::new(
                    if axis.horizontal() {
                        LenDef::MaxContent
                    } else {
                        LenDef::Fixed(Length::px(inner.width))
                    },
                    if axis.vertical() {
                        LenDef::MaxContent
                    } else {
                        LenDef::Fixed(Length::px(inner.height))
                    },
                )
            } else {
                SizeDef::fit(inner)
            };
            let chosen = ctx.compute_size(child, definition, inner.into());
            let offset = match self.node.kind {
                NodeKind::Row
                | NodeKind::Scroll {
                    axis: ScrollAxis::Horizontal,
                    ..
                } => {
                    let offset = Point::new(cursor, 0.);
                    cursor += chosen.width + self.node.props.gap;
                    offset
                }
                NodeKind::Column | NodeKind::Scroll { .. } => {
                    let offset = Point::new(0., cursor);
                    cursor += chosen.height + self.node.props.gap;
                    offset
                }
                _ => Point::ORIGIN,
            };
            extent.width = extent.width.max(offset.x + chosen.width);
            extent.height = extent.height.max(offset.y + chosen.height);
            ctx.run_layout(child, chosen);
            ctx.place_child(child, Point::new(padding + offset.x, padding + offset.y));
        }
        if let NodeKind::Scroll { axis, follow_end } = self.node.kind {
            self.scroll.layout(inner, extent, axis, follow_end);
        }
    }
    fn compose(&mut self, ctx: &mut ComposeCtx<'_>) {
        for (node, child) in &mut self.children {
            let transform = node.props.transform(ctx.child_size(child));
            ctx.set_child_transform(child, transform);
            ctx.set_child_scroll_translation(child, -self.scroll.offset());
        }
    }
    fn pre_paint(
        &mut self,
        _: &mut PaintCtx<'_>,
        _: &PropertiesRef<'_>,
        painter: &mut Painter<'_>,
    ) {
        if self.node.props.opacity != 1. {
            painter.push_group(GroupRef {
                composite: Composite {
                    alpha: self.node.props.opacity as f32,
                    ..Default::default()
                },
                ..Default::default()
            });
        }
    }
    fn paint(&mut self, ctx: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, painter: &mut Painter<'_>) {
        if let Some(background) = self.node.props.background {
            painter.fill_rect(self.size.to_rect(), color(background));
        }
        if self.node.props.decorated && self.is_button() && ctx.is_focus_target() {
            painter
                .stroke(
                    self.size.to_rect().inset(-2.).to_rounded_rect(3.),
                    &reel_ui::kurbo::Stroke::new(2.),
                    color(self.node.props.color),
                )
                .draw();
        }
        if self.node.props.decorated
            && let NodeKind::Toggle { value, .. } = self.node.kind
        {
            let p = self.node.props.padding;
            let rect = reel_ui::kurbo::Rect::new(
                p,
                self.size.height * 0.5 - 10.,
                p + 24.,
                self.size.height * 0.5 + 10.,
            );
            let tint = color(self.node.props.color);
            let tint =
                tint.with_alpha(tint.components[3] * if ctx.is_disabled() { 0.4 } else { 1. });
            painter
                .stroke(
                    rect.to_rounded_rect(4.),
                    &reel_ui::kurbo::Stroke::new(2.),
                    tint,
                )
                .draw();
            if value {
                painter
                    .fill(rect.inset(-4.).to_rounded_rect(2.), tint)
                    .draw();
            }
        }
    }
    fn post_paint(
        &mut self,
        _: &mut PaintCtx<'_>,
        _: &PropertiesRef<'_>,
        painter: &mut Painter<'_>,
    ) {
        if self.node.props.opacity != 1. {
            painter.pop_group();
        }
    }
    fn accessibility_role(&self) -> Role {
        if matches!(self.node.kind, NodeKind::Toggle { .. }) {
            Role::Switch
        } else if self.is_button() {
            Role::Button
        } else {
            Role::GenericContainer
        }
    }
    fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, node: &mut Node) {
        if self.is_button() {
            node.add_action(reel_ui::accesskit::Action::Click);
            if let Some(label) = Self::caption(&self.node.kind) {
                node.set_label(label);
            }
            if let NodeKind::Toggle { value, .. } = self.node.kind {
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
            .chain(self.children.iter().map(|(_, child)| child.id()))
            .collect()
    }
    fn accepts_pointer_interaction(&self) -> bool {
        self.is_button()
            || self.input.is_some()
            || matches!(self.node.kind, NodeKind::Scroll { .. })
    }
    fn accepts_focus(&self) -> bool {
        self.is_button() || self.input.is_some()
    }
    fn accepts_multiple_pointers(&self) -> bool {
        self.input.is_some()
    }
    fn propagates_pointer_interaction(&self) -> bool {
        !self.is_button()
    }
}
