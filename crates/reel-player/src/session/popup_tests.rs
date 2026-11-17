//! Real author callbacks use the same accepted frames in floating stage layers.
use super::tests::{mount_with_font, mouse, session};
use super::*;
use crate::aside_session::{AsideSessionUi, StoryProgress, StoryState};
use reel_core::identity::{FrameId, SurfaceEpoch};
use reel_engine::aside::{SliceBudget, VmLimits, source};
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::{BridgeLimits, NoticeResult, PresentationNotice, PresentationOutcome, RecordedSource},
    dpi::PhysicalSize,
    stage::{NodeId, NodeKind},
    widgets::Canvas,
};
use std::sync::Arc;
fn story(code: &str, encoded: bool) -> AsideSessionUi {
    let mut compiled = source::compile_story(code, "popup_player", 1, "main").unwrap();
    if encoded {
        use aside_vm::bytecode as bc;
        let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
        compiled.program = bc::verify(
            bc::decode(&bytes, bc::Limits::default()).unwrap(),
            bc::Limits::default(),
        )
        .unwrap();
    }
    let assets = crate::assets::tests::prepared_effect_assets(&compiled.effects);
    AsideSessionUi::from_compiled(session(), compiled, "en", VmLimits::default())
        .unwrap()
        .with_assets(assets)
}
fn inside(frame: &RecordedSource, child: WidgetId, ancestor: WidgetId) -> bool {
    let mut record = frame.geometry().get(child).unwrap();
    loop {
        if record.id == ancestor {
            return true;
        }
        match record.parent {
            Some(index) => record = &frame.geometry().records()[index],
            None => return false,
        }
    }
}
fn bounds(frame: &RecordedSource, widget: WidgetId) -> Rect {
    let geometry = frame.geometry().get(widget).unwrap();
    geometry
        .window_transform
        .transform_rect_bbox(geometry.size.to_rect())
}
fn button_point(
    driver: &AsideSessionUi,
    bridge: &PresentationBridge,
    root: WidgetId,
    frame: &RecordedSource,
    label: &str,
) -> Point {
    let node = node(
        driver,
        |kind| matches!(kind,NodeKind::Button { label: Some(text),..} if text == label),
    );
    bounds(
        frame,
        SessionUi::projected_node(bridge, root, node).unwrap(),
    )
    .center()
}
fn drive(driver: &mut AsideSessionUi) {
    for _ in 0..10_000 {
        let progress = driver
            .tick(
                SliceBudget {
                    instructions: 3,
                    ..Default::default()
                },
                1,
                || 0,
            )
            .unwrap();
        assert_eq!(driver.state(), StoryState::Running, "{progress:?}");
        if matches!(
            progress,
            StoryProgress::Idle | StoryProgress::Vm(reel_engine::aside::RunOutcome::Waiting(_))
        ) && !driver.has_runnable_tasks()
        {
            return;
        }
    }
    panic!("popup callback did not settle");
}
fn present(
    driver: &mut AsideSessionUi,
    bridge: &mut PresentationBridge,
    root: WidgetId,
    frame: u64,
) -> RecordedSource {
    driver.sync(bridge, root).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while driver.ui().images_pending() {
        assert!(
            std::time::Instant::now() < deadline,
            "prepared source effects did not become ready"
        );
        std::thread::yield_now();
        driver.sync(bridge, root).unwrap();
    }
    let (recording, receipt) = driver
        .record(bridge, root, FrameId(frame))
        .unwrap()
        .expect("stable frame");
    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            recording.key(),
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(frame))
        )),
        NoticeResult::Accepted
    );
    driver.presented(bridge, &receipt).unwrap();
    recording
}
fn mount(driver: &AsideSessionUi, canvas: bool) -> (PresentationBridge, WidgetId) {
    let font = Some(reel_ui::peniko::Blob::new(Arc::new(
        include_bytes!("../../tests/fixtures/font-weight/400.ttf").to_vec(),
    )));
    if !canvas {
        return mount_with_font(driver.ui(), font);
    }
    let widget = driver.widget().prepare();
    let id = widget.id();
    let root = RenderRoot::new(
        Canvas::new(widget, [320, 210]).prepare(),
        |_| {},
        RenderRootOptions {
            default_properties: Arc::new(DefaultProperties::new()),
            use_system_fonts: false,
            size_policy: WindowSizePolicy::User,
            size: PhysicalSize::new(640, 420),
            scale_factor: 1.,
            default_font: font,
        },
    );
    (
        PresentationBridge::new(
            root,
            SurfaceEpoch(1),
            BridgeLimits {
                geometry: reel_ui::app::GeometryLimits { max_bytes: 1 << 20 },
                max_candidates: 4,
                max_bridge_bytes: 64 << 10,
                max_snapshot_bytes: 8 << 20,
            },
        )
        .unwrap(),
        id,
    )
}
fn counter(driver: &AsideSessionUi) -> String {
    driver
        .adapter()
        .stage()
        .nodes()
        .find_map(|node| match &node.kind {
            NodeKind::Text(text) if text.as_ref().starts_with("counts:") => Some(text.to_string()),
            _ => None,
        })
        .unwrap()
}
fn node(driver: &AsideSessionUi, predicate: impl Fn(&NodeKind) -> bool) -> NodeId {
    driver
        .adapter()
        .stage()
        .nodes()
        .find(|n| predicate(&n.kind))
        .unwrap()
        .id
}
fn click(driver: &mut AsideSessionUi, bridge: &mut PresentationBridge, point: Point) {
    for kind in ["move", "down", "up"] {
        driver
            .handle_pointer(bridge, mouse(kind, (point.x, point.y)))
            .unwrap();
    }
    drive(driver);
}
#[test]
fn source_and_asb_inline_text_fragments_preserve_edge_space_advance() {
    const SOURCE: &str = r##"fn main(){
        stage.replace(stack(width:640,height:420){
            row(x:20,y:10,gap:0){
                text("Add ",font_size:20,color:"#ff0000");
                button(decorated:false,padding:0){text("Polychrome",font_size:20);}
            }
            text("Add Polychrome",x:20,y:50,font_size:20);
            row(x:20,y:90,gap:0){text("  A  ",font_size:20);text(" B",font_size:20);}
            text("  A   B",x:20,y:130,font_size:20);
            row(x:20,y:170,gap:0){text("   ",font_size:20);text("C",font_size:20);}
            text("   C",x:20,y:210,font_size:20);
            row(x:20,y:250,gap:0){text("增强 ",font_size:20);text("倍率",font_size:20);}
            text("增强 倍率",x:20,y:290,font_size:20);
        });
        narrator("wait");
    }"##;
    let mut fonts = vec![reel_ui::peniko::Blob::from(
        include_bytes!(
            "../../../../examples/author_arcade/resources/fonts/AtkinsonHyperlegible-Regular.ttf"
        )
        .to_vec(),
    )];
    if let Some(path) = std::env::var_os("REEL_SESSION_FONT") {
        fonts.push(std::fs::read(path).unwrap().into());
    }
    for font in fonts {
        for encoded in [false, true] {
            let mut driver = story(SOURCE, encoded);
            drive(&mut driver);
            let (mut bridge, root) = mount_with_font(driver.ui(), Some(font.clone()));
            let frame = present(&mut driver, &mut bridge, root, 1);
            let base = SessionUi::stage_widget(&bridge, root).unwrap();
            let geometry = |value: &str| {
                let text = node(
                    &driver,
                    |kind| matches!(kind, NodeKind::Text(text) if text.as_ref() == value),
                );
                driver
                    .ui()
                    .read_geometry(
                        &bridge,
                        &frame,
                        root,
                        base,
                        reel_engine::aside::GeometryQuery {
                            sequence: 1,
                            node: text,
                            relative_to: None,
                            reveal_margins: None,
                        },
                    )
                    .unwrap()
            };
            for (left_text, right_text, combined_text) in [
                ("Add ", "Polychrome", "Add Polychrome"),
                ("  A  ", " B", "  A   B"),
                ("   ", "C", "   C"),
                ("增强 ", "倍率", "增强 倍率"),
            ] {
                let left = geometry(left_text);
                let right = geometry(right_text);
                let combined = geometry(combined_text);
                assert!(left.bounds[2] > 0., "space-only fragments retain advance");
                assert!((left.bounds[0] + left.bounds[2] - right.bounds[0]).abs() < 0.001);
                // Layout rounds node edges to pixels; text carets retain full advances.
                assert!(
                    (left.bounds[2] + right.bounds[2] - combined.bounds[2]).abs() <= 1.,
                    "{combined_text:?}, encoded={encoded}: {} + {} != {}",
                    left.bounds[2],
                    right.bounds[2],
                    combined.bounds[2]
                );
                let left_caret = left.text_caret.unwrap();
                let right_caret = right.text_caret.unwrap();
                let combined_caret = combined.text_caret.unwrap();
                assert!((left_caret[0] - right.bounds[0]).abs() <= 1.);
                assert!(
                    (right_caret[0] - combined_caret[0]).abs() <= 1.,
                    "fragment endpoints follow the same pixel snapping as their nodes"
                );
                assert!(
                    ((left_caret[0] - left.bounds[0]) + (right_caret[0] - right.bounds[0])
                        - (combined_caret[0] - combined.bounds[0]))
                        .abs()
                        < 0.001,
                    "{combined_text:?}, encoded={encoded}: unrounded text advances agree"
                );
            }
        }
    }
}

