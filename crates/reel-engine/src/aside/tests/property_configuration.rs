use super::*;
use aside_vm::PropertyId;
use aside_vm::snapshot::SnapshotBudget;
use std::cell::Cell;

fn native_symbol(vm: &SessionVm, ticket: HostTicket) -> &str {
    let package = vm.vm.program().package();
    &package.symbols[package.natives[ticket.native.index()].symbol.index()]
}

fn run_configuration(
    session: &mut Session,
    vm: &mut SessionVm,
    policy: NativeBudget,
) -> (u64, usize) {
    let mut work = 0;
    let mut calls = 0;
    for _ in 0..100_000 {
        match next(vm, session) {
            RunOutcome::HostCall(ticket) => {
                let configuration = native_symbol(vm, ticket).ends_with("::with");
                let result = vm
                    .dispatch(
                        session,
                        ticket,
                        "en",
                        if configuration {
                            policy
                        } else {
                            NativeBudget::RetryMinimum(128)
                        },
                    )
                    .unwrap();
                if configuration {
                    calls += 1;
                    work += u64::from(match result {
                        NativeDispatch::Ready { work }
                        | NativeDispatch::BudgetYield { work, .. } => work,
                        other => panic!("unexpected configuration dispatch: {other:?}"),
                    });
                }
            }
            RunOutcome::Returned(id) => {
                assert_eq!(vm.vm.delivery_value(id).unwrap().as_bool(), Some(true));
                return (work, calls);
            }
            RunOutcome::Fault(id) => panic!(
                "configuration fault: {:?}; value {:?}",
                vm.vm.delivery_fault(id).unwrap(),
                text::read_string(vm.vm.heap(), vm.vm.delivery_value(id).unwrap(), 4096)
            ),
            other => panic!("configuration did not return: {other:?}"),
        }
    }
    panic!("configuration continuation did not finish");
}

fn restore_configuration(session: &Session, vm: &mut SessionVm) -> (Session, SessionVm) {
    let program = vm.vm.program().clone();
    let mut capture = vm.snapshot_begin(session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let mut image =
        snapshot::SessionSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        program,
        VmLimits::default(),
        SessionIdentity {
            session_id: SessionId(42),
            epoch: Epoch(10),
        },
        crate::ReadProgress::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    restore.finish().unwrap()
}

#[test]
fn with_source_asb_preserves_identity_presence_and_final_binding_graph() {
    let code = r#"fn opening() {
        let state=model({position:1,other:7});
        let node=text("first",x:bind(state.position),width:80);
        let viewport=scroll();
        if viewport.overscroll!="clamp" {throw "overscroll default";}
        viewport.with(overscroll:"bounce",scroll_y:20);
        if viewport.overscroll!="bounce" || viewport.scroll_y!=20 {throw "overscroll writeback";}
        let mut invalid_overscroll=false;
        try {viewport.overscroll="invalid";} catch error {invalid_overscroll=true;}
        if !invalid_overscroll || viewport.overscroll!="bounce" {throw "overscroll validation";}
        let result=node.with(value:"second",y:4);
        if result!=node || node.x!=1 || node.y!=4 || node.width!=80 || node.value!="second" {throw "identity/presence";}
        node.with(width:null).with(x:3);
        if node.width!=null || state.position!=3 {throw "null/writeback";}
        let control=slider(value:1,min:0,max:2);
        control.with(min:10,max:20,value:15);
        control.with(value:25,max:30,min:20);
        if control.min!=20 || control.max!=30 || control.value!=25 {throw "joint bounds";}
        let mut errors=0;
        try {node.with(x:bind(state.other),opacity:2);} catch error {errors+=1;}
        state.position=5;
        state.other=9;
        if errors!=1 || node.x!=5 || node.opacity!=1 {throw "failed rebind changed old graph";}
        let shared=text("same",x:bind(state.position),y:bind(state.position));
        shared.with(x:6,y:6);
        try {shared.with(x:7,y:8);} catch error {errors+=1;}
        if errors!=2 || state.position!=6 || shared.x!=6 || shared.y!=6 {throw "canonical conflict";}
        let derived=text("derived",x:derive(|| state.position*2));
        try {derived.with(x:9);} catch error {errors+=1;}
        if errors!=3 || derived.x!=12 {throw "derived is writable";}
        true
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        let (work, calls) =
            run_configuration(&mut session, &mut vm, NativeBudget::RetryMinimum(4096));
        eprintln!("with semantics: asb={encoded} calls={calls} work={work}");
    }
}

