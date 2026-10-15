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
        NodeKind, NodeText, Stage, StageNode,
    },
    widgets::{
        ButtonBehavior, Image, Label, ScrollAxis, ScrollState, Slider, SliderMoved, TextInput,
        TextInputEvent,
    },
};
use std::collections::BTreeMap;
mod input;
mod selection;
#[cfg(test)]
mod tests;
use input::NodeInput;

#[derive(Clone, Default)]
pub(crate) struct StageView {
    pub revision: u64,
    pub button_revision: u64,
    pub presentation_request: u64,
    pub feedback: bool,
    root: Option<SceneNode>,
    pub resources: Vec<ResourceLease>,
}
#[derive(Clone)]
struct SceneNode {
    node: StageNode,
    children: Vec<SceneNode>,
    image: Option<ImageData>,
    tiled_size: Option<Size>,
    tiles: Vec<reel_ui::widgets::ImageTile>,
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
                tiled_size: None,
                tiles: Vec::new(),
            }
        }
        Self {
            revision: stage.revision(),
            button_revision: 0,
            presentation_request: 0,
            feedback: false,
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
    #[cfg(feature = "window")]
    pub fn has_root(&self) -> bool {
        self.root.is_some()
    }
    /// Compare button content only. Animated geometry uses accepted hit testing;
    /// changes outside a control cannot invalidate its activation.
    pub fn button_changes(&self, previous: Option<&Self>, feedback: bool) -> Vec<(NodeId, bool)> {
        fn buttons(view: &StageView) -> BTreeMap<NodeId, &SceneNode> {
            let mut found = BTreeMap::new();
            let mut pending: Vec<_> = view.root.iter().collect();
            while let Some(node) = pending.pop() {
                if matches!(node.node.kind, NodeKind::Button { .. }) {
                    found.insert(node.node.id, node);
                }
                pending.extend(node.children.iter());
            }
            found
        }
        fn same_content(a: &SceneNode, b: &SceneNode) -> bool {
            let mut pending = vec![(a, b)];
            while let Some((a, b)) = pending.pop() {
                if a.node.id != b.node.id
                    || a.node.kind != b.node.kind
                    || a.node.props.visible != b.node.props.visible
                    || a.node.props.enabled != b.node.props.enabled
                    || a.children.len() != b.children.len()
                {
                    return false;
                }
                pending.extend(a.children.iter().zip(&b.children));
            }
            true
        }
        let previous = previous.map(buttons).unwrap_or_default();
        buttons(self)
            .into_iter()
            .map(|(id, node)| {
                let changed = previous
                    .get(&id)
                    .is_none_or(|old| !feedback && !same_content(old, node));
                (id, changed)
            })
            .collect()
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
    pub fn resolve_metadata(&mut self, images: &crate::images::Images) {
        self.resources.clear();
        let mut resources = std::collections::BTreeSet::new();
        let mut pending: Vec<_> = self.root.iter_mut().collect();
        while let Some(node) = pending.pop() {
            if let NodeKind::Image { source, .. } = &node.node.kind {
                if let Some(manifest) = images.manifests.get(source) {
                    node.image = None;
                    node.tiled_size =
                        Some(Size::new(manifest.width.into(), manifest.height.into()));
                    node.tiles.clear();
                } else if let Some(lease) = images.loaded.get(source) {
                    let ResourceData::Image { image, .. } = lease.data() else {
                        unreachable!()
                    };
                    node.image = Some(image.clone());
                    node.tiled_size = None;
                    if resources.insert(source.clone()) {
                        self.resources.push(lease.clone());
                    }
                }
            }
            pending.extend(node.children.iter_mut());
        }
    }
    pub fn resolve_tiles(
        &mut self,
        images: &crate::images::Images,
        regions: &BTreeMap<NodeId, crate::images::ImageRegion>,
    ) {
        let mut pending: Vec<_> = self.root.iter_mut().collect();
        let mut resources = std::collections::BTreeSet::new();
        while let Some(node) = pending.pop() {
            if let Some(region) = regions.get(&node.node.id) {
                node.tiles = images.tile_paint(region);
                resources.extend(region.tiles.iter().cloned());
            }
            pending.extend(node.children.iter_mut());
        }
        let mut handles: std::collections::BTreeSet<_> =
            self.resources.iter().map(ResourceLease::handle).collect();
        self.resources.extend(
            resources
                .iter()
                .filter_map(|key| images.loaded.get(key))
                .filter(|lease| handles.insert(lease.handle()))
                .cloned(),
        );
    }
}

pub(crate) struct StageWidget {
    root: Option<WidgetPod<SceneWidget>>,
    root_id: Option<NodeId>,
    root_input: bool,
    root_props: reel_ui::stage::NodeProps,
}
impl StageWidget {
    pub(crate) fn image_widgets(
        this: &mut WidgetMut<'_, Self>,
    ) -> Vec<(NodeId, crate::images::tiles::ImageGeometry)> {
        fn visit(
            this: &mut WidgetMut<'_, SceneWidget>,
            out: &mut Vec<(NodeId, crate::images::tiles::ImageGeometry)>,
        ) {
            if let (Some(image), NodeKind::Image { source, fit, .. }) =
                (&this.widget.image, &this.widget.node.kind)
            {
                out.push((
                    this.widget.node.id,
                    crate::images::tiles::ImageGeometry {
                        source: source.clone(),
                        widget: image.id(),
                        fit: *fit,
                    },
                ));
            }
            for (_, pod) in &mut this.widget.children {
                if let Some(mut child) = this.ctx.get_mut(pod) {
                    visit(&mut child, out);
                }
            }
        }
        let mut out = Vec::new();
        if let Some(root) = &mut this.widget.root
            && let Some(mut root) = this.ctx.get_mut(root)
        {
            visit(&mut root, &mut out);
        }
        out
    }
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
            SceneWidget::sync(&mut child, node, view.feedback);
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
    editor: Option<WidgetPod<TextInput>>,
    image: Option<WidgetPod<Image>>,
    image_size: Size,
    children: Vec<(StageNode, WidgetPod<SceneWidget>)>,
    size: Size,
    scroll: ScrollState,
    scroll_request: Option<Vec2>,
    scroll_feedback: Option<(Vec2, Size, Size)>,
    scroll_baseline: Vec2,
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
fn text_outline(props: &reel_ui::stage::NodeProps) -> Option<reel_ui::core::TextOutline> {
    (props.text_outline > 0.).then(|| reel_ui::core::TextOutline {
        width: props.text_outline,
        color: color(props.text_outline_color),
    })
}
impl SceneWidget {
    fn caption(kind: &NodeKind) -> Option<&str> {
        match kind {
            NodeKind::Text(text) => Some(text),
            NodeKind::Button { label, .. }
            | NodeKind::Toggle { label, .. }
            | NodeKind::Radio { label, .. } => label.as_deref(),
            _ => None,
        }
    }
    fn is_button(&self) -> bool {
        matches!(
            self.node.kind,
            NodeKind::Button { .. } | NodeKind::Toggle { .. } | NodeKind::Radio { .. }
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
            NodeKind::Text(_)
            | NodeKind::Button { .. }
            | NodeKind::Toggle { .. }
            | NodeKind::Radio { .. } => Some(
                NewWidget::new(
                    Label::new("")
                        .with_rich_text(match &view.node.kind {
                            NodeKind::Text(text) => text.clone(),
                            kind => Self::caption(kind).unwrap_or("").into(),
                        })
                        .with_visible_bytes(Self::reveal_range(&view.node))
                        .with_style(StyleProperty::FontSize(view.node.props.font_size as f32))
                        .with_text_alignment(view.node.props.text_align)
                        .with_text_fit(view.node.props.text_fit)
                        .with_outline(text_outline(&view.node.props))
                        .with_style(StyleProperty::LineHeight(
                            reel_ui::reel_text::LineHeight::FontSizeRelative(
                                view.node.props.line_height as f32,
                            ),
                        ))
                        .with_style(font_style(&view.node.props.font)),
                )
                .with_props(if matches!(view.node.kind, NodeKind::Text(_)) {
                    reel_ui::properties::LineBreaking::WordWrap
                } else {
                    reel_ui::properties::LineBreaking::Overflow
                })
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
                NodeKind::Input {
                    bindings,
                    keyboard,
                    pointer,
                    ..
                } => Some(NodeInput::new(bindings.clone(), *keyboard, *pointer)),
                _ => None,
            },
            label,
            editor: if let NodeKind::TextInput { value } = &view.node.kind {
                Some(
                    NewWidget::new(
                        TextInput::new(value.clone())
                            .with_style(StyleProperty::FontSize(view.node.props.font_size as f32))
                            .with_style(font_style(&view.node.props.font)),
                    )
                    .with_props(ContentColor::new(color(view.node.props.color)))
                    .to_pod(),
                )
            } else {
                None
            },
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
            image: view
                .image
                .as_ref()
                .map(|image| Image::new(image.clone()))
                .or_else(|| {
                    view.tiled_size
                        .map(|size| Image::tiled(size, view.tiles.clone()))
                })
                .map(|image| {
                    let NodeKind::Image { fit, .. } = view.node.kind else {
                        unreachable!()
                    };
                    NewWidget::new(image).with_props(fit).to_pod()
                }),
            image_size: view
                .image
                .as_ref()
                .map(|image| Size::new(image.width.into(), image.height.into()))
                .or(view.tiled_size)
                .unwrap_or(Size::ZERO),
            children: view
                .children
                .iter()
                .map(|child| (child.node.clone(), WidgetPod::new(Self::new(child))))
                .collect(),
            size: Size::ZERO,
            scroll: ScrollState::default(),
            scroll_request: match view.node.kind {
                NodeKind::Scroll { offset, .. } if offset != Vec2::ZERO => Some(offset),
                _ => None,
            },
            scroll_feedback: None,
            scroll_baseline: match view.node.kind {
                NodeKind::Scroll { offset, .. } => offset,
                _ => Vec2::ZERO,
            },
            #[cfg(test)]
            layouts: 0,
        }
    }
    fn sync(this: &mut WidgetMut<'_, Self>, view: &SceneNode, feedback: bool) -> bool {
        let mut content_changed = this.widget.node.kind != view.node.kind
            || this.widget.node.children != view.node.children
            || this.widget.node.props.visible != view.node.props.visible
            || this.widget.node.props.enabled != view.node.props.enabled;
        if let (Some(pod), NodeKind::TextInput { value }) =
            (&mut this.widget.editor, &view.node.kind)
            && let Some(mut editor) = this.ctx.get_mut(pod)
        {
            TextInput::set_value(&mut editor, value.clone());
            if this.widget.node.props.font_size != view.node.props.font_size {
                TextInput::insert_style(
                    &mut editor,
                    StyleProperty::FontSize(view.node.props.font_size as f32),
                );
            }
            if this.widget.node.props.font != view.node.props.font {
                TextInput::insert_style(&mut editor, font_style(&view.node.props.font));
            }
            if this.widget.node.props.color != view.node.props.color {
                editor.insert_prop(ContentColor::new(color(view.node.props.color)));
            }
        }
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
        if let (
            Some(input),
            NodeKind::Input {
                bindings,
                keyboard,
                pointer,
                ..
            },
        ) = (&mut this.widget.input, &view.node.kind)
            && (input.bindings != *bindings
                || input.keyboard != *keyboard
                || input.pointer_enabled != *pointer)
        {
            input.cancel(this.widget.node.id, |event| {
                this.ctx.submit_action::<NodeEvent>(event)
            });
            *input = NodeInput::new(bindings.clone(), *keyboard, *pointer);
        }
        if this.widget.node.props.enabled != view.node.props.enabled {
            this.ctx.set_disabled(!view.node.props.enabled);
        }
        if this.widget.node.props.modal != view.node.props.modal {
            this.ctx.set_modal(view.node.props.modal);
        }
        if let (Some(pod), Some(data)) = (&mut this.widget.image, &view.image)
            && (this.widget.node.kind != view.node.kind
                || this.widget.image_size != Size::new(data.width.into(), data.height.into()))
            && let Some(mut image) = this.ctx.get_mut(pod)
        {
            Image::set_image_data(&mut image, data.clone());
            let NodeKind::Image { fit, .. } = view.node.kind else {
                unreachable!()
            };
            image.insert_prop(fit);
            this.widget.image_size = Size::new(data.width.into(), data.height.into());
        }
        if let (Some(pod), Some(size)) = (&mut this.widget.image, view.tiled_size)
            && let Some(mut image) = this.ctx.get_mut(pod)
        {
            Image::set_tiles(&mut image, size, view.tiles.clone());
            let NodeKind::Image { fit, .. } = view.node.kind else {
                unreachable!()
            };
            image.insert_prop(fit);
            this.widget.image_size = size;
        }
        let caption_changed = match (&this.widget.node.kind, &view.node.kind) {
            (NodeKind::Text(a), NodeKind::Text(b)) => a != b,
            _ => Self::caption(&this.widget.node.kind) != Self::caption(&view.node.kind),
        };
        if caption_changed {
            this.ctx.request_layout();
        }
        if let Some(pod) = &mut this.widget.label
            && let Some(mut label) = this.ctx.get_mut(pod)
        {
            if caption_changed {
                Label::set_rich_text(
                    &mut label,
                    match &view.node.kind {
                        NodeKind::Text(text) => text.clone(),
                        kind => Self::caption(kind).unwrap_or("").into(),
                    },
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
            if this.widget.node.props.text_align != view.node.props.text_align {
                Label::set_text_alignment(&mut label, view.node.props.text_align);
            }
            Label::set_text_fit(&mut label, view.node.props.text_fit);
            Label::set_outline(&mut label, text_outline(&view.node.props));
            if this.widget.node.props.line_height != view.node.props.line_height {
                Label::insert_style(
                    &mut label,
                    StyleProperty::LineHeight(reel_ui::reel_text::LineHeight::FontSizeRelative(
                        view.node.props.line_height as f32,
                    )),
                );
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
                content_changed |= Self::sync(&mut child, next, feedback);
            }
        }
        if let (
            NodeKind::Scroll {
                offset: previous, ..
            },
            NodeKind::Scroll { offset, .. },
        ) = (&this.widget.node.kind, &view.node.kind)
            && previous != offset
        {
            this.widget.scroll_baseline = *offset;
            this.widget.scroll_request =
                (*offset != this.widget.scroll.offset()).then_some(*offset);
            this.ctx.request_compose();
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
                || old.text_fit != new.text_fit
                || old.clip != new.clip
                || old.decorated != new.decorated
                || match (&this.widget.node.kind, &view.node.kind) {
                    (
                        NodeKind::Scroll {
                            axis: a,
                            follow_end: b,
                            align: c,
                            ..
                        },
                        NodeKind::Scroll {
                            axis: d,
                            follow_end: e,
                            align: f,
                            ..
                        },
                    ) => (a, b, c) != (d, e, f),
                    _ => false,
                }
            {
                this.ctx.request_layout();
            }
            if matches!(view.node.kind, NodeKind::Camera(_)) {
                this.ctx.request_compose();
            }
            this.widget.node = view.node.clone();
            this.ctx.request_render();
        }
        if content_changed && !feedback && this.widget.is_button() {
            this.widget.button.cancel();
            this.ctx.invalidate_input_configuration();
        }
        content_changed
    }
    fn interaction(&mut self, mut next: InteractionState, available: bool) -> Option<NodeEvent> {
        next.hover &= available;
        next.pressed = available && (next.pressed || self.button.is_pressed());
        next.focused &= available;
        next.focus_visible &= next.focused;
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
        if let NodeKind::Radio { revision, .. } = self.node.kind {
            NodeEvent::Change(NodeChange {
                node: self.node.id,
                value: ControlValue::Selection(revision),
                range: None,
            })
        } else if let NodeKind::Toggle { value, .. } = &mut self.node.kind {
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
            InteractionState {
                hover: $ctx.has_hovered(),
                pressed: $ctx.has_active(),
                focused: $ctx.has_focus_target(),
                focus_visible: $ctx.focus_visible(),
                disabled: $ctx.is_disabled(),
            },
            !$ctx.is_disabled() && !$ctx.is_stashed() && $ctx.is_window_focused(),
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
        if let Some(input) = &mut self.input {
            input.drag(self.node.id, event, ctx);
            if input.pointer(self.node.id, event, |e| ctx.submit_action::<NodeEvent>(e)) {
                if matches!(event, PointerEvent::Down(_)) {
                    ctx.request_focus();
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
            // Event propagation has already given child controls first refusal.
            // Editors retain even unhandled keys; ordinary controls may bubble
            // unrelated keys to this author callback. Action maps still require focus.
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
        if self.editor.as_ref().is_some_and(|pod| pod.id() == source)
            && let Some(event) = action.downcast_ref::<TextInputEvent>()
        {
            if let TextInputEvent::Changed { value, .. } = event {
                self.node.kind = NodeKind::TextInput {
                    value: value.clone(),
                };
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
            Update::DisabledChanged(true) | Update::StashedChanged(true)
        ) {
            self.scroll.cancel();
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
            ctx.set_modal(self.node.props.modal);
        }
        if matches!(event, Update::DescendantFocusChanged(_))
            && let Some(input) = &mut self.input
        {
            // Includes sibling-to-sibling moves with unchanged ancestor focus state.
            input.cancel_keys(self.node.id, |e| ctx.submit_action::<NodeEvent>(e));
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
            InteractionState {
                hover: ctx.has_hovered(),
                pressed: ctx.has_active(),
                focused,
                focus_visible: ctx.focus_visible(),
                disabled,
            },
            !disabled && !stashed && ctx.is_window_focused(),
        ) {
            ctx.submit_action::<NodeEvent>(event);
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
        let cross = if axis == Axis::Vertical {
            props.width.map(Length::px).or(cross)
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
        if let NodeKind::Scroll {
            axis, follow_end, ..
        } = self.node.kind
        {
            self.scroll.layout(inner, extent, axis, follow_end);
        }
    }
    fn compose(&mut self, ctx: &mut ComposeCtx<'_>) {
        if let Some(offset) = self.scroll_request.take() {
            self.scroll.set_offset(offset);
        }
        let translation = if let NodeKind::Scroll { align, .. } = self.node.kind {
            let metrics = (
                self.scroll.offset(),
                self.scroll.viewport(),
                self.scroll.content(),
            );
            if self.scroll_feedback != Some(metrics) || self.scroll_baseline != metrics.0 {
                ctx.submit_action::<NodeEvent>(NodeEvent::Scroll(reel_ui::stage::NodeScroll {
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
            ctx.set_child_scroll_translation(child, translation);
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
        self.paint_selection(ctx, painter);
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
