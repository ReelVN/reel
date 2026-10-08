//! Instrumentation forwards behavior and drawing to the pinned upstream widgets.
use masonry::widgets::{Button, Image, Slider, SliderMoved};
use masonry_core::{
    accesskit::{Node, Role},
    app::{RenderRoot, RenderRootOptions, RenderRootSignal, WindowSizePolicy},
    core::*,
    dpi::{PhysicalPosition, PhysicalSize},
    imaging::{Composite, GroupRef, Painter},
    kurbo::{Affine, Axis, Point, Rect, Size},
    layout::{LenReq, Length},
    peniko::{Blob, ImageAlphaType, ImageData, ImageFormat},
};
use std::{any::TypeId, cell::RefCell, rc::Rc, sync::Arc};

pub type Trace = Rc<RefCell<Vec<Observation>>>;
pub type ChildQueryTrace = Rc<RefCell<Vec<ChildQueryObservation>>>;
#[derive(Debug, Clone)]
pub struct ChildQueryObservation {
    pub phase: &'static str,
    pub id: WidgetId,
    pub present: bool,
    pub children: Vec<WidgetId>,
}
pub type GeometryTrace = Rc<RefCell<Vec<GeometryObservation>>>;
#[derive(Debug, Clone)]
pub struct GeometryObservation {
    pub widget: WidgetId,
    pub kind: &'static str,
    pub size: Option<Size>,
    pub transform: Affine,
}
#[derive(Debug, Clone)]
pub struct Observation {
    pub widget: WidgetId,
    pub target: WidgetId,
    pub kind: &'static str,
    pub local: Option<Point>,
    pub size: Size,
    pub hovered: bool,
    pub active: bool,
}
fn position(event: &PointerEvent) -> Option<PhysicalPosition<f64>> {
    match event {
        PointerEvent::Down(e) | PointerEvent::Up(e) => Some(e.state.position),
        PointerEvent::Move(e) => Some(e.current.position),
        _ => None,
    }
}
fn observe(trace: &Trace, ctx: &EventCtx<'_>, event: &PointerEvent) {
    let kind = match event {
        PointerEvent::Down(_) => "down",
        PointerEvent::Up(_) => "up",
        PointerEvent::Move(_) => "move",
        PointerEvent::Cancel(_) => "cancel",
        _ => "other",
    };
    trace.borrow_mut().push(Observation {
        widget: ctx.widget_id(),
        target: ctx.target(),
        kind,
        local: position(event).map(|p| ctx.local_position(p)),
        size: ctx.content_box().size(),
        hovered: ctx.is_hovered(),
        active: ctx.is_active(),
    });
}