#[test]
fn scroll_boundary_policy_and_logical_position_survive_snapshot_restore() {
    use reel_ui::widgets::ScrollOverscroll;
    let code = r#"fn opening() {
        stage.replace(scroll(width:200,height:100,overscroll:"bounce",scroll_y:20) {
            stack(height:1000);
        });
        narrator("hold");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        models::drive(&mut vm, &mut session, false);
        let (_, restored) = restore_configuration(&session, &mut vm);
        let stage = restored.stage();
        match stage.node(stage.root().unwrap()).unwrap().kind {
            reel_ui::stage::NodeKind::Scroll {
                overscroll, offset, ..
            } => {
                assert_eq!(overscroll, ScrollOverscroll::Bounce);
                assert_eq!(offset.y, 20.);
            }
            _ => panic!("restored scroll changed kind"),
        }
    }
}

#[test]
fn with_exact_zero_and_retry_restore_evaluates_arguments_once() {
    let code = r#"fn opening() {
        let state=model({value:1,calls:0});
        let node=text("source",x:bind(state.value));
        let followers=[];
        for index in 0..40 {followers.append(text("follower",x:bind(state.value)));}
        let evaluate=|| {state.calls+=1; 9};
        node.with(x:evaluate(),y:4);
        if state.calls!=1 || state.value!=9 || node.x!=9 || node.y!=4 {throw "argument replay or partial result";}
        for follower in followers {if follower.x!=9 {throw "follower missing";}}
        true
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        let mut zero = false;
        let mut restored = false;
        for _ in 0..100_000 {
            match next(&mut vm, &mut session) {
                RunOutcome::HostCall(ticket) => {
                    if !native_symbol(&vm, ticket).ends_with("::with") {
                        vm.dispatch(&mut session, ticket, "en", NativeBudget::RetryMinimum(128))
                            .unwrap();
                        continue;
                    }
                    if !zero {
                        let before = vm.vm.heap().stats();
                        assert!(matches!(
                            vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(0))
                                .unwrap(),
                            NativeDispatch::BudgetYield {
                                work: 0,
                                required_work: 32
                            }
                        ));
                        let after = vm.vm.heap().stats();
                        assert_eq!(before.charged_bytes, after.charged_bytes);
                        assert_eq!(before.chunk_allocations, after.chunk_allocations);
                        zero = true;
                    } else {
                        let result = vm
                            .dispatch(&mut session, ticket, "en", NativeBudget::RetryMinimum(1))
                            .unwrap();
                        if !restored
                            && matches!(result,NativeDispatch::BudgetYield{work,..} if work>32)
                        {
                            (session, vm) = restore_configuration(&session, &mut vm);
                            restored = true;
                        }
                    }
                }
                RunOutcome::Returned(id) => {
                    assert_eq!(vm.vm.delivery_value(id).unwrap().as_bool(), Some(true));
                    assert!(zero && restored);
                    break;
                }
                RunOutcome::Fault(id) => panic!(
                    "configuration restore fault: {:?}; value {:?}",
                    vm.vm.delivery_fault(id).unwrap(),
                    text::read_string(vm.vm.heap(), vm.vm.delivery_value(id).unwrap(), 4096)
                ),
                other => panic!("unexpected {other:?}"),
            }
        }
    }
}

