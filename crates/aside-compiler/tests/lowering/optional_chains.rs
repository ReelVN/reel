//! Source/IR/B01 graph contracts only: no instruction or Value evaluator.
use super::collections_locations::{assert_cleared, assert_live, calls, definition, pcs, text};
use super::initializer_locals::dominators;
use super::*;
use aside_compiler::{DiagnosticCode, byte_range, semantic::*};

fn id(p: &bc::RuntimePackage, f: &bc::RuntimeFunction) -> bc::FunctionId {
    bc::FunctionId(p.functions.iter().position(|v| std::ptr::eq(v, f)).unwrap() as u32)
}
// Follow every ordinary CFG edge, never select a branch using an author value.
fn reachable(f: &bc::RuntimeFunction, start: usize) -> Vec<bool> {
    let mut seen = vec![false; f.code.end_pc()];
    let mut todo = vec![start];
    while let Some(pc) = todo.pop() {
        if std::mem::replace(&mut seen[pc], true) {
            continue;
        }
        let i = f.code.instruction(pc).unwrap().unwrap();
        match bc::instruction_spec(i.opcode).unwrap().flow {
            bc::Flow::Next => todo.push(f.code.next_pc(pc).unwrap().unwrap()),
            bc::Flow::Jump | bc::Flow::Leave => todo.push(i.operands[0] as usize),
            bc::Flow::Branch => todo.extend([i.operands[1] as usize, i.operands[2] as usize]),
            _ => (),
        }
    }
    seen
}
fn under_guard(
    c: &lowering::Compilation,
    f: &bc::RuntimeFunction,
    guard: usize,
    operations: &[usize],
) {
    let null = reachable(
        f,
        f.code.instruction(guard).unwrap().unwrap().operands[1] as usize,
    );
    let value = reachable(
        f,
        f.code.instruction(guard).unwrap().unwrap().operands[2] as usize,
    );
    let dom = dominators(c, id(c.program.package(), f));
    for &pc in operations {
        assert!(!null[pc], "null edge reaches {pc}");
        assert!(value[pc] && dom[pc][guard], "operation outside guard: {pc}");
    }
}
fn null_result(c: &lowering::Compilation, f: &bc::RuntimeFunction, guards: &[usize]) {
    let ret = *pcs(f, bc::Opcode::Return).last().unwrap();
    let out = f.code.instruction(ret).unwrap().unwrap().operands[0];
    let p = c.program.package();
    let dom = dominators(c, id(p, f));
    let init = definition(f, out, guards[0]);
    assert_eq!(
        f.code.instruction(init).unwrap().unwrap().opcode,
        bc::Opcode::LoadConst as u16
    );
    assert_eq!(
        p.constants[f.code.instruction(init).unwrap().unwrap().operands[1] as usize],
        bc::Const::Null
    );
    for &guard in guards {
        assert_ne!(f.code.instruction(guard).unwrap().unwrap().operands[0], out);
        assert_eq!(
            f.code.instruction(guard).unwrap().unwrap().operands[1],
            f.code.instruction(guards[0]).unwrap().unwrap().operands[1]
        );
        let seen = reachable(
            f,
            f.code.instruction(guard).unwrap().unwrap().operands[1] as usize,
        );
        assert!(seen[ret] && dom[guard][init]);
        for (pc, i) in f
            .code
            .indexed_iter()
            .map(Result::unwrap)
            .filter(|(pc, _)| seen[*pc])
        {
            bc::instruction_spec(i.opcode)
                .unwrap()
                .visit_registers(f, &i, |r, access| {
                    assert!(
                        !(r.0 == out
                            && matches!(access, bc::Access::Write | bc::Access::ReadWrite)),
                        "null exit overwrites result at {pc}"
                    );
                })
                .unwrap();
        }
    }
}