#[test]
fn source_and_asb_nonmodal_hover_popup_preserves_fixed_and_natural_anchors() {
    const SOURCE: &str = r#"fn main(){
        let data=model({built:0,closed:0,clicks:0,hover:false,caption:"Help"});
        let mut hint=null;
        let anchor=button(ANCHOR_SIZE decorated:false,padding:0,
            on_click:||{data.clicks+=1;data.caption="Updated Help";},
            on_interaction:|e|{
                data.hover=e.hover;
                if e.hover && hint==null {
                    data.built+=1;
                    hint=e.target.add(popup(placement:"top-start",offset:12,
                        modal:false){
                            text(derive(||data.caption),width:120,height:60);
                            PORTAL_CONTROL
                        });
                } else if !e.hover && hint!=null {
                    data.closed+=1;hint.remove();hint=null;
                }
            }){ANCHOR_CONTENT};
        stage.replace(stack(width:640,height:420){
            column(x:200,y:200,width:200){row(width:200,justify:"center"){anchor;}}
            text(derive(||f"counts:{data.built}:{data.closed}:{data.clicks}:{data.hover}"),y:380);
        });
        narrator("wait");
    }"#;
    for (fixed, encoded, image) in [
        (false, false, false),
        (false, true, false),
        (true, false, false),
        (true, true, false),
        (true, false, true),
        (true, true, true),
    ] {
        let source = SOURCE
            .replace(
                "ANCHOR_CONTENT",
                if image {
                    r#"image("panels.png",width:80,height:80,source_rect:(0,0,68,68));"#
                } else {
                    r#"text("AAA",font:"Reel Weight Fixture",font_size:30);"#
                },
            )
            .replace(
                "ANCHOR_SIZE",
                if fixed { "width:80,height:80," } else { "" },
            )
            .replace(
                "PORTAL_CONTROL",
                if fixed { "" } else { "button(\"Hint\");" },
            );
        let mut driver = story(&source, encoded);
        if image {
            driver = driver.with_assets(crate::assets::Assets::directory(
                crate::assets::AssetOptions {
                    root: std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../examples/assets"),
                    ..Default::default()
                },
            ));
        }
        drive(&mut driver);
        let anchor = node(&driver, |kind| matches!(kind, NodeKind::Button { .. }));
        let (mut bridge, root) = mount(&driver, false);
        let first = present(&mut driver, &mut bridge, root, 1);
        let widget = SessionUi::projected_node(&bridge, root, anchor).unwrap();
        let original = bounds(&first, widget);
        if fixed {
            assert_eq!(original.size(), reel_ui::kurbo::Size::new(80., 80.));
        }
        let point = original.center();
        let revision = first.geometry().get(widget).unwrap().input_revision;
        let binding = driver.ui().binding.borrow().buttons[&anchor];
        let mut hint_button = None;
        for (index, kind) in ["move", "down", "up"].into_iter().enumerate() {
            driver
                .handle_pointer(&mut bridge, mouse(kind, (point.x, point.y)))
                .unwrap();
            drive(&mut driver);
            let frame = present(&mut driver, &mut bridge, root, index as u64 + 2);
            drive(&mut driver);
            eprintln!(
                "fixed={fixed} encoded={encoded} {kind}: bounds={:?}, hit={:?}, input_revision={}, binding={}",
                bounds(&frame, widget),
                frame.geometry().hit_test(point),
                frame.geometry().get(widget).unwrap().input_revision,
                driver.ui().binding.borrow().buttons[&anchor]
            );
            assert_eq!(
                bounds(&frame, widget),
                original,
                "portal preserves anchor size"
            );
            assert_eq!(frame.geometry().hit_test(point), Some(widget));
            assert_eq!(
                frame.geometry().get(widget).unwrap().input_revision,
                revision
            );
            assert_eq!(driver.ui().binding.borrow().buttons[&anchor], binding);
            let popup = node(&driver, |kind| matches!(kind, NodeKind::Popup(_)));
            let popup = SessionUi::projected_node(&bridge, root, popup).unwrap();
            assert_eq!(bounds(&frame, popup).y1, original.y0 - 12.);
            if !fixed {
                let hint = node(
                    &driver,
                    |kind| matches!(kind, NodeKind::Button { label: Some(label), .. } if label == "Hint"),
                );
                assert!(driver.ui().binding.borrow().buttons.contains_key(&hint));
                hint_button = Some(hint);
            }
            assert_eq!(
                counter(&driver),
                if kind == "up" {
                    "counts:1:0:1:true"
                } else {
                    "counts:1:0:0:true"
                },
                "{kind}, fixed={fixed}, encoded={encoded}"
            );
        }
        node(
            &driver,
            |kind| matches!(kind, NodeKind::Text(text) if text.as_ref() == "Updated Help"),
        );
        for sequence in 5..=8 {
            let frame = present(&mut driver, &mut bridge, root, sequence);
            drive(&mut driver);
            assert_eq!(bounds(&frame, widget), original);
            assert_eq!(frame.geometry().hit_test(point), Some(widget));
            assert_eq!(
                frame.geometry().get(widget).unwrap().input_revision,
                revision
            );
            assert_eq!(driver.ui().binding.borrow().buttons[&anchor], binding);
            assert_eq!(
                counter(&driver),
                "counts:1:0:1:true",
                "stationary frame {sequence}, fixed={fixed}, encoded={encoded}"
            );
        }
        driver
            .handle_pointer(&mut bridge, mouse("move", (20., 350.)))
            .unwrap();
        drive(&mut driver);
        present(&mut driver, &mut bridge, root, 9);
        assert_eq!(counter(&driver), "counts:1:1:1:false");
        assert_eq!(SessionUi::physical_stages(&bridge, root).count(), 1);
        if let Some(hint) = hint_button {
            assert!(
                !driver.ui().binding.borrow().buttons.contains_key(&hint),
                "retired popup buttons release their bindings"
            );
        }
    }
}

