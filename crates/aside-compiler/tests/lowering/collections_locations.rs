//! L06 static compilation matrix. No values/instructions are evaluated here.
use super::*;
use aside_compiler::{DiagnosticCode, byte_range};

pub(super) fn pcs(f: &bc::RuntimeFunction, opcode: bc::Opcode) -> Vec<usize> {
    f.code
        .indexed_iter()
        .map(Result::unwrap)
        .filter_map(|(pc, i)| (i.opcode == opcode as u16).then_some(pc))
        .collect()
}
pub(super) fn text<'a>(p: &'a bc::RuntimePackage, f: &bc::RuntimeFunction, pc: usize) -> &'a str {
    let span = source_span(f, pc);
    &p.sources[span.file.index()].text[span.start as usize..span.end as usize]
}
pub(super) fn calls<'a>(p: &'a bc::RuntimePackage, f: &bc::RuntimeFunction) -> Vec<&'a str> {
    pcs(f, bc::Opcode::Call)
        .into_iter()
        .map(|pc| text(p, f, pc))
        .collect()
}
fn writes(f: &bc::RuntimeFunction, pc: usize, reg: u32) -> bool {
    if !f.code.is_boundary(pc) {
        return false;
    }
    let mut found = false;
    bc::instruction_spec(f.code.instruction(pc).unwrap().unwrap().opcode)
        .unwrap()
        .visit_registers(f, &f.code.instruction(pc).unwrap().unwrap(), |r, access| {
            found |= r.0 == reg && matches!(access, bc::Access::Write | bc::Access::ReadWrite);
        })
        .unwrap();
    found
}
pub(super) fn definition(f: &bc::RuntimeFunction, reg: u32, before: usize) -> usize {
    (0..before)
        .rfind(|&pc| writes(f, pc, reg))
        .expect("register definition")
}
pub(super) fn assert_live(f: &bc::RuntimeFunction, reg: u32, from: usize, to: usize) {
    for pc in from + 1..to {
        assert!(
            !writes(f, pc, reg),
            "r{reg} overwritten between PCs {from} and {to} at {pc}"
        );
    }
}
pub(super) fn assert_cleared(f: &bc::RuntimeFunction, reg: u32, after: usize) {
    let next = (after + 1..f.code.end_pc())
        .find(|&pc| writes(f, pc, reg))
        .unwrap();
    assert!(matches!(
        bc::instruction_spec(f.code.instruction(next).unwrap().unwrap().opcode)
            .unwrap()
            .opcode,
        bc::Opcode::Clear | bc::Opcode::Clear2 | bc::Opcode::Clear3
    ));
}
fn sequence(f: &bc::RuntimeFunction, pc: usize) -> bc::RegWindow {
    let bc::Descriptor::Sequence(window) =
        f.descriptors[f.code.instruction(pc).unwrap().unwrap().operands[1] as usize]
    else {
        panic!("sequence descriptor")
    };
    window
}
fn table(f: &bc::RuntimeFunction, pc: usize) -> &[[bc::Reg; 2]] {
    let bc::Descriptor::Table(pairs) =
        &f.descriptors[f.code.instruction(pc).unwrap().unwrap().operands[1] as usize]
    else {
        panic!("ordered table descriptor")
    };
    pairs
}

