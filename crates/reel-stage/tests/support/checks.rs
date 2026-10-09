use super::fixture::*;
use imaging_vello_cpu::VelloCpuRenderer;
use masonry_core::{
    app::VisualLayerKind,
    core::*,
    imaging::record::{Command, Scene},
    kurbo::{Point, Rect, Size},
};
use std::{cell::RefCell, rc::Rc, sync::Arc};

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
        for pre in [true, false] {
            root.edit_widget(button_id, |mut w| {
                if pre {
                    w.ctx.request_pre_paint();
                } else {
                    w.ctx.request_post_paint();
                }
            });
            let (partial, _) = root.redraw();
            assert_eq!(
                partial.layers.len(),
                plan.layers.len(),
                "cached paint declaration survives a different callback rerunning"
            );
        }
        let independent_invalid = plan
            .layers
            .iter()
            .filter(|l| matches!(&l.kind,VisualLayerKind::Scene(s) if s.validate().is_err()))
            .count();
        let mut scene = Scene::new();
        if isolated {
            let mut groups = 0;
            let mut clips = 0;
            for (index, layer) in plan.layers.iter().enumerate() {
                if let VisualLayerKind::Scene(fragment) = &layer.kind {
                    let before = (groups, clips);
                    let commands: Vec<_> = fragment
                        .commands()
                        .iter()
                        .map(|command| match command {
                            Command::PushGroup(_) => {
                                groups += 1;
                                "PushGroup"
                            }
                            Command::PopGroup => {
                                groups -= 1;
                                "PopGroup"
                            }
                            Command::PushClip(_) => {
                                clips += 1;
                                "PushClip"
                            }
                            Command::PopClip => {
                                clips -= 1;
                                "PopClip"
                            }
                            Command::PushContext(_) => "PushContext",
                            Command::PopContext => "PopContext",
                            Command::Draw(_) => "Draw",
                        })
                        .collect();
                    println!(
                        "isolated fragment {index}: (group,clip) {before:?} -> {:?}, commands={commands:?}",
                        (groups, clips)
                    );
                }
            }
        }
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
        let mut bridge = reel_stage::PresentationBridge::new(
            root,
            reel_core::identity::SurfaceEpoch(1),
            crate::bridge_limits(),
        )
        .unwrap();
        let candidate = bridge.record(reel_core::identity::FrameId(1)).unwrap();
        let rendered = VelloCpuRenderer::new(400, 300)
            .render_scene(candidate.scene(), 400, 300)
            .unwrap();
        assert_eq!(
            rendered.data, img.data,
            "production candidate preserves real cross-fragment group/clip output"
        );
        assert!(candidate.geometry().get(button_id).is_some());
    }
}
pub fn disabled_child_route() {
    let modes = [false, true];
    for presented in modes {
        let mut f = SliderFixture::new();
        let queries: GeometryTrace = Rc::new(RefCell::new(vec![]));
        f.root.edit_widget(f.group, |mut w| {
            let group = w.downcast::<StageGroup>();
            group.widget.consume = true;
            group.widget.queries = Some(queries.clone());
        });
        f.root.redraw();
        if presented {
            let view = Arc::new(f.root.capture_presented_geometry(super::limits()).unwrap());
            f.root
                .set_input_geometry_source(masonry_core::app::InputGeometrySource::Presented(
                    Some(view),
                ));
            f.change_layout();
        }
        f.root
            .edit_widget(f.slider, |mut w| w.ctx.set_disabled(true));
        f.trace.borrow_mut().clear();
        queries.borrow_mut().clear();
        assert_eq!(
            f.root
                .handle_pointer_event(mouse("down", Point::new(35., 20.))),
            Handled::Yes
        );
        assert!(f.values().is_empty());
        assert_eq!(f.root.pointer_capture_target(), None);
        assert!(
            !f.trace.borrow().iter().any(|o| o.widget == f.slider),
            "disabled child behavior must be skipped"
        );
        assert!(
            f.trace
                .borrow()
                .iter()
                .any(|o| o.widget == f.group && o.target == f.slider && o.kind == "down"),
            "disabled hit must retain the ancestor route"
        );
        let queries = queries.borrow();
        let raw = queries
            .iter()
            .find(|q| q.kind == "raw" && q.widget == f.slider)
            .unwrap();
        assert_eq!(raw.size, None);
        assert_eq!(raw.transform, masonry_core::kurbo::Affine::IDENTITY);
        println!(
            "disabled child presented={presented}: parent handled=Yes; child pointer callbacks=0, capture=None, actions=0"
        );
    }
}