#[test]
fn three_optional_forms_guard_gets_keys_arguments_and_return_null() {
    for (expr, get, call_count) in [
        ("a?.中文", bc::Opcode::MemberGet, 0),
        ("a?[key()]", bc::Opcode::IndexGet, 1),
        ("a?(arg(),中文:named())", bc::Opcode::Call, 3),
        ("a.method?(arg())", bc::Opcode::Call, 2),
        ("null?(arg())", bc::Opcode::Call, 2),
        ("false?.中文", bc::Opcode::MemberGet, 0),
        ("0?[key()]", bc::Opcode::IndexGet, 1),
    ] {
        let c = compiled(&format!(
            "fn key(){{0}}fn arg(){{1}}fn named(){{2}}fn f(a){{{expr}}}"
        ));
        let p = c.program.package();
        let f = function(p, "f");
        let guards = pcs(f, bc::Opcode::BranchNull);
        assert_eq!(guards.len(), 1);
        assert_eq!(pcs(f, bc::Opcode::Call).len(), call_count);
        assert!(!pcs(f, get).is_empty());
        assert!(pcs(f, bc::Opcode::BranchBool).is_empty());
        assert!(pcs(f, bc::Opcode::Eq).is_empty());
        let operations: Vec<_> = pcs(f, get)
            .into_iter()
            .chain(pcs(f, bc::Opcode::Call))
            .collect();
        under_guard(&c, f, guards[0], &operations);
        null_result(&c, f, &guards);
        if get == bc::Opcode::MemberGet {
            assert_eq!(
                p.symbols[f
                    .code
                    .instruction(pcs(f, get)[0])
                    .unwrap()
                    .unwrap()
                    .operands[2] as usize],
                "中文"
            );
        }
    }
}

#[test]
fn continuous_mixed_chain_has_only_explicit_optional_guards() {
    let c = compiled(
        "fn receiver(x){x}fn key(){0}fn arg(){1}fn f(a){receiver(a).base?.中文[key()](arg()).尾}",
    );
    let p = c.program.package();
    let f = function(p, "f");
    let guards = pcs(f, bc::Opcode::BranchNull);
    assert_eq!(guards.len(), 1);
    assert_eq!(
        calls(p, f),
        [
            "receiver(a)",
            "key()",
            "arg()",
            "receiver(a).base?.中文[key()](arg())"
        ]
    );
    let members = pcs(f, bc::Opcode::MemberGet);
    assert_eq!(members.len(), 3);
    let cp = pcs(f, bc::Opcode::Call);
    let index = pcs(f, bc::Opcode::IndexGet)[0];
    assert!(cp[0] < members[0] && members[0] < guards[0] && guards[0] < members[1]);
    assert!(
        members[1] < cp[1] && cp[1] < index && index < cp[2] && cp[2] < cp[3] && cp[3] < members[2]
    );
    under_guard(
        &c,
        f,
        guards[0],
        &[members[1], cp[1], index, cp[2], cp[3], members[2]],
    );
    null_result(&c, f, &guards);
}

#[test]
fn later_guards_share_a_separate_null_result_after_successful_segments() {
    let c = compiled("fn key(){0}fn arg(){1}fn f(a){a?.b?[key()]?(arg()).c}");
    let f = function(c.program.package(), "f");
    let guards = pcs(f, bc::Opcode::BranchNull);
    assert_eq!(guards.len(), 3);
    null_result(&c, f, &guards);
    let get = pcs(f, bc::Opcode::MemberGet);
    let cp = pcs(f, bc::Opcode::Call);
    let index = pcs(f, bc::Opcode::IndexGet)[0];
    assert!(get[0] < guards[1] && index < guards[2]);
    under_guard(&c, f, guards[1], &[cp[0], index, cp[1], cp[2], get[1]]);
    under_guard(&c, f, guards[2], &[cp[1], cp[2], get[1]]);
    assert_eq!(
        f.code.instruction(guards[0]).unwrap().unwrap().operands[0],
        f.code.instruction(guards[2]).unwrap().unwrap().operands[0]
    );
}

#[test]
fn nested_argument_and_index_chains_have_independent_joins() {
    let c = compiled("fn inner(){0}fn arg(){1}fn f(a,b,c){a?[b?[inner()]]?(value:c?.m(arg())).z}");
    let p = c.program.package();
    let f = function(p, "f");
    let g = pcs(f, bc::Opcode::BranchNull);
    assert_eq!(g.len(), 4);
    assert_eq!(
        f.code.instruction(g[0]).unwrap().unwrap().operands[1],
        f.code.instruction(g[2]).unwrap().unwrap().operands[1]
    );
    assert_ne!(
        f.code.instruction(g[0]).unwrap().unwrap().operands[1],
        f.code.instruction(g[1]).unwrap().unwrap().operands[1]
    );
    assert_ne!(
        f.code.instruction(g[2]).unwrap().unwrap().operands[1],
        f.code.instruction(g[3]).unwrap().unwrap().operands[1]
    );
    let cp = pcs(f, bc::Opcode::Call);
    under_guard(&c, f, g[0], &[g[1], cp[0], g[2], g[3], cp[1], cp[2], cp[3]]);
    under_guard(&c, f, g[1], &[cp[0]]);
    under_guard(&c, f, g[2], &[g[3], cp[1], cp[2], cp[3]]);
    under_guard(&c, f, g[3], &[cp[1], cp[2]]);
}

