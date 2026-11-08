use super::*;
use aside_vm::{RootKind, snapshot::SnapshotBudget};

pub(super) fn list(vm: &SessionVm, virtualized: bool) -> NodeId {
    vm.stage()
        .nodes()
        .find(|node| {
            if virtualized {
                return matches!(node.kind, NodeKind::VirtualList(_));
            }
            matches!(node.kind, NodeKind::Column(_))
                && !node.children.is_empty()
                && node.children.iter().all(|child| {
                    matches!(&vm.stage().node(*child).unwrap().kind,
                NodeKind::Button { label: Some(label), .. } if label.starts_with("key "))
                })
        })
        .unwrap()
        .id
}
pub(super) fn index(vm: &SessionVm, list: NodeId) -> aside_vm::Value {
    let heap = vm.vm.heap();
    let host = vm.stage.value(heap, list).unwrap();
    let state = heap
        .derived_state(heap.host_payload(host, 3).unwrap())
        .unwrap();
    heap.sequence_get(state, 1).unwrap()
}
fn restore_with_reversed_key_roots(
    vm: &mut SessionVm,
    session: &Session,
    list: NodeId,
) -> (Session, SessionVm) {
    let entries = index(vm, list);
    let keys: Vec<_> = (0..vm.vm.heap().sequence_len(entries).unwrap())
        .map(|i| {
            let entry = vm.vm.heap().sequence_get(entries, i).unwrap();
            vm.vm.heap().sequence_get(entry, 0).unwrap()
        })
        .collect();
    reverse_key_roots(vm, &keys);
    restore(vm, session)
}
pub(super) fn reverse_key_roots(vm: &mut SessionVm, keys: &[aside_vm::Value]) {
    let mut keys = keys.to_vec();
    keys.sort_by(|&a, &b| crate::aside::keys::compare(vm.vm.heap(), a, b, 256).unwrap());
    for &key in keys.iter().rev() {
        let owner = vm.root_owner;
        vm.vm
            .heap_mut()
            .add_root(owner, RootKind::Host, key)
            .unwrap();
    }
}
pub(super) fn restore(vm: &mut SessionVm, session: &Session) -> (Session, SessionVm) {
    let mut capture = vm.snapshot_begin(session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let bytes = capture.finish().unwrap().encode().unwrap();
    let mut image = snapshot::SessionSnapshot::decode(&bytes).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        vm.vm.program().clone(),
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(91),
            epoch: Epoch(13),
        },
        Default::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    restore.finish().unwrap()
}
fn rows(vm: &SessionVm, list: NodeId) -> std::collections::BTreeMap<String, NodeId> {
    vm.stage()
        .node(list)
        .unwrap()
        .children
        .iter()
        .map(|&id| {
            let NodeKind::Button {
                label: Some(label), ..
            } = &vm.stage().node(id).unwrap().kind
            else {
                panic!()
            };
            (label.clone(), id)
        })
        .collect()
}
fn virtual_layout(vm: &mut SessionVm, session: &mut Session) {
    use reel_ui::{
        kurbo::{Size, Vec2},
        stage::{NodeEvent, NodeScroll, NodeVirtualList, VirtualRowMeasurement},
    };
    for _ in 0..4 {
        let list = list(vm, true);
        let node = vm.stage().node(list).unwrap();
        let NodeKind::VirtualList(view) = &node.kind else {
            panic!()
        };
        let owner = node.parent.unwrap();
        let viewport = Size::new(320., vm.stage().node(owner).unwrap().props.height.unwrap());
        let limit = (view.total_height - viewport.height).max(0.);
        let NodeKind::Scroll {
            offset: requested, ..
        } = vm.stage().node(owner).unwrap().kind
        else {
            panic!()
        };
        let content = Size::new(320., view.total_height);
        let offset = Vec2::new(0., requested.y.clamp(0., limit));
        let event = NodeVirtualList {
            node: list,
            scroll: node.parent.unwrap(),
            revision: view.revision,
            requested,
            offset,
            viewport,
            scroll_scale: 1.,
            scroll_limit: limit,
            width: 320.,
            visible_start: offset.y,
            visible_end: offset.y + viewport.height,
            measurements: view
                .rows
                .iter()
                .map(|row| VirtualRowMeasurement {
                    node: row.node,
                    height: 40.,
                })
                .collect(),
            retained: [].into(),
            navigation: None,
        };
        let scroll = NodeScroll {
            input: false,
            node: owner,
            requested,
            offset,
            viewport,
            content,
        };
        vm.interactions(
            session,
            &[NodeEvent::Scroll(scroll), NodeEvent::VirtualList(event)],
        )
        .unwrap();
        drive(vm, session, false);
    }
}