#[test]
fn tooltip_follows_hover_and_visible_focus_without_capturing_input() {
    use reel_ui::core::{
        TextEvent,
        keyboard::{Key, NamedKey},
    };

    let source = r#"fn main() {
        let data=model({clicks:0});
        stage.replace(stack(width:640,height:420) {
            button("Help", x:40,y:50,width:120,height:40,on_click:|| {data.clicks+=1;}) {
                tooltip(offset:-25,width:120,height:40) {
                    button("Passive",width:120,height:40);
                };
            };
            text(derive(||f"counts:{data.clicks}"),y:180);
        });
        narrator("wait");
    }"#;
    for encoded in [false, true] {
        let mut driver = story(source, encoded);
        drive(&mut driver);
        let anchor = node(
            &driver,
            |kind| matches!(kind, NodeKind::Button { label: Some(label), .. } if label == "Help"),
        );
        let hint = node(&driver, |kind| matches!(kind, NodeKind::Popup(_)));
        let (mut bridge, root) = mount(&driver, false);
        let first = present(&mut driver, &mut bridge, root, 1);
        let widget = SessionUi::projected_node(&bridge, root, anchor).unwrap();
        assert!(SessionUi::projected_node(&bridge, root, hint).is_none());
        let at = bounds(&first, widget).center();
        driver
            .handle_pointer(&mut bridge, mouse("move", (at.x, at.y)))
            .unwrap();
        drive(&mut driver);
        let shown = present(&mut driver, &mut bridge, root, 2);
        let popup = SessionUi::projected_node(&bridge, root, hint).unwrap();
        assert!(bounds(&shown, popup).contains(at));
        assert_eq!(shown.geometry().hit_test(at), Some(widget));
        for kind in ["down", "up"] {
            driver
                .handle_pointer(&mut bridge, mouse(kind, (at.x, at.y)))
                .unwrap();
        }
        drive(&mut driver);
        assert_eq!(counter(&driver), "counts:1");
        driver
            .handle_pointer(&mut bridge, mouse("move", (20., 350.)))
            .unwrap();
        drive(&mut driver);
        present(&mut driver, &mut bridge, root, 3);
        assert!(SessionUi::projected_node(&bridge, root, hint).is_none());
        driver
            .handle_text(&mut bridge, TextEvent::key_down(Key::Named(NamedKey::Tab)))
            .unwrap();
        drive(&mut driver);
        present(&mut driver, &mut bridge, root, 4);
        assert!(SessionUi::projected_node(&bridge, root, hint).is_some());
    }
}