#[test]
fn group_ends_propagation_and_an_outer_question_starts_a_new_chain() {
    for expr in ["(a?.b).c", "((a?.b))[key()]", "(((a?.b)))(arg())"] {
        let c = compiled(&format!("fn key(){{0}}fn arg(){{1}}fn f(a){{{expr}}}"));
        let p = c.program.package();
        let f = function(p, "f");
        let g = pcs(f, bc::Opcode::BranchNull);
        assert_eq!(g.len(), 1);
        let null = reachable(
            f,
            f.code.instruction(g[0]).unwrap().unwrap().operands[1] as usize,
        );
        let outer = match expr.chars().last().unwrap() {
            'c' => pcs(f, bc::Opcode::MemberGet)[1],
            ']' => pcs(f, bc::Opcode::IndexGet)[0],
            _ => *pcs(f, bc::Opcode::Call).last().unwrap(),
        };
        assert!(null[outer]);
        for pc in pcs(f, bc::Opcode::Call) {
            assert!(null[pc]);
        }
    }
    for expr in ["(a?.b)?.c", "((a?.b))?[key()]", "(((a?.b)))?(arg()).c"] {
        let c = compiled(&format!("fn key(){{0}}fn arg(){{1}}fn f(a){{{expr}}}"));
        let f = function(c.program.package(), "f");
        let g = pcs(f, bc::Opcode::BranchNull);
        assert_eq!(g.len(), 2);
        assert_ne!(
            f.code.instruction(g[0]).unwrap().unwrap().operands[1],
            f.code.instruction(g[1]).unwrap().unwrap().operands[1]
        );
        assert!(
            reachable(
                f,
                f.code.instruction(g[0]).unwrap().unwrap().operands[1] as usize
            )[g[1]]
        );
        let ops: Vec<_> = pcs(f, bc::Opcode::Call)
            .into_iter()
            .chain(pcs(f, bc::Opcode::IndexGet))
            .chain(pcs(f, bc::Opcode::MemberGet).into_iter().skip(1))
            .collect();
        under_guard(&c, f, g[1], &ops);
    }
}

#[test]
fn null_coalescing_begins_after_chain_join_and_guards_its_own_rhs() {
    let c = compiled("fn fallback(x){x}fn f(a,b){a?.x ?? fallback(b?[0])}");
    let p = c.program.package();
    let f = function(p, "f");
    let g = pcs(f, bc::Opcode::BranchNull);
    assert_eq!(g.len(), 3);
    let chain_join = f.code.instruction(g[0]).unwrap().unwrap().operands[1] as usize;
    assert!(reachable(f, chain_join)[g[1]]);
    let rhs = pcs(f, bc::Opcode::Call)[0];
    let index = pcs(f, bc::Opcode::IndexGet)[0];
    assert!(
        reachable(
            f,
            f.code.instruction(g[1]).unwrap().unwrap().operands[1] as usize
        )[rhs]
    );
    assert!(
        !reachable(
            f,
            f.code.instruction(g[1]).unwrap().unwrap().operands[2] as usize
        )[rhs]
    );
    assert!(
        reachable(
            f,
            f.code.instruction(g[2]).unwrap().unwrap().operands[1] as usize
        )[rhs]
    );
    assert!(
        !reachable(
            f,
            f.code.instruction(g[2]).unwrap().unwrap().operands[1] as usize
        )[index]
    );
    assert_ne!(
        f.code.instruction(g[0]).unwrap().unwrap().operands[1],
        f.code.instruction(g[2]).unwrap().unwrap().operands[1]
    );
    let def = definition(
        f,
        f.code.instruction(g[1]).unwrap().unwrap().operands[0],
        g[1],
    );
    assert_eq!(
        f.code.instruction(def).unwrap().unwrap().opcode,
        bc::Opcode::Move as u16
    );
    let dom = dominators(&c, id(p, f));
    assert!(!dom[g[1]][def]); // the earlier null edge skips the successful value copy
    let init = definition(
        f,
        f.code.instruction(g[1]).unwrap().unwrap().operands[0],
        g[0],
    );
    assert_eq!(
        f.code.instruction(init).unwrap().unwrap().opcode,
        bc::Opcode::LoadConst as u16
    );
    assert_eq!(
        p.constants[f.code.instruction(init).unwrap().unwrap().operands[1] as usize],
        bc::Const::Null
    );
    assert!(dom[g[1]][init]);
}