/// A transparent observer: all control behavior remains upstream code.
pub struct Observed<W> {
    pub inner: W,
    pub trace: Trace,
    pub isolated: bool,
    pub queries: Option<GeometryTrace>,
}
impl<W: Widget> Widget for Observed<W> {
    type Action = W::Action;
    fn on_pointer_event(
        &mut self,
        c: &mut EventCtx<'_>,
        p: &mut PropertiesMut<'_>,
        e: &PointerEvent,
    ) {
        observe(&self.trace, c, e);
        self.inner.on_pointer_event(c, p, e);
    }
    fn on_text_event(&mut self, c: &mut EventCtx<'_>, p: &mut PropertiesMut<'_>, e: &TextEvent) {
        self.inner.on_text_event(c, p, e);
    }
    fn on_access_event(
        &mut self,
        c: &mut EventCtx<'_>,
        p: &mut PropertiesMut<'_>,
        e: &AccessEvent,
    ) {
        self.inner.on_access_event(c, p, e);
    }
    fn register_children(&mut self, c: &mut RegisterCtx<'_>) {
        self.inner.register_children(c);
    }
    fn update(&mut self, c: &mut UpdateCtx<'_>, p: &mut PropertiesMut<'_>, e: &Update) {
        if let Some(trace) = &self.queries {
            let kind = match e {
                Update::HoveredChanged(false) => Some("hover_exit"),
                Update::HoveredChanged(true) => Some("hover_enter"),
                Update::ChildHoveredChanged(false) => Some("ancestor_hover_exit"),
                Update::ChildHoveredChanged(true) => Some("ancestor_hover_enter"),
                Update::ActiveChanged(false) => Some("active_exit"),
                Update::ActiveChanged(true) => Some("active_enter"),
                Update::ChildActiveChanged(false) => Some("ancestor_active_exit"),
                Update::ChildActiveChanged(true) => Some("ancestor_active_enter"),
                _ => None,
            };
            if let Some(kind) = kind {
                trace.borrow_mut().push(GeometryObservation {
                    widget: c.widget_id(),
                    kind,
                    size: Some(c.content_box().size()),
                    transform: c.window_transform(),
                });
            }
        }
        if matches!(e, Update::HoveredChanged(_) | Update::ActiveChanged(_))
            && let Some(trace) = &self.queries
        {
            trace.borrow_mut().push(GeometryObservation {
                widget: c.widget_id(),
                kind: "update",
                size: Some(c.content_box().size()),
                transform: c.window_transform(),
            });
        }
        self.inner.update(c, p, e);
    }
    fn property_changed(&mut self, c: &mut UpdateCtx<'_>, t: TypeId) {
        self.inner.property_changed(c, t);
    }
    fn measure(
        &mut self,
        c: &mut MeasureCtx<'_>,
        p: &PropertiesRef<'_>,
        a: Axis,
        r: LenReq,
        x: Option<Length>,
    ) -> Length {
        self.inner.measure(c, p, a, r, x)
    }
    fn layout(&mut self, c: &mut LayoutCtx<'_>, p: &PropertiesRef<'_>, s: Size) {
        self.inner.layout(c, p, s);
    }
    fn pre_paint(&mut self, c: &mut PaintCtx<'_>, p: &PropertiesRef<'_>, s: &mut Painter<'_>) {
        self.inner.pre_paint(c, p, s);
    }
    fn paint(&mut self, c: &mut PaintCtx<'_>, p: &PropertiesRef<'_>, s: &mut Painter<'_>) {
        if self.isolated {
            c.set_paint_layer_mode(PaintLayerMode::IsolatedScene);
        }
        self.inner.paint(c, p, s);
    }
    fn post_paint(&mut self, c: &mut PaintCtx<'_>, p: &PropertiesRef<'_>, s: &mut Painter<'_>) {
        self.inner.post_paint(c, p, s);
    }
    fn accessibility_role(&self) -> Role {
        self.inner.accessibility_role()
    }
    fn accessibility(&mut self, c: &mut AccessCtx<'_>, p: &PropertiesRef<'_>, n: &mut Node) {
        self.inner.accessibility(c, p, n);
    }
    fn children_ids(&self) -> ChildrenIds {
        self.inner.children_ids()
    }
    fn accepts_pointer_interaction(&self) -> bool {
        self.inner.accepts_pointer_interaction()
    }
    fn propagates_pointer_interaction(&self) -> bool {
        self.inner.propagates_pointer_interaction()
    }
    fn accepts_focus(&self) -> bool {
        self.inner.accepts_focus()
    }
    fn accepts_text_input(&self) -> bool {
        self.inner.accepts_text_input()
    }
    fn get_cursor(&self, c: &QueryCtx<'_>, pos: Point) -> CursorIcon {
        if let Some(trace) = &self.queries {
            trace.borrow_mut().push(GeometryObservation {
                widget: c.widget_id(),
                kind: "cursor",
                size: Some(c.content_box().size()),
                transform: c.window_transform(),
            });
        }
        self.inner.get_cursor(c, pos)
    }
}

