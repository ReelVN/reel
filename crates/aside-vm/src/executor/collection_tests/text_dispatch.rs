use super::*;

fn match_package(cases: &[&str], reject_first: bool) -> BytecodePackage {
    let mut p = package(vec![Const::Int(-1)], vec![], vec![], 3);
    p.signatures[0].parameters.push(Parameter {
        name: SymbolId(4),
        ty: TypeId(0),
        mutable: false,
        default: None,
    });
    let code = &mut p.functions[0].code;
    for (index, &key) in cases.iter().enumerate() {
        let ty = p.types.len() as u32;
        p.types.push(TypeDef::StringLiteral(key.to_owned()));
        let result = p.constants.len() as u32;
        p.constants.push(Const::Int(index as i64));
        let entry = code.len() as u32;
        code.extend([
            op(Opcode::Is, 1, 0, ty),
            op(Opcode::BranchBool, 1, entry + 2, entry + 4),
            if index == 0 && reject_first {
                op(Opcode::Jump, entry + 4, 0, 0)
            } else {
                op(Opcode::LoadConst, 2, result, 0)
            },
            op(Opcode::Return, 2, 0, 0),
        ]);
    }
    code.extend([op(Opcode::LoadConst, 2, 0, 0), op(Opcode::Return, 2, 0, 0)]);
    p
}

fn packed(cases: &[&str], reject_first: bool) -> Vec<u8> {
    encode(&match_package(cases, reject_first), Limits::default()).unwrap()
}

fn load_packed(bytes: &[u8], indexed: bool) -> Vm {
    let mut program = VerifiedProgram::decode(bytes, Limits::default()).unwrap();
    assert!(program.text_match(FunctionId(0), Pc(0)).is_some());
    if !indexed {
        program.disable_text_matches();
    }
    Vm::load(
        program,
        RuntimeSupport::default(),
        VmLimits::default(),
        SessionEpoch(1),
    )
    .unwrap()
}

fn run(vm: &mut Vm, subject: Value, budget: SliceBudget) -> (i64, u64, u64) {
    let task = vm
        .start(FunctionId(0), &[subject], RootOwner(1), Context::Ordinary)
        .unwrap();
    let mut instructions = 0;
    let mut work = 0;
    let mut slice = budget;
    for _ in 0..1024 {
        let result = vm.run_slice(task, slice, || 0).unwrap();
        instructions += u64::from(vm.last_usage.instructions);
        work += vm.last_usage.work;
        match result {
            RunOutcome::BudgetYield(next) => {
                slice.work = budget.work.max(next.required_work);
            }
            RunOutcome::Returned(delivery) => {
                let value = vm.delivery_value(delivery).unwrap().as_int().unwrap();
                vm.discard_delivery(delivery).unwrap();
                return (value, instructions, work);
            }
            _ => panic!("{result:?}"),
        }
    }
    panic!("bounded test driver exhausted: {:?}", vm.last_usage);
}

#[test]
fn packed_text_dispatch_preserves_first_match_and_fallthrough() {
    let cases = ["same", "界", "", "case", "\0", "same", "🦊", "abc", "ab"];
    for reject_first in [false, true] {
        let bytes = packed(&cases, reject_first);
        for indexed in [false, true] {
            let mut vm = load_packed(&bytes, indexed);
            for (subject, expected) in [
                ("same", if reject_first { 5 } else { 0 }),
                ("界", 1),
                ("", 2),
                ("\0", 4),
                ("🦊", 6),
                ("ab", 8),
                ("absent", -1),
                ("abcx", -1),
                (&"a".repeat(65), -1),
            ] {
                let subject = vm.heap.alloc_string(subject).unwrap();
                assert_eq!(run(&mut vm, subject, SliceBudget::default()).0, expected);
            }
            assert_eq!(run(&mut vm, Value::int(9), SliceBudget::default()).0, -1);
        }
    }
}