#[test]
fn with_validation_fault_uses_the_original_call_location() {
    let code = "fn opening() {let node=text(\"invalid\"); node.with(opacity:2);}";
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        loop {
            match next(&mut vm, &mut session) {
                RunOutcome::HostCall(ticket) => {
                    vm.dispatch(&mut session, ticket, "en", NativeBudget::RetryMinimum(1))
                        .unwrap();
                }
                RunOutcome::Fault(id) => {
                    let fault = vm.vm.delivery_fault(id).unwrap().unwrap();
                    assert_eq!(fault.kind, FaultKind::Type);
                    let span = fault.location.source.unwrap();
                    assert_eq!(
                        &code[span.start as usize..span.end as usize],
                        "node.with(opacity:2)"
                    );
                    break;
                }
                other => panic!("expected configuration validation fault: {other:?}"),
            }
        }
    }
}

#[test]
fn cancelling_a_pending_with_releases_the_plan_and_preserves_old_binding() {
    let code = r#"fn opening() {
        let state=model({old:1,next:7});
        let node=text("source",x:bind(state.old));
        let followers=[];
        for index in 0..40 {followers.append(text("follower",x:bind(state.old)));}
        node.with(x:bind(state.next),y:9);
        throw "cancelled configuration resumed";
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        loop {
            let RunOutcome::HostCall(ticket) = next(&mut vm, &mut session) else {
                panic!("configuration did not suspend")
            };
            if !native_symbol(&vm, ticket).ends_with("::with") {
                vm.dispatch(&mut session, ticket, "en", NativeBudget::RetryMinimum(128))
                    .unwrap();
                continue;
            }
            let receiver = vm.vm.host_arguments(ticket).unwrap()[0];
            let x = PropertyId {
                host: receiver,
                field: 2 + stage::NodeProperty::X.index() as u32,
            };
            let old = vm.vm.heap().property_source(x).unwrap().0;
            let root = vm
                .vm
                .heap_mut()
                .add_root(RootOwner(777), RootKind::Host, receiver)
                .unwrap();
            let result = vm
                .dispatch(&mut session, ticket, "en", NativeBudget::Exact(64))
                .unwrap();
            assert!(matches!(result, NativeDispatch::BudgetYield { .. }));
            let baseline = vm.vm.heap().stats();
            // The native has responded with its private continuation. Cancellation
            // can now drop it without ever publishing a replacement edge.
            vm.vm.cancel_task(ticket.task).unwrap();
            vm.task = None;
            for _ in 0..10_000 {
                match vm.vm.run_slice(ticket.task, SliceBudget::default(), || 0) {
                    Ok(RunOutcome::BudgetYield(_)) => {}
                    Err(VmError::Idle) => break,
                    other => panic!("unexpected cancelled native outcome: {other:?}"),
                }
            }
            collect(&mut vm);
            let current = vm.vm.heap().property_source(x).unwrap().0;
            assert!(current.host.same_bits(old.host));
            assert_eq!(current.field, old.field);
            assert_eq!(vm.vm.heap().host_property(x).unwrap().as_int(), Some(1));
            vm.vm
                .heap_mut()
                .write_host_property(old, Value::int(5), true)
                .unwrap();
            assert_eq!(vm.vm.heap().host_property(x).unwrap().as_int(), Some(5));
            assert!(vm.vm.heap().stats().live_objects < baseline.live_objects);
            vm.vm.heap_mut().release_root(root).unwrap();
            break;
        }
    }
}