/// Author group implemented entirely through public Masonry extension points.
/// It lays out real child widgets; pre/post paint delimit one isolated Imaging group.
pub struct StageGroup {
    pub accepts_pointer: bool,
    pub query_ids: Vec<WidgetId>,
    pub child_queries: Option<ChildQueryTrace>,
    pub children: Vec<(WidgetPod<dyn Widget>, Point, Size)>,
    pub opacity: f32,
    pub clip: Rect,
    pub trace: Trace,
    pub consume: bool,
    pub remove_on_terminal: bool,
    pub remove_on_hover_enter: bool,
    pub remove_on_active_enter: bool,
    pub queries: Option<GeometryTrace>,
}
impl StageGroup {
    pub fn new(trace: Trace, opacity: f32, clip: Rect) -> Self {
        Self {
            accepts_pointer: false,
            query_ids: vec![],
            child_queries: None,
            children: vec![],
            opacity,
            clip,
            trace,
            consume: false,
            remove_on_terminal: false,
            remove_on_hover_enter: false,
            remove_on_active_enter: false,
            queries: None,
        }
    }
    pub fn add<W: Widget + ?Sized>(
        &mut self,
        child: NewWidget<W>,
        origin: Point,
        size: Size,
    ) -> WidgetId {
        let id = child.id();
        self.children.push((child.erased().to_pod(), origin, size));
        id
    }
    pub fn remove(this: &mut WidgetMut<'_, Self>, index: usize) {
        let (child, _, _) = this.widget.children.remove(index);
        this.ctx.remove_child(child);
    }
    pub fn geometry(this: &mut WidgetMut<'_, Self>, index: usize, origin: Point, size: Size) {
        this.widget.children[index].1 = origin;
        this.widget.children[index].2 = size;
        this.ctx.request_layout();
    }
}
impl Widget for StageGroup {
    type Action = NoAction;
    fn on_pointer_event(
        &mut self,
        c: &mut EventCtx<'_>,
        _: &mut PropertiesMut<'_>,
        e: &PointerEvent,
    ) {
        observe(&self.trace, c, e);
        if let Some(trace) = &self.child_queries {
            for (child, _, _) in &mut self.children {
                let id = child.id();
                let raw = c.get_raw(child);
                trace.borrow_mut().push(ChildQueryObservation {
                    phase: "raw",
                    id,
                    present: raw.is_some(),
                    children: vec![],
                });
            }
        }
        if let Some(trace) = &self.queries {
            for (child, _, _) in &mut self.children {
                let (_, raw) = c.get_raw(child).unwrap();
                trace.borrow_mut().push(GeometryObservation {
                    widget: raw.widget_id(),
                    kind: "raw",
                    size: None,
                    transform: raw.transform(),
                });
            }
        }
        if self.remove_on_terminal
            && matches!(e, PointerEvent::Up(_) | PointerEvent::Cancel(_))
            && let Some(index) = self
                .children
                .iter()
                .position(|(child, _, _)| child.id() == c.target())
        {
            let (child, _, _) = self.children.remove(index);
            c.remove_child(child);
        }
        if self.consume {
            c.set_handled();
        }
    }
    fn register_children(&mut self, c: &mut RegisterCtx<'_>) {
        for (child, _, _) in &mut self.children {
            c.register_child(child);
        }
    }
    fn update(&mut self, c: &mut UpdateCtx<'_>, _: &mut PropertiesMut<'_>, e: &Update) {
        if let Some(trace) = &self.queries {
            let kind = match e {
                Update::HoveredChanged(false) => Some("hover_exit"),
                Update::HoveredChanged(true) => Some("hover_enter"),
                Update::ChildHoveredChanged(false) => Some("ancestor_hover_exit"),
                Update::ChildHoveredChanged(true) => Some("ancestor_hover_enter"),
                Update::ActiveChanged(false) => Some("active_exit"),
                Update::ActiveChanged(true) => Some("active_enter"),
                Update::ChildActiveChanged(false) => Some("ancestor_active_exit"),
                Update::ChildActiveChanged(true) => Some("ancestor_active_enter"),
                _ => None,
            };
            if let Some(kind) = kind {
                trace.borrow_mut().push(GeometryObservation {
                    widget: c.widget_id(),
                    kind,
                    size: Some(c.content_box().size()),
                    transform: c.window_transform(),
                });
            }
        }
        if matches!(
            e,
            Update::ChildHoveredChanged(_) | Update::ChildActiveChanged(_)
        ) && let Some(trace) = &self.queries
        {
            trace.borrow_mut().push(GeometryObservation {
                widget: c.widget_id(),
                kind: "update",
                size: Some(c.content_box().size()),
                transform: c.window_transform(),
            });
        }
        if (self.remove_on_hover_enter && matches!(e, Update::ChildHoveredChanged(true)))
            || (self.remove_on_active_enter && matches!(e, Update::ChildActiveChanged(true)))
        {
            let (child, _, _) = self.children.remove(0);
            c.remove_child(child);
        }
    }
    fn get_cursor(&self, c: &QueryCtx<'_>, _: Point) -> CursorIcon {
        if let Some(trace) = &self.child_queries {
            for &id in &self.query_ids {
                let child = c.get(id);
                trace.borrow_mut().push(ChildQueryObservation {
                    phase: "query",
                    id,
                    present: child.is_some(),
                    children: child
                        .map(|w| w.children().iter().map(|c| c.id()).collect())
                        .unwrap_or_default(),
                });
            }
        }
        CursorIcon::Default
    }
    fn property_changed(&mut self, _: &mut UpdateCtx<'_>, _: TypeId) {}
    fn measure(
        &mut self,
        _: &mut MeasureCtx<'_>,
        _: &PropertiesRef<'_>,
        a: Axis,
        _: LenReq,
        _: Option<Length>,
    ) -> Length {
        Length::px(if a == Axis::Horizontal {
            self.clip.width()
        } else {
            self.clip.height()
        })
    }
    fn layout(&mut self, c: &mut LayoutCtx<'_>, _: &PropertiesRef<'_>, _: Size) {
        c.set_clip_path(self.clip);
        for (child, origin, size) in &mut self.children {
            if c.child_is_stashed(child) == Some(true) {
                continue;
            }
            c.run_layout(child, *size);
            c.place_child(child, *origin);
        }
    }
    fn pre_paint(&mut self, _: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, p: &mut Painter<'_>) {
        if self.opacity != 1. {
            p.push_group(
                GroupRef::new().with_composite(Composite::new(Default::default(), self.opacity)),
            );
        }
    }
    fn paint(&mut self, _: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, _: &mut Painter<'_>) {}
    fn post_paint(&mut self, _: &mut PaintCtx<'_>, _: &PropertiesRef<'_>, p: &mut Painter<'_>) {
        if self.opacity != 1. {
            p.pop_group();
        }
    }
    fn accessibility_role(&self) -> Role {
        Role::GenericContainer
    }
    fn accessibility(&mut self, _: &mut AccessCtx<'_>, _: &PropertiesRef<'_>, _: &mut Node) {}
    fn children_ids(&self) -> ChildrenIds {
        self.children.iter().map(|(c, _, _)| c.id()).collect()
    }
    fn accepts_pointer_interaction(&self) -> bool {
        self.accepts_pointer
    }
}

pub fn image_data(rgba: [u8; 4]) -> ImageData {
    ImageData {
        data: Blob::new(Arc::new(rgba.to_vec())),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: 1,
        height: 1,
    }
}
pub fn image(rgba: [u8; 4]) -> Image {
    Image::new(image_data(rgba)).decorative(true)
}
pub fn root<W: Widget>(widget: NewWidget<W>) -> (RenderRoot, Rc<RefCell<Vec<RenderRootSignal>>>) {
    let signals = Rc::new(RefCell::new(vec![]));
    let sink = signals.clone();
    (
        RenderRoot::new(
            widget,
            move |s| sink.borrow_mut().push(s),
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(400, 300),
                scale_factor: 1.,
                test_font: None,
            },
        ),
        signals,
    )
}
pub struct SliderFixture {
    pub root: RenderRoot,
    pub group: WidgetId,
    pub slider: WidgetId,
    pub trace: Trace,
    pub signals: Rc<RefCell<Vec<RenderRootSignal>>>,
}
impl SliderFixture {
    pub fn new() -> Self {
        let trace = Rc::new(RefCell::new(vec![]));
        let mut group = StageGroup::new(trace.clone(), 1., Rect::new(0., 0., 300., 200.));
        let slider = group.add(
            Observed {
                inner: Slider::new(0., 1., 0.),
                trace: trace.clone(),
                isolated: false,
                queries: None,
            }
            .prepare(),
            Point::new(10., 10.),
            Size::new(100., 30.),
        );
        let group = group.prepare();
        let id = group.id();
        let (root, signals) = root(group);
        Self {
            root,
            group: id,
            slider,
            trace,
            signals,
        }
    }
    pub fn change_layout(&mut self) {
        self.root.edit_widget(self.group, |mut w| {
            StageGroup::geometry(
                &mut w.downcast::<StageGroup>(),
                0,
                Point::new(160., 80.),
                Size::new(200., 40.),
            )
        });
    }
    pub fn values(&mut self) -> Vec<f64> {
        self.signals
            .borrow_mut()
            .drain(..)
            .filter_map(|s| match s {
                RenderRootSignal::Action(a, id) if id == self.slider => {
                    a.downcast_ref::<SliderMoved>().map(|a| a.value)
                }
                _ => None,
            })
            .collect()
    }
}
pub fn mouse(kind: &str, point: Point) -> PointerEvent {
    let pointer = PointerInfo {
        pointer_id: Some(PointerId::PRIMARY),
        persistent_device_id: None,
        pointer_type: PointerType::Mouse,
    };
    let state = PointerState {
        position: PhysicalPosition::new(point.x, point.y),
        ..Default::default()
    };
    match kind {
        "move" => PointerEvent::Move(PointerUpdate {
            pointer,
            current: state,
            coalesced: vec![],
            predicted: vec![],
        }),
        "down" => PointerEvent::Down(PointerButtonEvent {
            pointer,
            state,
            button: Some(PointerButton::Primary),
        }),
        "up" => PointerEvent::Up(PointerButtonEvent {
            pointer,
            state,
            button: Some(PointerButton::Primary),
        }),
        "cancel" => PointerEvent::Cancel(pointer),
        _ => panic!("unknown event"),
    }
}
pub fn image_button(trace: Trace, isolated: bool) -> (NewWidget<Observed<Button>>, WidgetId) {
    let child = image([0, 0, 255, 255]).prepare();
    let image_id = child.id();
    (
        Observed {
            inner: Button::new(child),
            trace,
            isolated,
            queries: None,
        }
        .prepare(),
        image_id,
    )
}
pub fn transform() -> Affine {
    Affine::translate((140., 30.))
        * Affine::rotate(std::f64::consts::FRAC_PI_2)
        * Affine::scale_non_uniform(1.5, 1.)
}
