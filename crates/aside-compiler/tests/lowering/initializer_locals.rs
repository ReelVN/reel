//! Static identity and CFG contracts; no instruction/value evaluation.
use super::*;
use aside_compiler::{AnalysisContext, EntryPoint, ParseOptions, analyze_source, semantic::*};
use hir::{BindingOrigin as O, Initialization};

fn symbol(m: &hir::AnalyzedModule, name: &str, origin: O) -> hir::SymbolId {
    hir::SymbolId(
        m.symbols()
            .iter()
            .position(|s| m.hir().name(s.name) == name && s.origin == origin)
            .unwrap() as u32,
    )
}
fn binding(c: &lowering::Compilation, f: bc::FunctionId, s: hir::SymbolId) -> bc::BindingId {
    c.metadata
        .bindings
        .iter()
        .find(|b| b.function == f && b.source == s)
        .unwrap()
        .binding
}
fn creator(
    m: &hir::AnalyzedModule,
    c: &lowering::Compilation,
    output: bc::FunctionId,
) -> Option<usize> {
    c.metadata
        .functions
        .iter()
        .position(|f| f.output == output)
        .or_else(|| {
            c.metadata.functions.iter().position(|f| {
                let body = &c.program.package().functions[f.output.index()];
                c.program.package().signatures[body.signature.index()]
                    .parameters
                    .iter()
                    .any(|p| p.default == Some(bc::ParameterDefault::Function(output)))
            })
        })
        .inspect(|&id| {
            assert!(id < m.functions().len());
        })
}

/// A conservative graph includes ordinary edges, all enclosing exception/
/// cleanup targets selected by B01, and each cleanup's Leave continuation.
/// Dominance over this graph proves preparation precedes uses on every edge;
/// it does not prove the runtime semantics of the instructions.
pub(super) fn dominators(c: &lowering::Compilation, id: bc::FunctionId) -> Vec<Vec<bool>> {
    let f = &c.program.package().functions[id.index()];
    let n = f.code.len();
    let mut edges = vec![Vec::new(); n];
    for (pc, i) in f.code.iter().enumerate() {
        let spec = bc::instruction_spec(i.opcode).unwrap();
        edges[pc].extend(match spec.flow {
            bc::Flow::Next => vec![pc + 1],
            bc::Flow::Jump | bc::Flow::Leave => vec![i.operands[0] as usize],
            bc::Flow::Branch => vec![i.operands[1] as usize, i.operands[2] as usize],
            _ => vec![],
        });
        if spec.may_throw {
            let mut consumed = None;
            while let Some(h) = c
                .program
                .exception_handler(id, bc::Pc(pc as u32), consumed)
                .unwrap()
            {
                edges[pc].push(h.pc.index());
                consumed = Some(h.region);
            }
        }
        let destination = match spec.flow {
            bc::Flow::Return => Some(bc::CleanupDestination::Return),
            bc::Flow::Leave => Some(bc::CleanupDestination::Leave(bc::Pc(i.operands[0]))),
            _ => None,
        };
        if let Some(destination) = destination {
            let mut consumed = None;
            while let Some(h) = c
                .program
                .cleanup_handler(id, bc::Pc(pc as u32), destination, consumed)
                .unwrap()
            {
                edges[pc].push(h.pc.index());
                if let bc::CleanupDestination::Leave(target) = destination {
                    let end = f.regions[h.region.index()].finally.unwrap().end.index() - 1;
                    assert_eq!(f.code[end].opcode, bc::Opcode::EndFinally as u16);
                    edges[end].push(target.index());
                }
                consumed = Some(h.region);
            }
        }
    }
    let mut reachable = vec![false; n];
    let mut pending = vec![0];
    while let Some(pc) = pending.pop() {
        if !reachable[pc] {
            reachable[pc] = true;
            pending.extend(&edges[pc]);
        }
    }
    let mut predecessors = vec![Vec::new(); n];
    for (pc, next) in edges.iter().enumerate().filter(|(pc, _)| reachable[*pc]) {
        for &next in next {
            predecessors[next].push(pc);
        }
    }
    let mut dom = vec![reachable.clone(); n];
    dom[0].fill(false);
    dom[0][0] = true;
    let mut changed = true;
    while changed {
        changed = false;
        for pc in 1..n {
            if !reachable[pc] {
                dom[pc].fill(false);
                continue;
            }
            let mut next = reachable.clone();
            for &pred in &predecessors[pc] {
                for (bit, &old) in next.iter_mut().zip(&dom[pred]) {
                    *bit &= old;
                }
            }
            next[pc] = true;
            if next != dom[pc] {
                dom[pc] = next;
                changed = true;
            }
        }
    }
    dom
}