#[test]
fn source_and_asb_popups_float_outside_parent_clip_and_route_actual_callbacks() {
    const SOURCE: &str = r#"fn main(){
        let data=model({hits:0,closed:0,shift:0});
        let trigger=button("trigger",width:80,height:40);
        trigger.add(popup(width:120,height:70,offset:4,opacity:0.5,
            on_dismiss:|event|{data.closed+=1;event.target.remove();}){
            button("option",width:120,height:35,on_click:||{data.hits+=1;data.shift=110;});
            text("AAA",x:7,y:40,width:60,height:30,font:"Reel Weight Fixture",font_size:20);
        });
        stage.replace(stack(width:320,height:210){
            stack(x:derive(||20+data.shift),y:30,width:80,height:40,clip:true,opacity:0,mask:mask.param("progress"),mask_progress:0){trigger;}
            text(derive(||f"counts:{data.hits}:{data.closed}"),y:180);
        });
        narrator("wait");
    }"#;
    for encoded in [false, true] {
        for canvas in [false, true] {
            let mut driver = story(SOURCE, encoded);
            drive(&mut driver);
            let popup = node(&driver, |k| matches!(k, NodeKind::Popup(_)));
            let option = node(
                &driver,
                |k| matches!(k,NodeKind::Button{label:Some(label),..} if label=="option"),
            );
            let (mut bridge, root) = mount(&driver, canvas);
            let frame = present(&mut driver, &mut bridge, root, 1);
            let widget = SessionUi::projected_node(&bridge, root, popup).unwrap();
            let geometry = frame.geometry().get(widget).unwrap();
            let first_bounds = geometry
                .window_transform
                .transform_rect_bbox(geometry.size.to_rect());
            let factor = if canvas { 2. } else { 1. };
            assert_eq!(
                first_bounds,
                Rect::new(20. * factor, 74. * factor, 140. * factor, 144. * factor)
            );
            let base = SessionUi::stage_widget(&bridge, root).unwrap();
            let caption = node(
                &driver,
                |kind| matches!(kind,NodeKind::Text(text) if text.as_ref()=="AAA"),
            );
            let anchor = node(
                &driver,
                |kind| matches!(kind,NodeKind::Button{label:Some(text),..} if text=="trigger"),
            );
            let queried = driver
                .ui()
                .read_geometry(
                    &bridge,
                    &frame,
                    root,
                    base,
                    reel_engine::aside::GeometryQuery {
                        sequence: 1,
                        node: caption,
                        relative_to: Some(anchor),
                        reveal_margins: None,
                    },
                )
                .unwrap();
            assert_eq!(
                queried.bounds,
                [7., 84., 60., 30.],
                "cross-layer relative coordinates use the accepted frame"
            );
            let text_end = queried
                .text_end
                .expect("popup text retains its real text endpoint");
            assert!(text_end[0] > 7. && text_end[1] >= 84., "{text_end:?}");
            let caret = queried.text_caret.expect("text caret extent");
            assert_eq!(caret[0], text_end[0]);
            assert_eq!(caret[1] + caret[3], text_end[1]);
            assert!(caret[3] > 0. && caret[2] == 0.);
            let content = SessionUi::transition_target(&bridge, root, None).unwrap();
            let range = |id| {
                frame
                    .plan()
                    .subtrees
                    .iter()
                    .find(|range| range.widget == id)
                    .unwrap()
                    .commands
                    .clone()
            };
            let whole = range(content);
            assert!(whole.start <= range(base).start && range(base).end <= whole.end);
            assert!(
                whole.start <= range(widget).start && range(widget).end <= whole.end,
                "whole-stage drawing is one balanced range including popup content"
            );
            assert!(
                frame
                    .plan()
                    .layers
                    .iter()
                    .flat_map(|layer| layer.command_owners())
                    .filter(|commands| commands.owner == root)
                    .all(|commands| commands.commands.end <= whole.start
                        || commands.commands.start >= whole.end),
                "the SessionWidget background is outside the author content range"
            );
            assert!(
                frame.plan().layers.iter().any(|layer| {
                    let reel_ui::app::VisualLayerKind::Scene(scene) = &layer.kind else {
                        return false;
                    };
                    layer
                        .groups()
                        .iter()
                        .filter(|group| group.owner == widget)
                        .any(|group| {
                            let reel_ui::imaging::record::Command::PushGroup(id) =
                                scene.commands()[group.command]
                            else {
                                return false;
                            };
                            scene.group(id).composite.alpha == 0.5
                        })
                }),
                "the popup retains its own opacity composite"
            );
            assert!(
                bridge
                    .root()
                    .get_widget(base)
                    .unwrap()
                    .downcast::<StageWidget>()
                    .unwrap()
                    .node_widget(option)
                    .is_none(),
                "portal content has one physical owner"
            );
            let gate = SessionUi::transition_target(&bridge, root, Some(anchor)).unwrap();
            bridge.set_input_gates(&[gate]).unwrap();
            let target = frame
                .geometry()
                .get(SessionUi::projected_node(&bridge, root, option).unwrap())
                .unwrap();
            click(
                &mut driver,
                &mut bridge,
                target.window_transform * target.size.to_rect().center(),
            );
            let second = present(&mut driver, &mut bridge, root, 2);
            assert_eq!(counter(&driver), "counts:1:0");
            assert_eq!(
                bounds(&second, widget),
                Rect::new(130. * factor, 74. * factor, 250. * factor, 144. * factor),
                "the new anchor layout positions its popup in the same candidate"
            );
            assert_eq!(
                bounds(&frame, widget),
                first_bounds,
                "the previous candidate keeps its own immutable location"
            );
            bridge.set_input_gates(&[content]).unwrap();
            click(&mut driver, &mut bridge, Point::new(630., 410.));
            present(&mut driver, &mut bridge, root, 3);
            assert_eq!(
                counter(&driver),
                "counts:1:0",
                "the whole author-content gate also blocks popup dismissal"
            );
            click(&mut driver, &mut bridge, Point::new(630., 410.));
            present(&mut driver, &mut bridge, root, 4);
            assert_eq!(counter(&driver), "counts:1:1");
            assert!(SessionUi::projected_node(&bridge, root, popup).is_none());
            assert_eq!(SessionUi::physical_stages(&bridge, root).count(), 1);
            assert!(
                second.geometry().get(widget).is_some(),
                "accepted frame keeps immutable popup geometry after retirement"
            );
        }
    }
}

