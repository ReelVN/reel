use super::*;
fn source_wire(source: &[u8], declared: usize) -> Vec<u8> {
    let packed = miniz_oxide::deflate::compress_to_vec_zlib(source, 1);
    let mut wire = Vec::new();
    for value in [0u32, declared as u32, packed.len() as u32] {
        wire.extend_from_slice(&value.to_le_bytes());
    }
    wire.extend(packed);
    wire
}
fn source_reader(wire: &[u8], limits: Limits) -> Reader<'_> {
    let mut reader = Reader::new(wire, limits);
    reader.counts[0] = 1;
    reader.materialize = true;
    reader
}

#[test]
fn compressed_sources_admit_expanded_bytes_and_work_before_allocation() {
    let source = "中文🎬\n".repeat(4096);
    let wire = source_wire(source.as_bytes(), source.len());
    assert!(wire.len() < source.len() / 10);
    let mut tight = source_reader(
        &wire,
        Limits {
            max_allocation_bytes: source.len() - 1,
            ..Limits::default()
        },
    );
    let allocation = allocation_counter::measure(|| {
        assert_eq!(
            SourceFile::get(&mut tight).unwrap_err().kind,
            ErrorKind::Limit
        );
    });
    assert!(allocation.bytes_max < source.len() as u64);
    let mut tight_work = source_reader(
        &wire,
        Limits {
            max_work: (source.len() * 2) as u64,
            ..Limits::default()
        },
    );
    assert_eq!(
        SourceFile::get(&mut tight_work).unwrap_err().kind,
        ErrorKind::Limit
    );
    assert_eq!(
        SourceFile::get(&mut source_reader(&wire, Limits::default()))
            .unwrap()
            .text,
        source
    );
    let workspace = allocation_counter::measure(|| {
        compressed_source(source.as_bytes(), |_| Ok(())).unwrap();
    });
    assert!(workspace.bytes_max as usize <= SOURCE_COMPRESSION_SCRATCH);
}

#[test]
fn compressed_source_rejects_wrong_lengths_trailing_data_and_invalid_utf8() {
    for wire in [
        source_wire(b"hello", 4),
        source_wire(b"hello", 6),
        source_wire(&[0xff], 1),
    ] {
        assert_eq!(
            SourceFile::get(&mut source_reader(&wire, Limits::default()))
                .unwrap_err()
                .kind,
            ErrorKind::Encoding
        );
    }
    let mut trailing = source_wire(b"hello", 5);
    trailing.push(0);
    let length = u32::from_le_bytes(trailing[8..12].try_into().unwrap()) + 1;
    trailing[8..12].copy_from_slice(&length.to_le_bytes());
    assert_eq!(
        SourceFile::get(&mut source_reader(&trailing, Limits::default()))
            .unwrap_err()
            .kind,
        ErrorKind::Encoding
    );
    let wire = source_wire(b"", 0);
    assert!(
        SourceFile::get(&mut source_reader(&wire, Limits::default()))
            .unwrap()
            .text
            .is_empty()
    );
}
#[test]
fn compressed_code_precharges_construction_workspace_before_inflating() {
    let mut builder = CodeStoreBuilder::new();
    builder.append([Ok(Opcode::Return as u32)]).unwrap();
    let store = builder.finish().unwrap();
    let mut wire = Vec::new();
    for value in [
        1u32,
        1,
        0,
        store.packed().len() as u32,
        store.packed().len() as u32,
    ] {
        wire.extend_from_slice(&value.to_le_bytes());
    }
    wire.extend_from_slice(store.packed());
    let capacity = CodeStore::planned_capacity_bytes(1, store.packed().len(), 1).unwrap()
        + CodeStore::construction_scratch_bytes();
    let mut rejected = Reader::new(
        &wire,
        Limits {
            max_allocation_bytes: capacity - 1,
            ..Limits::default()
        },
    );
    let allocated = allocation_counter::measure(|| {
        assert_eq!(
            get_code_store(&mut rejected).unwrap_err().kind,
            ErrorKind::Limit
        );
    });
    assert!(rejected.code_store.is_none());
    assert!(
        allocated.bytes_max < super::super::code_residency::PAGE_BYTES as u64,
        "inflater workspace was allocated before admission"
    );
    let mut admitted = Reader::new(
        &wire,
        Limits {
            max_allocation_bytes: capacity,
            ..Limits::default()
        },
    );
    get_code_store(&mut admitted).unwrap();
    assert_eq!(admitted.allocation, capacity);
    assert_eq!(
        admitted
            .code_store
            .unwrap()
            .cache_stats()
            .resident_capacity_bytes,
        0
    );
}