#[test]
fn real_with_updates_report_vm_native_and_storage_cost_against_assignments() {
    for followers in [0, 100, 1000] {
        for chained in [false, true] {
            let update = if chained {
                "node.with(x:index,y:index,opacity:0.5);"
            } else {
                "node.x=index;node.y=index;node.opacity=0.5;"
            };
            let code = format!(
                r#"fn opening() {{
                let node=text("source");
                let followers=[];
                for index in 0..{followers} {{followers.append(text("follower",x:bind(node.x)));}}
                for index in 0..21 {{{update}}}
                if node.x!=20 || node.y!=20 || node.opacity!=0.5 {{throw "configuration result";}}
                for follower in followers {{if follower.x!=20 {{throw "follower result";}}}}
                true
            }}"#
            );
            let (mut session, mut vm) = models::prepared(&code, false);
            let mut vm_work = 0;
            let mut native_work = 0;
            let mut configure_dispatches = 0;
            let mut first_configuration = None;
            let mut last_configuration = None;
            let mut finished = false;
            let started = std::time::Instant::now();
            for _ in 0..100_000 {
                let outcome = vm
                    .step(
                        &mut session,
                        SessionBudget::RetryMinimum(SliceBudget {
                            instructions: 1024,
                            ..Default::default()
                        }),
                        || 0,
                    )
                    .unwrap();
                if let SessionStep::Vm(_) = outcome {
                    vm_work += vm.vm.stats().usage.work;
                }
                match outcome {
                    SessionStep::Vm(RunOutcome::HostCall(ticket)) => {
                        let configuration = native_symbol(&vm, ticket).ends_with("::with");
                        let before = vm.vm.heap().stats();
                        let result = vm
                            .dispatch(&mut session, ticket, "en", NativeBudget::RetryMinimum(4096))
                            .unwrap();
                        if configuration {
                            configure_dispatches += 1;
                            native_work += u64::from(match result {
                                NativeDispatch::Ready { work }
                                | NativeDispatch::BudgetYield { work, .. } => work,
                                other => panic!("{other:?}"),
                            });
                            if matches!(result, NativeDispatch::Ready { .. }) {
                                let stats = vm.vm.heap().stats();
                                if followers == 0 && configure_dispatches > 1 {
                                    assert_eq!(stats.payload_bytes, before.payload_bytes);
                                    assert_eq!(stats.external_bytes, before.external_bytes);
                                }
                                // The first two method lookups finish the lazily built field index.
                                if configure_dispatches >= 2 {
                                    first_configuration.get_or_insert(stats);
                                }
                                last_configuration = Some(stats);
                            }
                        }
                    }
                    SessionStep::Vm(RunOutcome::Returned(id)) => {
                        assert_eq!(vm.vm.delivery_value(id).unwrap().as_bool(), Some(true));
                        finished = true;
                        break;
                    }
                    SessionStep::Vm(RunOutcome::Fault(id)) => panic!(
                        "cost fixture fault: {:?}",
                        vm.vm.delivery_fault(id).unwrap()
                    ),
                    _ => {}
                }
            }
            assert!(finished);
            let elapsed = started.elapsed();
            let growth = first_configuration
                .zip(last_configuration)
                .map(|(first, last)| {
                    (
                        last.charged_bytes - first.charged_bytes,
                        last.chunk_allocations - first.chunk_allocations,
                    )
                });
            if chained && followers == 0 {
                let first = first_configuration.unwrap();
                let last = last_configuration.unwrap();
                assert_eq!(last.payload_bytes, first.payload_bytes);
                assert_eq!(last.external_bytes, first.external_bytes);
                assert_eq!(last.chunk_allocations, first.chunk_allocations);
                // Existing bound-method objects may cross an object-slot capacity boundary.
                // Configuration itself retains no new payload or external storage.
                assert_eq!(
                    last.charged_bytes - first.charged_bytes,
                    last.reserved_bytes - first.reserved_bytes
                );
                assert_eq!(configure_dispatches, 21);
            }
            eprintln!(
                "real DSL updates: with={chained} followers={followers} updates=21 vm_work={vm_work} native_work={native_work} configure_dispatches={configure_dispatches} storage_growth={growth:?} elapsed={elapsed:?}"
            );
        }
    }
}

