use super::{
    models::{drive, prepared},
    *,
};
use reel_ui::{
    kurbo::{Size, Vec2},
    stage::{ControlValue, NodeChange, NodeEvent, NodeKind, NodeScroll},
};

fn ids(vm: &SessionVm) -> (reel_ui::stage::NodeId, reel_ui::stage::NodeId) {
    let bar = vm
        .stage()
        .nodes()
        .find(|n| matches!(n.kind, NodeKind::ScrollBar(_)))
        .unwrap()
        .id;
    let scroll = vm
        .stage()
        .nodes()
        .find(|n| matches!(n.kind, NodeKind::Scroll { .. }))
        .unwrap()
        .id;
    (bar, scroll)
}
fn metrics(vm: &mut SessionVm, s: &mut Session, content: f64, offset: f64) {
    let (_, scroll) = ids(vm);
    let host = vm.stage.value(vm.vm.heap(), scroll).unwrap();
    let previous = stage::number(
        vm.vm
            .heap()
            .host_property(aside_vm::PropertyId {
                host,
                field: stage::scroll::ScrollField::Y.index() as u32,
            })
            .unwrap(),
    )
    .unwrap();
    vm.interactions(
        s,
        &[NodeEvent::Scroll(NodeScroll {
            input: false,
            node: scroll,
            requested: Vec2::new(0., previous),
            offset: Vec2::new(0., offset),
            viewport: Size::new(100., 100.),
            content: Size::new(100., content),
        })],
    )
    .unwrap();
    drive(vm, s, true);
    vm.sync_stage(s).unwrap();
}
fn click(vm: &mut SessionVm, s: &mut Session, label: &str) {
    let id = vm
        .stage()
        .nodes()
        .find_map(|n| match &n.kind {
            NodeKind::Button { label: Some(v), .. } if v == label => Some(n.id),
            _ => None,
        })
        .unwrap();
    assert!(vm.click(s, id).unwrap());
    drive(vm, s, true);
}
fn value(vm: &SessionVm, scroll: reel_ui::stage::NodeId) -> f64 {
    let host = vm.stage.value(vm.vm.heap(), scroll).unwrap();
    stage::number(
        vm.vm
            .heap()
            .host_property(aside_vm::PropertyId {
                host,
                field: stage::scroll::ScrollField::Y.index() as u32,
            })
            .unwrap(),
    )
    .unwrap()
}
fn slots(vm: &SessionVm, bar: reel_ui::stage::NodeId) -> [aside_vm::Value; 2] {
    let host = vm.stage.value(vm.vm.heap(), bar).unwrap();
    let fields = vm.vm.heap().host_fields(host).unwrap();
    let offset = vm.vm.heap().table_len(fields).unwrap() - 2;
    [0, 1].map(|index| vm.vm.heap().table_entry(fields, offset + index).unwrap().1)
}
#[test]
fn native_scrollbar_borrows_viewport_and_preserves_slots_through_gc_and_restore() {
    let code = r##"fn opening(){
        let position=model({y:0.0});
        let viewport=scroll(width:100,height:100,scroll_y:bind(position.y)){text("content")};
        let bar=scrollbar(viewport,width:35,height:600,thumb_size:52,inset_start:13,inset_end:5,step:90);
        let track=bar.track;let thumb=bar.thumb;
        thumb.add(stack(stretch:"both",background:"#abcdef"));
        stage.replace(column{viewport;bar;
            button("methods",on_click:||{if bar.scroll_to(700)!=400||bar.scroll_by(-75)!=325||position.y!=325{throw "offset methods";}});
            button("identity",on_click:||{if bar.track!=track||bar.thumb!=thumb||bar.viewport!=viewport{throw "slot identity";}});
            button("retire",on_click:||{bar.remove();viewport.scroll_y=25;if position.y!=25{throw "viewport retired";}});
        });
    }"##;
    for encoded in [false, true] {
        let (mut s, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut s, true);
        let (bar, scroll) = ids(&vm);
        metrics(&mut vm, &mut s, 500., 0.);
        let NodeKind::ScrollBar(view) = vm.stage().node(bar).unwrap().kind else {
            panic!()
        };
        assert_eq!(
            (view.viewport, view.content, view.thumb_size),
            (100., 500., Some(52.))
        );
        assert_eq!(vm.stage().node(bar).unwrap().children.len(), 2);
        click(&mut vm, &mut s, "methods");
        assert_eq!(value(&vm, scroll), 325.);
        let parts = slots(&vm, bar).map(|v| vm.stage.id(vm.vm.heap(), v).unwrap());
        collect(&mut vm);
        (s, vm) = scopes::restored(&mut vm, &s);
        drive(&mut vm, &mut s, true);
        assert_eq!(
            slots(&vm, bar).map(|v| vm.stage.id(vm.vm.heap(), v).unwrap()),
            parts
        );
        click(&mut vm, &mut s, "identity");
        click(&mut vm, &mut s, "retire");
        assert!(vm.stage().node(bar).is_none());
        assert!(vm.stage().node(scroll).is_some());
        assert_eq!(value(&vm, scroll), 25.);
    }
}
#[test]
fn native_scrollbar_numeric_input_uses_target_property_without_callback_tasks() {
    for encoded in [false, true] {
        let (mut s, mut vm) = prepared(
            r#"fn opening(){let viewport=scroll(width:100,height:100){text("content")};let bar=scrollbar(viewport,width:16,height:100);stage.replace(row{viewport;bar;});}"#,
            encoded,
        );
        drive(&mut vm, &mut s, true);
        metrics(&mut vm, &mut s, 500., 0.);
        let (bar, scroll) = ids(&vm);
        let NodeKind::ScrollBar(view) = vm.stage().node(bar).unwrap().kind else {
            panic!()
        };
        let tasks = vm.tasks.len();
        assert!(
            vm.change(
                &mut s,
                &NodeChange {
                    node: bar,
                    value: ControlValue::Number(175.),
                    range: view.range(),
                },
            )
            .unwrap()
        );
        assert_eq!(vm.tasks.len(), tasks);
        assert_eq!(value(&vm, scroll), 175.);
        metrics(&mut vm, &mut s, 200., 100.);
        assert!(
            !vm.change(
                &mut s,
                &NodeChange {
                    node: bar,
                    value: ControlValue::Number(200.),
                    range: view.range(),
                },
            )
            .unwrap()
        );
        assert_eq!(value(&vm, scroll), 100.);
        let target = vm.stage.value(vm.vm.heap(), scroll).unwrap();
        vm.stage.remove_node(vm.vm.heap_mut(), target).unwrap();
        vm.sync_stage(&s).unwrap();
        let NodeKind::ScrollBar(view) = vm.stage().node(bar).unwrap().kind else {
            panic!()
        };
        assert!(!view.live);
        assert!(view.range().is_none());
    }
}
#[test]
fn native_scrollbar_source_exposes_readonly_slots_and_common_configuration() {
    for name in ["viewport", "track", "thumb"] {
        assert!(
            source::api()
                .member("reel::types::ScrollBarNode", name)
                .unwrap()
                .write_type
                .is_none()
        );
        let code = format!(
            "use reel::types::ScrollBarNode;fn change(bar:ScrollBarNode){{bar.{name}=bar.{name};}}"
        );
        assert!(source::compile_module(&code, "bar", "bar.aside", 1).is_err());
    }
    source::compile_module("fn main(){let v=scroll();let bar=scrollbar(v);bar.with(opacity:0.5,visible:false);bar.thumb.add(stack());}","bar","bar.aside",1).unwrap();
}
