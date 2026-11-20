use super::*;
use aside_vm::PropertyId;
use reel_ui::stage::effect::EffectValue;

fn property(vm: &SessionVm, id: reel_ui::stage::NodeId, field: stage::NodeProperty) -> PropertyId {
    let host = vm.stage.value(vm.vm.heap(), id).unwrap();
    PropertyId {
        host,
        field: vm.stage.property_offset(vm.vm.heap(), host).unwrap() + field.index() as u32,
    }
}

#[test]
fn unrelated_writes_and_single_node_changes_preserve_projection_locality() {
    let code = r#"fn opening() {
        let root = stack();
        for i in 0..64 { root.add(text("stationary", x: i)); }
        stage.replace(root);
        narrator("ready");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        let root = vm.stage().root().unwrap();
        let child = vm.stage().node(root).unwrap().children[17];
        let x = property(&vm, child, stage::NodeProperty::X);
        let private = vm.vm.heap_mut().alloc_array(&[Value::NULL]).unwrap();
        let visits = vm.stage.projection_visits;
        for i in 0..1024 {
            vm.vm
                .heap_mut()
                .set_field(private, 0, Value::int(i))
                .unwrap();
        }
        assert!(vm.stage.mounted_properties_current(vm.vm.heap()).unwrap());
        vm.sync_stage(&session).unwrap();
        assert_eq!(vm.stage.projection_visits, visits);
        vm.vm
            .heap_mut()
            .write_host_property(x, Value::int(91), true)
            .unwrap();
        assert!(!vm.stage.mounted_properties_current(vm.vm.heap()).unwrap());
        vm.sync_stage(&session).unwrap();
        assert_eq!(vm.stage.projection_visits - visits, 1);
        assert_eq!(vm.stage().node(child).unwrap().props.x, 91.);
        assert!(vm.stage.mounted_properties_current(vm.vm.heap()).unwrap());
        let visits = vm.stage.projection_visits;
        for i in 0..513 {
            vm.vm
                .heap_mut()
                .write_host_property(x, Value::int(i), true)
                .unwrap();
        }
        assert!(!vm.stage.mounted_properties_current(vm.vm.heap()).unwrap());
        vm.sync_stage(&session).unwrap();
        assert_eq!(vm.stage.projection_visits - visits, 65);
        assert_eq!(vm.stage().node(child).unwrap().props.x, 512.);
        collect(&mut vm);
        vm.sync_stage(&session).unwrap();
        assert!(vm.stage.mounted_properties_current(vm.vm.heap()).unwrap());
    }
}

#[test]
fn detached_roots_are_checked_at_the_collection_cutoff() {
    let code = r#"fn abandon() {
        let unused = stack() { stack(); };
        unused.x;
    }
    fn opening() {
        let retained = [];
        for i in 0..128 { retained.append(stack(x: i)); }
        stage.replace(stack());
        narrator("ready");
        retained[0].remove();
        abandon();
        narrator("settled");
        if retained[1].x != 1 { throw "detached root lost"; }
    }"#;
    let (mut session, mut vm) = models::prepared(code, false);
    models::drive(&mut vm, &mut session, false);
    let first = vm.stage().detached_roots().next().unwrap();
    assert_eq!(vm.stage().detached_roots().len(), 128);
    let x = property(&vm, vm.stage().root().unwrap(), stage::NodeProperty::X);
    let checks = vm.stage.detached_root_checks;
    for frame in 1..=512 {
        vm.vm
            .heap_mut()
            .write_host_property(x, Value::int(frame), true)
            .unwrap();
        vm.sync_stage(&session).unwrap();
    }
    assert_eq!(vm.stage.detached_root_checks, checks);

    let interaction = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(interaction))
            .unwrap();
    }
    vm.sync_reading(&session).unwrap();
    models::drive(&mut vm, &mut session, false);
    let abandoned = vm.stage().detached_roots().next_back().unwrap();
    let abandoned_child = vm.stage().node(abandoned).unwrap().children[0];
    assert!(vm.stage().node(first).is_none());
    assert_eq!(vm.stage().detached_roots().len(), 128);
    assert_eq!(vm.stage.detached_root_checks, checks);

    let step = GcBudget {
        units: 1,
        ..Default::default()
    };
    vm.begin_collection().unwrap();
    vm.vm
        .heap_mut()
        .write_host_property(x, Value::int(513), true)
        .unwrap();
    vm.sync_stage(&session).unwrap();
    assert!(vm.stage().node(abandoned).is_some());
    assert_eq!(vm.stage.detached_root_checks, checks);
    while vm.vm.heap().phase() == aside_vm::GcPhase::Mark {
        vm.gc_step(step).unwrap();
    }
    assert_ne!(vm.vm.heap().phase(), aside_vm::GcPhase::Idle);
    vm.sync_stage(&session).unwrap();
    assert!(vm.stage().node(abandoned).is_none());
    assert!(vm.stage().node(abandoned_child).is_none());
    assert_eq!(vm.stage().detached_roots().len(), 127);
    assert_eq!(vm.stage.detached_root_checks - checks, 128);
    while vm.vm.heap().phase() != aside_vm::GcPhase::Idle {
        vm.gc_step(step).unwrap();
    }
    vm.sync_stage(&session).unwrap();
    assert_eq!(vm.stage.detached_root_checks - checks, 128);

    let snapshot_budget = aside_vm::snapshot::SnapshotBudget { units: 32 };
    let program = vm.vm.program().clone();
    let previous_heap = vm.vm.heap().id();
    let mut capture = vm.snapshot_begin(&session).unwrap();
    while !capture.step(snapshot_budget).unwrap().complete {}
    let bytes = capture.finish().unwrap().encode().unwrap();
    let mut image = snapshot::SessionSnapshot::decode(&bytes).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        program,
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(41),
            epoch: Epoch(10),
        },
        crate::ReadProgress::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(snapshot_budget).unwrap().complete {}
    let (restored_session, mut restored) = restore.finish().unwrap();
    assert_ne!(restored.vm.heap().id(), previous_heap);
    assert_eq!(restored.stage().detached_roots().len(), 127);
    restored.sync_stage(&restored_session).unwrap();
    assert_eq!(restored.stage.detached_root_checks, 127);
}

