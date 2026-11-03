//! Warm Host own-field hints still refresh their derived upstream on demand.
use super::*;

#[test]
fn direct_host_read_preserves_derived_callback_work_and_gc_results() {
    for tiny in [false, true] {
        let run = |reference| {
            let (mut vm, upstream, source, _) = prepared(false);
            vm.legacy_named_read_completion = reference;
            let closure = vm
                .heap
                .alloc_record(
                    ObjectKind::Closure,
                    &[Value::int(2), Value::int(1), Value::int(0), upstream],
                )
                .unwrap();
            let initial = vm
                .start(FunctionId(1), &[closure], RootOwner(1), Context::Ordinary)
                .unwrap();
            let RunOutcome::Returned(delivery) = drive(&mut vm, initial, tiny).0 else {
                panic!("initial")
            };
            let downstream = vm
                .heap
                .property_ref(vm.delivery_value(delivery).unwrap())
                .unwrap()
                .0
                .host;
            vm.heap
                .publish_host_property(
                    crate::PropertyId {
                        host: source,
                        field: 0,
                    },
                    Value::int(2),
                )
                .unwrap();
            let before = vm.stats();
            let task = vm.start_derived(downstream, RootOwner(1)).unwrap();
            let (result, work, instructions, ranges, frames, values) = drive(&mut vm, task, tiny);
            assert!(matches!(result, RunOutcome::Returned(_)));
            let after = vm.stats();
            assert_eq!(after.derived_starts - before.derived_starts, 2);
            assert_eq!(after.native_entries, before.native_entries);
            assert_eq!(
                vm.heap
                    .host_property(crate::PropertyId {
                        host: upstream,
                        field: 0
                    })
                    .unwrap()
                    .as_int(),
                Some(20)
            );
            assert_eq!(
                vm.heap
                    .host_property(crate::PropertyId {
                        host: downstream,
                        field: 0
                    })
                    .unwrap()
                    .as_int(),
                Some(61)
            );
            (
                work,
                instructions,
                ranges,
                frames,
                values,
                after.heap.collections - before.heap.collections,
                after.heap.gc_units - before.heap.gc_units,
            )
        };
        assert_eq!(run(false), run(true), "tiny={tiny}");
    }
}
