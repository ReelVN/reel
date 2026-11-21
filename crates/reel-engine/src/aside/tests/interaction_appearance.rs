use super::*;
use reel_ui::stage::{InteractionState, NodeId, NodeInteraction, NodeKind};

fn control(vm: &SessionVm) -> NodeId {
    vm.stage()
        .nodes()
        .find(|node| matches!(node.kind, NodeKind::Button { .. }))
        .unwrap()
        .id
}
fn state(vm: &mut SessionVm, session: &Session, node: NodeId, state: InteractionState) {
    vm.stage
        .interaction(vm.vm.heap_mut(), &NodeInteraction { node, state })
        .unwrap();
    vm.sync_stage(session).unwrap();
}
fn advance(vm: &mut SessionVm, session: &mut Session) {
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
    }
    box_style::drive(vm, session);
}

#[test]
fn image_skin_projects_asset_and_atlas_region_from_control_state() {
    let code = r#"fn opening() {
        let control = button("Start", width: 100, height: 100);
        let art = image(image_skin(control, "base.png",
            highlight: {source: "atlas.png", source_rect: (20, 0, 20, 20)},
            focus_visible: {source: "atlas.png", source_rect: (60, 0, 20, 20)},
            pressed: (40, 0, 20, 20)),
            source_rect: (0, 0, 20, 20));
        control.add(art);
        stage.replace(control);
        narrator("ready");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        box_style::drive(&mut vm, &mut session);
        let id = control(&vm);
        let art = vm.stage().node(id).unwrap().children[0];
        let check = |vm: &SessionVm, key, crop| {
            let NodeKind::Image {
                source,
                source_rect,
                ..
            } = &vm.stage().node(art).unwrap().kind
            else {
                panic!("expected image");
            };
            assert_eq!(source.key(), key);
            assert_eq!(
                source.skin_paths.as_deref().unwrap(),
                ["base.png", "atlas.png"]
            );
            assert_eq!(source_rect.unwrap().x0, crop);
        };
        check(&vm, "base.png", 0.);
        state(
            &mut vm,
            &session,
            id,
            InteractionState {
                hover: true,
                ..Default::default()
            },
        );
        check(&vm, "atlas.png", 20.);
        state(
            &mut vm,
            &session,
            id,
            InteractionState {
                hover: true,
                focus_visible: true,
                ..Default::default()
            },
        );
        check(&vm, "atlas.png", 60.);
        state(
            &mut vm,
            &session,
            id,
            InteractionState {
                hover: true,
                pressed: true,
                focus_visible: true,
                ..Default::default()
            },
        );
        check(&vm, "base.png", 40.);
        (session, vm) = scopes::restored(&mut vm, &session);
        vm.sync_stage(&session).unwrap();
        check(&vm, "base.png", 0.);
    }
}

#[test]
fn image_quad_binding_is_projected_and_restored() {
    let code = r#"fn opening() {
        let pose = model({points: ((0, 0), (40, 0), (40, 40), (0, 40))});
        stage.replace(image("card.png", fit: "stretch", corners: bind(pose.points),
            source_rect: (16, 8, 40, 40), width: 40, height: 40));
        narrator("front");
        pose.points = ((4, 3), (38, 0), (40, 40), (1, 35));
        narrator("tilted");
    }"#;
    let (mut session, mut vm) = models::prepared(code, true);
    let check = |vm: &SessionVm, expected| {
        let node = vm
            .stage()
            .nodes()
            .find(|node| matches!(node.kind, NodeKind::Image { .. }))
            .unwrap();
        let NodeKind::Image {
            corners: Some(points),
            source_rect,
            ..
        } = &node.kind
        else {
            panic!("expected a warped image");
        };
        assert_eq!(points[0], reel_ui::kurbo::Point::from(expected));
        assert_eq!(source_rect.unwrap().x0, 16.);
    };
    box_style::drive(&mut vm, &mut session);
    check(&vm, (0., 0.));
    advance(&mut vm, &mut session);
    check(&vm, (4., 3.));
    (session, vm) = scopes::restored(&mut vm, &session);
    vm.sync_stage(&session).unwrap();
    check(&vm, (4., 3.));
}

