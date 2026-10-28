use super::*;
use crate::{GcBudget, HeapLimits};

#[test]
fn watched_history_covers_value_writes_without_retaining_objects() {
    let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let array = heap.alloc_array(&[Value::int(1)]).unwrap();
    let cell = heap.alloc_cell(Value::int(2)).unwrap();
    let table = heap.alloc_table().unwrap();
    let name = heap.alloc_string("x").unwrap();
    heap.table_store(table, 0, name, Value::int(1)).unwrap();
    let host = heap.alloc_host(table, &[Value::NULL], None).unwrap();
    heap.observe_host_properties(host, 1, 0).unwrap();
    let buffer = heap.alloc_byte_buffer(b"a").unwrap();
    let values = [array, cell, table, host, buffer];
    let handles: Vec<_> = values
        .iter()
        .map(|&value| heap.watch_object(value).unwrap())
        .collect();
    let cursor = heap.change_cursor();
    heap.array_push(array, Value::int(3)).unwrap();
    heap.cell_write(cell, Value::int(4)).unwrap();
    heap.write_host_property(crate::PropertyId { host, field: 0 }, Value::int(5), true)
        .unwrap();
    heap.set_host_payload(host, 0, Value::int(6)).unwrap();
    heap.bytes_append(buffer, b"b").unwrap();
    let changed: Vec<_> = heap.changes_since(cursor).unwrap().collect();
    for handle in &handles {
        assert!(changed.contains(handle));
    }
    let quiet = heap.change_cursor();
    let private = heap.alloc_array(&[]).unwrap();
    for _ in 0..1024 {
        heap.array_push(private, Value::NULL).unwrap();
    }
    assert_eq!(heap.changes_since(quiet).unwrap().count(), 0);
    heap.begin_collection().unwrap();
    while heap.phase() != crate::GcPhase::Idle {
        heap.gc_step(GcBudget::default()).unwrap();
    }
    for handle in handles {
        assert!(heap.import_handle(handle).is_err());
    }
}

#[test]
fn history_wrap_and_foreign_cursors_request_full_projection() {
    let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    let value = heap.alloc_array(&[Value::NULL]).unwrap();
    let handle = heap.watch_object(value).unwrap();
    let cursor = heap.change_cursor();
    for _ in 0..512 {
        heap.set_field(value, 0, Value::int(1)).unwrap();
    }
    assert_eq!(heap.changes_since(cursor).unwrap().count(), 512);
    heap.set_field(value, 0, Value::int(2)).unwrap();
    assert!(heap.changes_since(cursor).is_none());
    let cursor = heap.change_cursor();
    heap.set_field(value, 0, Value::int(3)).unwrap();
    assert_eq!(
        heap.changes_since(cursor).unwrap().collect::<Vec<_>>(),
        [handle]
    );
    let foreign = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
    assert!(foreign.changes_since(cursor).is_none());
}