#[test]
fn shared_effect_changes_reach_each_dependent_node() {
    let code = r#"
use reel::types::Fragment;
fn coverage(pixel: Fragment, amount: float = 0.5) -> float { amount }
fn opening() {
    let paint = effect(coverage);
    stage.replace(stack(effect: paint) { stack(effect: paint); stack(); });
    narrator("ready");
}"#;
    let (mut session, mut vm) = models::prepared(code, true);
    models::drive(&mut vm, &mut session, false);
    let root = vm.stage().root().unwrap();
    let child = vm.stage().node(root).unwrap().children[0];
    let field = property(&vm, root, stage::NodeProperty::Effect);
    let fields = vm.vm.heap().host_fields(field.host).unwrap();
    let effect = vm.vm.heap().table_entry(fields, field.field).unwrap().1;
    let visits = vm.stage.projection_visits;
    vm.vm
        .heap_mut()
        .write_host_property(
            PropertyId {
                host: effect,
                field: 0,
            },
            Value::float(0.75),
            true,
        )
        .unwrap();
    assert!(!vm.stage.mounted_properties_current(vm.vm.heap()).unwrap());
    vm.sync_stage(&session).unwrap();
    assert_eq!(vm.stage.projection_visits - visits, 2);
    for id in [root, child] {
        assert_eq!(
            vm.stage()
                .node(id)
                .unwrap()
                .props
                .effect
                .as_ref()
                .unwrap()
                .values(),
            &[EffectValue::Float(0.75_f32.to_bits())]
        );
    }
}

#[test]
#[ignore = "release CPU measurement of indexed and full projection with identical mutations"]
fn indexed_projection_cost() {
    let code = r#"fn opening() {
        let root = stack();
        for i in 0..256 { root.add(text("stationary", x: i)); }
        stage.replace(root);
        narrator("ready");
    }"#;
    for indexed in [false, true] {
        let (mut session, mut vm) = models::prepared(code, false);
        models::drive(&mut vm, &mut session, false);
        let root = vm.stage().root().unwrap();
        let child = vm.stage().node(root).unwrap().children[17];
        let x = property(&vm, child, stage::NodeProperty::X);
        let before = vm.vm.heap().budget_usage().charged_bytes();
        let visits = vm.stage.projection_visits;
        let began = std::time::Instant::now();
        for i in 0..10_000 {
            vm.vm
                .heap_mut()
                .write_host_property(x, Value::int(i), true)
                .unwrap();
            if indexed {
                vm.sync_stage(&session).unwrap();
            } else {
                vm.stage.sync_tree(vm.vm.heap_mut(), root, 1 << 20).unwrap();
            }
        }
        println!(
            "indexed={indexed} frames=10000 nodes=257 visits={} elapsed_us={} retained_growth={}",
            vm.stage.projection_visits - visits,
            began.elapsed().as_micros(),
            vm.vm.heap().budget_usage().charged_bytes() - before
        );
        assert_eq!(vm.stage().node(child).unwrap().props.x, 9999.);
    }
}

#[test]
#[ignore = "release CPU measurement of repeated shared-effect invalidation"]
fn shared_effect_projection_cost() {
    let code = r#"
use reel::types::Fragment;
fn coverage(pixel: Fragment, amount: float = 0.5) -> float { amount }
fn opening() {
    let paint = effect(coverage);
    let root = stack(effect: paint);
    for i in 0..511 { root.add(stack(effect: paint)); }
    stage.replace(root);
    narrator("ready");
}"#;
    let (mut session, mut vm) = models::prepared(code, false);
    models::drive(&mut vm, &mut session, false);
    let root = vm.stage().root().unwrap();
    let field = property(&vm, root, stage::NodeProperty::Effect);
    let fields = vm.vm.heap().host_fields(field.host).unwrap();
    let effect = vm.vm.heap().table_entry(fields, field.field).unwrap().1;
    let parameter = PropertyId {
        host: effect,
        field: 0,
    };
    let visits = vm.stage.projection_visits;
    let started = std::time::Instant::now();
    for frame in 0..200 {
        for write in 0..128 {
            vm.vm
                .heap_mut()
                .write_host_property(parameter, Value::float((frame * 128 + write) as f64), true)
                .unwrap();
        }
        vm.sync_stage(&session).unwrap();
    }
    println!(
        "shared_effect frames=200 nodes=512 writes_per_frame=128 visits={} elapsed_us={}",
        vm.stage.projection_visits - visits,
        started.elapsed().as_micros()
    );
    assert_eq!(vm.stage.projection_visits - visits, 200 * 512);
    for node in vm.stage().nodes() {
        assert_eq!(
            node.props.effect.as_ref().unwrap().values(),
            &[EffectValue::Float(25599_f32.to_bits())]
        );
    }
}