#[test]
fn with_large_actions_validates_and_adopts_model_data_before_publication() {
    let code = r#"fn opening() {
        let state=model({actions:null});
        let pad=input(actions:bind(state.actions));
        let replacement={};
        for index in 0..64 {replacement[f"action{index}"]={bindings:["KeyD"],on_press:||{}};}
        let bad={invalid:{bindings:["not a control"]}};
        let mut errors=0;
        try {pad.with(actions:bad);} catch error {errors+=1;}
        if errors!=1 || state.actions!=null || pad.actions!=null {throw "invalid actions changed model";}
        let previous={old:{bindings:["KeyA"]}};
        state.actions=previous;
        if pad.actions!=previous {throw "failed actions lost binding";}
        pad.with(actions:replacement);
        if pad.actions!=replacement || state.actions!=replacement {throw "model assignment identity";}
        let nested=button(on_click:bind(replacement.action0.on_press));
        let next=||{};
        replacement.action0.on_press=next;
        if nested.on_click!=next {throw "nested data was not observed";}
        true
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        let (work, calls) = run_configuration(&mut session, &mut vm, NativeBudget::RetryMinimum(1));
        assert!(
            calls > 64,
            "large model data must prepare across native slices"
        );
        eprintln!("large actions with: asb={encoded} work={work} dispatches={calls}");
    }
}

#[test]
fn with_actions_validates_nested_model_followers_against_the_complete_proposal() {
    let code = r#"fn opening() {
        let pad=input();
        let record=model({bindings:["KeyA"],on_action:null,on_change:null});
        let exclusive=|| {};
        let changed=|| {};
        record.on_action=exclusive;
        record.on_change=bind(pad.on_key);
        let actions={main:record};
        let previous={};
        pad.actions=previous;
        let mut errors=0;
        try {pad.with(actions:actions,on_key:changed);} catch error {errors+=1;}
        if errors!=1 || pad.actions!=previous || pad.on_key!=null || record.on_change!=null || record.on_action!=exclusive {
            throw "joint actions validation published an invalid proposal";
        }
        let descriptor=model(["KeyA",1,0]);
        let mut array_rejected=false;
        try {bind(descriptor[2]);} catch error {array_rejected=true;}
        if !array_rejected {throw "array element exposed a property binding";}
        descriptor[2]=bind(pad.x);
        try {pad.with(actions:{array:{bindings:[descriptor]}},x:2);} catch error {errors+=1;}
        if errors!=2 || pad.x!=0 || pad.actions!=previous {throw "descriptor binding passed numeric validation";}
        pad.on_key=changed;
        if record.on_change!=changed {throw "rejected proposal changed the old binding";}
        record.on_action=null;
        for index in 0..32 {
            let follower=model({bindings:["KeyD"],on_change:null});
            follower.on_change=bind(pad.on_key);
            actions[f"action{index}"]=follower;
        }
        pad.with(actions:actions,on_key:exclusive);
        if pad.actions!=actions || pad.on_key!=exclusive || record.on_change!=exclusive {throw "valid proposal missing";}
        for index in 0..32 {if actions[f"action{index}"].on_change!=exclusive {throw "nested follower missing";}}
        true
    }"#;
    for encoded in [false, true] {
        for grant in [1, 4096] {
            let (mut session, mut vm) = models::prepared(code, encoded);
            let (_, calls) =
                run_configuration(&mut session, &mut vm, NativeBudget::RetryMinimum(grant));
            if grant == 1 {
                assert!(
                    calls > 3,
                    "nested proposal validation must resume under small grants"
                );
            }
        }
    }
}

#[test]
fn with_slider_followers_preserves_budget_yields_during_joint_range_validation() {
    let code = r#"fn opening() {
        let state=model({value:1});
        let control=slider(value:bind(state.value),min:0,max:100);
        let followers=[];
        for index in 0..40 {followers.append(slider(value:bind(state.value),min:0,max:100));}
        control.with(value:9,min:5,max:10);
        let mut rejected=false;
        try {control.with(value:11,min:5,max:10);} catch error {rejected=true;}
        if !rejected || control.value!=9 || control.min!=5 || control.max!=10 || state.value!=9 {
            throw "joint slider validation changed committed values";
        }
        for follower in followers {if follower.value!=9 {throw "slider follower missing";}}
        true
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        let (_, calls) = run_configuration(&mut session, &mut vm, NativeBudget::RetryMinimum(1));
        assert!(
            calls > 40,
            "large joint validation must resume under small grants"
        );
    }
}