pub(super) fn assert_local_structure(m: &hir::AnalyzedModule, c: &lowering::Compilation) {
    let p = c.program.package();
    assert_eq!(c.metadata.globals.len(), p.modules[0].globals.len());
    for g in &c.metadata.globals {
        assert_eq!(m.symbols()[g.source.0 as usize].origin, O::ModuleGlobal);
    }
    for (output, f) in p.functions.iter().enumerate() {
        let id = bc::FunctionId(output as u32);
        assert_eq!(
            c.metadata
                .bindings
                .iter()
                .filter(|b| b.function == id)
                .count(),
            f.bindings.len()
        );
        if let Some(owner) = creator(m, c, id) {
            let modes: Vec<_> = m.functions()[owner]
                .captures
                .iter()
                .map(|cap| match cap.mode {
                    hir::CaptureMode::Value => bc::CaptureKind::Value,
                    hir::CaptureMode::Cell => bc::CaptureKind::Cell,
                })
                .collect();
            assert_eq!(f.captures, modes);
        }
        let dom = dominators(c, id);
        let mut preparations = vec![None; f.bindings.len()];
        for identity in c.metadata.bindings.iter().filter(|b| b.function == id) {
            let s = &m.symbols()[identity.source.0 as usize];
            let b = &f.bindings[identity.binding.index()];
            assert_ne!(s.origin, O::ModuleGlobal);
            assert_ne!(s.initialization, Initialization::External);
            if s.origin == O::EntryLocal {
                assert_eq!(id, c.entry);
            } else if let O::FunctionLocal(owner) = s.origin {
                assert_eq!(creator(m, c, id), Some(owner.0 as usize));
            }
            if b.initialization != bc::BindingInitialization::Explicit {
                continue;
            }
            let reset = match b.storage {
                bc::BindingStorage::Cell(_) => bc::Opcode::CellNew,
                _ => bc::Opcode::Clear,
            };
            let operand = match b.storage {
                bc::BindingStorage::Cell(_) => identity.binding.0,
                bc::BindingStorage::Value(r) => r.0,
            };
            let resets: Vec<_> = f
                .code
                .iter()
                .enumerate()
                .filter(|(_, i)| {
                    if reset == bc::Opcode::Clear {
                        clears(i, operand)
                    } else {
                        i.opcode == reset as u16 && i.operands[0] == operand
                    }
                })
                .map(|(pc, _)| pc)
                .collect();
            // Tests here use reachable declarations. One static reset site is
            // revisited on reentry, with the same fixed BindingId.
            assert_eq!(
                resets.len(),
                1,
                "{} in function {output}",
                m.hir().name(s.name)
            );
            let pc = resets[0];
            preparations[identity.binding.index()] = Some(pc);
            let mapped = aside_compiler::TextRange::new(
                source_span(f, pc).start.into(),
                source_span(f, pc).end.into(),
            );
            // A function's parameter/body scope spans its full declaration,
            // while entry instructions map to its actual body Block node.
            assert!(
                m.hir().nodes.iter().enumerate().any(|(n, node)| {
                    matches!(node.kind, hir::HirKind::Module | hir::HirKind::Block)
                        && m.facts()[n].scope == Some(s.scope)
                        && node.span == mapped
                }) || (s.kind == hir::SymbolKind::Catch
                    && m.scopes()[s.scope.0 as usize].span == mapped)
            );
        }
        for (pc, i) in f.code.iter().enumerate() {
            let mut uses = Vec::new();
            if matches!(
                bc::instruction_spec(i.opcode).unwrap().opcode,
                bc::Opcode::BindInit | bc::Opcode::BindRead | bc::Opcode::BindWrite
            ) {
                uses.push(bc::BindingId(if i.opcode == bc::Opcode::BindRead as u16 {
                    i.operands[1]
                } else {
                    i.operands[0]
                }));
            }
            if i.opcode == bc::Opcode::MakeClosure as u16 {
                let bc::Descriptor::Closure { function, captures } =
                    &f.descriptors[i.operands[1] as usize]
                else {
                    panic!("closure descriptor");
                };
                let target = c
                    .metadata
                    .functions
                    .iter()
                    .position(|f| f.output == *function)
                    .unwrap();
                assert_eq!(captures.len(), m.functions()[target].captures.len());
                for (source, expected) in captures.iter().zip(&m.functions()[target].captures) {
                    match *source {
                        bc::CaptureSource::Binding(b) => {
                            uses.push(b);
                            assert_eq!(
                                c.metadata
                                    .bindings
                                    .iter()
                                    .find(|x| x.function == id && x.binding == b)
                                    .unwrap()
                                    .source,
                                expected.symbol
                            );
                        }
                        bc::CaptureSource::Capture(capture) => {
                            assert_eq!(
                                m.functions()[creator(m, c, id).unwrap()].captures[capture.index()]
                                    .symbol,
                                expected.symbol
                            );
                        }
                    }
                }
            }
            for b in uses {
                if let Some(reset) = preparations[b.index()] {
                    assert!(
                        reset < pc && dom[pc][reset],
                        "binding {} not prepared on all CFG paths to PC {pc}",
                        b.0
                    );
                } else {
                    assert!(matches!(
                        f.bindings[b.index()].initialization,
                        bc::BindingInitialization::Parameter(_)
                    ));
                }
            }
        }
    }
}