#[test]
fn appearance_selects_paint_preserves_base_bindings_and_resets_input_on_restore() {
    let code = r##"fn opening() {
        let data = model({background: "#102030"});
        let hover = {background: "#ffffff", color: "#000000"};
        let look = appearance(base: {border_width: 1, corner_radius: 6}, hover: hover,
            focus_visible: {border_width: 3, border_color: "#112233"},
            pressed: {opacity: 0.5}, disabled: {opacity: 0.25, background: null});
        hover.background = "#ff0000";
        let node = button("Continue", decorated: false, appearance: look,
            background: bind(data.background));
        stage.replace(node);
        narrator("ready");
        data.background = "#405060";
        if node.background != "#405060" || node.opacity != 1 { throw "appearance wrote base properties"; }
        narrator("updated");
    }"##;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        box_style::drive(&mut vm, &mut session);
        let id = control(&vm);
        assert_eq!(
            vm.stage().node(id).unwrap().props.background,
            Some([0x10, 0x20, 0x30, 255])
        );
        state(
            &mut vm,
            &session,
            id,
            InteractionState {
                hover: true,
                focused: true,
                focus_visible: true,
                pressed: true,
                ..Default::default()
            },
        );
        let props = &vm.stage().node(id).unwrap().props;
        assert_eq!(
            props.background,
            Some([255; 4]),
            "input table alias changed immutable paint"
        );
        assert_eq!(props.color, [0, 0, 0, 255]);
        assert_eq!((props.opacity, props.border_width), (0.5, 3.));
        advance(&mut vm, &mut session);
        assert_eq!(
            vm.stage().node(id).unwrap().props.background,
            Some([255; 4])
        );
        state(
            &mut vm,
            &session,
            id,
            InteractionState {
                hover: true,
                pressed: true,
                disabled: true,
                ..Default::default()
            },
        );
        let props = &vm.stage().node(id).unwrap().props;
        assert_eq!((props.opacity, props.background), (0.25, None));
        (session, vm) = scopes::restored(&mut vm, &session);
        vm.sync_stage(&session).unwrap();
        let props = &vm.stage().node(id).unwrap().props;
        assert_eq!(props.background, Some([0x40, 0x50, 0x60, 255]));
        assert_eq!((props.opacity, props.border_width), (1., 1.));
    }
}

#[test]
fn appearance_scope_override_wins_and_constructor_rejects_mutable_or_layout_values() {
    let code = r##"fn dynamic(value) { value } fn opening() {
        let data = model({alpha: 0.5});
        let mut rejected = 0;
        for invalid in [{width: 40}, {visible: false}, {on_click: || {}},
            {opacity: 2}, {background: "bad"}, {opacity: bind(data.alpha)}] {
            try { let _ = appearance(hover: invalid); } catch error { rejected += 1; }
        }
        let paint = appearance(base: {background: "#123456"}, hover: {opacity: 0.8},
            pressed: {opacity: 0.6}, disabled: {opacity: 0.2});
        try { dynamic(paint).base = {}; } catch error { rejected += 1; }
        try { let _ = button(appearance: dynamic({})); } catch error { rejected += 1; }
        if rejected != 8 { throw "appearance validation"; }
        let node = button("Continue", appearance: paint, enabled: false);
        stage.replace(node);
        narrator("disabled initially");
        node.enabled = true;
        scope(body: |s| {
            s.override(node, {opacity: 0.9, background: null});
            narrator("covered");
        });
        narrator("uncovered");
    }"##;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        box_style::drive(&mut vm, &mut session);
        let id = control(&vm);
        assert_eq!(vm.stage().node(id).unwrap().props.opacity, 0.2);
        advance(&mut vm, &mut session);
        state(
            &mut vm,
            &session,
            id,
            InteractionState {
                hover: true,
                pressed: true,
                ..Default::default()
            },
        );
        assert_eq!(vm.stage().node(id).unwrap().props.opacity, 0.9);
        assert_eq!(vm.stage().node(id).unwrap().props.background, None);
        advance(&mut vm, &mut session);
        assert_eq!(vm.stage().node(id).unwrap().props.opacity, 0.6);
        assert_eq!(
            vm.stage().node(id).unwrap().props.background,
            Some([0x12, 0x34, 0x56, 255])
        );
    }
}

