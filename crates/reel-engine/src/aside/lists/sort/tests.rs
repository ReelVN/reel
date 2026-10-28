use super::*;
use aside_vm::{HeapLimits, SessionEpoch};
fn heap() -> Heap {
    Heap::new(
        SessionEpoch(1),
        HeapLimits {
            max_objects: 131072,
            max_bytes: 128 << 20,
            ..Default::default()
        },
    )
    .unwrap()
}
fn entries(heap: &mut Heap, keys: &[Value]) -> Value {
    let array = heap.alloc_array(&[]).unwrap();
    for &key in keys {
        let row = heap.alloc_array(&[key]).unwrap();
        heap.array_push(array, row).unwrap();
    }
    array
}
fn run(heap: &mut Heap, input: Value, budget: u32) -> (Value, u64, u32) {
    let mut state = Value::NULL;
    let mut total = 0;
    let mut slices = 0;
    loop {
        slices += 1;
        assert!(slices < 1_000_000);
        match advance(heap, input, state, NativeBudget::RetryMinimum(budget), 256).unwrap() {
            Step::More { state: next, work } => {
                assert!((1..=budget.clamp(3, MAX_WORK)).contains(&work));
                state = next;
                total += u64::from(work);
            }
            Step::Ready { result, work } => {
                assert!((1..=budget.clamp(3, MAX_WORK)).contains(&work));
                return (result, total + u64::from(work), slices);
            }
            Step::Budget { .. } => panic!("RetryMinimum must admit the next bounded unit"),
        }
    }
}
fn values(heap: &Heap, array: Value) -> Vec<Value> {
    (0..heap.sequence_len(array).unwrap())
        .map(|i| entry(heap, heap.sequence_get(array, i).unwrap(), Entry::Key).unwrap())
        .collect()
}
fn assert_values(actual: &[Value], expected: &[Value]) {
    assert_eq!(actual.len(), expected.len());
    assert!(actual.iter().zip(expected).all(|(a, b)| a.same_bits(*b)));
}
#[test]
fn budgets_cover_setup_and_match_shared_key_order_across_odd_sized_merges() {
    for budget in [1, 3, 7, 256, 4096] {
        for count in [0, 1, 2, 3, 17, 63] {
            for order in 0..3 {
                let mut heap = heap();
                let keys: Vec<_> = (0..count)
                    .map(|n| {
                        Value::int(match order {
                            0 => n,
                            1 => count - n - 1,
                            _ => (n * 11) % count,
                        })
                    })
                    .collect();
                let input = entries(&mut heap, &keys);
                let mut expected = keys.clone();
                expected.sort_by(|&a, &b| compare(&heap, a, b, 256).unwrap());
                let (actual, work, _) = run(&mut heap, input, budget);
                assert_values(&values(&heap, actual), &expected);
                assert_values(&values(&heap, input), &keys);
                if count > 1 {
                    assert!(work >= 14, "control/buffer setup is charged too");
                }
            }
        }
    }
}
#[test]
fn exact_zero_retains_storage_and_exact_minimum_reports_long_string_bytes() {
    let mut heap = heap();
    let prefix = "长键".repeat(128);
    let a = heap.alloc_string(&(prefix.clone() + "b")).unwrap();
    let b = heap.alloc_string(&(prefix + "a")).unwrap();
    let input = entries(&mut heap, &[a, b]);
    let before = heap.stats().charged_bytes;
    assert!(matches!(
        advance(&mut heap, input, Value::NULL, NativeBudget::Exact(0), 256).unwrap(),
        Step::Budget { required_work: 1 }
    ));
    assert_eq!(heap.stats().charged_bytes, before);
    let mut state = Value::NULL;
    for _ in 0..14 {
        match advance(&mut heap, input, state, NativeBudget::Exact(1), 256).unwrap() {
            Step::More {
                state: next,
                work: 1,
            } => state = next,
            _ => panic!("setup must proceed one bounded field at a time"),
        }
    }
    assert_eq!(heap.sequence_len(state).unwrap(), 13);
    assert!(matches!(
        advance(&mut heap, input, state, NativeBudget::Exact(1), 256).unwrap(),
        Step::Budget { required_work: 3 }
    ));
    assert_eq!(Cursor::load(&heap, state).unwrap().offset, 0);
    let mut offset = 0;
    let work = match key_step(&heap, a, b, &mut offset, 7, 256).unwrap() {
        KeyStep::More(work) => work,
        _ => panic!("shared prefix must suspend"),
    };
    assert_eq!((work, offset), (7, 3));
}
#[test]
fn mixed_types_long_strings_and_nonadjacent_duplicates_keep_key_identity() {
    let mut heap = heap();
    let prefix = "共同前缀".repeat(32);
    let a = heap.alloc_string(&(prefix.clone() + "a")).unwrap();
    let b = heap.alloc_string(&(prefix + "b")).unwrap();
    let array = heap.alloc_array(&[]).unwrap();
    let table = heap.alloc_table().unwrap();
    let keys = [
        b,
        Value::int(1),
        Value::float(1.),
        Value::NULL,
        table,
        Value::bool(false),
        a,
        array,
    ];
    let input = entries(&mut heap, &keys);
    let mut expected = keys.to_vec();
    expected.sort_by(|&a, &b| compare(&heap, a, b, 256).unwrap());
    let (result, _, _) = run(&mut heap, input, 7);
    assert_values(&values(&heap, result), &expected);
    let duplicate = heap.alloc_string(&("共同前缀".repeat(32) + "a")).unwrap();
    let input = entries(&mut heap, &[a, Value::int(4), b, duplicate]);
    let mut state = Value::NULL;
    for _ in 0..10000 {
        match advance(&mut heap, input, state, NativeBudget::Exact(7), 256) {
            Ok(Step::More { state: next, .. }) => state = next,
            Err(AdapterError::Input("list keys must be unique")) => return,
            _ => panic!("duplicate content escaped bounded merge"),
        }
    }
    panic!("duplicate validation did not finish");
}
#[test]
fn ordered_index_reuses_input_and_reverse_needs_only_linear_copy_work() {
    for reverse in [false, true] {
        let mut heap = heap();
        let keys: Vec<_> = (0..1000)
            .map(|n| Value::int(if reverse { 999 - n } else { n }))
            .collect();
        let input = entries(&mut heap, &keys);
        let (result, work, _) = run(&mut heap, input, 32);
        assert_eq!(result.same_bits(input), !reverse);
        assert!(
            work <= 14 + 2 * 1000,
            "ordered/reversed inputs avoid merge passes"
        );
    }
}

#[test]
fn one_value_heap_windows_preserve_batch_prefix_and_cursor_state() {
    let mut heap = Heap::new(
        SessionEpoch(1),
        HeapLimits {
            max_mutation_values: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let keys: Vec<_> = (0..193).map(|n| Value::int((n * 73) % 193)).collect();
    let input = entries(&mut heap, &keys);
    let (result, _, _) = run(&mut heap, input, 256);
    let expected: Vec<_> = (0..193).map(Value::int).collect();
    assert_values(&values(&heap, result), &expected);
    assert_values(&values(&heap, input), &keys);
}

mod cost;
