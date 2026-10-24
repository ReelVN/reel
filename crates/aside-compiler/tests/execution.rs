//! Source -> analysis -> B01 round trip -> real VM acceptance.
use aside_compiler::{AnalysisOptions, analyze_module, lowering, semantic::NoModules};
use aside_vm::{GcBudget, GcPhase, RootOwner, SessionEpoch, Value, bytecode as bc, executor::*};

#[path = "execution/generators.rs"]
mod generators;

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
fn bytes_annotations_check_real_values_and_survive_bytecode() {
    let source = r#"
        fn keep(value: Bytes) -> Bytes { value }
        fn invalid(value: any) -> Bytes { value }
        fn main(value: Bytes) -> int {
            let retained: Bytes|null = keep(value);
            if !(retained is Bytes) || retained is string { throw "byte kind"; }
            let wrong: any = "bytes";
            let mut rejected = 0;
            try { keep(wrong); } catch error { rejected += 1; }
            try { invalid(wrong); } catch error { rejected += 1; }
            40 + rejected
        }
    "#;
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
    assert!(
        package
            .types
            .contains(&bc::TypeDef::Builtin(bc::BuiltinType::Bytes))
    );
    let entry = bc::FunctionId(
        package
            .functions
            .iter()
            .position(|function| package.symbols[function.name.index()] == "main")
            .unwrap() as u32,
    );
    let bytes = bc::encode(package, bc::Limits::default()).unwrap();
    for serialized in [false, true] {
        let program = if serialized {
            bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap()
        } else {
            compiled.program.clone()
        };
        let mut vm = Vm::load(
            program,
            bc::RuntimeSupport {
                host_types: vec![],
                opcodes: vec![],
                natives: vec![],
                capabilities: vec![],
            },
            VmLimits::default(),
            SessionEpoch(1),
        )
        .unwrap();
        let task = vm
            .start(compiled.entry, &[], RootOwner(1), bc::Context::Init)
            .unwrap();
        finish(&mut vm, task, false);
        let value = vm.heap_mut().alloc_bytes(b"PNG").unwrap();
        let task = vm
            .start(entry, &[value], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        assert_eq!(
            finish(&mut vm, task, true).as_int(),
            Some(42),
            "serialized={serialized}"
        );
    }
}

#[test]
fn table_remove_preserves_order_results_and_key_contracts_with_gc() {
    let source = r#"fn main() {
        let item={n:42};
        let table={first:1, middle:item, last:null, [(2,"key")]:9};
        let alias=table;
        let remove=table.remove;
        if remove(key:"middle")!=item || alias.has_own("middle") {throw "returned identity";}
        let keys=table.keys(); let values=table.values(); let entries=table.entries();
        if keys.len()!=3 || keys[0]!="first" || keys[1]!="last" || keys[2]!=(2,"key")
            || values[1]!=null || entries[2]!=((2,"key"),9) {throw "retained insertion order";}
        if table.get("middle",default:8)!=8 || table.get("last",default:8)!=null {throw "lookup defaults";}
        if table.remove((2.0,"key"))!=9 {throw "structural key equality";}
        let mut missing=false;
        try {table.remove("absent");} catch error {missing=true;}
        if !missing || table.len()!=2 {throw "missing key";}
        let mut invalid=false;
        try {table.remove([]);} catch error {invalid=true;}
        if !invalid || table.len()!=2 {throw "invalid key";}
        table.middle=item;
        if table.keys()[2]!="middle" {throw "reinsertion order";}
        if table.remove("first")!=1 {throw "first removal";}
        if table.remove("last")!=null || table.remove("middle").n!=42 || table.len()!=0 {throw "last removal";}
        42
    }"#;
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
                host_types: Vec::new(),
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
        finish(&mut vm, init, tiny);
        let task = vm
            .start(entry, &[], RootOwner(1), bc::Context::Ordinary)
            .unwrap();
        assert_eq!(
            finish(&mut vm, task, tiny).as_int(),
            Some(42),
            "tiny={tiny}"
        );
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
            r#"class Ordering {fn compare(self,a,b){a-b}}
                fn main(){let values=[3,1,2];values.sort(compare:Ordering().compare);
                if values[0]!=1 || values[1]!=2 || values[2]!=3 {throw "callable comparator";}
                let mut rejected=false;try {values.sort(compare:Ordering());} catch error {rejected=true;}
                if !rejected {throw "instance without a callable adapter";}42}"#,
            42,
        ),
        (
            r#"fn main() {
                let record = {name:"Ada"};
                let values = [null, 0, 42, 42.0, "文字", (1, ("中文", record)), record];
                if values.find(42.0)!=2 || !values.contains(value:42) {throw "numeric equality";}
                if values.find(false)!=null || values.find(null)!=0 {throw "null or boolean";}
                if values.find((1.0,("中文",record)))!=5 {throw "tuple equality";}
                if values.contains({name:"Ada"}) || values.find(record)!=6 {throw "record identity";}
                if [].contains(null) || [].find(null)!=null {throw "empty array";}
                if (1,"two").find("two")!=1 || !(1,"two").contains(1.0) {throw "tuple receiver";}
                let find = values.find;
                let contains = values.contains;
                if find(value:"文字")!=4 || !contains(null) {throw "bound methods";}
                let nan = (1e308*1e308)-(1e308*1e308);
                if [nan].contains(nan) || [(nan,)].contains((nan,)) {throw "nan equality";}
                if [9007199254740993].contains(9007199254740992.0) {throw "exact integer comparison";}
                let mut rejected=0;
                fn unknown(value){value}
                let dynamic=unknown(values);
                try {dynamic.find();} catch e {rejected+=1;}
                try {dynamic.contains(other:1);} catch e {rejected+=1;}
                try {dynamic.find(1,2);} catch e {rejected+=1;}
                if rejected!=3 {throw "argument contracts";}
                42
            }"#,
            42,
        ),
        (
            r#"fn options(flag) {
                if flag { {accept: [40]} } else { {back: [2]} }
            }
            fn empty_block() { {} }
            fn empty_table() { ({}) }
            fn main() {
                let mut calls=0;
                fn key() { calls+=1; "answer" }
                let keys=["answer"];
                let first=if true { {[keys[0]]: options(true).accept[0]} }
                    else { {back: 0} };
                let second=if false { {unused: key()} }
                    else { {[key()]: options(false).back[0]} };
                let copy=|| { {...first, f"{"中文"}": second.answer} };
                let result=match 1 { _ => { {answer: copy().answer + copy()["中文"]} } };
                let mut visits=0;
                { outer: loop { visits+=1; break outer; } }
                { outer: while true { visits+=1; break outer; } }
                { outer: for item in [1] { visits+=1; break outer; } }
                if calls!=1 || visits!=3 || empty_block()!=null || empty_table().len()!=0 {
                    throw "table/block selection changed execution";
                }
                result.answer
            }"#,
            42,
        ),
        (
            r#"fn main() {
                let mut calls=0; let mut guards=0;
                fn source(){calls+=1; "hello"}
                fn guard(){guards+=1; false}
                let result=match source() {
                    "other" if guard() => 0,
                    ("hello" | "hi") if guard() => 0,
                    name is string if name=="hello" => 40,
                    _ => 0,
                };
                if calls!=1 || guards!=1 {throw "evaluation order";}
                fn scalar(value) {match value {
                    -9223372036854775808 => 1,
                    -2.5 => 2,
                    true => 3,
                    false => 4,
                    null => 5,
                    x is int | x is string => if x==7 {6} else {7},
                    _ => 8,
                }}
                if scalar(-9223372036854775808)!=1 || scalar(-2.5)!=2 {throw "negative";}
                if scalar(true)!=3 || scalar(false)!=4 || scalar(null)!=5 {throw "literal";}
                if scalar(7)!=6 || scalar("中")!=7 || scalar([])!=8 {throw "typed OR";}
                result+calls+guards
            }"#,
            42,
        ),
        (
            r#"fn main() {
                let callbacks=[]; let rejected=[];
                fn reject(callback){rejected.append(callback); false}
                for seed in [10,20] {
                    match seed {
                        item if reject(||item) => {throw "guard";},
                        item => {callbacks.append(||item);},
                    };
                }
                if rejected[0]()!=10 || rejected[1]()!=20 {throw "rejected captures";}
                if callbacks[0]()!=10 || callbacks[1]()!=20 {throw "accepted captures";}
                let mut sum=0;
                for seed in 0..5 {
                    try {match seed {0=>{continue;}, 4=>{break;}, n=>{sum+=n;}};}
                    finally {sum+=1;}
                }
                fn classify(value) {match value {1=>{return 20;}, _=>{return 22;}}}
                if sum!=11 {throw "match cleanup";}
                classify(1)+classify(null)
            }"#,
            42,
        ),
        (
            r#"fn main() {
                let mut caught=0; let mut cleaned=0;
                fn classify(value){match value {1=>10, "name"=>20}}
                fn reject(){throw "guard";}
                try {classify(2);} catch e {caught+=1;} finally {cleaned+=1;}
                try {match 7 {n if reject() =>n, _=>0};}
                catch e {if e=="guard" {caught+=1;}} finally {cleaned+=1;}
                if caught==2 && cleaned==2 {42} else {0}
            }"#,
            42,
        ),
        (
            r#"fn main(node) {
                fn children(children=[]){children}
                let nodes=children() {
                    match "a" {"a"=>node, _=>null};
                    match 2 {1=>null, 2=>{node; node;}, _=>null};
                    match true {true=>match null {null=>node, _=>null}, _=>null};
                };
                if nodes.len()==4 && nodes[0]==node && nodes[3]==node {42} else {0}
            }"#,
            42,
        ),
        (
            r#"fn main() {
                fn render(a, b=20, children=[]) { a+b+children.len() }
                fn forward(callback, values, options) { callback(...values,...options) }
                if forward(render,[19],{b:22,children:[0]})!=42 {throw "forward";}
                let with_children=render(...{b:40,a:2}) {null;};
                if with_children!=42 {throw "children";}
                if render(a:20,...{b:22})!=42 {throw "named then table";}
                fn defaults(a, b=a+1) {a+b}
                if defaults(...{a:20})!=41 {throw "default prefix";}
                fn values(a=7,...rest) {rest}
                if values(...{a:1}).len()!=0 {throw "empty rest";}
                42
            }"#,
            42,
        ),
        (
            r#"fn main() {
                let shared={n:1}; let options={a:shared,b:2};
                fn change(){options.a=null;options.b=9;shared.n=40;2}
                fn f(a,b,c){a.n+b+c}
                if f(...options,c:change())!=44 {throw "shallow capture";}
                fn style(a,b,c){if a==false && b==0 && c==null {42}else{0}}
                style(...{...{a:true,b:8,c:9},...{a:false,b:0,c:null}})
            }"#,
            42,
        ),
        (
            r#"fn main() {
                let mut evaluated=0; let mut entered=0; let mut caught=0;
                fn effect(){evaluated+=1;9}
                fn f(a,b=0,...rest){entered+=1;0}
                try {f(...{a:1},a:effect());} catch e {caught+=1;}
                try {f(...{unknown:1},b:effect());} catch e {caught+=1;}
                try {f(...{b:1},effect());} catch e {caught+=1;}
                try {f(...{a:1},...{a:effect()});} catch e {caught+=1;}
                try {f(...{rest:[]},b:effect());} catch e {caught+=1;}
                try {f(...{[3]:1},b:effect());} catch e {caught+=1;}
                if evaluated!=5 || entered!=0 || caught!=6 {throw "evaluation/binding boundary";}
                fn with_children(children=[]){entered+=1;}
                fn child(){evaluated+=1;null}
                try {with_children(...{children:[]}) {child();};} catch e {caught+=1;}
                if evaluated==6 && entered==0 && caught==7 {42} else {0}
            }"#,
            42,
        ),
        (
            r#"fn main() {
                let a=[1,2]; let resize=a.resize;
                fn unknown(value){value}
                let dynamic=unknown(a);
                resize(...{value:8,length:4});
                if a.len()!=4 || a[3]!=8 {throw "bound builtin";}
                a.insert(index:1,...{value:3});
                if a.remove(...{index:1})!=3 {throw "builtin names";}
                if !"中文🙂".starts_with(...{prefix:"中文"}) {throw "string builtin";}
                if !({key:1}).has_own(...{key:"key"}) {throw "table builtin";}
                let mut caught=0;
                try {dynamic.resize(...{length:6},...{length:8});} catch e {caught+=1;}
                try {dynamic.insert(...{value:1},0);} catch e {caught+=1;}
                try {dynamic.append(...{value:1});} catch e {caught+=1;}
                if caught==3 && a.len()==4 {42} else {0}
            }"#,
            42,
        ),
        (
            r#"fn main() {
            let s="A中🙂é\0";
            if s.byte_len()!=12 || s.scalar_count()!=6 {throw "text units";}
            if s.slice_bytes(range:1..8)!="中🙂" || s.slice_scalars(1..=2)!="中🙂" {throw "slice";}
            let cut=s.slice_scalars;
            if cut(range:4..5)!="́" || cut(6..6)!="" || s.slice_bytes(12..12)!="" {throw "scalar boundary";}
            if !s.starts_with(prefix:"A中") || !s.ends_with(suffix:"é\0") {throw "prefix/suffix";}
            if s.starts_with("中") || s.ends_with("A") || s.starts_with(s+"x") {throw "mismatch";}
            if !s.starts_with("") || !"".ends_with("") || "".byte_len()!=0 || "".scalar_count()!=0 {throw "empty text";}
            if " · ".join(items:["中","🙂",""])!="中 · 🙂 · " {throw "join array";}
            if "".join(("e","́","\0"))!="é\0" || "x".join([])!="" || "x".join(())!="" {throw "join tuple";}
            if "x".join([""])!="" || "x".join(["",""])!="x" {throw "empty elements";}
            if "é"=="é" {throw "implicit normalization";}
            if "EMily中Éß🙂".to_ascii_lowercase()!="emily中Éß🙂" {throw "ASCII lower";}
            if "emILY中éß🙂".to_ascii_uppercase()!="EMILY中éß🙂" {throw "ASCII upper";}
            let lower="".to_ascii_lowercase;
            if lower()!="" || "".to_ascii_uppercase()!="" {throw "empty case";}
            42
        }"#,
            42,
        ),
        (
            r#"fn main() {
            let s="A中🙂é\0"; let mut caught=0;
            fn unknown(value){value}
            let dynamic=unknown(s);
            try {dynamic.slice_bytes(2..4);} catch e {caught+=1;}
            try {dynamic.slice_bytes(1..2);} catch e {caught+=1;}
            try {dynamic.slice_bytes(0..13);} catch e {caught+=1;}
            try {dynamic.slice_scalars(0..7);} catch e {caught+=1;}
            try {dynamic.slice_scalars(3..2);} catch e {caught+=1;}
            try {dynamic.slice_bytes(-1..0);} catch e {caught+=1;}
            try {dynamic.slice_scalars(0..=6);} catch e {caught+=1;}
            try {dynamic.starts_with(null);} catch e {caught+=1;}
            try {dynamic.join(["a",1]);} catch e {caught+=1;}
            try {dynamic.join(null);} catch e {caught+=1;}
            try {dynamic.starts_with(suffix:"A");} catch e {caught+=1;}
            try {dynamic.join();} catch e {caught+=1;}
            try {dynamic.starts_with("A",prefix:"A");} catch e {caught+=1;}
            if caught==13 && s=="A中🙂é\0" {42} else {0}
        }"#,
            42,
        ),
        (
            r#"fn main() {
            let shared={n:1}; let a=[shared,2,3]; let alias=a;
            if a.insert(value:4,index:1)!=a {throw "identity";}
            if a.remove(index:2)!=2 || a.top()!=3 {throw "remove";}
            a.resize(value:shared,length:5); a[3].n=9;
            if a[4].n!=9 || alias[0].n!=9 {throw "sharing";}
            a.reverse(); a.swap(b:4,a:0);
            let copy=a.slice(range:1..=3);
            a.clear();
            if alias.len()!=0 || copy.len()!=3 || copy[0].n!=9 || copy[1]!=3 || copy[2]!=4 {throw "slice/clear";}
            a.extend((1,2)); a.extend(items:a); a.resize(length:6);
            if a.len()!=6 || a[2]!=1 || a[3]!=2 || a[4]!=null || a[5]!=null {throw "extend/resize";}
            if (1,2,3).slice(1..3)!=(2,3) || ().slice(0..0)!=() {throw "tuple slice";}
            let t={x:shared,y:2}; let keys=t.keys(); let values=t.values(); let entries=t.entries();
            if keys[0]!="x" || keys[1]!="y" || values[0]!=shared || entries[0]!=("x",shared) {throw "own fields";}
            if t.clear()!=t || t.len()!=0 || entries.len()!=2 {throw "table clear";}
            42
        }"#,
            42,
        ),
        (
            r#"fn main() {
            let a=[1,2]; let mut caught=0; let mut evaluated=0;
            fn unknown(value){value}
            let dynamic=unknown(a);
            fn effect(){evaluated+=1; 9}
            try {dynamic.insert(0,index:effect(),value:3);} catch e {caught+=1;}
            try {dynamic.insert(value:3);} catch e {caught+=1;}
            try {dynamic.resize(length:3,unknown:effect());} catch e {caught+=1;}
            try {dynamic.insert(index:3,value:0);} catch e {caught+=1;}
            try {dynamic.remove(-1);} catch e {caught+=1;}
            try {dynamic.slice(0..3);} catch e {caught+=1;}
            try {dynamic.slice(1..0);} catch e {caught+=1;}
            try {dynamic.extend(null);} catch e {caught+=1;}
            try {[].top();} catch e {caught+=1;}
            try {dynamic.swap(0,7);} catch e {caught+=1;}
            if caught==10 && evaluated==2 && a.len()==2 && a[0]==1 && a[1]==2 {42} else {0}
        }"#,
            42,
        ),
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
            r#"fn main() {
                let value={n:42};
                let t={nil:null, off:false, zero:0, value:value, [null]:7, [(1,"x")]:9};
                if t.get("nil",1)!=null || t.get("off",true)!=false || t.get("zero",1)!=0 {throw "present values";}
                if t.get("absent")!=null || t.get("get",default:3)!=3 {throw "missing slots";}
                if t.get("value")!=value || t.get("absent",default:value)!=value {throw "identity";}
                if t.get(null)!=7 || t.get((1.0,"x"))!=9 || t.len()!=6 {throw "keys/read only";}
                42
            }"#,
            42,
        ),
        (
            r#"fn main() {
                let mut t={n:42}; let get=t.get; t={n:0};
                let mut evaluated=0;
                fn fallback(){evaluated+=1; ({n:42})}
                if get(default:fallback(),key:"n")!=42 || evaluated!=1 {throw "bound receiver/evaluation";}
                if get(...["absent",fallback()]).n!=42 || evaluated!=2 {throw "packed argument roots";}
                if get("absent",default:{n:42}).n!=42 {throw "argument roots";}
                let own={get:|key,default| key+default};
                own.get(40,default:2)
            }"#,
            42,
        ),
        (
            r#"fn main() {
                let t={}; let mut caught=0;
                try {t.get([]);} catch e {caught+=1;}
                try {t.get((1,{}));} catch e {caught+=1;}
                try {t["absent"];} catch e {caught+=1;}
                try {t.get();} catch e {caught+=1;}
                try {t.get("absent",key:"again");} catch e {caught+=1;}
                try {t.get("absent",unknown:1);} catch e {caught+=1;}
                if caught==6 && t.get("absent",42)==42 && t.len()==0 {42} else {0}
            }"#,
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
    // Every UTF-8 scalar width crosses the heap's byte-chunk boundary; the
    // standard library supplies the expected byte/scalar slices independently.
    let text_cases: Vec<_> = (1021..=1025).map(|padding| {
        let text = format!("{}中🙂é👩‍🚀{}", "x".repeat(padding), "尾".repeat(700));
        let head: String = text.chars().take(padding + 6).collect();
        let tail: String = text.chars().skip(padding + 3).collect();
        let slice: String = text.chars().skip(padding).take(8).collect();
        format!(r#"fn main() {{
            let s="{text}";
            if match s {{"{text}x"=>0, "{text}"=>42, _=>0}} !=42 {{throw "long match";}}
            if s.byte_len()!={} || s.scalar_count()!={} {{throw "long count";}}
            if s.slice_scalars({padding}..{})!="{slice}" {{throw "long scalar slice";}}
            if s.slice_bytes({padding}..{})!="{slice}" {{throw "long byte slice";}}
            if !s.starts_with("{head}") || !s.ends_with("{tail}") {{throw "long comparison";}}
            if s.ends_with("x{tail}") || s.starts_with("{head}x") {{throw "long mismatch";}}
            let parts=[]; for n in 0..32 {{parts.append(s);}}
            let joined=" · ".join(parts);
            if joined.byte_len()!=s.byte_len()*32+4*31 || joined.scalar_count()!=s.scalar_count()*32+3*31 {{throw "long join";}}
            42
        }}"#, text.len(), text.chars().count(), padding+8, padding+slice.len())
    }).collect();
    cases.extend(text_cases.iter().map(|source| (source.as_str(), 42)));
    let long_separator = format!("{}b", "a".repeat(1030));
    let repetitive = format!("{}b尾{}b", "a".repeat(4100), "a".repeat(1030));
    let crossing = format!("{}中文🙂尾中文🙂", "x".repeat(1023));
    let split_cases: Vec<_> = [
        ("characters/sayori.chr", "/"),
        ("/a//b/", "/"),
        ("aaaaa", "aa"),
        ("", "/"),
        ("", ""),
        ("中🙂é", ""),
        ("abc", "longer"),
        (crossing.as_str(), "中文🙂"),
        (repetitive.as_str(), long_separator.as_str()),
    ]
    .into_iter()
    .map(|(text, separator)| {
        let expected: Vec<_> = text.split(separator).collect();
        let checks = expected
            .iter()
            .enumerate()
            .map(|(i, value)| format!("parts[{i}]!={value:?}"))
            .collect::<Vec<_>>()
            .join(" || ");
        format!(
            r#"fn main() {{
            let split={text:?}.split;
            let parts=split(separator:{separator:?});
            if parts.len()!={} || {checks} {{throw "split result";}}
            if {separator:?}.join(parts)!={text:?} {{throw "split/join";}}
            parts.append("mutable result");
            let mut rejected=false;
            fn unknown(value){{value}}
            let dynamic=unknown(split);
            try {{dynamic(null);}} catch error {{rejected=true;}}
            if rejected {{42}} else {{0}}
        }}"#,
            expected.len()
        )
    })
    .collect();
    cases.extend(split_cases.iter().map(|source| (source.as_str(), 42)));
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
                    host_types: Vec::new(),
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
                    0,
                    "range and cursor fit inline; iteration allocates no payload blocks"
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