#[test]
fn packed_text_dispatch_reduces_steps_and_keeps_small_budgets_live() {
    let keys = (0..192).map(|i| format!("case/{i:03}")).collect::<Vec<_>>();
    let cases = keys.iter().map(String::as_str).collect::<Vec<_>>();
    let bytes = packed(&cases, false);
    let mut baseline = load_packed(&bytes, false);
    let mut indexed = load_packed(&bytes, true);
    let key = baseline.heap.alloc_string(cases[191]).unwrap();
    let previous = run(&mut baseline, key, SliceBudget::default());
    let key = indexed.heap.alloc_string(cases[191]).unwrap();
    let optimized = run(&mut indexed, key, SliceBudget::default());
    assert_eq!(previous.0, 191);
    assert_eq!(optimized.0, previous.0);
    assert_eq!(optimized.1 + 382, previous.1);
    // The preceding run warmed the code page. This budget can execute one
    // original test but lacks enough work for an indexed lookup.
    let tight = run(
        &mut indexed,
        key,
        SliceBudget {
            instructions: 1,
            work: 64,
            ..SliceBudget::default()
        },
    );
    assert_eq!(tight.0, previous.0);
    assert_eq!(tight.1, previous.1);
}

#[test]
fn packed_text_dispatch_is_optional_at_package_admission() {
    let bytes = packed(&["a", "b", "c", "d", "e", "f", "g", "h"], false);
    let full = VerifiedProgram::decode(&bytes, Limits::default()).unwrap();
    assert!(full.text_match(FunctionId(0), Pc(0)).is_some());
    let limited = VerifiedProgram::decode(
        &bytes,
        Limits {
            max_work: full.stats().validation_work - 1,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(limited.text_match(FunctionId(0), Pc(0)).is_none());
}

#[test]
fn text_dispatch_uses_final_word_pcs_and_accepts_segmented_heap_reads() {
    let p = match_package(&["aa", "bb", "cc", "dd", "ee", "ff", "gg", "hh"], false);
    let checked = verify(p, Limits::default()).unwrap();
    assert!(checked.text_match(FunctionId(0), Pc(0)).is_some());
    let mut vm = Vm::load(
        checked,
        RuntimeSupport::default(),
        VmLimits {
            heap: HeapLimits {
                max_mutation_bytes: 1,
                ..HeapLimits::default()
            },
            ..VmLimits::default()
        },
        SessionEpoch(1),
    )
    .unwrap();
    let subject = vm.heap.alloc_string("hh").unwrap();
    assert_eq!(vm.heap.byte_segment(subject, 0).unwrap(), b"h");
    assert_eq!(run(&mut vm, subject, SliceBudget::default()).0, 7);
    let absent = vm.heap.alloc_string("hi").unwrap();
    assert_eq!(run(&mut vm, absent, SliceBudget::default()).0, -1);
}

#[test]
#[ignore = "release differential text dispatch measurement"]
fn packed_text_dispatch_cost() {
    let keys = (0..373)
        .map(|i| format!("assets/components/example/{i:04}.png"))
        .collect::<Vec<_>>();
    let cases = keys.iter().map(String::as_str).collect::<Vec<_>>();
    let bytes = packed(&cases, false);
    for indexed in [false, true, true, false] {
        let budget = SliceBudget::default();
        let mut vm = load_packed(&bytes, indexed);
        let values = cases
            .iter()
            .map(|key| vm.heap.alloc_string(key).unwrap())
            .collect::<Vec<_>>();
        run(&mut vm, values[372], budget);
        let started = std::time::Instant::now();
        let mut instructions = 0;
        let mut work = 0;
        for i in 0..8192 {
            let index = (i * 53) % values.len();
            let (result, steps, charged_work) = run(&mut vm, values[index], budget);
            assert_eq!(result, index as i64);
            instructions += steps;
            work += charged_work;
        }
        eprintln!(
            "packed_text_dispatch indexed={indexed} elapsed_us={} instructions={instructions} work={work}",
            started.elapsed().as_micros()
        );
    }
}
