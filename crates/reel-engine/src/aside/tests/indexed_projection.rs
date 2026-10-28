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