#[test]
fn source_and_asb_nested_popups_and_internal_modals_share_the_active_input_scope() {
    const SOURCE: &str = r#"fn main(){
        let data=model({inner:0,nested:0,closed:0,base:0,outer:0});
        let inside=button("nested-anchor",y:40,width:80,height:30);
        inside.add(popup(width:110,height:40,offset:4,
            on_dismiss:|e|{data.closed+=1;e.target.remove();}){
            button("nested",width:110,height:40,on_click:||{data.nested+=1;});
        });
        let anchor=button("outer-anchor",x:20,y:20,width:80,height:30);
        anchor.add(popup(width:160,height:110,modal:false,
            on_dismiss:|e|{data.outer+=1;}){
            stack(width:160,height:110,modal:true){
                button("inner",width:80,height:30,on_click:||{data.inner+=1;});
                inside;
            }
        });
        stage.replace(stack(width:640,height:420){
            anchor;
            button("base",x:450,y:20,width:100,height:40,modal:true,on_click:||{data.base+=1;});
            text(derive(||f"counts:{data.inner}:{data.nested}:{data.closed}:{data.base}:{data.outer}"),y:380);
        });
        narrator("wait");
    }"#;
    for encoded in [false, true] {
        let mut driver = story(SOURCE, encoded);
        drive(&mut driver);
        let (mut bridge, root) = mount(&driver, false);
        let first = present(&mut driver, &mut bridge, root, 1);
        let outer = bridge
            .root()
            .get_widget(root)
            .unwrap()
            .downcast::<SessionWidget>()
            .unwrap()
            .popup_layers[0]
            .node;
        let gate = SessionUi::transition_target(&bridge, root, Some(outer)).unwrap();
        bridge.set_input_gates(&[gate]).unwrap();
        let nested = button_point(&driver, &bridge, root, &first, "nested");
        click(&mut driver, &mut bridge, nested);
        let second = present(&mut driver, &mut bridge, root, 2);
        assert_eq!(counter(&driver), "counts:0:1:0:0:0");
        let inner = button_point(&driver, &bridge, root, &second, "inner");
        click(&mut driver, &mut bridge, inner);
        let third = present(&mut driver, &mut bridge, root, 3);
        assert_eq!(
            counter(&driver),
            "counts:0:1:1:0:0",
            "outside closes only the higher nested popup, without clicking through"
        );
        let inner = button_point(&driver, &bridge, root, &third, "inner");
        click(&mut driver, &mut bridge, inner);
        let fourth = present(&mut driver, &mut bridge, root, 4);
        assert_eq!(
            counter(&driver),
            "counts:1:1:1:0:0",
            "the nonmodal popup's ordinary inner modal wins over a later base modal"
        );
        let base = button_point(&driver, &bridge, root, &fourth, "base");
        click(&mut driver, &mut bridge, base);
        present(&mut driver, &mut bridge, root, 5);
        assert_eq!(
            counter(&driver),
            "counts:1:1:1:0:0",
            "the enclosing nonmodal popup does not capture across its inner modal"
        );
    }
}

