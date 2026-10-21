use aside_host::{aside_compiler, aside_vm};
use aside_vm::{RootOwner, SessionEpoch, Value, bytecode as bc, executor::*};

fn drive(vm: &mut Vm, library: &mut aside_std::StandardLibrary, task: TaskId) -> DeliveryId {
    loop {
        match vm
            .run_slice(
                task,
                SliceBudget {
                    instructions: 3,
                    allocation_bytes: 2048,
                    ..Default::default()
                },
                || 0,
            )
            .unwrap()
        {
            RunOutcome::HostCall(ticket) => {
                library.dispatch(vm, ticket, 32).unwrap().unwrap();
            }
            RunOutcome::BudgetYield(usage) => {
                assert!(
                    usage.required_allocation <= 2048,
                    "diagnostic operation exceeds small allocation slice: {usage:?}"
                );
            }
            RunOutcome::Returned(id) => return id,
            RunOutcome::Fault(id) => panic!(
                "script failure: {:?}, value {:?}",
                vm.delivery_fault(id).unwrap(),
                <String as aside_host::FromValue>::from_value(
                    vm.heap(),
                    vm.delivery_value(id).unwrap()
                )
            ),
            other => panic!("unexpected diagnostic outcome: {other:?}"),
        }
    }
}

#[test]
fn error_info_retains_shared_readonly_graph_with_repeatable_for_views() {
    let source = r#"
        use std::core;
        fn helper(){core.error_info()}
        fn restricted(){core.error_info()}
        fn replacing(){try{return 1;}finally{throw "replacement";}}
        fn main(){
            if core.error_info()!=null {throw "outside catch";}
            let mut saved=null;
            try {throw "outer";} catch outer {
                saved=helper();
                if saved.value!="outer" || saved.code!="Thrown" {throw "outer info";}
                if saved.origin.source==null {throw "missing source";}
                let mut count=0;for frame in saved.stack {count+=1;if frame.origin.pc<0 {throw "pc";}}
                let mut again=0;for frame in saved.stack {again+=1;}
                if count==0 || again!=count {throw "stack traversal";}
                let mut empty=0;for cause in saved.causes {empty+=1;}
                if empty!=0 {throw "unexpected cause";}
                try {throw "inner";} catch inner {
                    let info=core.error_info();let mut found=false;
                    for cause in info.causes {if cause.kind=="CaughtFault" && cause.fault==saved && cause.value=="outer" {found=true;}}
                    if !found {throw "missing shared cause";}
                }
                if helper()!=saved {throw "outer catch lost";}
            }
            if helper()!=null || saved.value!="outer" {throw "retention";}
            let mut readonly=false;try {saved.code="changed";} catch error {readonly=true;}
            if !readonly || saved.code!="Thrown" {throw "mutable info";}
            readonly=false;try {saved.origin.pc=12;} catch error {readonly=true;}
            if !readonly {throw "mutable origin";}
            let mut replaced=false;
            try {replacing();} catch error {
                for cause in core.error_info().causes {
                    if cause.kind=="ReplacedExit" {
                        if cause.exit.kind!="return" {throw "exit metadata";}
                        replaced=true;
                    }
                }
            }
            if !replaced {throw "missing replaced exit";}
            return (saved,saved.stack,saved.causes,saved.origin);
        }
        fn inspect(values){
            if values[0].value!="outer" || values[3].source==null {throw "restored metadata";}
            let mut count=0;for frame in values[1] {count+=1;}
            if count==0 {throw "restored stack";}
            for cause in values[2] {throw "restored empty causes";}
            true
        }
    "#;
    let contracts = aside_std::contracts();
    let mut resolver = aside_compiler::semantic::MemoryResolver::default();
    for module in aside_std::modules() {
        resolver.insert(module);
    }
    let module = aside_compiler::analyze_module(source, &Default::default(), &mut resolver)
        .unwrap()
        .into_module()
        .unwrap();
    for encoded in [false, true] {
        let compiled = aside_host::lower(&module, &contracts, &[], &[], &[], Default::default()).unwrap();
        let initializer = compiled.entry;
        let program = if encoded {
            bc::VerifiedProgram::decode(
                &bc::encode(compiled.program.package(), bc::Limits::default()).unwrap(),
                bc::Limits::default(),
            )
            .unwrap()
        } else {
            compiled.program
        };
        let main = bc::FunctionId(
            program
                .package()
                .functions
                .iter()
                .position(|f| program.package().symbols[f.name.index()] == "main")
                .unwrap() as u32,
        );
        let inspect = bc::FunctionId(
            program
                .package()
                .functions
                .iter()
                .position(|f| program.package().symbols[f.name.index()] == "inspect")
                .unwrap() as u32,
        );
        let restricted = bc::FunctionId(
            program
                .package()
                .functions
                .iter()
                .position(|f| program.package().symbols[f.name.index()] == "restricted")
                .unwrap() as u32,
        );
        let support = aside_host::runtime_support(&program, &contracts, &[], &[]).unwrap();
        let mut library = aside_std::StandardLibrary::new(&program);
        let mut vm = Vm::load(program, support, VmLimits::default(), SessionEpoch(1)).unwrap();
        let task = vm
            .start(initializer, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        let id = drive(&mut vm, &mut library, task);
        vm.discard_delivery(id).unwrap();
        let task = vm
            .start(main, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        let id = drive(&mut vm, &mut library, task);
        let value = vm.delivery_value(id).unwrap();
        assert!(!value.same_bits(Value::NULL));
        let info = vm
            .heap()
            .fault_info(
                vm.heap()
                    .fault_id(vm.heap().sequence_get(value, 0).unwrap())
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(info.kind, FaultKind::Thrown);
        vm.begin_collection().unwrap();
        while !vm
            .gc_step(aside_vm::GcBudget {
                units: 1,
                ..Default::default()
            })
            .unwrap()
            .cycle_complete
        {}
        let registry = aside_vm::snapshot::SnapshotRegistry::default();
        let mut capture = vm.snapshot_begin(&registry).unwrap();
        while !capture
            .step(aside_vm::snapshot::SnapshotBudget { units: 1 })
            .unwrap()
            .complete
        {}
        let image =
            aside_vm::snapshot::VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap())
                .unwrap();
        let support = aside_host::runtime_support(vm.program(), &contracts, &[], &[]).unwrap();
        let mut restore = Vm::restore_begin(
            &image,
            vm.program().clone(),
            support,
            VmLimits::default(),
            SessionEpoch(2),
            &registry,
        )
        .unwrap();
        while !restore
            .step(aside_vm::snapshot::SnapshotBudget { units: 1 })
            .unwrap()
            .complete
        {}
        let (mut resumed, map) = restore.finish().unwrap();
        let restored = map.value(value).unwrap();
        let mut resumed_library = aside_std::StandardLibrary::new(resumed.program());
        let task = resumed
            .start(inspect, &[restored], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        let result = drive(&mut resumed, &mut resumed_library, task);
        assert_eq!(
            resumed.delivery_value(result).unwrap().as_bool(),
            Some(true)
        );
        resumed.discard_delivery(result).unwrap();
        vm.discard_delivery(id).unwrap();
        let task = vm
            .start(restricted, &[], RootOwner(1), bc::Context::Derived)
            .unwrap();
        loop {
            match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Fault(id) => {
                    assert_eq!(
                        vm.delivery_fault(id).unwrap().unwrap().kind,
                        FaultKind::Effect
                    );
                    vm.discard_delivery(id).unwrap();
                    break;
                }
                other => panic!("diagnostic read escaped derived context: {other:?}"),
            }
        }
    }
}