mod sorting {
    //! Source/B01/VM sorting contracts, including suspended work and ordinary callbacks.
    use aside_compiler::{AnalysisOptions, analyze_module, lowering, semantic::NoModules};
    use aside_vm::{
        GcBudget, GcPhase, RootKind, RootOwner, SessionEpoch, Value, bytecode as bc,
        executor::*,
        snapshot::{SnapshotBudget, SnapshotRegistry, VmSnapshot},
    };

    fn compile(source: &str) -> lowering::Compilation {
        let module = analyze_module(source, &AnalysisOptions::default(), &mut NoModules)
            .unwrap()
            .into_module()
            .unwrap_or_else(|e| panic!("{source}\n{e:?}"));
        lowering::compile(
            &module,
            &lowering::Linkage::default(),
            lowering::Options::default(),
        )
        .unwrap()
    }

    fn load(source: &str) -> Vm {
        let compiled = compile(source);
        let bytes = bc::encode(compiled.program.package(), bc::Limits::default()).unwrap();
        let mut vm = Vm::load(
            bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap(),
            bc::RuntimeSupport::default(),
            VmLimits::default(),
            SessionEpoch(1),
        )
        .unwrap();
        let task = vm
            .start(compiled.entry, &[], RootOwner(1), bc::Context::Init)
            .unwrap();
        finish(&mut vm, task, false);
        vm
    }