#[test]
fn empty_unary_nested_collections_and_group_have_distinct_constructors() {
    let c = compiled(
        "fn empty(){()} fn unary(){(1,)} fn group(){(1)} fn arrays(){[[],[1,2]]} fn objects(){ ({empty:{},nested:{pair:(1,[2])}}) }",
    );
    let p = c.program.package();
    for (name, len) in [("empty", 0), ("unary", 1)] {
        let f = function(p, name);
        let make = pcs(f, bc::Opcode::MakeTuple);
        assert_eq!(make.len(), 1);
        assert_eq!(sequence(f, make[0]).len, len);
        assert_eq!(text(p, f, make[0]), if len == 0 { "()" } else { "(1,)" });
    }
    assert!(pcs(function(p, "group"), bc::Opcode::MakeTuple).is_empty());
    let f = function(p, "arrays");
    assert_eq!(
        pcs(f, bc::Opcode::MakeArray)
            .iter()
            .map(|&pc| sequence(f, pc).len)
            .collect::<Vec<_>>(),
        [0, 2, 2]
    );
    let f = function(p, "objects");
    assert_eq!(
        pcs(f, bc::Opcode::MakeTable)
            .iter()
            .map(|&pc| table(f, pc).len())
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(pcs(f, bc::Opcode::MakeTuple).len(), 1);
}

#[test]
fn sequence_elements_are_saved_in_order_and_live_across_calls() {
    let c = compiled("fn one(){1} fn two(){2} fn take(x){x} fn f(){[one(),take(two()),one()]}");
    let p = c.program.package();
    let f = function(p, "f");
    assert_eq!(calls(p, f), ["one()", "two()", "take(two())", "one()"]);
    let make = pcs(f, bc::Opcode::MakeArray)[0];
    let window = sequence(f, make);
    assert_eq!(window.len, 3);
    let call_pcs = pcs(f, bc::Opcode::Call);
    for (offset, call) in [call_pcs[0], call_pcs[2], call_pcs[3]]
        .into_iter()
        .enumerate()
    {
        let slot = window.start.0 + offset as u32;
        let def = definition(f, slot, make);
        assert_eq!(
            f.code.instruction(def).unwrap().unwrap().opcode,
            bc::Opcode::Move as u16
        );
        assert_eq!(
            f.code.instruction(def).unwrap().unwrap().operands[1],
            f.code.instruction(call).unwrap().unwrap().operands[0]
        );
        assert!(call < def);
        assert_live(f, slot, def, make);
        assert_cleared(f, slot, make);
        for &later in call_pcs.iter().filter(|&&pc| pc > def) {
            let bc::Descriptor::Call(desc) =
                &f.descriptors[f.code.instruction(later).unwrap().unwrap().operands[1] as usize]
            else {
                panic!()
            };
            let bc::Arguments::Window(w) = desc.arguments else {
                panic!()
            };
            assert!(!(w.start.0..w.start.0 + w.len).contains(&slot));
        }
    }
}

#[test]
fn table_key_value_order_preserves_duplicate_entries_until_one_make() {
    let c = compiled(
        "fn key(x){x} fn value(x){x} fn f(){ ({named:value(0),[key(1)]:value(1),[key(1)]:value(2)}) }",
    );
    let p = c.program.package();
    let f = function(p, "f");
    assert_eq!(
        calls(p, f),
        ["value(0)", "key(1)", "value(1)", "key(1)", "value(2)"]
    );
    let makes = pcs(f, bc::Opcode::MakeTable);
    assert_eq!(makes.len(), 1);
    let make = makes[0];
    let pairs = table(f, make);
    assert_eq!(pairs.len(), 3);
    let mut definitions = Vec::new();
    for pair in pairs {
        for reg in pair {
            let def = definition(f, reg.0, make);
            assert_eq!(
                f.code.instruction(def).unwrap().unwrap().opcode,
                bc::Opcode::Move as u16
            );
            definitions.push(def);
            assert_live(f, reg.0, def, make);
            assert_cleared(f, reg.0, make);
        }
    }
    assert!(definitions.windows(2).all(|w| w[0] < w[1]));
    let load = definition(
        f,
        f.code
            .instruction(definitions[0])
            .unwrap()
            .unwrap()
            .operands[1],
        definitions[0],
    );
    assert_eq!(
        f.code.instruction(load).unwrap().unwrap().opcode,
        bc::Opcode::LoadConst as u16
    );
    assert_eq!(
        p.constants[f.code.instruction(load).unwrap().unwrap().operands[1] as usize],
        bc::Const::Utf8("named".into())
    );
    assert_ne!(pairs[1][0], pairs[2][0]);
    assert!(pcs(f, bc::Opcode::IndexSet).is_empty());
    // An unhashable key still compiles: all expressions precede MakeTable.
    let c = compiled("fn after(){1} fn f(){ ({[[]]:after(),last:after()}) }");
    let f = function(c.program.package(), "f");
    assert_eq!(calls(c.program.package(), f), ["after()", "after()"]);
    assert!(
        pcs(f, bc::Opcode::Call)
            .into_iter()
            .all(|pc| pc < pcs(f, bc::Opcode::MakeTable)[0])
    );
    let c = compiled("fn f(){({x:1,x:2})}");
    let p = c.program.package();
    let f = function(p, "f");
    let make = pcs(f, bc::Opcode::MakeTable)[0];
    let pairs = table(f, make);
    assert_eq!(pairs.len(), 2);
    for pair in pairs {
        let save = definition(f, pair[0].0, make);
        let load = if f.code.instruction(save).unwrap().unwrap().opcode == bc::Opcode::Move as u16 {
            definition(
                f,
                f.code.instruction(save).unwrap().unwrap().operands[1],
                save,
            )
        } else {
            save
        };
        assert_eq!(
            f.code.instruction(load).unwrap().unwrap().opcode,
            bc::Opcode::LoadConst as u16
        );
        assert_eq!(
            p.constants[f.code.instruction(load).unwrap().unwrap().operands[1] as usize],
            bc::Const::Utf8("x".into())
        );
    }
}

#[test]
fn sequential_collections_and_locations_reclaim_all_temporary_slots() {
    let mut source = String::from("fn id(x){x}fn f(a){");
    for _ in 0..300 {
        source.push_str(
            "[id(1),(id(2),{})];({[id(0)]:id(1)});a[id(0)]=a[id(1)]=[id(2)];a[id(0)]+=id(1);",
        );
    }
    source.push_str("0}");
    let c = compiled(&source);
    let f = function(c.program.package(), "f");
    assert!(f.register_count <= 12, "{}", f.register_count);
    assert_eq!(pcs(f, bc::Opcode::MakeArray).len(), 600);
    assert_eq!(pcs(f, bc::Opcode::IndexSet).len(), 900);
}

#[test]
fn index_reads_prepare_receiver_then_key_once_and_keep_nested_results() {
    let c = compiled("fn receiver(x){x} fn key(x){x} fn f(a){receiver(a)[key(0)][key(1)]}");
    let p = c.program.package();
    let f = function(p, "f");
    assert_eq!(calls(p, f), ["receiver(a)", "key(0)", "key(1)"]);
    let cp = pcs(f, bc::Opcode::Call);
    let get = pcs(f, bc::Opcode::IndexGet);
    assert_eq!(get.len(), 2);
    assert_eq!(
        f.code.instruction(get[0]).unwrap().unwrap().operands[1..3],
        [
            f.code.instruction(cp[0]).unwrap().unwrap().operands[0],
            f.code.instruction(cp[1]).unwrap().unwrap().operands[0]
        ]
    );
    assert_eq!(
        f.code.instruction(get[1]).unwrap().unwrap().operands[1],
        f.code.instruction(get[0]).unwrap().unwrap().operands[0]
    );
    assert_eq!(
        f.code.instruction(get[1]).unwrap().unwrap().operands[2],
        f.code.instruction(cp[2]).unwrap().unwrap().operands[0]
    );
    assert_live(
        f,
        f.code.instruction(cp[0]).unwrap().unwrap().operands[0],
        cp[0],
        get[0],
    );
    assert_live(
        f,
        f.code.instruction(get[0]).unwrap().unwrap().operands[0],
        get[0],
        get[1],
    );
    assert_cleared(
        f,
        f.code.instruction(get[1]).unwrap().unwrap().operands[1],
        get[1],
    );
    assert_cleared(
        f,
        f.code.instruction(get[1]).unwrap().unwrap().operands[2],
        get[1],
    );
}

#[test]
fn mixed_chain_prepares_left_to_right_and_writes_right_to_left() {
    let c = compiled(
        "fn obj(x){x} fn key(x){x} fn rhs(){9} fn f(a,b){let mut n=0;n=obj(a)[key(0)]=obj(b)[key(1)]=rhs();n}",
    );
    let p = c.program.package();
    let f = function(p, "f");
    assert_eq!(
        calls(p, f),
        ["obj(a)", "key(0)", "obj(b)", "key(1)", "rhs()"]
    );
    let cp = pcs(f, bc::Opcode::Call);
    let set = pcs(f, bc::Opcode::IndexSet);
    let bind = pcs(f, bc::Opcode::BindWrite);
    assert_eq!(set.len(), 2);
    assert_eq!(bind.len(), 1);
    let rhs = f.code.instruction(cp[4]).unwrap().unwrap().operands[0];
    assert_eq!(
        f.code.instruction(set[0]).unwrap().unwrap().operands[..3],
        [
            f.code.instruction(cp[2]).unwrap().unwrap().operands[0],
            f.code.instruction(cp[3]).unwrap().unwrap().operands[0],
            rhs
        ]
    );
    assert_eq!(
        f.code.instruction(set[1]).unwrap().unwrap().operands[..3],
        [
            f.code.instruction(cp[0]).unwrap().unwrap().operands[0],
            f.code.instruction(cp[1]).unwrap().unwrap().operands[0],
            rhs
        ]
    );
    assert!(cp[4] < set[0] && set[0] < set[1] && set[1] < bind[0]);
    assert_eq!(
        f.code.instruction(bind[0]).unwrap().unwrap().operands[1],
        rhs
    );
    for (call, write) in [
        (0, set[1]),
        (1, set[1]),
        (2, set[0]),
        (3, set[0]),
        (4, bind[0]),
    ] {
        let reg = f.code.instruction(cp[call]).unwrap().unwrap().operands[0];
        assert_live(f, reg, cp[call], write);
        assert_cleared(f, reg, write);
    }
    assert!(pcs(f, bc::Opcode::IndexGet).is_empty());
    println!(
        "L06 chain: calls={:?}; IndexSet sources={:?}; final BindWrite={:?}",
        calls(p, f),
        set.iter().map(|&pc| text(p, f, pc)).collect::<Vec<_>>(),
        text(p, f, bind[0])
    );
}

#[test]
fn chain_supports_globals_captured_names_and_nested_index_locations() {
    let c =
        compiled("let mut global=0;fn f(a){let mut local=0;let c=||{global=local=a[0][1]=4;};c}");
    let p = c.program.package();
    let f = function(p, "$closure");
    let get = pcs(f, bc::Opcode::IndexGet);
    let set = pcs(f, bc::Opcode::IndexSet);
    let cell = pcs(f, bc::Opcode::CellWrite);
    let global = pcs(f, bc::Opcode::GlobalWrite);
    assert_eq!(
        (get.len(), set.len(), cell.len(), global.len()),
        (1, 1, 1, 1)
    );
    assert_eq!(
        f.code.instruction(set[0]).unwrap().unwrap().operands[0],
        f.code.instruction(get[0]).unwrap().unwrap().operands[0]
    );
    assert!(get[0] < set[0] && set[0] < cell[0] && cell[0] < global[0]);
    assert_eq!(
        f.code.instruction(set[0]).unwrap().unwrap().operands[2],
        f.code.instruction(cell[0]).unwrap().unwrap().operands[1]
    );
    assert_eq!(
        f.code.instruction(set[0]).unwrap().unwrap().operands[2],
        f.code.instruction(global[0]).unwrap().unwrap().operands[0]
    );
}

#[test]
fn index_compound_reads_old_value_before_rhs_and_writes_original_position() {
    for (operator, opcode) in [
        ("+=", bc::Opcode::Add),
        ("-=", bc::Opcode::Sub),
        ("*=", bc::Opcode::Mul),
        ("/=", bc::Opcode::Div),
        ("%=", bc::Opcode::Rem),
        ("&=", bc::Opcode::BitAnd),
        ("|=", bc::Opcode::BitOr),
        ("^=", bc::Opcode::BitXor),
        ("<<=", bc::Opcode::Shl),
        (">>=", bc::Opcode::Shr),
    ] {
        let c = compiled(&format!(
            "fn obj(x){{x}} fn key(){{0}} fn rhs(){{2}} fn f(a){{obj(a)[key()]{operator}rhs();}}"
        ));
        let p = c.program.package();
        let f = function(p, "f");
        assert_eq!(calls(p, f), ["obj(a)", "key()", "rhs()"]);
        let cp = pcs(f, bc::Opcode::Call);
        let get = pcs(f, bc::Opcode::IndexGet)[0];
        let op = pcs(f, opcode)[0];
        let set = pcs(f, bc::Opcode::IndexSet)[0];
        assert!(cp[1] < get && get < cp[2] && cp[2] < op && op < set);
        assert_eq!(
            f.code.instruction(get).unwrap().unwrap().operands[1..3],
            f.code.instruction(set).unwrap().unwrap().operands[..2]
        );
        assert_eq!(
            f.code.instruction(op).unwrap().unwrap().operands[1],
            f.code.instruction(get).unwrap().unwrap().operands[0]
        );
        assert_eq!(
            f.code.instruction(op).unwrap().unwrap().operands[2],
            f.code.instruction(cp[2]).unwrap().unwrap().operands[0]
        );
        assert_eq!(
            f.code.instruction(set).unwrap().unwrap().operands[2],
            f.code.instruction(op).unwrap().unwrap().operands[0]
        );
        for &reg in &f.code.instruction(set).unwrap().unwrap().operands[..2] {
            let def = definition(f, reg, get);
            assert_live(f, reg, def, set);
            assert_cleared(f, reg, set);
        }
        assert_live(
            f,
            f.code.instruction(get).unwrap().unwrap().operands[0],
            get,
            op,
        );
    }
}

#[test]
fn immutable_binding_allows_index_write_while_tuple_write_is_semantic_error() {
    let c = compiled("fn f(){let a=[1];let t={x:0};(a)[0]=2;t[\"x\"]+=3;}");
    assert_eq!(
        pcs(function(c.program.package(), "f"), bc::Opcode::IndexSet).len(),
        2
    );
    for source in [
        "fn f(){let t=(1,);t[0]=2;}",
        "fn f(){let t=(1,);t[0]+=2;}",
        "fn f(){let a=[];a=[];}",
    ] {
        let a = analyze_module(source, &AnalysisOptions::default(), &mut NoModules).unwrap();
        assert!(!a.is_ok());
        assert!(
            a.diagnostics
                .iter()
                .any(|d| d.code == DiagnosticCode::ImmutableBinding)
        );
    }
}

#[test]
fn defaults_closures_and_initializer_collection_captures_roundtrip() {
    let c = compiled(
        "let root=[];if true{let a=[root];let c=||a[0];fn f(x={pair:(a,c)},y=[x]){y[0]}f();}",
    );
    let p = c.program.package();
    let defaults: Vec<_> = p
        .signatures
        .iter()
        .flat_map(|s| s.parameters.iter())
        .filter(|p| p.default.is_some())
        .map(function_default)
        .collect();
    assert_eq!(defaults.len(), 2);
    assert_eq!(
        pcs(&p.functions[defaults[0].index()], bc::Opcode::MakeTuple).len(),
        1
    );
    assert_eq!(
        pcs(&p.functions[defaults[0].index()], bc::Opcode::MakeTable).len(),
        1
    );
    assert_eq!(
        pcs(&p.functions[defaults[1].index()], bc::Opcode::MakeArray).len(),
        1
    );
    for id in defaults {
        assert_eq!(p.functions[id.index()].context, bc::Context::NoWait);
        assert!(!p.functions[id.index()].captures.is_empty());
    }
    assert_eq!(pcs(function(p, "$closure"), bc::Opcode::IndexGet).len(), 1);
}

#[test]
fn terminal_collection_element_or_key_omits_make_and_later_evaluation() {
    for expression in [
        "[before(),if flag{return 1;}else{throw 2;},after()]",
        "(before(),if flag{return 1;}else{throw 2;},after())",
        "{first:before(),[if flag{return 1;}else{throw 2;}]:after(),last:after()}",
        "{first:before(),[before()]:if flag{return 1;}else{throw 2;},last:after()}",
    ] {
        let c = compiled(&format!(
            "fn before(){{0}}fn after(){{1}}fn f(flag){{try{{let value={expression};}}finally{{before();}}}}"
        ));
        let p = c.program.package();
        let f = function(p, "f");
        assert!(calls(p, f).iter().all(|&s| s == "before()"));
        for op in [
            bc::Opcode::MakeArray,
            bc::Opcode::MakeTuple,
            bc::Opcode::MakeTable,
        ] {
            assert!(pcs(f, op).is_empty());
        }
        assert_eq!(f.regions.len(), 1);
        let id =
            bc::FunctionId(p.functions.iter().position(|d| std::ptr::eq(d, f)).unwrap() as u32);
        for op in [bc::Opcode::Return, bc::Opcode::Throw] {
            let pc = pcs(f, op)[0];
            let handler = if op == bc::Opcode::Return {
                c.program
                    .cleanup_handler(id, bc::Pc(pc as u32), bc::CleanupDestination::Return, None)
                    .unwrap()
            } else {
                c.program
                    .exception_handler(id, bc::Pc(pc as u32), None)
                    .unwrap()
            };
            assert_eq!(handler.unwrap().pc, f.regions[0].finally.unwrap().start);
        }
    }
}

#[test]
fn terminal_location_receiver_key_or_rhs_omits_all_pending_writes() {
    for statement in [
        "a[if flag{return 1;}else{throw 2;}]=after();",
        "a[before()]=a[if flag{return 1;}else{throw 2;}]=after();",
        "(if flag{return 1;}else{throw 2;})[after()]=after();",
        "a[before()]=if flag{return 1;}else{throw 2;};",
        "a[before()]+=if flag{return 1;}else{throw 2;};",
    ] {
        let c = compiled(&format!(
            "fn before(){{0}}fn after(){{1}}fn f(a,flag){{try{{{statement}}}finally{{before();}}}}"
        ));
        let p = c.program.package();
        let f = function(p, "f");
        assert!(pcs(f, bc::Opcode::IndexSet).is_empty());
        assert!(pcs(f, bc::Opcode::BindWrite).is_empty());
        assert!(calls(p, f).iter().all(|&s| s == "before()"));
        assert_eq!(
            pcs(f, bc::Opcode::IndexGet).len(),
            usize::from(statement.contains("+="))
        );
        assert_eq!(f.regions.len(), 1);
    }
}

#[test]
fn conditional_fallthrough_keeps_prepared_values_and_shared_cfg_dominance() {
    let c = compiled(
        "fn before(){0}fn after(){2}fn f(a,flag){try{a[before()]=[before(),if flag{return 1;}else{after()}];}finally{before();}}",
    );
    let p = c.program.package();
    let f = function(p, "f");
    let id = bc::FunctionId(p.functions.iter().position(|d| std::ptr::eq(d, f)).unwrap() as u32);
    let dom = super::initializer_locals::dominators(&c, id);
    let make = pcs(f, bc::Opcode::MakeArray)[0];
    let set = pcs(f, bc::Opcode::IndexSet)[0];
    assert!(dom[set][make]);
    let window = sequence(f, make);
    let first = definition(f, window.start.0, make);
    assert!(dom[make][first]);
    assert_live(f, window.start.0, first, make);
    for &reg in &f.code.instruction(set).unwrap().unwrap().operands[..2] {
        let def = definition(f, reg, set);
        assert!(dom[set][def]);
        assert_live(f, reg, def, set);
    }
    let returns = pcs(f, bc::Opcode::Return);
    let terminal = returns
        .iter()
        .copied()
        .find(|&pc| text(p, f, pc) == "return 1;")
        .unwrap();
    assert_eq!(
        c.program
            .cleanup_handler(
                id,
                bc::Pc(terminal as u32),
                bc::CleanupDestination::Return,
                None
            )
            .unwrap()
            .unwrap()
            .pc,
        f.regions[0].finally.unwrap().start
    );
}

#[test]
fn deferred_collection_and_pattern_forms_are_located() {
    for (source, span, detail) in [
        ("fn f(){let [a]=[1];}", "[a]", "pattern"),
        (
            "fn f(){let mut a=0;let mut b=0;(a,b)=(1,2);}",
            "(a,b)",
            "pattern",
        ),
    ] {
        let m = analyzed(source);
        let e = lowering::lower(&m, &Linkage::default(), Options::default()).unwrap_err();
        assert_eq!(e.kind, lowering::ErrorKind::Unsupported);
        assert_eq!(&source[byte_range(e.span)], span);
        assert!(e.detail.to_lowercase().contains(detail), "{}", e.detail);
    }
}

#[test]
fn collection_spreads_roundtrip_in_calls_and_optional_chains() {
    for source in [
        "fn f() { [1,...[]] }",
        "fn f(a){[...a]}",
        "fn f(a){({...a})}",
        "fn f(a){a.x([...a])}",
        "fn f(a){a?([...a])}",
    ] {
        compiled(source);
    }
}

#[test]
fn collection_and_location_quotas_refuse_at_the_failing_source() {
    for source in ["fn f(){[1,2,3,4,5,6,7,8]}", "fn f(){({a:1,b:2,c:3,d:4})}"] {
        let m = analyzed(source);
        let e = lowering::lower(
            &m,
            &Linkage::default(),
            Options {
                limits: lowering::Limits {
                    max_registers: 7,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(e.kind, lowering::ErrorKind::Limit);
        assert!(e.detail.contains("window"));
        let text = &source[byte_range(e.span)];
        assert!(text.starts_with('[') || text.starts_with('{'));
        assert!(!text.starts_with("fn"));
    }
    let source = "fn f(a){a[0]=a[1]=a[2]=1;}";
    let m = analyzed(source);
    let e = lowering::lower(
        &m,
        &Linkage::default(),
        Options {
            limits: lowering::Limits {
                max_registers: 6,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.kind, lowering::ErrorKind::Limit);
    assert!(byte_range(e.span).start >= source.find("a[2]").unwrap());
    let source = format!("fn f(){{{}1{}}}", "[".repeat(20), "]".repeat(20));
    let m = analyzed(&source);
    let e = lowering::lower(
        &m,
        &Linkage::default(),
        Options {
            limits: lowering::Limits {
                max_depth: 12,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(e.kind, lowering::ErrorKind::Limit);
    assert!(e.detail.contains("depth"));
    assert!(byte_range(e.span).len() < source.len());
    let m = analyzed("fn f(){({a:1,b:2,c:3})} fn g(a){a[0]=a[1]=[a[2]];}");
    let ir = lowering::lower(&m, &Linkage::default(), Options::default()).unwrap();
    let stats = ir.stats();
    for limits in [
        lowering::Limits {
            max_work: stats.work_units - 1,
            ..Default::default()
        },
        lowering::Limits {
            max_capacity_bytes: stats.capacity_peak_bytes - 1,
            ..Default::default()
        },
    ] {
        let e = lowering::lower(
            &m,
            &Linkage::default(),
            Options {
                limits,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(e.kind, lowering::ErrorKind::Limit);
        assert!(e.span.len() > 0.into());
    }
    lowering::lower(
        &m,
        &Linkage::default(),
        Options {
            limits: lowering::Limits {
                max_work: stats.work_units,
                max_capacity_bytes: stats.capacity_peak_bytes,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
}