#[test]
fn object_key_indices_restore_in_the_current_heap_order_without_rebuilding_rows() {
    for virtualized in [false, true] {
        let positioning = if virtualized {
            r#"button("jump",on_click:||{let selected=await rows.scroll_to(data.items[0]);selected.opacity=0.5;});"#
        } else {
            ""
        };
        let code = format!(
            r#"
        let mut builds=0;
        fn opening() {{
            let keys=[]; for i in 0..3 {{ keys.append({{id:i}}); }}
            let data=model({{items:[]}}); data.items=[keys[2],keys[0],keys[1]];
            let rows=list(||data.items,key:|item|item,virtual:{virtualized},build:|row|{{
                builds+=1;button(f"key {{row.value.id}}",height:40)
            }});
            stage.replace(column {{
                if {virtualized} {{scroll(width:320,height:1000){{rows}}}} else {{rows}};
                {positioning}
                button("reverse",on_click:||data.items.reverse());
                button("verify",on_click:||{{if builds!=3 {{throw "restored identity index rebuilt rows";}}}});
            }});
            narrator("ready");
        }}"#
        );
        let (mut session, mut vm) = models::prepared(&code, true);
        drive(&mut vm, &mut session, false);
        if virtualized {
            virtual_layout(&mut vm, &mut session);
        }
        let id = list(&vm, virtualized);
        let before = rows(&vm, id);
        assert_eq!(before.len(), 3);
        let (mut session, mut vm) = restore_with_reversed_key_roots(&mut vm, &session, id);
        if virtualized {
            virtual_layout(&mut vm, &mut session);
            click(&mut vm, &mut session, "jump");
            virtual_layout(&mut vm, &mut session);
            vm.accept_presentation(&session, vm.presentation_request())
                .unwrap();
            drive(&mut vm, &mut session, false);
            assert_eq!(vm.stage().node(before["key 2"]).unwrap().props.opacity, 0.5);
        }
        click(&mut vm, &mut session, "reverse");
        if virtualized {
            virtual_layout(&mut vm, &mut session);
        }
        assert_eq!(rows(&vm, id), before, "virtual={virtualized}");
        click(&mut vm, &mut session, "verify");
    }
}

#[test]
fn restored_object_key_scroll_to_finds_a_target_outside_the_initial_window() {
    let code = r#"
    let mut builds=0;
    fn opening() {
        let data=model({items:[]});
        let items=[];for i in 0..80 {items.append({id:i});}data.items=items;
        let rows=list(||data.items,key:|item|item,virtual:true,build:|row|{
            builds+=1;button(f"key {row.value.id}",height:40)
        });
        stage.replace(column{
            scroll(width:320,height:120){rows};
            button("jump",on_click:||{let selected=await rows.scroll_to(data.items[70]);selected.opacity=0.5;});
            button("bounded",on_click:||{if builds>16 {throw "far object target built intermediate rows";}});
        });
        narrator("ready");
    }"#;
    let (mut session, mut vm) = models::prepared(code, true);
    drive(&mut vm, &mut session, false);
    virtual_layout(&mut vm, &mut session);
    let id = list(&vm, true);
    assert!(!rows(&vm, id).contains_key("key 70"));
    let (mut session, mut vm) = restore_with_reversed_key_roots(&mut vm, &session, id);
    virtual_layout(&mut vm, &mut session);
    click(&mut vm, &mut session, "jump");
    for _ in 0..3 {
        virtual_layout(&mut vm, &mut session);
    }
    assert!(vm.presentation_request() > 0);
    vm.accept_presentation(&session, vm.presentation_request())
        .unwrap();
    drive(&mut vm, &mut session, false);
    let target = rows(&vm, id)["key 70"];
    assert_eq!(vm.stage().node(target).unwrap().props.opacity, 0.5);
    assert!(vm.stage().node(id).unwrap().children.len() < 16);
    click(&mut vm, &mut session, "bounded");
}
