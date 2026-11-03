//! Unconsumed control-flow values disappear while evaluation/checks remain.
use super::*;

fn opcodes(function: &bc::RuntimeFunction, opcode: bc::Opcode) -> usize {
    function
        .code
        .iter()
        .map(Result::unwrap)
        .filter(|i| i.opcode == opcode as u16)
        .count()
}

fn null_loads(program: &bc::RuntimePackage, function: &bc::RuntimeFunction) -> usize {
    function
        .code
        .iter()
        .map(Result::unwrap)
        .filter(|i| {
            i.opcode == bc::Opcode::LoadConst as u16
                && matches!(program.constants[i.operands[1] as usize], bc::Const::Null)
        })
        .count()
}

#[test]
fn discarded_if_avoids_the_synthetic_null_and_merge_register() {
    let compiled = compiled("fn scan(flag) { let mut value=0; if flag { value=1; } value }");
    let program = compiled.program.package();
    let scan = function(program, "scan");
    assert_eq!(opcodes(scan, bc::Opcode::BranchBool), 1);
    assert_eq!(opcodes(scan, bc::Opcode::Move), 0);
    assert_eq!(null_loads(program, scan), 0);
}

#[test]
fn branch_and_discard_uses_keep_operand_checks_without_copying_a_bool_result() {
    let source = "fn scan(a,b,c) { if a && b || c { return 1; } else { return 2; } } \
                  fn discard(a,b) { a && b; 7 } fn coalesce(a,b) { a ?? b; 7 }";
    let compiled = compiled(source);
    let program = compiled.program.package();
    for (name, branches) in [("scan", 3), ("discard", 2)] {
        let function = function(program, name);
        assert_eq!(opcodes(function, bc::Opcode::BranchBool), branches);
        assert_eq!(opcodes(function, bc::Opcode::Move), 0);
        assert_eq!(null_loads(program, function), 0);
    }
    let coalesce = function(program, "coalesce");
    assert_eq!(opcodes(coalesce, bc::Opcode::BranchNull), 1);
    assert_eq!(opcodes(coalesce, bc::Opcode::Move), 0);
    let discard = function(program, "discard");
    let id = bc::FunctionId(
        program
            .functions
            .iter()
            .position(|f| std::ptr::eq(f, discard))
            .unwrap() as u32,
    );
    for (pc, instruction) in discard.code.indexed_iter().map(Result::unwrap) {
        if instruction.opcode == bc::Opcode::BranchBool as u16 {
            let location = compiled.program.source_at(id, bc::Pc(pc as u32)).unwrap();
            assert_eq!(
                &source[location.start as usize..location.end as usize],
                "a && b"
            );
        }
    }
}

#[test]
fn flat_boolean_branches_keep_iterative_lowering_and_work_admission() {
    let chain = std::iter::repeat_n("flag", 512)
        .collect::<Vec<_>>()
        .join(" && ");
    let source = format!("fn scan(flag) {{ if {chain} {{ return 1; }} 0 }}");
    let module = analyzed(&source);
    let options = Options {
        limits: lowering::Limits {
            max_depth: 16,
            ..Default::default()
        },
        ..Default::default()
    };
    let compiled = lowering::compile(&module, &Linkage::default(), options).unwrap();
    assert_eq!(
        opcodes(
            function(compiled.program.package(), "scan"),
            bc::Opcode::BranchBool
        ),
        512
    );
    let limits = lowering::Limits {
        max_depth: 16,
        max_work: compiled.stats.work_units - 1,
        ..Default::default()
    };
    let error = lowering::compile(
        &module,
        &Linkage::default(),
        Options {
            limits,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.kind, lowering::ErrorKind::Limit);
    assert!(error.stats.work_units <= limits.max_work);
}

#[test]
fn grouped_logical_checks_keep_the_operator_span_and_children_keep_outputs() {
    let source = "fn scan(a,b) { if (a && b) {return 1;} 0 } \
                  fn gather(children=[]){children} fn node(){null} \
                  fn tree(flag){ gather(){ if flag {node();} else {node();} } }";
    let compiled = compiled(source);
    let program = compiled.program.package();
    let scan = function(program, "scan");
    let id = bc::FunctionId(
        program
            .functions
            .iter()
            .position(|f| std::ptr::eq(f, scan))
            .unwrap() as u32,
    );
    for (pc, instruction) in scan.code.indexed_iter().map(Result::unwrap) {
        if instruction.opcode == bc::Opcode::BranchBool as u16 {
            let location = compiled.program.source_at(id, bc::Pc(pc as u32)).unwrap();
            assert_eq!(
                &source[location.start as usize..location.end as usize],
                "a && b"
            );
        }
    }
    assert_eq!(
        opcodes(function(program, "tree"), bc::Opcode::ChildrenPush),
        2
    );
}

#[test]
fn one_sided_terminal_branches_have_no_uninitialized_merge_exit() {
    compiled(
        "fn only_no(flag){ if flag && ((if flag{return 9;}else{throw 8;})==null){return 3;}7 } \
         fn only_yes(flag){ if flag || ((if flag{return 9;}else{throw 8;})==null){return 3;}7 } \
         fn loop_no(flag){ while flag && ((if flag{return 9;}else{throw 8;})==null){let value=4;}7 }",
    );
}
