//! Scripted host outcomes exercise the gate; these are not GPU/surface acceptance measurements.
use super::fixture::*;
use imaging_vello_cpu::VelloCpuRenderer;
use reel_ui::imaging::record::Scene;
use reel_ui::widgets::{ButtonPress, Image, Slider};
use reel_ui::{
    app::{PresentedGeometryView, RenderRoot},
    core::*,
    dpi::PhysicalSize,
    kurbo::{Affine, Point, Rect, Size},
};
use std::{cell::RefCell, rc::Rc};
use std::{sync::Arc, time::Instant};

#[derive(Debug, Clone, Copy)]
enum Outcome {
    Accepted(u64),
    Skipped,
    SurfaceUnavailable,
    Error,
}
struct Candidate {
    geometry: Arc<PresentedGeometryView>,
    frame_id: u64,
    surface_epoch: u64,
}
#[derive(Default)]
struct Gate {
    accepted: Option<u64>,
    surface_epoch: u64,
}
impl Gate {
    fn record(&self, root: &mut RenderRoot, frame: u64) -> Candidate {
        root.redraw();
        Candidate {
            geometry: Arc::new(root.capture_presented_geometry(super::limits()).unwrap()),
            frame_id: frame,
            surface_epoch: self.surface_epoch,
        }
    }
    fn resolve(&mut self, root: &mut RenderRoot, candidate: &Candidate, outcome: Outcome) -> bool {
        if matches!(outcome,Outcome::Accepted(id) if id == candidate.frame_id)
            && candidate.surface_epoch == self.surface_epoch
            && self.accepted.is_none_or(|last| candidate.frame_id > last)
        {
            root.set_input_geometry_source(reel_ui::app::InputGeometrySource::Presented(Some(
                candidate.geometry.clone(),
            )));
            self.accepted = Some(candidate.frame_id);
            true
        } else {
            false
        }
    }
}
fn assert_sample(f: &SliderFixture, kind: &str, local: Point, size: Size) {
    let trace = f.trace.borrow();
    let o = trace
        .iter()
        .rev()
        .find(|o| o.widget == f.slider && o.kind == kind)
        .expect("upstream slider must receive input");
    assert_eq!(o.target, f.slider);
    assert_eq!(o.local, Some(local));
    assert_eq!(o.size, size);
    assert!(
        trace
            .iter()
            .any(|o| o.widget == f.group && o.target == f.slider && o.kind == kind),
        "input must bubble through selected ancestor path"
    );
}
pub fn frozen_input() {
    let mut f = SliderFixture::new();
    let mut gate = Gate::default();
    let first = gate.record(&mut f.root, 1);
    assert!(gate.resolve(&mut f.root, &first, Outcome::Accepted(1)));
    f.root
        .handle_pointer_event(mouse("move", Point::new(35., 20.)));
    f.change_layout();
    let pending = gate.record(&mut f.root, 2);
    for outcome in [
        Outcome::Skipped,
        Outcome::SurfaceUnavailable,
        Outcome::Error,
        Outcome::Accepted(999),
    ] {
        assert!(!gate.resolve(&mut f.root, &pending, outcome));
        assert_eq!(gate.accepted, Some(1));
        assert!(f.root.get_widget(f.slider).unwrap().ctx().is_hovered());
    }
    assert!(
        !gate.resolve(&mut f.root, &first, Outcome::Accepted(1)),
        "stale acceptance must retain view"
    );
    gate.surface_epoch += 1;
    assert!(
        !gate.resolve(&mut f.root, &pending, Outcome::Accepted(2)),
        "expired surface epoch must retain view"
    );
    f.root.edit_widget(f.group, |mut w| {
        w.downcast::<StageGroup>().widget.consume = true
    });
    assert_eq!(
        f.root
            .handle_pointer_event(mouse("down", Point::new(35., 20.))),
        Handled::Yes
    );
    assert_eq!(f.root.pointer_capture_target(), Some(f.slider));
    assert_eq!(f.values(), vec![0.25]);
    assert_sample(&f, "down", Point::new(25., 10.), Size::new(100., 30.));
    let down = f
        .trace
        .borrow()
        .iter()
        .find(|o| o.widget == f.slider && o.kind == "down")
        .unwrap()
        .clone();
    assert!(down.hovered && !down.active);
    f.root
        .handle_pointer_event(mouse("move", Point::new(80., 20.)));
    assert_eq!(f.values(), vec![0.7]);
    assert_sample(&f, "move", Point::new(70., 10.), Size::new(100., 30.));
    assert!(
        f.trace
            .borrow()
            .iter()
            .rev()
            .find(|o| o.widget == f.slider)
            .unwrap()
            .active
    );
    f.root
        .handle_pointer_event(mouse("move", Point::new(130., 20.)));
    assert_eq!(f.values(), vec![1.]);
    assert!(!f.root.get_widget(f.slider).unwrap().ctx().is_hovered());
    f.root
        .handle_pointer_event(mouse("up", Point::new(130., 20.)));
    assert_eq!(f.root.pointer_capture_target(), None);
    f.root
        .handle_pointer_event(mouse("move", Point::new(80., 20.)));
    assert!(f.values().is_empty());
    let next = gate.record(&mut f.root, 3);
    assert!(gate.resolve(&mut f.root, &next, Outcome::Accepted(3)));
    assert!(
        !f.root.get_widget(f.slider).unwrap().ctx().is_hovered(),
        "acceptance recomputes stationary hover"
    );
    f.trace.borrow_mut().clear();
    f.root
        .handle_pointer_event(mouse("down", Point::new(35., 20.)));
    assert!(f.values().is_empty());
    assert_eq!(f.root.pointer_capture_target(), None);
    f.root
        .handle_pointer_event(mouse("down", Point::new(210., 90.)));
    assert_eq!(f.values(), vec![0.25]);
    assert_sample(&f, "down", Point::new(50., 10.), Size::new(200., 40.));
    println!(
        "presented input: retained frame=1 across skipped/unavailable/error/mismatched/stale/expired outcomes; real slider values=0.25,0.7,1; accepted frame=3 switches coordinates+hover"
    );
}
pub fn cancellation() {
    for cause in ["focus", "release", "destroy", "disable"] {
        let mut f = SliderFixture::new();
        let mut gate = Gate::default();
        let frame = gate.record(&mut f.root, 1);
        gate.resolve(&mut f.root, &frame, Outcome::Accepted(1));
        f.root
            .handle_pointer_event(mouse("down", Point::new(35., 20.)));
        f.values();
        f.trace.borrow_mut().clear();
        match cause {
            "focus" => {
                f.root
                    .handle_text_event(TextEvent::WindowFocusChange(false));
            }
            "release" => {
                f.root
                    .handle_pointer_event(mouse("up", Point::new(35., 20.)));
            }
            "destroy" => {
                f.root.edit_widget(f.group, |mut w| {
                    StageGroup::remove(&mut w.downcast::<StageGroup>(), 0)
                });
            }
            "disable" => {
                f.root
                    .edit_widget(f.slider, |mut w| w.ctx.set_disabled(true));
            }
            _ => unreachable!(),
        }
        assert_eq!(f.root.pointer_capture_target(), None);
        let events = f
            .trace
            .borrow()
            .iter()
            .filter(|o| o.widget == f.slider)
            .map(|o| o.kind)
            .collect::<Vec<_>>();
        assert_eq!(
            events,
            if cause == "release" {
                vec!["up"]
            } else {
                vec!["cancel"]
            },
            "exactly one terminal delivery for {cause}"
        );
        f.root
            .handle_pointer_event(mouse("move", Point::new(80., 20.)));
        f.root
            .handle_pointer_event(mouse("up", Point::new(80., 20.)));
        assert!(
            f.values().is_empty(),
            "terminated drag must not emit later slider changes"
        );
        println!("drag {cause}: terminal={events:?}, capture=None, later value changes=0");
    }
}
pub fn dpi() {
    let mut f = SliderFixture::new();
    let mut gate = Gate::default();
    let first = gate.record(&mut f.root, 1);
    gate.resolve(&mut f.root, &first, Outcome::Accepted(1));
    f.root.handle_window_event(WindowEvent::Rescale(2.));
    f.root
        .handle_window_event(WindowEvent::Resize(PhysicalSize::new(800, 600)));
    f.root
        .handle_pointer_event(mouse("down", Point::new(35., 20.)));
    assert_eq!(f.values(), vec![0.25]);
    f.root
        .handle_pointer_event(mouse("up", Point::new(35., 20.)));
    let second = gate.record(&mut f.root, 2);
    gate.resolve(&mut f.root, &second, Outcome::Accepted(2));
    f.root
        .handle_pointer_event(mouse("down", Point::new(70., 40.)));
    assert!(f.values().is_empty());
    assert_sample(&f, "down", Point::new(25., 10.), Size::new(100., 30.));
    assert_eq!(f.root.pointer_capture_target(), Some(f.slider));
    println!("DPI: physical-to-logical conversion uses accepted scale=1 then scale=2");
}
pub fn measure() {
    let mut f = SliderFixture::new();
    let mut gate = Gate::default();
    let first = gate.record(&mut f.root, 1);
    gate.resolve(&mut f.root, &first, Outcome::Accepted(1));
    f.root
        .handle_pointer_event(mouse("down", Point::new(35., 20.)));
    let mut samples = Vec::with_capacity(2000);
    for i in 0..2100 {
        f.trace.borrow_mut().clear();
        f.signals.borrow_mut().clear();
        let t = Instant::now();
        f.root
            .handle_pointer_event(mouse("move", Point::new(20. + (i % 80) as f64, 20.)));
        let elapsed = t.elapsed().as_nanos();
        if i >= 100 {
            samples.push(elapsed);
        }
    }
    samples.sort_unstable();
    let mut captures = Vec::with_capacity(1000);
    for i in 0..1100 {
        let t = Instant::now();
        let view = Arc::new(f.root.capture_presented_geometry(super::limits()).unwrap());
        std::hint::black_box(view);
        if i >= 100 {
            captures.push(t.elapsed().as_nanos());
        }
    }
    captures.sort_unstable();
    println!(
        "measure (headless, diagnostic trace included): widgets={}, retained_payload_lower_bound={}B; pointer ns p50={} p95={} p99={}; capture ns p50={} p95={} p99={}",
        first.geometry.len(),
        first.geometry.retained_bytes(),
        samples[1000],
        samples[1900],
        samples[1980],
        captures[500],
        captures[950],
        captures[990]
    );
}
pub fn geometry_queries() {
    let mut f = SliderFixture::new();
    let queries: GeometryTrace = Rc::new(RefCell::new(vec![]));
    f.root.edit_widget(f.slider, |mut w| {
        w.downcast::<Observed<Slider>>().widget.queries = Some(queries.clone());
    });
    f.root.edit_widget(f.group, |mut w| {
        w.downcast::<StageGroup>().widget.queries = Some(queries.clone());
    });
    let mut gate = Gate::default();
    let first = gate.record(&mut f.root, 1);
    assert!(gate.resolve(&mut f.root, &first, Outcome::Accepted(1)));
    f.change_layout();
    f.root.edit_widget(f.group, |mut w| {
        w.ctx.set_transform(Affine::translate((40., 15.)))
    });
    f.root.edit_widget(f.slider, |mut w| {
        w.ctx.set_transform(Affine::translate((7., 3.)))
    });
    let second = gate.record(&mut f.root, 2);
    assert!(!gate.resolve(&mut f.root, &second, Outcome::Skipped));
    let current = Affine::translate((207., 98.));
    assert_eq!(
        f.root
            .get_widget(f.slider)
            .unwrap()
            .ctx()
            .window_transform(),
        current
    );
    f.root.edit_widget(f.group, |mut w| {
        let mut group = w.downcast::<StageGroup>();
        let (_, raw) = group.ctx.get_raw(&mut group.widget.children[0].0).unwrap();
        assert_eq!(
            raw.transform(),
            Affine::translate((7., 3.)),
            "ordinary mutation RawCtx stays live"
        );
    });
    for (frame, point, size, transform) in [
        (
            1,
            Point::new(35., 20.),
            Size::new(100., 30.),
            Affine::translate((10., 10.)),
        ),
        (2, Point::new(257., 108.), Size::new(200., 40.), current),
    ] {
        if frame == 2 {
            assert!(gate.resolve(&mut f.root, &second, Outcome::Accepted(2)));
        }
        f.root
            .handle_pointer_event(mouse("move", Point::new(390., 290.)));
        queries.borrow_mut().clear();
        f.root.handle_pointer_event(mouse("move", point));
        f.root.handle_pointer_event(mouse("down", point));
        for kind in ["cursor", "update", "raw"] {
            let samples = queries.borrow();
            let found: Vec<_> = samples
                .iter()
                .filter(|q| q.widget == f.slider && q.kind == kind)
                .collect();
            assert!(
                !found.is_empty(),
                "{kind} must actually execute for frame {frame}"
            );
            for q in found {
                if kind == "raw" {
                    assert_eq!(
                        q.transform,
                        if frame == 1 {
                            Affine::IDENTITY
                        } else {
                            Affine::translate((7., 3.))
                        },
                        "raw local transform frame {frame}"
                    );
                } else {
                    assert_eq!(q.size, Some(size), "{kind} dimensions frame {frame}");
                    assert_eq!(
                        q.transform, transform,
                        "{kind} window transform frame {frame}"
                    );
                }
            }
        }
        let samples = queries.borrow();
        assert!(
            samples.iter().any(|q| q.widget == f.group
                && q.kind == "update"
                && q.transform
                    == if frame == 1 {
                        Affine::IDENTITY
                    } else {
                        Affine::translate((40., 15.))
                    }),
            "ancestor UpdateCtx must use selected geometry too"
        );
        drop(samples);
        f.root.handle_pointer_event(mouse("up", point));
    }
    println!(
        "geometry queries: real QueryCtx cursor, pointer UpdateCtx and EventCtx.get_raw use frame 1 until accepted frame 2; ordinary QueryCtx/MutateCtx RawCtx remain live"
    );
}
#[test]
fn cursor_update_and_raw_share_selected_geometry() {
    geometry_queries();
}
#[test]
fn actual_slider_frozen_input() {
    frozen_input();
}
#[test]
fn actual_slider_cancellation() {
    cancellation();
}
#[test]
fn actual_slider_dpi() {
    dpi();
}