    fn start(vm: &mut Vm, name: &str, args: &[Value]) -> TaskId {
        let p = vm.program().package();
        let id = bc::FunctionId(
            p.functions
                .iter()
                .position(|f| p.symbols[f.name.index()] == name)
                .unwrap() as u32,
        );
        vm.start(id, args, RootOwner(1), bc::Context::Ordinary)
            .unwrap()
    }

    fn tiny_budget() -> SliceBudget {
        SliceBudget {
            instructions: 1,
            work: 1,
            allocation_bytes: 0,
            deadline_ns: None,
        }
    }

    fn collect(vm: &mut Vm) {
        if vm.heap().phase() == GcPhase::Idle {
            vm.begin_collection().unwrap();
        }
        vm.gc_step(GcBudget {
            units: 1,
            ..Default::default()
        })
        .unwrap();
    }

    fn next(vm: &mut Vm, task: TaskId, tiny: bool) -> RunOutcome {
        let mut budget = if tiny {
            tiny_budget()
        } else {
            SliceBudget::default()
        };
        for _ in 0..1_000_000 {
            let result = vm.run_slice(task, budget, || 0).unwrap();
            if tiny {
                collect(vm);
            }
            match result {
                RunOutcome::BudgetYield(usage) => {
                    if tiny {
                        budget.work = usage.required_work.max(1);
                        budget.allocation_bytes = usage.required_allocation;
                    } else {
                        budget.work = budget.work.max(usage.required_work);
                        budget.allocation_bytes =
                            budget.allocation_bytes.max(usage.required_allocation);
                    }
                }
                _ => return result,
            }
        }
        panic!("sort must make progress");
    }

