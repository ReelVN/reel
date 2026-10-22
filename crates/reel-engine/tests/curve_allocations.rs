//! Compare authored scalar lookup and native curves in actual reactive consumers.
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{
        self, NativeDispatch, RunOutcome, SessionBudget, SessionStep, SessionVm, SliceBudget,
        VmLimits,
    },
};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::{alloc::System, time::Instant};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

fn drive(vm: &mut SessionVm, session: &mut Session) -> (u64, u64) {
    let mut work = 0;
    let mut samples = 0;
    loop {
        let result = vm
            .step(
                session,
                SessionBudget::RetryMinimum(SliceBudget::default()),
                || 0,
            )
            .unwrap();
        if matches!(result, SessionStep::Vm(_)) {
            work += vm.vm().stats().usage.work;
        }
        match result {
            SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                let package = vm.vm().program().package();
                if package.symbols[package.natives[ticket.native.index()].symbol.index()]
                    == "std::math::curve::sample"
                {
                    samples += 1;
                }
                if let NativeDispatch::Ready { work: used } = vm
                    .dispatch(
                        session,
                        ticket,
                        "en",
                        reel_engine::aside::NativeBudget::Exact(64),
                    )
                    .unwrap()
                {
                    work += u64::from(used);
                }
            }
            SessionStep::Vm(RunOutcome::Returned(_)) => vm.discard_delivery(session).unwrap(),
            SessionStep::Vm(RunOutcome::Fault(id)) => panic!("{:?}", vm.vm().delivery_fault(id)),
            SessionStep::Vm(RunOutcome::ResourceBlocked(error)) => panic!("{error:?}"),
            SessionStep::Idle | SessionStep::Vm(RunOutcome::Waiting(_))
                if !vm.has_runnable_tasks() && !vm.activities_pending() =>
            {
                vm.sync_stage(session).unwrap();
                return (work, samples);
            }
            _ => {}
        }
    }
}

#[test]
fn scalar_curve_reactive_sampling_cost() {
    let props = reel_ui::stage::NodeProps::default();
    assert!(std::sync::Arc::ptr_eq(
        &props.font,
        &reel_ui::stage::NodeProps::default().font
    ));
    let region = Region::new(GLOBAL);
    for _ in 0..1000 {
        std::hint::black_box(props.clone());
    }
    let clones = region.change();
    assert_eq!((clones.allocations, clones.reallocations), (0, 0));
    eprintln!(
        "node_props_bytes={} stage_node_bytes={} property_clones=1000 clone_allocations=0",
        std::mem::size_of::<reel_ui::stage::NodeProps>(),
        std::mem::size_of::<reel_ui::stage::StageNode>()
    );
    for visible in [1, 31, 144] {
        for native in [false, true] {
            let setup = if native { "curve(keys)" } else { "keys" };
            let sample = if native {
                "motion.sample(timer.elapsed)"
            } else {
                "sample(motion,timer.elapsed)"
            };
            let source = format!(
                r#"
            use std::math::curve;
            fn sample(keys,time) {{
                let mut index=0;
                for i in 0..keys.len() {{if keys[i].time<=time {{index=i;}} else {{break;}}}}
                let left=keys[index];
                if index+1==keys.len() {{return left.value;}}
                let right=keys[index+1];
                let t=(time-left.time)/(right.time-left.time);
                left.value*(1-t)+right.value*t
            }}
            fn opening() {{
                let timer=clock(interval:0.000001);
                let selected=model({{visible:{visible}}});
                let root=stack();stage.replace(root);
                for i in 0..144 {{
                    let keys=[{{time:0,value:0}},{{time:1,value:100}},{{time:2,value:0}},{{time:3,value:100}}];
                    let motion={setup};
                    root.add(stack(x:derive(||if i<selected.visible {{{sample}}} else {{0}})));
                }}
                narrator("running");
            }}"#
            );
            let story = aside::source::compile_story(&source, "curves", 1, "opening").unwrap();
            let mut session = Session::new(SessionId(1), Epoch(1)).unwrap();
            let mut vm = SessionVm::load(&session, story.program, VmLimits::default()).unwrap();
            vm.start(&session, story.initializer).unwrap();
            drive(&mut vm, &mut session);
            vm.start(&session, story.entry).unwrap();
            drive(&mut vm, &mut session);
            vm.advance_time(&session, 0., 64).unwrap();
            drive(&mut vm, &mut session);
            let region = Region::new(GLOBAL);
            let chunks = vm.vm().heap().stats().chunk_allocations;
            let started = Instant::now();
            let mut work = 0;
            let mut samples = 0;
            for frame in 1..=120 {
                work += u64::from(
                    vm.advance_time(&session, f64::from(frame) / 60., 64)
                        .unwrap()
                        .work,
                );
                let measured = drive(&mut vm, &mut session);
                work += measured.0;
                samples += measured.1;
            }
            let elapsed = started.elapsed();
            let stats = region.change();
            eprintln!(
                "curve reactive native={native} visible={visible}/144 frames=120 allocations={} reallocations={} VM_chunks={} work={work} native_samples={samples} elapsed={elapsed:?}",
                stats.allocations,
                stats.reallocations,
                vm.vm().heap().stats().chunk_allocations - chunks
            );
            assert_eq!(samples, if native { visible * 120 } else { 0 });
            let values: Vec<_> = vm
                .stage()
                .nodes()
                .filter(|node| node.id != vm.stage().root().unwrap())
                .map(|node| node.props.x)
                .collect();
            assert_eq!(values.len(), 144);
            for value in values {
                assert!(value.abs() < 0.000000001);
            }
        }
    }
}