fn checked(source: &str) -> (hir::AnalyzedModule, lowering::Compilation) {
    let m = analyzed(source);
    let c = compiled(source);
    assert_local_structure(&m, &c);
    (m, c)
}

#[test]
fn root_exports_shadowing_forward_recursion_and_shared_capture_sources() {
    let source = "pub let x=1; pub fn root(){x} if true{fn first(){later} fn a(){b()} fn b(){a()} let later=2; let mut x=3; let c=||{x+=1;x}; let d=||x; first();a();c();d();}";
    let (m, c) = checked(source);
    let p = c.program.package();
    assert_eq!(p.modules[0].globals.len(), 2);
    assert_eq!(p.modules[0].exports.len(), 2);
    let f = &p.functions[c.entry.index()];
    assert!(!f.code.iter().any(|i| matches!(
        bc::instruction_spec(i.opcode).unwrap().opcode,
        bc::Opcode::GlobalRead | bc::Opcode::GlobalWrite
    )));
    assert_eq!(
        f.code
            .iter()
            .filter(|i| i.opcode == bc::Opcode::GlobalInit as u16)
            .count(),
        2
    );
    let later = binding(&c, c.entry, symbol(&m, "later", O::EntryLocal));
    assert!(!f.bindings[later.index()].mutable);
    assert!(matches!(
        f.bindings[later.index()].storage,
        bc::BindingStorage::Cell(_)
    ));
    let closure = f.code.iter().position(|i| i.opcode == bc::Opcode::MakeClosure as u16 && matches!(&f.descriptors[i.operands[1] as usize], bc::Descriptor::Closure{ function: target, .. } if p.symbols[p.functions[target.index()].name.index()] == "first")).unwrap();
    let init = f
        .code
        .iter()
        .position(|i| i.opcode == bc::Opcode::BindInit as u16 && i.operands[0] == later.0)
        .unwrap();
    assert!(closure < init);
    let scope = m.symbols()[symbol(&m, "later", O::EntryLocal).0 as usize].scope;
    for &sym in &m.scopes()[scope.0 as usize].symbols {
        let b = binding(&c, c.entry, sym);
        let reset = f
            .code
            .iter()
            .position(|i| {
                (i.opcode == bc::Opcode::CellNew as u16 && i.operands[0] == b.0) || clears(i, b.0)
            })
            .unwrap();
        assert!(
            reset < closure,
            "every slot must be prepared before the first named closure"
        );
    }
    assert!(function(p, "root").captures.is_empty());
    for name in ["a", "b", "first"] {
        assert_eq!(function(p, name).captures, [bc::CaptureKind::Cell]);
    }
    let x = symbol(&m, "x", O::EntryLocal);
    assert_eq!(
        m.functions()
            .iter()
            .flat_map(|f| &f.captures)
            .filter(|cap| cap.symbol == x && cap.mode == hir::CaptureMode::Cell)
            .count(),
        2
    );
}