    fn finish(vm: &mut Vm, task: TaskId, tiny: bool) -> Value {
        match next(vm, task, tiny) {
            RunOutcome::Returned(id) => {
                let value = vm.delivery_value(id).unwrap();
                vm.discard_delivery(id).unwrap();
                value
            }
            RunOutcome::Fault(id) => panic!("sort fault: {:?}", vm.delivery_fault(id)),
            other => panic!("sort outcome: {other:?}"),
        }
    }

    #[test]
    fn default_order_stability_callbacks_and_atomic_failures_survive_small_slices_and_gc() {
        let prefix = "字".repeat(80);
        let source = format!(
            r#"
        class ByKey {{ let direction = 1; fn compare(self, a, b) {{ (a.key - b.key) * self.direction }} }}
        fn main() {{
            let empty = []; if empty.sort() != empty {{ throw "empty identity"; }}
            let one = [9]; if one.sort(null) != one || one[0] != 9 {{ throw "singleton"; }}
            let numbers = [9007199254740993, -2, 9007199254740992.0, 1.0, 1, 0];
            if numbers.sort() != numbers {{ throw "identity"; }}
            if numbers[0] != -2 || numbers[1] != 0 || numbers[2] != 1.0 || numbers[3] != 1
                || numbers[4] != 9007199254740992 || numbers[5] != 9007199254740993 {{ throw "numeric order"; }}
            let strings = ["{prefix}🙂", "z", "{prefix}a", "", "{prefix}"];
            strings.sort();
            if strings[0] != "" || strings[1] != "z" || strings[2] != "{prefix}"
                || strings[3] != "{prefix}a" || strings[4] != "{prefix}🙂" {{ throw "text order"; }}
            let items = [{{key: 2, id: 0}}, {{key: 1, id: 1}}, {{key: 2, id: 2}}, {{key: 1, id: 3}}];
            let first = items[0];
            items.sort(compare: ByKey().compare);
            if items[0].id != 1 || items[1].id != 3 || items[2] != first || items[3].id != 2 {{ throw "stable method"; }}
            let direction = -1;
            items.sort(|a, b| (a.key - b.key) * direction);
            if items[0] != first || items[1].id != 2 || items[2].id != 1 || items[3].id != 3 {{ throw "stable closure"; }}
            let source = [3, 2, 1];
            let mut caught = 0;
            try {{ source.sort(|a, b| {{ throw "compare"; }}); }} catch e {{ if e == "compare" {{ caught += 1; }} }}
            try {{ source.sort(|a, b| true); }} catch e {{ caught += 1; }}
            try {{ source.sort(42); }} catch e {{ caught += 1; }}
            if caught != 3 || source[0] != 3 || source[1] != 2 || source[2] != 1 {{ throw "failed copy published"; }}
            try {{ source.sort(|a, b| {{ source[1] = 99; a - b }}); }} catch e {{ caught += 1; }}
            if caught != 4 || source[0] != 3 || source[1] != 99 || source[2] != 1 {{ throw "concurrent write overwritten"; }}
            let mixed = [2, "1"];
            try {{ mixed.sort(); }} catch e {{ caught += 1; }}
            if caught != 5 || mixed[0] != 2 || mixed[1] != "1" {{ throw "invalid default"; }}
            42
        }}
    "#
        );
        for tiny in [false, true] {
            let mut vm = load(&source);
            let task = start(&mut vm, "main", &[]);
            assert_eq!(finish(&mut vm, task, tiny).as_int(), Some(42));
        }
    }