#[test]
fn optional_call_preserves_callee_named_offsets_and_live_windows_across_rebinding() {
    let c = compiled(
        "fn key(){0}fn f(mut a,b){let change=||{a=b;1};a?.method(change(),中文:b?[key()]).tail}",
    );
    let p = c.program.package();
    let f = function(p, "f");
    let cp = pcs(f, bc::Opcode::Call);
    assert_eq!(
        calls(p, f),
        ["change()", "key()", "a?.method(change(),中文:b?[key()])"]
    );
    let bc::Descriptor::Call(call) =
        &f.descriptors[f.code.instruction(cp[2]).unwrap().unwrap().operands[1] as usize]
    else {
        panic!()
    };
    let bc::CallTarget::Register(callee) = call.target else {
        panic!()
    };
    let bc::Arguments::Window(w) = call.arguments else {
        panic!()
    };
    assert_eq!(w.len, 2);
    assert_eq!(call.names.len(), 1);
    assert_eq!(call.names[0].offset, 1);
    assert_eq!(p.symbols[call.names[0].name.index()], "中文");
    let get = pcs(f, bc::Opcode::MemberGet)[0];
    let saved = definition(f, callee.0, cp[2]);
    assert_eq!(
        f.code.instruction(saved).unwrap().unwrap().opcode,
        bc::Opcode::Move as u16
    );
    assert_eq!(
        f.code.instruction(saved).unwrap().unwrap().operands[1],
        f.code.instruction(get).unwrap().unwrap().operands[0]
    );
    assert!(saved < cp[0]);
    assert_live(f, callee.0, saved, cp[2]);
    for &pc in &cp {
        let bc::Descriptor::Call(v) =
            &f.descriptors[f.code.instruction(pc).unwrap().unwrap().operands[1] as usize]
        else {
            panic!()
        };
        let bc::Arguments::Window(v) = v.arguments else {
            panic!()
        };
        assert!(!(v.start.0..v.start.0 + v.len).contains(&callee.0));
    }
}

#[test]
fn grouped_chain_receivers_remain_saved_locations_across_a_rebinding_rhs() {
    let c = compiled(
        "fn key(){0}fn f(mut a,b){let mut name=0;let rhs=||{a=b;9};name=(a?.child).中文=(b?[key()])[key()]=rhs();name}",
    );
    let p = c.program.package();
    let f = function(p, "f");
    let cp = pcs(f, bc::Opcode::Call);
    assert_eq!(calls(p, f), ["key()", "key()", "rhs()"]);
    let ms = pcs(f, bc::Opcode::MemberSet)[0];
    let ix = pcs(f, bc::Opcode::IndexSet)[0];
    assert!(cp[2] < ix && ix < ms);
    assert_eq!(
        f.code.instruction(ix).unwrap().unwrap().operands[2],
        f.code.instruction(ms).unwrap().unwrap().operands[1]
    );
    assert_eq!(pcs(f, bc::Opcode::MemberGet).len(), 1); // no premature read of final 中文
    for reg in [
        f.code.instruction(ms).unwrap().unwrap().operands[0],
        f.code.instruction(ix).unwrap().unwrap().operands[0],
        f.code.instruction(ix).unwrap().unwrap().operands[1],
    ] {
        let prepare = definition(f, reg, cp[2]);
        assert_live(
            f,
            reg,
            prepare,
            if reg == f.code.instruction(ms).unwrap().unwrap().operands[0] {
                ms
            } else {
                ix
            },
        );
    }
    compiled("fn rhs(){1}fn f(a){(a?.child).中文+=rhs();}");
}

#[test]
fn terminal_base_emits_no_guard_and_terminal_nonnull_segments_keep_null_fallthrough() {
    let c = compiled("fn f(flag){(if flag{return 1;}else{throw 2;})?.x}");
    let f = function(c.program.package(), "f");
    assert!(pcs(f, bc::Opcode::BranchNull).is_empty());
    assert!(pcs(f, bc::Opcode::MemberGet).is_empty());
    for expr in [
        "a?[if flag{return 1;}else{throw 2;}].x",
        "a?.method(if flag{return 1;}else{throw 2;},after()).x",
        "a?(first:0,last:if flag{return 1;}else{throw 2;}).x",
    ] {
        let c = compiled(&format!("fn after(){{0}}fn f(a,flag){{{expr}}}"));
        let p = c.program.package();
        let f = function(p, "f");
        let g = pcs(f, bc::Opcode::BranchNull);
        assert_eq!(g.len(), 1);
        assert!(pcs(f, bc::Opcode::IndexGet).is_empty());
        assert!(pcs(f, bc::Opcode::Call).is_empty());
        assert_eq!(
            pcs(f, bc::Opcode::MemberGet).len(),
            usize::from(expr.contains("method"))
        );
        null_result(&c, f, &g);
        assert_eq!(pcs(f, bc::Opcode::Return).len(), 2);
    }
}