#[test]
fn whole_stage_retention_groups_popups_once_and_excludes_preview_and_host_background() {
    use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
    use reel_ui::{
        frame::{BuildLimits, FrameBuild, FrameInfo, RetainBuild, StepBudget},
        resources::ResourceSnapshot,
    };
    const SOURCE: &str = r##"fn main(){
        let trigger=stack(x:20,y:30,width:80,height:40,background:"#00ff00");
        trigger.add(popup(width:120,height:70,background:"#ff0000",opacity:0.5));
        stage.replace(stack(width:640,height:420){trigger;});narrator("preview dialogue");
    }"##;
    for preview in [false, true] {
        let mut driver = story(SOURCE, false);
        drive(&mut driver);
        driver.ui_mut().preview = preview;
        driver.ui_mut().projection =
            Projection::from_session(driver.ui().session(), preview).unwrap();
        let (mut bridge, root) = mount_with_font(driver.ui(), None);
        driver.sync(&mut bridge, root).unwrap();
        let (recording, _) = driver
            .record(&mut bridge, root, FrameId(1))
            .unwrap()
            .unwrap();
        let content = SessionUi::transition_target(&bridge, root, None).unwrap();
        let popup = node(&driver, |kind| matches!(kind, NodeKind::Popup(_)));
        let popup = SessionUi::projected_node(&bridge, root, popup).unwrap();
        let anchor = node(&driver, |kind| matches!(kind, NodeKind::Stack));
        let anchor = SessionUi::projected_node(&bridge, root, anchor).unwrap();
        assert!(
            !inside(&recording, popup, anchor),
            "a named target is its independent physical subtree"
        );
        let chrome = bridge
            .root()
            .get_widget(root)
            .unwrap()
            .downcast::<SessionWidget>()
            .unwrap();
        if let Some(preview) = &chrome.preview {
            for id in [
                preview.backdrop.id(),
                preview.title.id(),
                preview.status.id(),
            ] {
                assert!(!inside(&recording, id, content));
            }
        }
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 32,
                max_bytes: 32 << 20,
            },
        );
        let limits = BuildLimits { max_bytes: 8 << 20 };
        let step = StepBudget {
            max_commands: 1,
            max_units: 8192,
            max_bytes: 64 << 10,
        };
        let resources = ResourceSnapshot::capture(std::iter::empty(), &pool).unwrap();
        let mut build = FrameBuild::new(
            recording,
            resources,
            FrameInfo {
                logical_size: [640., 420.],
                physical_size: [640, 420],
            },
            limits,
            &pool,
        )
        .unwrap();
        while !build.step(step).unwrap().complete {}
        let frame = build.finish().unwrap();
        let mut build = RetainBuild::new(&frame, Some(content), limits, &pool).unwrap();
        while !build.step(step).unwrap().complete {}
        let retained = build.finish().unwrap();
        assert!(
            retained
                .drawing()
                .commands()
                .iter()
                .any(|command| command.origin.at.owner == popup)
        );
        assert!(
            retained
                .drawing()
                .commands()
                .iter()
                .any(|command| command.origin.at.owner == anchor)
        );
        assert!(
            retained
                .drawing()
                .commands()
                .iter()
                .all(|command| command.origin.at.owner != root),
            "the session backdrop stays outside the single retained author group"
        );
    }
}

