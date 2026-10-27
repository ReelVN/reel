use reel_package::archive::{ArchiveIndex, ArchiveKind, ArchiveWriter, HEADER_LEN};
use std::io::{self, Cursor, Read, Seek, SeekFrom};

fn fixture() -> Vec<u8> {
    let mut writer = ArchiveWriter::new(Cursor::new(Vec::new()), ArchiveKind::Resources).unwrap();
    for name in ["z/卡.png", "a/image", "middle"] {
        writer.add_raw(name, &mut &b"content"[..]).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

struct Counted<'a> {
    input: Cursor<&'a [u8]>,
    read: usize,
}
impl Read for Counted<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let read = self.input.read(bytes)?;
        self.read += read;
        Ok(read)
    }
}
impl Seek for Counted<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.input.seek(position)
    }
}

#[test]
fn rejected_peak_admission_reads_only_header_and_allocates_no_index_storage() {
    let bytes = fixture();
    let mut input = Counted {
        input: Cursor::new(&bytes),
        read: 0,
    };
    let allocation = allocation_counter::measure(|| {
        let error = ArchiveIndex::read_with_admission(&mut input, |_| {
            Err(io::ErrorKind::OutOfMemory.into())
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::OutOfMemory);
    });
    assert_eq!(input.read as u64, HEADER_LEN);
    assert_eq!(allocation.count_total, 0);
}

#[test]
fn mount_capacity_and_peak_match_retained_records_arena_and_bounded_parser() {
    let bytes = fixture();
    let mut index = None;
    let mut admitted = 0;
    let mut peak = 0;
    let allocation = allocation_counter::measure(|| {
        index = Some(
            ArchiveIndex::read_with_admission(&mut Cursor::new(&bytes), |bytes| {
                admitted = bytes;
                peak = peak.max(bytes);
                Ok(())
            })
            .unwrap(),
        );
    });
    let index = index.unwrap();
    assert_eq!(admitted, index.capacity_bytes());
    assert_eq!(allocation.bytes_current as u64, index.capacity_bytes());
    assert_eq!(allocation.bytes_max as u64, peak);
    assert_eq!(
        allocation.count_total, 3,
        "records, paths and bounded input buffer"
    );
    let entries = index.entries().collect::<Vec<_>>();
    assert_eq!(
        entries.iter().map(|entry| entry.path).collect::<Vec<_>>(),
        ["a/image", "middle", "z/卡.png"]
    );
    for entry in entries {
        assert_eq!(index.get(entry.path), Some(entry));
    }
}

#[test]
fn rejected_final_admission_drops_all_allocated_index_storage() {
    let bytes = fixture();
    let mut calls = 0;
    let allocation = allocation_counter::measure(|| {
        let error = ArchiveIndex::read_with_admission(&mut Cursor::new(&bytes), |_| {
            calls += 1;
            if calls == 2 {
                Err(io::ErrorKind::OutOfMemory.into())
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::OutOfMemory);
    });
    assert_eq!(calls, 2);
    assert_eq!(allocation.bytes_current, 0);
}

#[test]
fn unsorted_v1_records_keep_case_sensitive_lookup_and_utf8_paths() {
    let mut bytes = fixture();
    let offset = u64::from_le_bytes(bytes[16..24].try_into().unwrap()) as usize;
    let mut records = Vec::new();
    let mut cursor = offset;
    while cursor < bytes.len() {
        let path = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
        records.push(bytes[cursor..cursor + 72 + path].to_vec());
        cursor += 72 + path;
    }
    bytes.truncate(offset);
    for record in records.into_iter().rev() {
        bytes.extend(record);
    }
    let index = ArchiveIndex::read(&mut Cursor::new(&bytes)).unwrap();
    assert_eq!(
        index.entries().map(|entry| entry.path).collect::<Vec<_>>(),
        ["a/image", "middle", "z/卡.png"]
    );
    assert!(index.get("A/image").is_none());
    assert_eq!(index.get("z/卡.png").unwrap().length, 7);
}