#[test]
fn condition_body_catch_and_finally_entries_cover_cleanup_backedges() {
    let source = "let mut i=0; outer:while if true{let mut condition=i; let probe=||condition; probe(); i<2}else{false}{let mut value=i; fn read(){value} let bump=||{value+=1;value}; i+=1; try{if i<2{continue outer;}throw i;}catch error{fn fault(){error}fault();break outer;}finally{let cleanup=i;let observe=||cleanup;observe();}}";
    let (m, c) = checked(source);
    let f = &c.program.package().functions[c.entry.index()];
    assert_eq!(c.program.package().modules[0].globals.len(), 1);
    for name in ["condition", "value", "error", "cleanup"] {
        assert!(m.symbols()[symbol(&m, name, O::EntryLocal).0 as usize].per_iteration);
    }
    let error = binding(&c, c.entry, symbol(&m, "error", O::EntryLocal));
    let catch = &f.regions[0].catch.as_ref().unwrap().range;
    assert_eq!(
        f.code[catch.start.index()].opcode,
        bc::Opcode::CellNew as u16
    );
    assert_eq!(f.code[catch.start.index()].operands[0], error.0);
    assert_eq!(
        f.code[catch.start.index() + 1].opcode,
        bc::Opcode::BindInit as u16
    );
    let head = f
        .code
        .iter()
        .find(|i| i.opcode == bc::Opcode::Jump as u16)
        .unwrap()
        .operands[0];
    for (pc, i) in f.code.iter().enumerate().filter(|(pc, _)| {
        matches!(
            &source[source_span(f, *pc).start as usize..source_span(f, *pc).end as usize],
            "continue outer;" | "break outer;"
        )
    }) {
        assert_eq!(i.opcode, bc::Opcode::Leave as u16);
        let h = c
            .program
            .cleanup_handler(
                c.entry,
                bc::Pc(pc as u32),
                bc::CleanupDestination::Leave(bc::Pc(i.operands[0])),
                None,
            )
            .unwrap()
            .unwrap();
        assert_eq!(h.kind, bc::HandlerKind::Finally);
        if &source[source_span(f, pc).start as usize..source_span(f, pc).end as usize]
            == "continue outer;"
        {
            assert_eq!(i.operands[0], head);
        }
    }
    let condition = binding(&c, c.entry, symbol(&m, "condition", O::EntryLocal));
    let reset = f
        .code
        .iter()
        .position(|i| i.opcode == bc::Opcode::CellNew as u16 && i.operands[0] == condition.0)
        .unwrap();
    assert!(head as usize <= reset);
    // A labelled continue from inside the condition takes the same head,
    // through shared finally cleanup, rather than entering the body.
    checked(
        "let mut i=0; outer:while if i<2{let mut x=i;let hold=||x;i+=1;try{continue outer;}finally{x;}}else{false}{}",
    );
}

