//! Source -> analysis -> B01 round trip -> real VM acceptance.
use aside_compiler::{AnalysisOptions, analyze_module, lowering, semantic::NoModules};
use aside_vm::{GcBudget, GcPhase, RootOwner, SessionEpoch, Value, bytecode as bc, executor::*};

fn finish(vm: &mut Vm, task: TaskId, tiny: bool) -> Value {
    let mut budget = if tiny {
        SliceBudget {
            instructions: 1,
            work: 1,
            allocation_bytes: 0,
            deadline_ns: None,
        }
    } else {
        SliceBudget::default()
    };
    loop {
        let outcome = vm.run_slice(task, budget, || 0).unwrap();
        if tiny {
            if vm.heap().phase() == GcPhase::Idle {
                vm.begin_collection().unwrap();
            }
            vm.gc_step(GcBudget {
                units: 1,
                ..Default::default()
            })
            .unwrap();
        }
        match outcome {
            RunOutcome::BudgetYield(usage) => {
                budget.work = usage.required_work.max(1);
                budget.allocation_bytes = usage.required_allocation;
            }
            RunOutcome::Returned(id) => {
                let value = vm.delivery_value(id).unwrap();
                // These acceptance cases return scalars; no unrooted object escapes.
                assert!(value.as_int().is_some() || value.same_bits(Value::NULL));
                vm.discard_delivery(id).unwrap();
                return value;
            }
            RunOutcome::Fault(id) => panic!("unexpected source fault: {:?}", vm.delivery_fault(id)),
            other => panic!("unexpected source outcome: {other:?}"),
        }
    }
}