#[test]
fn partial_return_throw_and_loop_exits_keep_existing_cleanup_routes() {
    for expr in [
        "a?[if flag{return 1;}else{key()}].x",
        "a?.method(if flag{throw 1;}else{key()}).x",
    ] {
        let c = compiled(&format!(
            "fn key(){{0}}fn cleanup(){{0}}fn f(a,flag){{try{{{expr};}}finally{{cleanup();}}}}"
        ));
        let p = c.program.package();
        let f = function(p, "f");
        let fid = id(p, f);
        let g = pcs(f, bc::Opcode::BranchNull)[0];
        assert_eq!(f.regions.len(), 1);
        let pending: Vec<_> = pcs(f, bc::Opcode::IndexGet)
            .into_iter()
            .chain(pcs(f, bc::Opcode::MemberGet))
            .chain(
                pcs(f, bc::Opcode::Call)
                    .into_iter()
                    .filter(|&pc| text(p, f, pc) != "cleanup()"),
            )
            .collect();
        under_guard(&c, f, g, &pending);
        for pc in pcs(f, bc::Opcode::Return)
            .into_iter()
            .filter(|&pc| text(p, f, pc) == "return 1;")
        {
            assert_eq!(
                c.program
                    .cleanup_handler(fid, bc::Pc(pc as u32), bc::CleanupDestination::Return, None)
                    .unwrap()
                    .unwrap()
                    .pc,
                f.regions[0].finally.unwrap().start
            );
        }
        for pc in pcs(f, bc::Opcode::Throw) {
            assert_eq!(
                c.program
                    .exception_handler(fid, bc::Pc(pc as u32), None)
                    .unwrap()
                    .unwrap()
                    .pc,
                f.regions[0].finally.unwrap().start
            );
        }
    }
    let c = compiled(
        "fn cleanup(){0}fn f(a,flag){while flag{try{a?[if flag{break;}else{continue;}].x;}finally{cleanup();}}7}",
    );
    let p = c.program.package();
    let f = function(p, "f");
    let fid = id(p, f);
    assert!(pcs(f, bc::Opcode::IndexGet).is_empty());
    assert!(pcs(f, bc::Opcode::MemberGet).is_empty());
    for pc in pcs(f, bc::Opcode::Leave)
        .into_iter()
        .filter(|&pc| matches!(text(p, f, pc), "break;" | "continue;"))
    {
        assert_eq!(
            c.program
                .cleanup_handler(
                    fid,
                    bc::Pc(pc as u32),
                    bc::CleanupDestination::Leave(bc::Pc(
                        f.code.instruction(pc).unwrap().unwrap().operands[0]
                    )),
                    None
                )
                .unwrap()
                .unwrap()
                .pc,
            f.regions[0].finally.unwrap().start
        );
    }
    assert_eq!(
        pcs(f, bc::Opcode::Leave)
            .iter()
            .filter(|&&pc| matches!(text(p, f, pc), "break;" | "continue;"))
            .count(),
        2
    );
    compiled("fn f(a,flag){outer:while a?[if flag{break outer;}else{continue outer;}]{}7}");
}

#[test]
fn getters_and_calls_keep_throwing_semantics_without_synthetic_catches() {
    let c = compiled(
        "fn key(){0}fn cleanup(){0}fn f(a){try{a?.m()[key()];}catch e{throw;}finally{cleanup();}}",
    );
    let p = c.program.package();
    let f = function(p, "f");
    let fid = id(p, f);
    assert_eq!(f.regions.len(), 1);
    for pc in pcs(f, bc::Opcode::MemberGet)
        .into_iter()
        .chain(pcs(f, bc::Opcode::IndexGet))
        .chain(
            pcs(f, bc::Opcode::Call)
                .into_iter()
                .filter(|&pc| text(p, f, pc) != "cleanup()"),
        )
    {
        assert!(
            bc::instruction_spec(f.code.instruction(pc).unwrap().unwrap().opcode)
                .unwrap()
                .may_throw
        );
        assert_eq!(
            c.program
                .exception_handler(fid, bc::Pc(pc as u32), None)
                .unwrap()
                .unwrap()
                .pc,
            f.regions[0].catch.as_ref().unwrap().range.start
        );
    }
    assert_eq!(
        bc::Opcode::MemberGet.spec().semantic_rule,
        bc::SemanticRule::MemberLookup
    );
    let plain = compiled("fn f(a){a?.missing[0](1)}");
    assert!(function(plain.program.package(), "f").regions.is_empty());
}