    #[test]
    fn snapshots_at_every_execution_boundary_preserve_comparator_effects_and_sort_cursors() {
        let source = r#"
        fn main() {
            let a = [4, 1, 3, 2];
            let mut calls = "";
            a.sort(|x, y| { calls += f"{x}{y},"; x - y });
            if calls != "41,32,12,42,43," { throw calls; }
            for i in 0..4 { if a[i] != i + 1 { throw "order"; } }
            let b = ["zz", "za"];
            b.sort();
            if b[0] != "za" || b[1] != "zz" { throw "text"; }
            42
        }
    "#;
        let mut vm = load(source);
        let package = vm.program().package().clone();
        let registry = SnapshotRegistry::default();
        let mut task = start(&mut vm, "main", &[]);
        let mut budget = tiny_budget();
        for epoch in 2..10_000 {
            let result = vm.run_slice(task, budget, || 0).unwrap();
            collect(&mut vm);
            if let RunOutcome::BudgetYield(usage) = result {
                budget.work = usage.required_work.max(1);
                budget.allocation_bytes = usage.required_allocation;
            }
            let mut capture = vm.snapshot_begin(&registry).unwrap();
            while !capture.step(SnapshotBudget { units: 3 }).unwrap().complete {}
            let image = VmSnapshot::decode(&capture.finish().unwrap().encode().unwrap()).unwrap();
            let mut restore = Vm::restore_begin(
                &image,
                bc::verify(package.clone(), bc::Limits::default()).unwrap(),
                bc::RuntimeSupport::default(),
                VmLimits::default(),
                SessionEpoch(epoch),
                &registry,
            )
            .unwrap();
            while !restore.step(SnapshotBudget { units: 3 }).unwrap().complete {}
            let (loaded, map) = restore.finish().unwrap();
            vm = loaded;
            task = map.task(task).unwrap();
            match result {
                RunOutcome::Returned(id) => {
                    assert_eq!(
                        vm.delivery_value(map.delivery(id).unwrap())
                            .unwrap()
                            .as_int(),
                        Some(42)
                    );
                    return;
                }
                RunOutcome::BudgetYield(_) => (),
                other => panic!("snapshot outcome: {other:?}"),
            }
        }
        panic!("restored sort must finish");
    }