#[test]
fn source_contracts_survive_codec_tiny_slices_and_interleaved_gc() {
    let text = "中文🙂".repeat(1100);
    let long_text = format!(
        r#"fn main() {{
        let mut value = "{text}";
        value += value;
        if value == "{text}{text}" {{ 42 }} else {{ 0 }}
    }}"#
    );
    let long_sum = format!("fn main() {{ {} }}", vec!["1"; 256].join("+"));
    let long_members = format!(
        "fn main() {{ let x={{next:null,result:42}}; x.next=x; x{}.result + x?.next{}.result }}",
        ".next".repeat(256),
        ".next".repeat(255)
    );
    let nested_children = format!(
        "fn main(node) {{ fn wrap(children=[]) {{ children[0] }} let out={}node;{} if out==node {{42}} else {{0}} }}",
        "wrap() {".repeat(24),
        "};".repeat(24)
    );
    let fragment = "中文🙂".repeat(100);
    let interpolations: Vec<_> = [1, 32, 128]
        .map(|parts| {
            format!(
                "fn main() {{ let fragment=\"{fragment}\"; let value=f\"{}\"; if value==\"{}\" {{42}} else {{0}} }}",
                "{fragment}".repeat(parts), fragment.repeat(parts),
            )
        })
        .into();
    let mut cases = vec![
        (
            "fn main() { let inner={n:1}; let a=[inner,2]; fn change(){a[0]=null; a[1]=9; inner.n=5; 7} let b=[0,...a,change(),...(8,9),null,...[]]; if b[1]==inner && b[1].n==5 && b[2]==2 && b[3]==7 && b[4]==8 && b[5]==9 && b[6]==null && b.len()==7 {42} else {0} }",
            42,
        ),
        (
            "fn main() { let inner={n:1}; let a={x:inner,y:2,[null]:3}; fn change(){a.x=null; a.y=9; inner.n=5; 7} let b={head:0,...a,z:change(),y:8,...{tail:9},...{}}; if b.x==inner && b.x.n==5 && b.y==8 && b.z==7 && b[null]==3 && b.tail==9 && b.len()==6 {42} else {0} }",
            42,
        ),
        (
            "fn main() { let mut n=0; fn side(){n+=1; 0} try { let t={[[]]:side(),...{x:1},last:side()}; } catch e {n+=10;} try { let a=[...null,side()]; } catch e {n+=100;} try { let t={...[],last:side()}; } catch e {n+=1000;} n }",
            1112,
        ),
        (
            "fn main() { fn f(){try { let t={...{x:1},tail:if true {return 42;} else {0}}; } finally { let x=[...[]]; } 0} f() }",
            42,
        ),
        (
            "fn main() { let a=[]; for i in 0..513 { a.append(i); } let b=[...a]; a[0]=9; if b[0]==0 && b[512]==512 && b.len()==513 {42} else {0} }",
            42,
        ),
        (
            "fn main() { fn f(v:(int,string)|null=(41,\"x\"))->(int,string) { if v is null { (0,\"\") } else { v } } let value=f(); if value is (int,string) { value[0]+1 } else { 0 } }",
            42,
        ),
        (
            "let mut global:(int,string)=(1,\"old\"); fn main() { let mut pair:(int,string)=(2,\"local\"); let mut errors=0; fn replace(v) { pair=v; } fn ret(v)->(int,string) { v } fn arg(v:(int,string)) { v } let bad:any=(9,null); try { global=bad; } catch e { errors+=1; } try { replace(bad); } catch e { errors+=1; } try { ret(bad); } catch e { errors+=1; } try { arg(bad); } catch e { errors+=1; } try { let value:(int,string)=bad; } catch e { errors+=1; } replace((7,\"ok\")); global=(30,\"ok\"); global[0]+pair[0]+errors }",
            42,
        ),
        (
            "fn main() { let a=(((1,\"x\"),),true); if a is (((int,string),),bool) && !(a is (((int,int),),bool)) && !((1,2,3) is (int,int)) && ((1,2) is (null|(int,int))) && (() is ()) {42} else {0} }",
            42,
        ),
        (long_text.as_str(), 42),
        (long_sum.as_str(), 256),
        (long_members.as_str(), 84),
        (nested_children.as_str(), 42),
        (
            "fn main() { let mut total=0; for i in 0..1024 { total+=i; } total }",
            523776,
        ),
        (
            "fn main() { let mut total=0; for i in 9223372036854775806..=9223372036854775807 { total+=1; } total }",
            2,
        ),
        (
            "fn main() { let mut total=0; for i in 5..3 { total+=1; } for i in 0..9223372036854775807 { total+=i; if i==3 { break; } } total }",
            6,
        ),
        (
            "fn main() { let mut a=[1,2]; let append=a.append; let pop=a.pop; a=[]; append(...[3,4]); pop()*10+pop() }",
            43,
        ),
        (
            "fn main() { let t={len:||42, value:null}; if t.has_own(\"value\") && !t.has_own(\"missing\") { t.len() } else { 0 } }",
            42,
        ),
        (
            r#"fn main() { let mut n=0; fn bump() { n+=1; n } let s=f"中{bump()}🙂{f"[{bump()}]"}{{字}}"; if s=="中1🙂[2]{字}" {n} else {0} }"#,
            2,
        ),
        (
            r#"fn main() { try { [].pop(); } catch e { return 42; } 0 }"#,
            42,
        ),
        ("fn main() { 6 * 7 }", 42),
        (
            "fn main() { let mut n=0; fn add() { n+=1; n } add(); add(); n }",
            2,
        ),
        ("fn main() { fn f(a, b=a+1) { b } f(41) }", 42),
        (
            "let mut n=0; fn main() { try { try { throw 7; } catch e { n=e; } finally { n+=10; } } finally { n+=100; } n }",
            117,
        ),
        (
            "fn main() { let mut a=[1,2]; let old=a; fn replace() { a=[9]; a[0] } old[0]=replace(); old[0]*10+a[0] }",
            99,
        ),
        ("fn main() { let t={x:2}; t.x+=3; t[\"x\"] }", 5),
        (
            "fn main() { let mut n=0; fn side() { n+=1; 0 } let absent=null; let result=absent?.x[side()]; n }",
            0,
        ),
        (
            "fn main() { fn f(a,...rest) { a+rest[0]+rest[1] } let values=[1,2]; f(...values,3) }",
            6,
        ),
        (
            "fn main() { fn f(...rest) { rest } let first=f(1); let second=f(2); first[0]=9; second[0] }",
            2,
        ),
        ("fn main() { let mut n=0; while n<3 { n+=1; } n }", 3),
        ("fn main() { let mut n=0; null?() { n+=1; }; n }", 0),
        (
            "fn main() { let mut n=0; for value in [1,2,3,4] { if value==2 { continue; } n+=value; if value==3 { break; } } n }",
            4,
        ),
        (
            "fn main() { let callbacks=[null,null,null]; let mut i=0; for value in (1,2,3) { callbacks[i]=||value; i+=1; } callbacks[0]()*100+callbacks[1]()*10+callbacks[2]() }",
            123,
        ),
        (
            r#"fn main(node) {
            fn collect(children=[]) { children }
            fn helper() { node.x+=1; node; }
            fn wrap(value=null, children=[]) { children[0] }
            let children=collect() {
                let ignored=helper();
                node.x=10;
                null;
                for value in [1,2,3] { if (if true { value!=2 } else { false }) { node; } }
                wrap(if true { "argument value" } else { "other" }) { node; };
                { node; }
                node
            };
            children[0]["x"]+=7;
            if children[4]==node { node.x } else { 0 }
        }"#,
            17,
        ),
    ];
    cases.extend(interpolations.iter().map(|source| (source.as_str(), 42)));
    for (source, expected) in cases {
        let module = analyze_module(source, &AnalysisOptions::default(), &mut NoModules)
            .unwrap()
            .into_module()
            .unwrap();
        let compiled = lowering::compile(
            &module,
            &lowering::Linkage::default(),
            lowering::Options::default(),
        )
        .unwrap();
        let package = compiled.program.package();
        let entry = bc::FunctionId(
            package
                .functions
                .iter()
                .position(|f| package.symbols[f.name.index()] == "main")
                .unwrap() as u32,
        );
        let bytes = bc::encode(package, bc::Limits::default()).unwrap();
        for tiny in [false, true] {
            let program = bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap();
            let mut vm = Vm::load(
                program,
                bc::RuntimeSupport {
                    opcodes: vec![],
                    natives: vec![],
                    capabilities: vec![],
                },
                VmLimits::default(),
                SessionEpoch(1),
            )
            .unwrap();
            let init = vm
                .start(compiled.entry, &[], RootOwner(1), bc::Context::Init)
                .unwrap();
            assert!(finish(&mut vm, init, tiny).same_bits(Value::NULL));
            let mut arguments = Vec::new();
            if source.contains("main(node)") {
                let heap = vm.heap_mut();
                let fields = heap.alloc_table().unwrap();
                let name = heap.alloc_string("x").unwrap();
                heap.table_store(fields, 0, name, Value::int(0)).unwrap();
                let node = heap.alloc_composable_host(fields, &[], None).unwrap();
                arguments.push(node);
            }
            let task = vm
                .start(entry, &arguments, RootOwner(1), bc::Context::Ordinary)
                .unwrap();
            let allocations_before = vm.heap().stats().chunk_allocations;
            assert_eq!(
                finish(&mut vm, task, tiny).as_int(),
                Some(expected),
                "{source}, tiny={tiny}"
            );
            if source.contains("for i in 0..1024") {
                assert_eq!(
                    vm.heap().stats().chunk_allocations - allocations_before,
                    2,
                    "range and cursor allocate once; iteration allocates nothing"
                );
            }
            if source.contains("let fragment=") {
                let bytes = source.matches("{fragment}").count() * fragment.len();
                let chunks = vm.heap().stats().chunk_allocations - allocations_before;
                assert_eq!(
                    chunks,
                    bytes.div_ceil(aside_vm::BYTE_CHUNK_LEN) as u64,
                    "interpolation allocates only its final payload, with no copied prefixes"
                );
                println!("interpolation bytes={bytes} payload_chunks={chunks} tiny={tiny}");
            }
            assert!(vm.is_idle());
        }
    }
}