#[test]
fn defaults_initializer_and_forwarded_captures_use_the_existing_binding_model() {
    let source = "let root={x:null};{let mut local=root;let c=||local?.x;let relay=||{let nested=||local?[0];nested};c();relay();}fn f(a,x=a?.x){x}";
    let m = analyzed(source);
    let c = compiled(source);
    super::initializer_locals::assert_local_structure(&m, &c);
    let p = c.program.package();
    let f = function(p, "f");
    let default = function_default(&p.signatures[f.signature.index()].parameters[1]);
    let d = &p.functions[default.index()];
    assert_eq!(d.context, bc::Context::NoWait);
    assert_eq!(pcs(d, bc::Opcode::BranchNull).len(), 1);
    assert!(
        p.functions
            .iter()
            .any(|f| !f.captures.is_empty() && !pcs(f, bc::Opcode::BranchNull).is_empty())
    );
    compiled("let a=null;{let x=a?[0];let c=||x;c();}");
}

#[test]
fn optional_assignment_and_deferred_spread_and_class_are_located_rejections() {
    for (source, expected) in [
        ("fn f(a){a?.b.c=1;}", "a?.b.c"),
        ("fn f(a){a?[0][1]+=1;}", "a?[0][1]"),
        ("fn f(a){((a?.b.c))=1;}", "a?.b.c"),
        ("fn f(a){let mut x=0;x=a?.b.c=1;}", "a?.b.c"),
    ] {
        let e = lowering::lower(&analyzed(source), &Linkage::default(), Options::default())
            .unwrap_err();
        assert_eq!(e.kind, lowering::ErrorKind::Unsupported);
        assert!(e.detail.contains("assignment target"));
        assert_eq!(&source[byte_range(e.span)], expected);
        assert_eq!(e.stats.capacity_bytes, 0);
    }
    for source in [
        "fn f(a){a?.b=1;}",
        "fn f(a){a?[0]=1;}",
        "fn f(a){let x=(a?.b.c=1);}",
    ] {
        let a = analyze_module(source, &AnalysisOptions::default(), &mut NoModules).unwrap();
        assert!(!a.is_ok());
        assert!(
            a.diagnostics
                .iter()
                .any(|d| d.code == DiagnosticCode::InvalidAssignment)
        );
    }
    {
        let (source, detail) = ("fn f(){let [a]=[1];}", "pattern");
        let e = lowering::lower(&analyzed(source), &Linkage::default(), Options::default())
            .unwrap_err();
        assert_eq!(e.kind, lowering::ErrorKind::Unsupported);
        assert!(
            e.detail.to_lowercase().contains(&detail.to_lowercase()),
            "{}",
            e.detail
        );
        assert_eq!(e.stats.capacity_bytes, 0);
    }
}