pub fn terminal_bubble_removal() {
    let modes = [false, true];
    for presented in modes {
        for control in ["button", "slider"] {
            for terminal in ["up", "cancel"] {
                let trace = Rc::new(RefCell::new(vec![]));
                let mut group = StageGroup::new(trace.clone(), 1., Rect::new(0., 0., 300., 200.));
                group.remove_on_terminal = true;
                let child = if control == "button" {
                    image_button(trace.clone(), false).0.erased()
                } else {
                    Observed {
                        inner: masonry::widgets::Slider::new(0., 1., 0.),
                        trace: trace.clone(),
                        isolated: false,
                        queries: None,
                    }
                    .prepare()
                    .erased()
                };
                let child_id = group.add(child, Point::new(10., 10.), Size::new(100., 30.));
                let group = group.prepare();
                let group_id = group.id();
                let (mut root, signals) = root(group);
                root.redraw();
                if presented {
                    let view = Arc::new(root.capture_presented_geometry(super::limits()).unwrap());
                    root.set_input_geometry_source(
                        masonry_core::app::InputGeometrySource::Presented(Some(view)),
                    );
                }
                root.handle_pointer_event(mouse("move", Point::new(35., 20.)));
                root.handle_pointer_event(mouse("down", Point::new(35., 20.)));
                assert_eq!(root.pointer_capture_target(), Some(child_id));
                trace.borrow_mut().clear();
                signals.borrow_mut().clear();
                root.handle_pointer_event(mouse(terminal, Point::new(35., 20.)));
                let samples = trace.borrow();
                let delivered: Vec<_> = samples
                    .iter()
                    .filter(|o| o.widget == child_id)
                    .map(|o| o.kind)
                    .collect();
                assert_eq!(
                    delivered,
                    vec![terminal],
                    "one terminal before ancestor removal: {control}/{terminal}/presented={presented}"
                );
                assert!(
                    samples.iter().any(|o| o.widget == group_id
                        && o.target == child_id
                        && o.kind == terminal)
                );
                assert!(
                    samples
                        .iter()
                        .find(|o| o.widget == child_id)
                        .unwrap()
                        .active,
                    "real Up/Cancel handler must still see capture as active"
                );
                if control == "button" && terminal == "up" {
                    assert!(
                        samples
                            .iter()
                            .find(|o| o.widget == child_id)
                            .unwrap()
                            .hovered,
                        "real Button release keeps its hover condition before ancestor deletion"
                    );
                }
                assert!(root.get_widget(child_id).is_none());
                assert_eq!(root.pointer_capture_target(), None);
                let clicks = signals.borrow().iter().filter(|s| matches!(s, masonry_core::app::RenderRootSignal::Action(a, id) if *id == child_id && a.downcast_ref::<masonry::widgets::ButtonPress>().is_some())).count();
                // The unchanged action pass intentionally drops actions whose source
                // was deleted before action propagation (passes/action.rs:65).
                assert_eq!(
                    clicks, 0,
                    "deleted-source action must follow upstream lifetime rules"
                );
                println!(
                    "ancestor removal {control}/{terminal} presented={presented}: terminal={delivered:?}, capture=None, button clicks={clicks}"
                );
            }
        }
    }
}
#[test]
fn disabled_child_bubbles_to_consuming_parent() {
    disabled_child_route();
}
#[test]
fn ancestor_removes_child_after_terminal_delivery() {
    terminal_bubble_removal();
}
#[test]
fn real_upstream_groups() {
    groups();
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
    let mut bridge = reel_stage::PresentationBridge::new(
        root,
        reel_core::identity::SurfaceEpoch(1),
        crate::bridge_limits(),
    )
    .unwrap();
    assert!(matches!(
        bridge.record(reel_core::identity::FrameId(1)),
        Err(reel_stage::RecordError::UnsupportedRootTransform)
    ));
    assert_eq!(bridge.pending_count(), 0);
    assert_eq!(bridge.snapshot_usage().bytes, 0);
}
#[test]
fn base_layer_transform_boundary() {
    base_layer_boundary();
}
