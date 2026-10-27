//! Streaming text collection operations.
use super::*;

#[test]
fn text_finish_uses_strict_utf8_across_one_byte_and_chunked_reads() {
    let valid: &[&[u8]] = &[b"", b"ASCII\0", "中文é🙂é👩‍🚀".as_bytes()];
    let invalid: &[&[u8]] = &[
        &[0x80],
        &[0xc0, 0xaf],
        &[0xe0, 0x80, 0xaf],
        &[0xed, 0xa0, 0x80],
        &[0xf4, 0x90, 0x80, 0x80],
        &[0xf0, 0x9f, 0x99],
        &[0xf0, 0x28, 0x8c, 0xbc],
    ];
    for step in [1, 2, 3, 1024] {
        for (input, valid) in valid
            .iter()
            .map(|s| (*s, true))
            .chain(invalid.iter().map(|s| (*s, false)))
        {
            let mut p = method_package("unused");
            p.functions[0].code =
                vec![op(Opcode::TextFinish, 1, 0, 0), op(Opcode::Return, 1, 0, 0)];
            p.functions[0].source_map.truncate(2);
            p.functions[0].descriptors.clear();
            let mut limits = VmLimits::default();
            limits.heap.max_mutation_bytes = step;
            let mut vm = load_with_limits(p, limits);
            let buffer = vm.heap_mut().alloc_byte_buffer(&[]).unwrap();
            for part in input.chunks(step as usize) {
                vm.heap_mut().bytes_append(buffer, part).unwrap();
            }
            let task = vm
                .start(FunctionId(0), &[buffer], RootOwner(9), Context::Ordinary)
                .unwrap();
            if valid {
                let value = returned(&mut vm, task);
                assert_eq!(vm.heap().info(value).unwrap().kind, ObjectKind::String);
                let mut actual = vec![];
                for offset in (0..input.len()).step_by(step as usize) {
                    actual.extend_from_slice(vm.heap().byte_segment(value, offset as u32).unwrap());
                }
                assert_eq!(actual, input);
            } else {
                fault(&mut vm, task, FaultKind::Type);
            }
        }
    }
}

#[test]
fn string_join_roots_results_cancels_and_detects_mutated_sources() {
    for action in 0..3 {
        let mut vm = load(text_method_package("join"));
        let separator = vm.heap_mut().alloc_string(" · ").unwrap();
        let text = vm.heap_mut().alloc_string(&"中🙂é".repeat(257)).unwrap();
        let source = vm.heap_mut().alloc_array(&[text, text, text]).unwrap();
        let root = vm
            .heap_mut()
            .add_root(RootOwner(8), RootKind::Host, source)
            .unwrap();
        let task = vm
            .start(
                FunctionId(0),
                &[separator, source],
                RootOwner(9),
                Context::Ordinary,
            )
            .unwrap();
        let buffer;
        loop {
            assert!(matches!(tick(&mut vm, task), RunOutcome::BudgetYield(_)));
            let Frame::Script(s) = *vm.active.frames.data.last().unwrap() else {
                panic!()
            };
            if matches!(s.phase, ScriptPhase::TextBuiltin(_)) {
                let candidate = vm.value(s.env, 2).unwrap();
                if vm.heap().info(candidate).unwrap().len >= 1024 {
                    assert_eq!(
                        vm.heap().info(candidate).unwrap().kind,
                        ObjectKind::ByteBuffer
                    );
                    buffer = candidate;
                    break;
                }
            }
        }
        match action {
            0 => {
                let value = returned(&mut vm, task);
                assert!(value.same_bits(buffer));
                let expected = vec!["中🙂é".repeat(257); 3].join(" · ");
                let mut actual = Vec::new();
                let mut offset = 0;
                while offset < expected.len() {
                    let segment = vm.heap().byte_segment(value, offset as u32).unwrap();
                    actual.extend_from_slice(segment);
                    offset += segment.len();
                }
                assert_eq!(actual, expected.as_bytes());
            }
            1 => {
                vm.heap_mut().set_field(source, 0, Value::NULL).unwrap();
                fault(&mut vm, task, FaultKind::ConcurrentModification);
            }
            _ => {
                vm.cancel(RootOwner(9)).unwrap();
                while let Ok(outcome) = vm.run_slice(task, SliceBudget::default(), || 0) {
                    assert!(matches!(outcome, RunOutcome::BudgetYield(_)));
                }
            }
        }
        while vm.heap().phase() != GcPhase::Idle {
            gc(&mut vm);
        }
        vm.begin_collection().unwrap();
        while vm.heap().phase() != GcPhase::Idle {
            gc(&mut vm);
        }
        if action != 0 {
            assert!(vm.heap().info(buffer).is_err(), "unpublished text retained");
        }
        assert_eq!(vm.heap().info(source).unwrap().len, 3);
        assert!(vm.heap().sequence_get(source, 1).unwrap().same_bits(text));
        assert_eq!(vm.stats().frames, 0);
        vm.heap_mut().release_root(root).unwrap();
    }
}

#[test]
fn text_count_and_join_work_scale_with_bytes_without_copied_prefixes() {
    let mut costs = Vec::new();
    for len in [1024, 4096, 16384] {
        let mut vm = load(text_method_package("join"));
        let separator = vm.heap_mut().alloc_string("").unwrap();
        let part = vm.heap_mut().alloc_string(&"中🙂é".repeat(128)).unwrap();
        let source = vm.heap_mut().alloc_array(&[]).unwrap();
        for _ in 0..len / 128 {
            vm.heap_mut().array_push(source, part).unwrap();
        }
        let task = vm
            .start(
                FunctionId(0),
                &[separator, source],
                RootOwner(9),
                Context::Ordinary,
            )
            .unwrap();
        let before = vm.heap().stats().chunk_allocations;
        let mut work = 0;
        loop {
            let result = vm.run_slice(task, SliceBudget::default(), || 0).unwrap();
            work += vm.stats().usage.work;
            match result {
                RunOutcome::BudgetYield(_) => {}
                RunOutcome::Returned(id) => {
                    let value = vm.delivery_value(id).unwrap();
                    assert_eq!(vm.heap().info(value).unwrap().len, len * 10);
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        let chunks = vm.heap().stats().chunk_allocations - before;
        assert_eq!(chunks, (len as u64 * 10).div_ceil(BYTE_CHUNK_LEN as u64));
        println!("join bytes={} work={work} chunks={chunks}", len * 10);
        costs.push(work);
    }
    assert_eq!(costs[2] - costs[1], 4 * (costs[1] - costs[0]));

    // Counting retains only the bound method object, irrespective of text size,
    // and std must decode scalars even when the host admits one byte per step.
    for step in [1, 3, 1024] {
        let mut limits = VmLimits::default();
        limits.heap.max_mutation_bytes = step;
        let mut vm = load_with_limits(method_package("scalar_count"), limits);
        let text = "中🙂é👩‍🚀".repeat(129);
        let source = vm.heap_mut().alloc_string(&text).unwrap();
        let task = vm
            .start(FunctionId(0), &[source], RootOwner(9), Context::Ordinary)
            .unwrap();
        let before = vm.heap().stats().chunk_allocations;
        assert_eq!(
            returned(&mut vm, task).as_int(),
            Some(text.chars().count() as i64)
        );
        assert_eq!(vm.heap().stats().chunk_allocations - before, 0);
    }
}