#[test]
fn definition_environment_is_forwarded_through_defaults_and_nested_closures() {
    let (m, c) = checked(
        "pub let escaped=if true{let mut x=1; fn outer(a=x,b=||{x+=1;x},mut p=1,q=||{p+=1;p}){let middle=||{||x};middle()} outer()}else{||0};",
    );
    let p = c.program.package();
    assert_eq!(p.modules[0].globals.len(), 1);
    assert_eq!(p.modules[0].exports.len(), 1);
    let x = symbol(&m, "x", O::EntryLocal);
    let owner = m
        .functions()
        .iter()
        .position(|f| m.hir().node(f.node).kind == hir::HirKind::Function)
        .unwrap();
    assert!(
        m.functions()[owner]
            .captures
            .iter()
            .any(|cap| cap.symbol == x && cap.used_in_default)
    );
    let outer = function(p, "outer");
    for param in &p.signatures[outer.signature.index()].parameters {
        let Some(bc::ParameterDefault::Function(default)) = param.default else {
            assert!(matches!(
                param.default,
                Some(bc::ParameterDefault::Constant(_))
            ));
            continue;
        };
        let thunk = &p.functions[default.index()];
        assert_eq!(thunk.captures, outer.captures);
        assert!(
            !c.metadata
                .bindings
                .iter()
                .any(|b| b.function == default && b.source == x)
        );
        assert!(
            !thunk
                .code
                .iter()
                .any(|i| i.opcode == bc::Opcode::CellNew as u16)
        );
    }
    assert!(matches!(
        outer.bindings[2].storage,
        bc::BindingStorage::Cell(bc::Reg(2))
    ));
    assert!(
        !outer
            .code
            .iter()
            .any(|i| i.opcode == bc::Opcode::CellNew as u16 && i.operands[0] == 2)
    );
}

#[test]
fn later_named_capture_promotes_earlier_value_users_of_the_same_entry_binding() {
    let (m, _) = checked(
        "if true{let x=1;let first=||x;if true{fn named(){x}named();}let last=||x;first();last();}",
    );
    let x = symbol(&m, "x", O::EntryLocal);
    assert_eq!(m.symbols()[x.0 as usize].storage, hir::Storage::SharedCell);
    let captures: Vec<_> = m
        .functions()
        .iter()
        .flat_map(|f| &f.captures)
        .filter(|cap| cap.symbol == x)
        .collect();
    assert_eq!(captures.len(), 3);
    assert!(
        captures
            .iter()
            .all(|cap| cap.mode == hir::CaptureMode::Cell)
    );
}

#[test]
fn enclosing_lexical_symbols_require_real_value_or_cell_links_before_allocation() {
    let reference = SymbolReference {
        module: "host::lexical".into(),
        version: "v1".into(),
        export: Some("external".into()),
    };
    for context in [
        AnalysisContext::Module,
        AnalysisContext::Function,
        AnalysisContext::NoWait,
    ] {
        let source = "let first=||{||external};if true{fn later(){external}later();}first();";
        let m = analyze_source(
            source,
            ParseOptions {
                entry: EntryPoint::Block,
                ..Default::default()
            },
            &AnalysisOptions {
                context,
                enclosing: vec![EnclosingBinding {
                    symbol: ExternalSymbol::dynamic("external").with_reference(reference.clone()),
                    mutable: false,
                }],
                ..Default::default()
            },
            &mut NoModules,
        )
        .unwrap()
        .into_module()
        .unwrap();
        let external = hir::SymbolId(0);
        let s = &m.symbols()[0];
        assert_eq!(s.kind, hir::SymbolKind::Local);
        assert_eq!(s.initialization, Initialization::External);
        assert_eq!(s.origin.function(), s.owner);
        assert_ne!(s.origin, O::External);
        assert_ne!(s.origin, O::ModuleGlobal);
        assert_eq!(
            m.functions()
                .iter()
                .flat_map(|f| &f.captures)
                .filter(|cap| cap.symbol == external)
                .count(),
            3
        );
        let native_links = [lowering::NativeLink {
            source_signature: None,
            target: lowering::ExternalTarget::Symbol(external),
            native: bc::NativeId(0),
            reference: &reference,
        }];
        let seed = native_seed();
        for linkage in [
            Linkage::default(),
            Linkage {
                package: Some(&seed),
                natives: &native_links,
                expansions: &[],
            },
        ] {
            let error = lowering::lower(&m, &linkage, Options::default()).unwrap_err();
            assert_eq!(error.kind, lowering::ErrorKind::MissingLink);
            assert_eq!(error.span, s.declaration);
            assert!(error.detail.contains("external Value/Cell link"));
            assert_eq!(error.stats.capacity_bytes, 0);
            assert_eq!(error.stats.instructions, 0);
        }
    }
}