fn settle_images(ui: &mut SessionUi, bridge: &mut PresentationBridge, root: WidgetId) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        ui.sync(bridge, root).unwrap();
        if !ui.images_pending() && !ui.images.assets.needs_poll() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "popup assets did not settle"
        );
        std::thread::yield_now();
    }
}
fn raster_frame(
    ui: &mut SessionUi,
    bridge: &mut PresentationBridge,
    root: WidgetId,
    id: u64,
) -> reel_ui::frame::RenderFrame {
    use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
    use reel_ui::{
        frame::{BuildLimits, FrameBuild, FrameInfo},
        resources::ResourceSnapshot,
    };
    settle_images(ui, bridge, root);
    let (source, receipt) = ui.record(bridge, root, FrameId(id)).unwrap();
    let key = source.key();
    let pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 32,
            max_bytes: 32 << 20,
        },
    );
    let resources =
        ResourceSnapshot::capture(ui.image_resources().iter().cloned().map(Ok), &pool).unwrap();
    let mut build = FrameBuild::new(
        source,
        resources,
        FrameInfo {
            logical_size: [640., 420.],
            physical_size: [640, 420],
        },
        BuildLimits { max_bytes: 8 << 20 },
        &pool,
    )
    .unwrap();
    while !build
        .step(reel_ui::frame::StepBudget {
            max_commands: 1,
            max_units: 8192,
            max_bytes: 64 << 10,
        })
        .unwrap()
        .complete
    {}
    let frame = build.finish().unwrap();
    assert_eq!(
        bridge.resolve(PresentationNotice::new(
            key,
            SurfaceEpoch(1),
            PresentationOutcome::Accepted(FrameId(id))
        )),
        NoticeResult::Accepted
    );
    ui.presented(bridge, &receipt);
    frame
}
fn popup_tree(stage: &mut reel_ui::stage::Stage, children: Vec<NodeId>) -> (NodeId, NodeId) {
    use reel_ui::stage::{NodeProps, PopupView};
    let popup = stage
        .create(
            NodeKind::Popup(PopupView {
                placement: Default::default(),
                offset: 4.,
                listening: false,
                hint: None,
            }),
            NodeProps {
                width: Some(320.),
                height: Some(100.),
                modal: true,
                ..Default::default()
            },
            children,
        )
        .unwrap();
    let anchor = stage
        .create(
            NodeKind::Stack,
            NodeProps {
                x: 30.,
                y: 20.,
                width: Some(80.),
                height: Some(30.),
                ..Default::default()
            },
            vec![popup],
        )
        .unwrap();
    let root = stage
        .create(
            NodeKind::Stack,
            NodeProps {
                width: Some(640.),
                height: Some(420.),
                ..Default::default()
            },
            vec![anchor],
        )
        .unwrap();
    stage.replace(root).unwrap();
    (anchor, popup)
}
#[test]
fn popup_whole_and_prepared_images_share_resources_and_retire_with_their_logical_owner() {
    use reel_ui::frame::{FrameDraw, FrameOp};
    use reel_ui::stage::{ImageSource, NodeProps, Stage};
    for prepared in [false, true] {
        let (mut ui, mut bridge, root, mut stage, kind) = if prepared {
            let (ui, bridge, root, mut stage, image, _, _) = super::prepared_image_tests::fixture();
            let kind = stage.node(image).unwrap().kind.clone();
            stage.clear();
            (ui, bridge, root, stage, kind)
        } else {
            let ui = SessionUi::new(session()).unwrap().with_asset_options(
                crate::assets::AssetOptions {
                    root: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("../../examples"),
                    ..Default::default()
                },
            );
            let (bridge, root) = mount_with_font(&ui, None);
            (
                ui,
                bridge,
                root,
                Stage::default(),
                NodeKind::Image {
                    sampling: Default::default(),
                    source_rect: None,
                    source: ImageSource::from("assets/panels.png"),
                    origin: None,
                    fit: Default::default(),
                    slice: None,
                    slice_scale: 1.,
                    slice_center: true,
                    repeat: [false; 2],
                    tile_scale: 1.,
                    corner_colors: None,
                    alpha_layout: Default::default(),
                    adjustment: Default::default(),
                },
            )
        };
        let image = stage
            .create(
                kind,
                NodeProps {
                    width: Some(320.),
                    height: Some(64.),
                    ..Default::default()
                },
                vec![],
            )
            .unwrap();
        let (anchor, popup) = popup_tree(&mut stage, vec![image]);
        ui.set_stage(&stage);
        let frame = raster_frame(&mut ui, &mut bridge, root, 1);
        assert!(!ui.image_resources().is_empty());
        assert!(
            frame.commands().iter().any(|command| matches!(
                command.op,
                FrameOp::Draw(FrameDraw::Fill {
                    brush: reel_ui::peniko::Brush::Image(_),
                    ..
                })
            )),
            "both whole and prepared popup images reach the real frame compiler"
        );
        assert!(SessionUi::projected_node(&bridge, root, image).is_some());
        let base = SessionUi::stage_widget(&bridge, root).unwrap();
        assert!(
            bridge
                .root()
                .get_widget(base)
                .unwrap()
                .downcast::<StageWidget>()
                .unwrap()
                .node_widget(image)
                .is_none()
        );
        let removed = stage.remove(anchor).unwrap();
        assert!(removed.contains(&popup));
        ui.set_stage(&stage);
        ui.sync(&mut bridge, root).unwrap();
        assert!(SessionUi::projected_node(&bridge, root, popup).is_none());
        assert!(ui.image_resources().is_empty());
        assert!(
            ui.images.loaded.is_empty(),
            "retirement releases the current resource ownership"
        );
        assert!(
            !frame.resources().leases().is_empty(),
            "an already recorded frame retains its shared leases"
        );
    }
}
#[test]
fn ineligible_popup_images_do_not_block_visible_content_or_load_resources() {
    use reel_ui::stage::{ImageSource, NodeProps, Stage};
    for reason in ["disabled", "root", "cursor"] {
        let mut stage = Stage::default();
        let image = stage
            .create(
                NodeKind::Image {
                    sampling: Default::default(),
                    source_rect: None,
                    source: ImageSource::from("missing.png"),
                    origin: None,
                    fit: Default::default(),
                    slice: None,
                    slice_scale: 1.,
                    slice_center: true,
                    repeat: [false; 2],
                    tile_scale: 1.,
                    corner_colors: None,
                    alpha_layout: Default::default(),
                    adjustment: Default::default(),
                },
                NodeProps::default(),
                vec![],
            )
            .unwrap();
        let (anchor, popup) = popup_tree(&mut stage, vec![image]);
        match reason {
            "disabled" => {
                let n = stage.node(anchor).unwrap();
                let mut props = n.props.clone();
                props.enabled = false;
                stage.update(anchor, n.kind.clone(), props).unwrap();
            }
            "root" => {
                let mut detached = Stage::default();
                let kind = stage.node(image).unwrap().kind.clone();
                let image = detached.create(kind, NodeProps::default(), vec![]).unwrap();
                let old = stage.node(popup).unwrap();
                let popup = detached
                    .create(old.kind.clone(), old.props.clone(), vec![image])
                    .unwrap();
                detached.replace(popup).unwrap();
                stage = detached;
            }
            "cursor" => {
                let n = stage.node(anchor).unwrap();
                stage
                    .update(anchor, NodeKind::Cursor, n.props.clone())
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let mut ui = SessionUi::new(session()).unwrap();
        ui.set_stage(&stage);
        let (mut bridge, root) = mount_with_font(&ui, None);
        ui.sync(&mut bridge, root).unwrap();
        assert!(
            !ui.images_pending(),
            "{reason} popup must not wait for its unused missing image"
        );
        assert!(ui.images.loaded.is_empty());
        assert_eq!(SessionUi::physical_stages(&bridge, root).count(), 1);
    }
}

#[test]
fn popup_without_a_focusable_child_accepts_paired_back_dismissal() {
    use reel_ui::core::keyboard::{Key, NamedKey};
    let mut driver = story(
        r#"fn main(){
        let data=model({closed:0});let anchor=stack(width:40,height:30);
        anchor.add(popup(width:80,height:40,on_dismiss:|e|{data.closed+=1;e.target.remove();}));
        stage.replace(stack{anchor;text(derive(||f"counts:{data.closed}"));});narrator("wait");
    }"#,
        false,
    );
    drive(&mut driver);
    let (mut bridge, root) = mount(&driver, false);
    present(&mut driver, &mut bridge, root, 1);
    driver
        .handle_text(
            &mut bridge,
            TextEvent::key_down(Key::Named(NamedKey::Escape)),
        )
        .unwrap();
    driver
        .handle_text(&mut bridge, TextEvent::key_up(Key::Named(NamedKey::Escape)))
        .unwrap();
    drive(&mut driver);
    present(&mut driver, &mut bridge, root, 2);
    assert_eq!(counter(&driver), "counts:1");
}

#[test]
#[ignore = "serial allocator check of steady popup projection; run with --test-threads=1"]
fn popup_stable_projection_cost() {
    use reel_ui::stage::Stage;
    for popup in [false, true] {
        let mut stage = Stage::default();
        let (_, id) = popup_tree(&mut stage, vec![]);
        if !popup {
            stage.remove(id).unwrap();
        }
        let mut ui = SessionUi::new(session()).unwrap();
        ui.set_stage(&stage);
        let (mut bridge, root) = mount_with_font(&ui, None);
        ui.sync(&mut bridge, root).unwrap();
        super::tests::accept(&mut ui, &mut bridge, root, 1);
        let before = SessionUi::projected_node(&bridge, root, id);
        let start = std::time::Instant::now();
        let stats = allocation_counter::measure(|| {
            for _ in 0..120 {
                ui.sync(&mut bridge, root).unwrap();
            }
        });
        let elapsed = start.elapsed();
        assert_eq!(SessionUi::projected_node(&bridge, root, id), before);
        assert_eq!(stats.count_total, 0, "stable popup sync does not allocate");
        assert_eq!(stats.bytes_total, 0);
        eprintln!(
            "popup={popup} stable_syncs=120 allocations={} allocated_bytes={} elapsed_us={}",
            stats.count_total,
            stats.bytes_total,
            elapsed.as_micros()
        );
    }
}
