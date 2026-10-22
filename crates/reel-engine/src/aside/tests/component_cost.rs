use super::*;

#[test]
#[ignore = "reproducible component construction, stable updates and retirement measurement"]
fn component_storage_cost() {
    let nodes = std::env::var("REEL_COST_NODES")
        .map(|value| value.parse::<usize>().unwrap())
        .unwrap_or(1000);
    let code = format!(
        r#"fn opening() {{
        narrator("baseline", history: false);
        let parent = column();
        for i in 0..{nodes} {{
            parent.add(button("Item", x: i, on_click: |event| {{ event.target.opacity = 0.5; }}));
        }}
        stage.replace(parent);
        narrator("mounted", history: false);
        for i in 0..120 {{ parent.x = i; parent.opacity = 0.5; }}
        narrator("updated", history: false);
        stage.replace(stack());
        narrator("retired", history: false);
    }}"#
    );
    let (mut session, mut vm) = models::prepared(&code, false);
    models::drive(&mut vm, &mut session, false);
    collect(&mut vm);
    let baseline = vm.vm.heap().stats();
    let started = std::time::Instant::now();
    activities::advance_dialogue(&mut vm, &mut session);
    vm.sync_stage(&session).unwrap();
    let build_elapsed = started.elapsed();
    collect(&mut vm);
    let mounted = vm.vm.heap().stats();
    assert_eq!(vm.stage().nodes().count(), nodes + 1);
    let started = std::time::Instant::now();
    activities::advance_dialogue(&mut vm, &mut session);
    vm.sync_stage(&session).unwrap();
    let update_elapsed = started.elapsed();
    let updated = vm.vm.heap().stats();
    assert_eq!(
        vm.stage().node(vm.stage().root().unwrap()).unwrap().props.x,
        119.
    );
    activities::advance_dialogue(&mut vm, &mut session);
    vm.sync_stage(&session).unwrap();
    collect(&mut vm);
    let retired = vm.vm.heap().stats();
    assert_eq!(vm.stage().nodes().count(), 1);
    println!(
        "components,nodes={nodes},build_ns={},update_ns={},base_charged={},mounted_charged={},mounted_payload={},mounted_objects={},update_chunks={},retired_charged={},retired_objects={}",
        build_elapsed.as_nanos(),
        update_elapsed.as_nanos(),
        baseline.charged_bytes,
        mounted.charged_bytes,
        mounted.payload_bytes,
        mounted.live_objects,
        updated.chunk_allocations - mounted.chunk_allocations,
        retired.charged_bytes,
        retired.live_objects,
    );
}