#[test]
fn with_revalidates_an_initially_empty_actions_table_before_atomic_commit() {
    let code = r#"fn opening() {
        let actions={};
        let pad=input();
        let mut caught=false;
        try {pad.with(actions:actions);} catch error {caught=true;}
        if !caught || pad.actions!=null {throw "changed actions passed old validation";}
        true
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        let mut injected = false;
        let mut finished = false;
        for _ in 0..100_000 {
            match next(&mut vm, &mut session) {
                RunOutcome::HostCall(ticket) => {
                    if native_symbol(&vm, ticket).ends_with("::with") && !injected {
                        let args = vm.vm.host_arguments(ticket).unwrap();
                        let receiver = args[0];
                        let actions = args[1];
                        if let Some(plan) =
                            vm.vm.host_continuation(ticket).unwrap().first().copied()
                        {
                            let writes = [(
                                PropertyId {
                                    host: receiver,
                                    field: 0,
                                },
                                actions,
                            )];
                            // Prepare through the public two-phase protocol so
                            // the mutation occurs before commit regardless of
                            // whether both phases fit one native work grant.
                            let progress = vm
                                .vm
                                .heap_mut()
                                .step_property_batch(
                                    aside_vm::PropertyBatch::from_value(plan),
                                    &writes,
                                    4096,
                                )
                                .unwrap();
                            if matches!(progress, aside_vm::PropertyBatchProgress::Prepared { .. })
                            {
                                let key =
                                    text::alloc_string(vm.vm.heap_mut(), "bad", 4096).unwrap();
                                vm.vm
                                    .heap_mut()
                                    .table_store(actions, 0, key, Value::int(1))
                                    .unwrap();
                                injected = true;
                            }
                        }
                    }
                    vm.dispatch(&mut session, ticket, "en", NativeBudget::RetryMinimum(1))
                        .unwrap();
                }
                RunOutcome::Returned(id) => {
                    assert_eq!(vm.vm.delivery_value(id).unwrap().as_bool(), Some(true));
                    finished = true;
                    break;
                }
                RunOutcome::Fault(id) => panic!(
                    "empty actions fault: {:?}; value {:?}",
                    vm.vm.delivery_fault(id).unwrap(),
                    text::read_string(vm.vm.heap(), vm.vm.delivery_value(id).unwrap(), 4096)
                ),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(injected && finished);
    }
}

struct CountedWrites<'a> {
    values: &'a [(PropertyId, Value)],
    reads: Cell<u64>,
}
impl aside_vm::PropertyWrites for CountedWrites<'_> {
    fn len(&self) -> usize {
        self.values.len()
    }
    fn get(&self, index: usize) -> Option<(PropertyId, Value)> {
        self.reads.set(self.reads.get() + 1);
        self.values.get(index).copied()
    }
}
fn prepared(followers: usize) -> (SessionVm, Value, Value) {
    let code = format!(
        r#"fn opening() {{
        let control=slider(value:0,min:0,max:1);
        let followers=[];
        for index in 0..{followers} {{followers.append(text("follower",x:bind(control.value)));}}
        {{control:control,followers:followers}}
    }}"#
    );
    let (mut session, mut vm) = setup(&code, Context::Ordinary);
    loop {
        match next(&mut vm, &mut session) {
            RunOutcome::HostCall(ticket) => {
                vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(4096))
                    .unwrap();
            }
            RunOutcome::Returned(id) => {
                let result = vm.vm.delivery_value(id).unwrap();
                let control = vm.vm.heap().table_entry(result, 0).unwrap().1;
                let followers = vm.vm.heap().table_entry(result, 1).unwrap().1;
                return (vm, control, followers);
            }
            other => panic!("configuration fixture did not return: {other:?}"),
        }
    }
}