    #[test]
    fn cancellation_and_other_tasks_cannot_publish_a_partial_sort() {
        let source = r#"
        let values = [3, 1, 2]; let mut comparing = false; let mut proceed = false;
        fn sorting() { values.sort(|a, b| { comparing = true; while !proceed {} a - b }); }
        fn observed() { comparing }
        fn mutate() { values[1] = 99; proceed = true; }
        fn unchanged() { values[0] == 3 && values[1] == 1 && values[2] == 2 }
        fn preserved() { values[0] == 3 && values[1] == 99 && values[2] == 2 }
    "#;
        for cancel in [true, false] {
            let mut vm = load(source);
            let task = start(&mut vm, "sorting", &[]);
            assert!(matches!(
                vm.run_slice(task, SliceBudget::default(), || 0).unwrap(),
                RunOutcome::BudgetYield(_)
            ));
            let check = start(&mut vm, "observed", &[]);
            assert_eq!(finish(&mut vm, check, true).as_bool(), Some(true));
            if cancel {
                vm.cancel_cooperatively(task).unwrap();
            } else {
                let mutate = start(&mut vm, "mutate", &[]);
                finish(&mut vm, mutate, true);
            }
            let RunOutcome::Fault(id) = next(&mut vm, task, true) else {
                panic!("sort must fault");
            };
            assert_eq!(
                vm.delivery_fault(id).unwrap().unwrap().kind,
                if cancel {
                    FaultKind::Cancelled
                } else {
                    FaultKind::ConcurrentModification
                }
            );
            vm.discard_delivery(id).unwrap();
            let check = start(&mut vm, if cancel { "unchanged" } else { "preserved" }, &[]);
            assert_eq!(finish(&mut vm, check, true).as_bool(), Some(true));
        }
    }