#[test]
fn flattened_chains_charge_only_nested_operands_and_preserve_exact_budgets() {
    for source in [
        format!("fn f(a){{a?.x{}}}", ".x".repeat(20)),
        format!("fn f(a){{a?.x{}?[a?.y{}]}}", ".x".repeat(8), ".z".repeat(8)),
    ] {
        let m = analyzed(&source);
        let minimum = (1..64)
            .find(|&depth| {
                lowering::lower(
                    &m,
                    &Linkage::default(),
                    Options {
                        limits: lowering::Limits {
                            max_depth: depth,
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                )
                .is_ok()
            })
            .unwrap();
        assert!(minimum <= 6, "flat chain consumed depth {minimum}");
        let e = lowering::lower(
            &m,
            &Linkage::default(),
            Options {
                limits: lowering::Limits {
                    max_depth: minimum - 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(e.kind, lowering::ErrorKind::Limit);
        assert!(e.detail.contains("depth"));
        assert!(byte_range(e.span).len() < source.len());
    }
    let source = "fn key(){0}fn f(a,b){a?.中文[key()]?(value:b?[key()]).尾}";
    let m = analyzed(source);
    let s = lowering::lower(&m, &Linkage::default(), Options::default())
        .unwrap()
        .stats();
    let exact = lowering::Limits {
        max_work: s.work_units,
        max_capacity_bytes: s.capacity_peak_bytes,
        max_records: s.records,
        max_blocks: s.blocks,
        max_instructions: s.instructions,
        max_registers: s.max_function_registers,
        ..Default::default()
    };
    lowering::lower(
        &m,
        &Linkage::default(),
        Options {
            limits: exact,
            ..Default::default()
        },
    )
    .unwrap();
    for limits in [
        lowering::Limits {
            max_work: s.work_units - 1,
            ..exact
        },
        lowering::Limits {
            max_capacity_bytes: s.capacity_peak_bytes - 1,
            ..exact
        },
        lowering::Limits {
            max_records: s.records - 1,
            ..exact
        },
        lowering::Limits {
            max_blocks: s.blocks - 1,
            ..exact
        },
        lowering::Limits {
            max_instructions: s.instructions - 1,
            ..exact
        },
        lowering::Limits {
            max_registers: s.max_function_registers - 1,
            ..exact
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
        assert!(!byte_range(e.span).is_empty());
        assert!(source.is_char_boundary(byte_range(e.span).start));
    }
    let s = lowering::compile(&m, &Linkage::default(), Options::default())
        .unwrap()
        .stats;
    let exact = lowering::Limits {
        max_work: s.work_units,
        max_capacity_bytes: s.capacity_peak_bytes,
        max_records: s.records,
        max_blocks: s.blocks,
        max_instructions: s.instructions,
        max_registers: s.max_function_registers,
        ..Default::default()
    };
    lowering::compile(
        &m,
        &Linkage::default(),
        Options {
            limits: exact,
            ..Default::default()
        },
    )
    .unwrap();
    for limits in [
        lowering::Limits {
            max_work: s.work_units - 1,
            ..exact
        },
        lowering::Limits {
            max_capacity_bytes: s.capacity_peak_bytes - 1,
            ..exact
        },
        lowering::Limits {
            max_records: s.records - 1,
            ..exact
        },
    ] {
        let error = lowering::compile(
            &m,
            &Linkage::default(),
            Options {
                limits,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.kind, lowering::ErrorKind::Limit);
    }
}

#[test]
fn sequential_chains_clear_temporaries_and_reuse_registers() {
    let statement = "a?.x[0]?(value:b?[0]).y;(a?.x).y=1;";
    let once = compiled(&format!("fn f(a,b){{{statement}}}"));
    let repeated = compiled(&format!("fn f(a,b){{{}}}", statement.repeat(300)));
    let f = function(repeated.program.package(), "f");
    assert_eq!(
        f.register_count,
        function(once.program.package(), "f").register_count
    );
    assert_eq!(pcs(f, bc::Opcode::BranchNull).len(), 1200);
    for pc in pcs(f, bc::Opcode::MemberSet) {
        assert_cleared(f, f.code.instruction(pc).unwrap().unwrap().operands[0], pc);
    }
    for pc in pcs(f, bc::Opcode::Call) {
        let bc::Descriptor::Call(v) =
            &f.descriptors[f.code.instruction(pc).unwrap().unwrap().operands[1] as usize]
        else {
            panic!()
        };
        let bc::Arguments::Window(w) = v.arguments else {
            panic!()
        };
        for offset in 0..w.len {
            assert_cleared(f, w.start.0 + offset, pc);
        }
    }
}

#[test]
fn trusted_native_values_calls_and_optional_chains_share_the_registry() {
    let reference = SymbolReference {
        module: "registry::test".into(),
        version: "native-v19".into(),
        export: Some("run".into()),
    };
    let external = ExternalSymbol::function(
        "run",
        ExternalSignature {
            result_parameter: None,
            result_cases: Vec::new(),
            parameters: vec![ExternalParameter {
                name: "a".into(),
                ty: TypeHint::Int,
                mutable: false,
                default: true,
                rest: false,
            }],
            result: TypeHint::Int,
            effect: CallEffect::NoWait,
            init_safe: Some(true),
            derived_safe: Some(false),
        },
    )
    .with_reference(reference.clone());
    let mut resolver = MemoryResolver::default();
    resolver.insert(ModuleInterface {
        canonical_path: "reg::exports".into(),
        version: "metadata-2".into(),
        exports: vec![external.clone()],
    });
    let seed = native_seed();
    for depth in 0..=10 {
        for expression in [
            format!(
                "{}native.run{}(a:1)?.中文",
                "(".repeat(depth),
                ")".repeat(depth)
            ),
            format!(
                "{}native.run(a:1){}?.中文",
                "(".repeat(depth),
                ")".repeat(depth)
            ),
        ] {
            let source = format!("use reg::exports as native;fn f(){{{expression}}}");
            let m = analyze_module(&source, &AnalysisOptions::default(), &mut resolver)
                .unwrap()
                .into_module()
                .unwrap();
            let node = m
                .external_bindings()
                .iter()
                .find_map(|e| {
                    e.node
                        .filter(|&n| m.hir().node(n).kind == hir::HirKind::Member)
                })
                .unwrap();
            let link = lowering::NativeLink {
                source_signature: None,
                target: lowering::ExternalTarget::Node(node),
                native: bc::NativeId(0),
                reference: &reference,
            };
            let c = lowering::compile(
                &m,
                &Linkage {
                    package: Some(&seed),
                    natives: &[link],
                    expansions: &[],
                },
                Options::default(),
            )
            .unwrap();
            let p = c.program.package();
            let f = function(p, "f");
            let cp = pcs(f, bc::Opcode::Call);
            assert_eq!(cp.len(), 1);
            assert_eq!(p.natives[0], seed.package().natives[0]);
            assert_eq!(p.functions[0], seed.package().functions[0]);
            let bc::Descriptor::Call(v) =
                &f.descriptors[f.code.instruction(cp[0]).unwrap().unwrap().operands[1] as usize]
            else {
                panic!()
            };
            assert_eq!(v.target, bc::CallTarget::Native(bc::NativeId(0)));
            let g = pcs(f, bc::Opcode::BranchNull);
            assert_eq!(g.len(), 1);
            assert!(cp[0] < g[0]);
            null_result(&c, f, &g);
            assert_eq!(pcs(f, bc::Opcode::MemberGet).len(), 1);
            let e = lowering::lower(&m, &Linkage::default(), Options::default()).unwrap_err();
            assert_eq!(e.kind, lowering::ErrorKind::MissingLink);
            assert_eq!(e.stats.capacity_bytes, 0);
            let wrong = SymbolReference {
                version: "wrong".into(),
                ..reference.clone()
            };
            let e = lowering::lower(
                &m,
                &Linkage {
                    package: Some(&seed),
                    natives: &[lowering::NativeLink {
                        source_signature: None,
                        reference: &wrong,
                        ..link
                    }],
                    expansions: &[],
                },
                Options::default(),
            )
            .unwrap_err();
            assert_eq!(e.kind, lowering::ErrorKind::LinkContract);
        }
    }
    for expr in [
        "native.run?()",
        "((native.run))?()",
        "native?.run()",
        "(native)?.run()",
        "native.run?.x",
        "((native.run)).x()",
    ] {
        let source = format!("use reg::exports as native;fn f(){{{expr}}}");
        let m = analyze_module(&source, &AnalysisOptions::default(), &mut resolver)
            .unwrap()
            .into_module()
            .unwrap();
        let node = m
            .external_bindings()
            .iter()
            .find_map(|e| {
                e.node
                    .filter(|&n| m.hir().node(n).kind == hir::HirKind::Member)
            })
            .unwrap();
        let c = lowering::compile(
            &m,
            &Linkage {
                package: Some(&seed),
                natives: &[lowering::NativeLink {
                    source_signature: None,
                    target: lowering::ExternalTarget::Node(node),
                    native: bc::NativeId(0),
                    reference: &reference,
                }],
                expansions: &[],
            },
            Options::default(),
        )
        .unwrap();
        assert!(!pcs(function(c.program.package(), "f"), bc::Opcode::MakeClosure).is_empty());
    }
    for expr in ["host?()", "((host))?()"] {
        let options = AnalysisOptions {
            hosts: vec![ExternalSymbol {
                name: "host".into(),
                ..external.clone()
            }],
            ..Default::default()
        };
        let m = analyze_module(&format!("fn f(){{{expr}}}"), &options, &mut NoModules)
            .unwrap()
            .into_module()
            .unwrap();
        let symbol = m.external_bindings().iter().find_map(|e| e.symbol).unwrap();
        let c = lowering::compile(
            &m,
            &Linkage {
                package: Some(&seed),
                natives: &[lowering::NativeLink {
                    source_signature: None,
                    target: lowering::ExternalTarget::Symbol(symbol),
                    native: bc::NativeId(0),
                    reference: &reference,
                }],
                expansions: &[],
            },
            Options::default(),
        )
        .unwrap();
        assert_eq!(
            pcs(function(c.program.package(), "f"), bc::Opcode::BranchNull).len(),
            1
        );
    }
}