#[test]
fn real_slider_batch_validates_final_bounds_and_retains_bindings_on_failure() {
    use stage::controls::SliderField;
    let (mut vm, control, followers) = prepared(3);
    let field = |field: SliderField| PropertyId {
        host: control,
        field: field.index() as u32,
    };
    let values = [
        (field(SliderField::Min), Value::int(10)),
        (field(SliderField::Max), Value::int(20)),
        (field(SliderField::Value), Value::int(15)),
    ];
    vm.vm
        .heap_mut()
        .write_host_properties(
            &CountedWrites {
                values: &values,
                reads: Cell::new(0),
            },
            true,
        )
        .unwrap();
    assert_eq!(
        vm.vm
            .heap()
            .host_property(field(SliderField::Value))
            .unwrap()
            .as_int(),
        Some(15)
    );
    for index in 0..3 {
        let host = vm.vm.heap().sequence_get(followers, index).unwrap();
        assert_eq!(
            vm.vm
                .heap()
                .host_property(PropertyId {
                    host,
                    field: 2 + stage::NodeProperty::X.index() as u32
                })
                .unwrap()
                .as_int(),
            Some(15)
        );
    }
    let invalid = [
        (field(SliderField::Min), Value::int(30)),
        (field(SliderField::Max), Value::int(25)),
    ];
    assert!(
        vm.vm
            .heap_mut()
            .write_host_properties(
                &CountedWrites {
                    values: &invalid,
                    reads: Cell::new(0)
                },
                true
            )
            .is_err()
    );
    assert_eq!(
        vm.vm
            .heap()
            .host_property(field(SliderField::Min))
            .unwrap()
            .as_int(),
        Some(10)
    );
    assert_eq!(
        vm.vm
            .heap()
            .host_property(field(SliderField::Max))
            .unwrap()
            .as_int(),
        Some(20)
    );
    assert_eq!(
        vm.vm
            .heap()
            .host_property(field(SliderField::Value))
            .unwrap()
            .as_int(),
        Some(15)
    );
}

#[test]
fn actual_node_configuration_cost_includes_proposal_reads_and_shared_followers() {
    use stage::controls::SliderField;
    for count in [0, 100, 1000] {
        let (mut vm, control, _followers) = prepared(count);
        let field = |field: SliderField| PropertyId {
            host: control,
            field: field.index() as u32,
        };
        let values = [
            (field(SliderField::Min), Value::int(10)),
            (field(SliderField::Max), Value::int(20)),
            (field(SliderField::Value), Value::int(15)),
        ];
        let writes = CountedWrites {
            values: &values,
            reads: Cell::new(0),
        };
        vm.vm
            .heap_mut()
            .write_host_properties(&writes, true)
            .unwrap();
        let before = vm.vm.heap().stats();
        let started = std::time::Instant::now();
        writes.reads.set(0);
        for _ in 0..100 {
            vm.vm
                .heap_mut()
                .write_host_properties(&writes, true)
                .unwrap();
        }
        let elapsed = started.elapsed();
        let after = vm.vm.heap().stats();
        let ordinary_started = std::time::Instant::now();
        for _ in 0..100 {
            for &(property, value) in &values {
                vm.vm
                    .heap_mut()
                    .write_host_property(property, value, true)
                    .unwrap();
            }
        }
        let ordinary_elapsed = ordinary_started.elapsed();
        assert_eq!(before.charged_bytes, after.charged_bytes);
        assert_eq!(before.chunk_allocations, after.chunk_allocations);
        eprintln!(
            "node batch: provided=3 followers={count} iterations=100 input_reads={} elapsed={elapsed:?} ordinary_elapsed={ordinary_elapsed:?} charged_growth={} chunks_growth={}",
            writes.reads.get(),
            after.charged_bytes - before.charged_bytes,
            after.chunk_allocations - before.chunk_allocations
        );
    }
}