pub fn topology_and_skin() {
    let trace = Rc::new(RefCell::new(vec![]));
    let mut inner = StageGroup::new(trace.clone(), 0.5, Rect::new(0., 0., 60., 55.));
    let (bottom, _) = image_button(trace.clone(), false);
    let bottom_id = inner.add(bottom, Point::ZERO, Size::new(60., 50.));
    let (top, image_id) = image_button(trace.clone(), false);
    let top_id = inner.add(top, Point::ZERO, Size::new(60., 50.));
    let inner = inner.prepare();
    let inner_id = inner.id();
    let mut cast = StageGroup::new(trace.clone(), 0.5, Rect::new(0., 0., 90., 60.));
    cast.add(inner, Point::ZERO, Size::new(90., 60.));
    let cast = cast.prepare().with_transform(transform());
    let cast_id = cast.id();
    let mut shell = StageGroup::new(trace.clone(), 1., Rect::new(0., 0., 400., 300.));
    shell.add(cast, Point::ZERO, Size::new(90., 60.));
    let shell = shell.prepare();
    let shell_id = shell.id();
    let (mut root, signals) = root(shell);
    let mut gate = Gate::default();
    let first = gate.record(&mut root, 1);
    gate.resolve(&mut root, &first, Outcome::Accepted(1));
    let world = transform() * Point::new(30., 20.);
    let clicks = || {
        signals
            .borrow_mut()
            .drain(..)
            .filter_map(|s| match s {
                reel_ui::app::RenderRootSignal::Action(a, id)
                    if a.downcast_ref::<ButtonPress>().is_some() =>
                {
                    Some(id)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    root.handle_pointer_event(mouse("move", world));
    root.handle_pointer_event(mouse("up", world));
    assert!(clicks().is_empty(), "orphan release must not click");
    // Project the upstream control's real state to the author's ordinary Image node.
    let skin = |root: &mut RenderRoot| {
        let button = root.get_widget(top_id).unwrap();
        let color = if button.ctx().is_active() {
            [0, 255, 0, 255]
        } else if button.ctx().is_hovered() {
            [255, 255, 0, 255]
        } else {
            [0, 0, 255, 255]
        };
        root.edit_widget(image_id, |mut w| {
            Image::set_image_data(&mut w.downcast::<Image>(), image_data(color))
        });
        let (plan, _) = root.redraw();
        let mut scene = Scene::new();
        plan.replay_into(&mut scene).unwrap();
        let img = VelloCpuRenderer::new(400, 300)
            .render_scene(&scene, 400, 300)
            .unwrap();
        let i = (world.y.floor() as usize * 400 + world.x.floor() as usize) * 4;
        assert_eq!(&img.data[i..i + 3], &color[..3]);
    };
    skin(&mut root);
    root.handle_pointer_event(mouse("down", world));
    skin(&mut root);
    root.edit_widget(cast_id, |mut w| {
        w.ctx.set_transform(Affine::translate((250., 40.)))
    });
    root.edit_widget(inner_id, |mut w| {
        let mut group = w.downcast::<StageGroup>();
        group.widget.children.swap(0, 1);
        group.widget.clip = Rect::new(0., 0., 15., 15.);
        group.ctx.request_layout();
    });
    let pending = gate.record(&mut root, 2);
    assert!(!gate.resolve(&mut root, &pending, Outcome::Error));
    trace.borrow_mut().clear();
    root.handle_pointer_event(mouse("up", world));
    assert_eq!(
        clicks(),
        vec![top_id],
        "frozen top control receives the paired release once"
    );
    let observations = trace.borrow();
    assert_eq!(
        observations.iter().map(|o| o.widget).collect::<Vec<_>>(),
        vec![top_id, inner_id, cast_id, shell_id]
    );
    assert!(observations.iter().all(|o| o.target == top_id));
    assert!(
        observations[0]
            .local
            .unwrap()
            .distance(Point::new(30., 20.))
            < 1e-9
    );
    drop(observations);
    assert!(
        root.get_widget(top_id).unwrap().ctx().is_hovered(),
        "hover must ignore current transform/order/clip"
    );
    assert!(gate.resolve(&mut root, &pending, Outcome::Accepted(2)));
    root.handle_pointer_event(mouse("down", Point::new(260., 50.)));
    assert_eq!(
        root.pointer_capture_target(),
        Some(bottom_id),
        "acceptance switches reverse painter order"
    );
    root.handle_pointer_event(mouse("up", Point::new(260., 50.)));
    assert_eq!(clicks(), vec![bottom_id]);
    root.handle_pointer_event(mouse("move", Point::new(280., 60.)));
    assert!(
        !root.get_widget(bottom_id).unwrap().ctx().is_hovered(),
        "accepted rectangular clip participates in hover"
    );
    println!(
        "button: orphan up=0, paired up=1; author image reflects hover/active; frozen rotation/scale/clip/order/ancestor route survive failed present; acceptance switches all"
    );
}
pub fn singular() {
    let trace = Rc::new(RefCell::new(vec![]));
    let mut group = StageGroup::new(trace.clone(), 1., Rect::new(0., 0., 400., 300.));
    let (button, _) = image_button(trace, false);
    let id = group.add(
        button.with_transform(Affine::scale_non_uniform(0., 1.)),
        Point::new(30., 30.),
        Size::new(60., 50.),
    );
    let (mut root, _) = root(group.prepare());
    root.redraw();
    let view = Arc::new(root.capture_presented_geometry(super::limits()).unwrap());
    assert_ne!(view.hit_test(Point::new(30., 40.)), Some(id));
    root.set_input_geometry_source(reel_ui::app::InputGeometrySource::Presented(Some(view)));
    root.handle_pointer_event(mouse("down", Point::new(30., 40.)));
    assert_eq!(root.pointer_capture_target(), None);
    println!("singular affine: excluded from snapshot hit/capture");
}
pub fn scaled_measure() {
    for count in [32, 256] {
        let trace = Rc::new(RefCell::new(vec![]));
        let mut group = StageGroup::new(trace.clone(), 1., Rect::new(0., 0., 1920., 1080.));
        for i in 0..count {
            group.add(
                Observed {
                    inner: Slider::new(0., 1., 0.),
                    trace: trace.clone(),
                    isolated: false,
                    queries: None,
                }
                .prepare(),
                Point::new((i % 16) as f64 * 100., (i / 16) as f64 * 30.),
                Size::new(100., 30.),
            );
        }
        let (mut root, signals) = root(group.prepare());
        root.handle_window_event(WindowEvent::Resize(PhysicalSize::new(1920, 1080)));
        root.redraw();
        let view = Arc::new(root.capture_presented_geometry(super::limits()).unwrap());
        root.set_input_geometry_source(reel_ui::app::InputGeometrySource::Presented(Some(
            view.clone(),
        )));
        let p = Point::new(
            ((count - 1) % 16) as f64 * 100. + 25.,
            ((count - 1) / 16) as f64 * 30. + 10.,
        );
        root.handle_pointer_event(mouse("down", p));
        let mut inputs = vec![];
        let mut captures = vec![];
        let mut misses = vec![];
        for i in 0..1100 {
            trace.borrow_mut().clear();
            signals.borrow_mut().clear();
            let t = Instant::now();
            root.handle_pointer_event(mouse("move", p + ((i % 50) as f64, 0.)));
            if i >= 100 {
                inputs.push(t.elapsed().as_nanos());
            }
            let t = Instant::now();
            std::hint::black_box(Arc::new(
                root.capture_presented_geometry(super::limits()).unwrap(),
            ));
            if i >= 100 {
                captures.push(t.elapsed().as_nanos());
            }
            let t = Instant::now();
            std::hint::black_box(view.hit_test(Point::new(1910., 1000.)));
            if i >= 100 {
                misses.push(t.elapsed().as_nanos());
            }
        }
        inputs.sort_unstable();
        captures.sort_unstable();
        misses.sort_unstable();
        println!(
            "measure 1080p contract tree: controls={count} widgets={} payload_lower_bound={}B; pointer p95={}ns capture p95={}ns miss_scan p95={}ns",
            view.len(),
            view.retained_bytes(),
            inputs[950],
            captures[950],
            misses[950]
        );
    }
}
#[test]
fn actual_button_topology_and_skin() {
    topology_and_skin();
}
#[test]
fn singular_transform_is_not_hittable() {
    singular();
}