    #[test]
    fn reverse_input_has_bounded_merge_comparisons() {
        let mut vm = load(
            r#"
        fn main() {
            let values = []; for i in 0..4096 { values.append(4095 - i); }
            let mut calls = 0;
            values.sort(|a, b| { calls += 1; a - b });
            for i in 0..4096 { if values[i] != i { throw "order"; } }
            if calls > 4096 * 12 { throw "quadratic comparisons"; }
            calls
        }
    "#,
        );
        let task = start(&mut vm, "main", &[]);
        assert_eq!(finish(&mut vm, task, false).as_int(), Some(4096 * 6));
    }

    #[test]
    fn native_comparators_share_call_binding_and_cannot_wait() {
        let source = r#"
        let values = [3, 1, 2];
        fn sorting(compare) { values.sort(compare); values[0] * 100 + values[1] * 10 + values[2] }
        fn awaiting(request) { values.sort(|a, b| { await request; a - b }); }
        fn unchanged() { values[0] == 3 && values[1] == 1 && values[2] == 2 }
    "#;
        let compiled = compile(source);
        let mut package = compiled.program.to_unpacked().unwrap();
        let any = bc::TypeId(
            package
                .types
                .iter()
                .position(|t| *t == bc::TypeDef::Builtin(bc::BuiltinType::Any))
                .unwrap() as u32,
        );
        let mut support = bc::RuntimeSupport::default();
        let mut functions = Vec::new();
        for (symbol, names) in [
            ("host::compare", &["receiver", "a", "b"][..]),
            ("host::wait", &["request"][..]),
        ] {
            let name = bc::SymbolId(package.symbols.len() as u32);
            package.symbols.push(symbol.into());
            let parameters = names
                .iter()
                .map(|&name| {
                    let symbol = if let Some(index) = package.symbols.iter().position(|s| s == name)
                    {
                        bc::SymbolId(index as u32)
                    } else {
                        let symbol = bc::SymbolId(package.symbols.len() as u32);
                        package.symbols.push(name.into());
                        symbol
                    };
                    bc::Parameter {
                        name: symbol,
                        ty: any,
                        mutable: false,
                        default: None,
                    }
                })
                .collect();
            let contract = bc::Signature {
                parameters,
                rest: None,
                return_type: any,
                effects: bc::Effects::ALL,
                property_bag_tail: None,
            };
            let signature = bc::SignatureId(package.signatures.len() as u32);
            package.signatures.push(contract.clone());
            let native = bc::NativeId(package.natives.len() as u32);
            package.natives.push(bc::NativeDef {
                symbol: name,
                signature,
                abi_version: 1,
                capabilities: vec![],
            });
            let function = bc::FunctionId(package.functions.len() as u32);
            package.functions.push(bc::FunctionDef {
                module: bc::ModuleId(0),
                name,
                kind: bc::FunctionKind::Native(native),
                signature,
                context: bc::Context::Ordinary,
                register_count: 0,
                captures: vec![],
                bindings: vec![],
                descriptors: vec![],
                code: vec![],
                regions: vec![],
                source_map: vec![],
            });
            functions.push(function);
            support.natives.push(bc::NativeSupport {
                symbol: symbol.into(),
                abi_version: 1,
                signature: contract,
                required_capabilities: vec![],
            });
        }
        let bytes = bc::encode(&package, bc::Limits::default()).unwrap();
        for mode in 0..3 {
            let mut vm = Vm::load(
                bc::VerifiedProgram::decode(&bytes, bc::Limits::default()).unwrap(),
                support.clone(),
                VmLimits::default(),
                SessionEpoch(1),
            )
            .unwrap();
            let init = vm
                .start(compiled.entry, &[], RootOwner(1), bc::Context::Init)
                .unwrap();
            finish(&mut vm, init, true);
            let fields = vm.heap_mut().alloc_table().unwrap();
            let host = vm.heap_mut().alloc_host(fields, &[], None).unwrap();
            let callable = vm.bind_native(host, functions[0]).unwrap();
            vm.bind_awaitable(host, bc::NativeId(1)).unwrap();
            let root = vm
                .heap_mut()
                .add_root(RootOwner(1), RootKind::Host, callable)
                .unwrap();
            let task = start(
                &mut vm,
                if mode == 2 { "awaiting" } else { "sorting" },
                &[if mode == 2 { host } else { callable }],
            );
            let mut calls = 0;
            loop {
                match next(&mut vm, task, true) {
                    RunOutcome::HostCall(ticket) => {
                        assert_ne!(mode, 2, "await is prohibited before adapter dispatch");
                        calls += 1;
                        let args = vm.host_arguments(ticket).unwrap();
                        assert!(args[0].same_bits(host));
                        let order = args[1].as_int().unwrap() - args[2].as_int().unwrap();
                        let response = if mode == 1 {
                            HostResponse::Pending(ticket.wait_id())
                        } else {
                            HostResponse::Ready(Value::int(order))
                        };
                        vm.respond(ticket, response).unwrap();
                    }
                    RunOutcome::Returned(id) => {
                        assert_eq!(mode, 0);
                        assert_eq!(vm.delivery_value(id).unwrap().as_int(), Some(123));
                        assert_eq!(calls, 3);
                        vm.discard_delivery(id).unwrap();
                        break;
                    }
                    RunOutcome::Fault(id) => {
                        assert_ne!(mode, 0);
                        assert_eq!(
                            vm.delivery_fault(id).unwrap().unwrap().kind,
                            FaultKind::Effect
                        );
                        vm.discard_delivery(id).unwrap();
                        let check = start(&mut vm, "unchanged", &[]);
                        assert_eq!(finish(&mut vm, check, true).as_bool(), Some(true));
                        break;
                    }
                    other => panic!("native sort outcome: {other:?}"),
                }
            }
            vm.heap_mut().release_root(root).unwrap();
        }
    }
}