#[test]
fn scope_appearance_attaches_and_releases_interaction_observation() {
    let code = r##"fn opening() {
        let node = button("Continue", decorated: false);
        stage.replace(node);
        scope(body: |s| {
            s.override(node, {appearance: appearance(hover: {opacity: 0.5})});
            narrator("covered");
        });
        narrator("uncovered");
    }"##;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        box_style::drive(&mut vm, &mut session);
        let id = control(&vm);
        assert!(vm.stage.has_handlers());
        state(
            &mut vm,
            &session,
            id,
            InteractionState {
                hover: true,
                ..Default::default()
            },
        );
        assert_eq!(vm.stage().node(id).unwrap().props.opacity, 0.5);
        advance(&mut vm, &mut session);
        assert!(!vm.stage.has_handlers());
        assert_eq!(vm.stage().node(id).unwrap().props.opacity, 1.);
    }
}

#[test]
fn appearance_interaction_projection_allocates_no_vm_objects_or_tasks() {
    let code = r##"fn opening() {
        let look = appearance(base: {background: "#123456"},
            hover: {background: "#abcdef"}, pressed: {opacity: 0.5});
        stage.replace(button("Continue", appearance: look, decorated: false));
        narrator("ready");
    }"##;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        box_style::drive(&mut vm, &mut session);
        let id = control(&vm);
        collect(&mut vm);
        let before = vm.vm.heap().stats();
        let tasks = vm.tasks.len();
        let started = std::time::Instant::now();
        for change in 0..4096 {
            state(
                &mut vm,
                &session,
                id,
                InteractionState {
                    hover: change % 2 == 0,
                    pressed: change % 4 == 0,
                    ..Default::default()
                },
            );
        }
        let after = vm.vm.heap().stats();
        assert_eq!(
            (
                after.live_objects,
                after.payload_bytes,
                after.chunk_allocations,
                after.roots
            ),
            (
                before.live_objects,
                before.payload_bytes,
                before.chunk_allocations,
                before.roots
            )
        );
        assert_eq!(vm.tasks.len(), tasks);
        eprintln!(
            "appearance source/ASB={encoded}: 4096 changed projections in {:?}; VM allocations/tasks=0",
            started.elapsed()
        );
        let syncs = vm.stage.sync_count;
        for _ in 0..4096 {
            vm.sync_stage(&session).unwrap();
        }
        assert_eq!(
            vm.stage.sync_count, syncs,
            "static appearance triggered projection work"
        );
    }
}

#[test]
fn shared_appearance_reduces_control_construction_storage() {
    let mut measurements = Vec::new();
    for shared in [false, true] {
        let component = if shared {
            r##"button("Continue", width: 120, height: 40, decorated: false, appearance: look)"##
        } else {
            r##"
                let control = button(width: 120, height: 40, decorated: false);
                control.background = derive(|| if control.hover { "#ffffff" } else { "#304060" });
                control.opacity = derive(|| if control.pressed { 0.5 } else { 1 });
                control.add(text("Continue", color: derive(|| if control.hover { "#000000" } else { "#ffffff" })));
                control
            "##
        };
        let source = format!(
            r##"fn control(look) {{ {component} }} fn opening() {{
            narrator("baseline", history: false);
            let look = appearance(base: {{background: "#304060"}},
                hover: {{background: "#ffffff", color: "#000000"}}, pressed: {{opacity: 0.5}});
            let root = column();
            for _ in 0..32 {{ root.add(control(look)); }}
            stage.replace(root);
            narrator("mounted", history: false);
        }}"##
        );
        let (mut session, mut vm) = models::prepared(&source, false);
        box_style::drive(&mut vm, &mut session);
        collect(&mut vm);
        let baseline = vm.vm.heap().stats();
        advance(&mut vm, &mut session);
        collect(&mut vm);
        let mounted = vm.vm.heap().stats();
        let nodes = vm.stage().nodes().count();
        let payload = mounted.payload_bytes - baseline.payload_bytes;
        measurements.push((nodes, payload));
        eprintln!(
            "32 controls: shared appearance={shared}, nodes={nodes}, added VM payload={payload} bytes"
        );
    }
    assert_eq!((measurements[0].0, measurements[1].0), (65, 33));
    assert!(measurements[1].1 < measurements[0].1);
}