#[test]
fn ownerless_nowait_declarations_and_nonroot_exports_stay_located_rejections() {
    for (source, context, name) in [
        ("let hidden=1;", AnalysisContext::NoWait, "hidden"),
        ("fn hidden(){1}", AnalysisContext::NoWait, "hidden"),
        (
            "if true{pub let hidden=1;}",
            AnalysisContext::Module,
            "hidden",
        ),
        (
            "if true{pub fn hidden(){1}}",
            AnalysisContext::Module,
            "hidden",
        ),
        (
            "fn f(){pub let hidden=1;}",
            AnalysisContext::Module,
            "hidden",
        ),
    ] {
        let m = analyze_module(
            source,
            &AnalysisOptions {
                context,
                ..Default::default()
            },
            &mut NoModules,
        )
        .unwrap()
        .into_module()
        .unwrap();
        let error = lowering::lower(&m, &Linkage::default(), Options::default()).unwrap_err();
        assert_eq!(error.kind, lowering::ErrorKind::Unsupported);
        assert_eq!(&source[aside_compiler::byte_range(error.span)], name);
        assert_eq!(error.stats.capacity_bytes, 0);
    }
}

#[test]
fn initializer_slots_and_capture_growth_are_bounded_with_located_failures() {
    let source = "if true{let mut x=0;let first=||x;let nested=||{||x};}";
    let (m, good) = checked(source);
    for limits in [
        lowering::Limits {
            max_work: 1,
            ..Default::default()
        },
        lowering::Limits {
            max_capacity_bytes: 64,
            ..Default::default()
        },
        lowering::Limits {
            max_records: 8,
            ..Default::default()
        },
        lowering::Limits {
            max_instructions: 3,
            ..Default::default()
        },
        lowering::Limits {
            max_blocks: 1,
            ..Default::default()
        },
        lowering::Limits {
            max_registers: 2,
            ..Default::default()
        },
    ] {
        let error = lowering::lower(
            &m,
            &Linkage::default(),
            Options {
                limits,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.kind, lowering::ErrorKind::Limit);
        assert!(error.stats.capacity_peak_bytes <= limits.max_capacity_bytes);
        assert!(error.stats.work_units <= limits.max_work);
        assert!(error.stats.records <= limits.max_records);
        assert!(error.stats.instructions <= limits.max_instructions);
        assert!(error.stats.blocks <= limits.max_blocks);
        assert!(source.is_char_boundary(usize::from(error.span.start())));
        assert!(source.is_char_boundary(usize::from(error.span.end())));
        if limits.max_registers == 2 {
            assert_eq!(&source[aside_compiler::byte_range(error.span)], "nested");
            assert_eq!(error.stats.instructions, 0);
        }
    }
    let opts = Options {
        limits: lowering::Limits {
            max_capacity_bytes: good.stats.capacity_peak_bytes,
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(lowering::compile(&m, &Linkage::default(), opts).is_ok());
}
