//! Opt-in index residency and allocation measurement; payloads remain on disk.
use reel_package::archive::{ArchiveIndex, Entry};
use std::{fs::File, time::Instant};

#[test]
#[ignore = "set REEL_ARCHIVE_INDEX_PATHS to existing release archives"]
fn archive_index_mount_and_lookup_cost() {
    let paths = std::env::split_paths(&std::env::var_os("REEL_ARCHIVE_INDEX_PATHS").unwrap())
        .collect::<Vec<_>>();
    assert!(!paths.is_empty());
    let mut files = paths
        .iter()
        .map(|path| File::open(path).unwrap())
        .collect::<Vec<_>>();
    // The collection of owners belongs to the harness; the measured region
    // contains only the production index reader and its retained allocations.
    let mut indexes = Vec::with_capacity(paths.len());
    let started = Instant::now();
    let allocation = allocation_counter::measure(|| {
        for file in &mut files {
            indexes.push(ArchiveIndex::read(file).unwrap());
        }
    });
    let mount_ns = started.elapsed().as_nanos();
    let entries = indexes
        .iter()
        .map(|index| index.entries().len())
        .sum::<usize>();
    let path_bytes = indexes
        .iter()
        .flat_map(|index| index.entries())
        .map(|entry| entry.path.len())
        .sum::<usize>();
    let mut lookups = 0_u64;
    let started = Instant::now();
    for _ in 0..8 {
        for index in &indexes {
            for entry in index.entries().step_by(97) {
                let found = index
                    .get(std::hint::black_box(entry.path.as_ref()))
                    .unwrap();
                assert_eq!(found.version, entry.version);
                std::hint::black_box(found.offset);
                lookups += 1;
            }
        }
    }
    println!(
        "{}",
        serde_json::json!({
            "paths": paths, "entries": entries, "path_bytes": path_bytes,
            "entry_view_bytes": size_of::<Entry>(),
            "mount_ns": mount_ns, "lookup_ns": started.elapsed().as_nanos(), "lookups": lookups,
            "allocation_calls": allocation.count_total, "allocation_bytes": allocation.bytes_total,
            "resident_allocation_bytes": allocation.bytes_current,
            "peak_allocation_bytes": allocation.bytes_max,
            "cache_condition": "current OS cache; no cold-cache or target-device claim",
        })
    );
}
