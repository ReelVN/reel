use crate::fixture::*;
use imaging_vello_cpu::VelloCpuRenderer;
use masonry_core::{
    app::VisualLayerKind,
    core::*,
    imaging::record::{Command, Scene},
    kurbo::{Point, Rect, Size},
};
use std::{cell::RefCell, rc::Rc};

pub fn groups() {
    for isolated in [false, true] {
        let trace = Rc::new(RefCell::new(vec![]));
        let mut inner = StageGroup::new(trace.clone(), 0.5, Rect::new(0., 0., 60., 55.));
        inner.add(
            image([255, 0, 0, 255]).prepare(),
            Point::ZERO,
            Size::new(60., 50.),
        );
        let (button, _) = image_button(trace.clone(), isolated);
        let button_id = inner.add(button, Point::new(20., 0.), Size::new(60., 50.));
        let mut outer = StageGroup::new(trace, 0.5, Rect::new(0., 0., 90., 60.));
        outer.add(inner.prepare(), Point::ZERO, Size::new(90., 60.));
        let mut shell = StageGroup::new(
            Rc::new(RefCell::new(vec![])),
            1.,
            Rect::new(0., 0., 400., 300.),
        );
        shell.add(
            outer.prepare().with_transform(transform()),
            Point::ZERO,
            Size::new(90., 60.),
        );
        let (mut root, _) = root(shell.prepare());
        let (plan, _) = root.redraw();
        let (cached, _) = root.redraw();
        let independent_invalid = plan
            .layers
            .iter()
            .filter(|l| matches!(&l.kind,VisualLayerKind::Scene(s) if s.validate().is_err()))
            .count();
        let mut scene = Scene::new();
        plan.replay_into(&mut scene);
        scene
            .validate()
            .expect("flattened scene must preserve group and clip balance");
        let group_count = scene
            .commands()
            .iter()
            .filter(|c| matches!(c, Command::PushGroup(_)))
            .count();
        assert_eq!(group_count, 2);
        let img = VelloCpuRenderer::new(400, 300)
            .render_scene(&scene, 400, 300)
            .unwrap();
        let pixel = |local: Point| {
            let p = transform() * local;
            let i = ((p.y.floor() as usize) * 400 + p.x.floor() as usize) * 4;
            <[u8; 4]>::try_from(&img.data[i..i + 4]).unwrap()
        };
        let image_only = pixel(Point::new(10., 20.));
        let overlap = pixel(Point::new(30., 20.));
        let clipped = pixel(Point::new(70., 20.));
        assert!(
            (image_only[3] as i32 - 64).abs() <= 1,
            "nested image alpha {image_only:?}"
        );
        assert!(
            (overlap[3] as i32 - 64).abs() <= 1,
            "group alpha must apply once over overlapping image + button {overlap:?}"
        );
        assert_eq!(clipped[3], 0);
        root.handle_pointer_event(mouse("move", transform() * Point::new(30., 20.)));
        assert!(root.get_widget(button_id).unwrap().ctx().is_hovered());
        root.handle_pointer_event(mouse("move", transform() * Point::new(70., 20.)));
        assert!(!root.get_widget(button_id).unwrap().ctx().is_hovered());
        println!(
            "groups isolated={isolated}: cached_layers={} layers={} invalid_independent={independent_invalid}, image={image_only:?}, overlap={overlap:?}, clip={clipped:?}",
            cached.layers.len(),
            plan.layers.len()
        );
    }
}
#[cfg(not(feature = "experimental"))]
pub fn upstream_input_gap() {
    let mut f = SliderFixture::new();
    f.root.redraw();
    f.root
        .handle_pointer_event(mouse("move", Point::new(35., 20.)));
    assert!(f.root.get_widget(f.slider).unwrap().ctx().is_hovered());
    f.change_layout();
    f.root.redraw();
    let hover = f.root.get_widget(f.slider).unwrap().ctx().is_hovered();
    let handled = f
        .root
        .handle_pointer_event(mouse("down", Point::new(35., 20.)));
    let captured = f.root.pointer_capture_target();
    println!(
        "upstream unaccepted-layout: hover={hover}, capture={captured:?}, handled={handled:?}, values={:?}",
        f.values()
    );
    assert!(
        !hover && captured.is_none(),
        "upstream gap must reproduce against real Masonry"
    );
    f.root
        .handle_pointer_event(mouse("down", Point::new(210., 90.)));
    let observations = f.trace.borrow();
    let sample = observations
        .iter()
        .rev()
        .find(|o| o.widget == f.slider)
        .unwrap();
    assert_eq!(sample.target, f.slider);
    assert_eq!(sample.local, Some(Point::new(50., 10.)));
    assert_eq!(sample.size, Size::new(200., 40.));
    assert!(!sample.active && !sample.hovered);
    drop(observations);
    assert_eq!(f.values(), vec![0.25]);
    f.trace.borrow_mut().clear();
    f.root.edit_widget(f.group, |mut w| {
        StageGroup::remove(&mut w.downcast::<StageGroup>(), 0)
    });
    assert_eq!(f.root.pointer_capture_target(), None);
    let cancels = f
        .trace
        .borrow()
        .iter()
        .filter(|o| o.kind == "cancel")
        .count();
    println!("upstream captured-node removal: cancel_callbacks={cancels}");
    assert_eq!(
        cancels, 0,
        "upstream removes capture without cancel callback"
    );
}
pub fn run() {
    groups();
    base_layer_boundary();
    #[cfg(not(feature = "experimental"))]
    upstream_input_gap();
    #[cfg(feature = "experimental")]
    crate::presentation::run();
}
#[test]
fn real_upstream_groups() {
    groups();
}
#[cfg(not(feature = "experimental"))]
#[test]
fn real_upstream_input_gap() {
    upstream_input_gap();
}

pub fn base_layer_boundary() {
    let trace = Rc::new(RefCell::new(vec![]));
    let mut group = StageGroup::new(trace.clone(), 1., Rect::new(0., 0., 90., 60.));
    let (button, _) = image_button(trace, false);
    let id = group.add(button, Point::ZERO, Size::new(60., 50.));
    let (mut root, _) = root(group.prepare().with_transform(transform()));
    let (plan, _) = root.redraw();
    let mut scene = Scene::new();
    plan.replay_into(&mut scene);
    let img = VelloCpuRenderer::new(400, 300)
        .render_scene(&scene, 400, 300)
        .unwrap();
    let p = transform() * Point::new(30., 20.);
    let i = (p.y.floor() as usize * 400 + p.x.floor() as usize) * 4;
    assert_eq!(
        img.data[i + 3],
        0,
        "base layer transform is stripped in upstream paint output"
    );
    assert_eq!(img.data[(20 * 400 + 30) * 4 + 3], 255);
    root.handle_pointer_event(mouse("move", p));
    assert!(root.get_widget(id).unwrap().ctx().is_hovered());
    println!(
        "base-layer boundary: native root transform stripped from draw, retained by hit; identity host root + transformed stage child required"
    );
}
#[test]
fn base_layer_transform_boundary() {
    base_layer_boundary();
}
