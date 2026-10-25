use reel_package::archive::{
    ArchiveIndex, ArchiveKind, ArchiveWriter, BLOCK_BYTES, EntryStorage, HEADER_LEN, ReadScratch,
    SCRATCH_BYTES,
};
use std::{
    cell::Cell,
    io::{self, Cursor, Read, Seek, SeekFrom},
    rc::Rc,
};

fn fixture(kind: ArchiveKind) -> Vec<u8> {
    let mut writer = ArchiveWriter::new(Cursor::new(Vec::new()), kind).unwrap();
    writer
        .add("video/intro.mp4", &mut &b"video payload"[..])
        .unwrap();
    writer
        .add("locales/zh-CN/第一章.ftl", &mut &b"chapter = hello"[..])
        .unwrap();
    writer.add("empty", &mut io::empty()).unwrap();
    writer.finish().unwrap().into_inner()
}

#[test]
fn resource_and_text_archives_share_streaming_format_and_exact_content_versions() {
    for kind in [ArchiveKind::Resources, ArchiveKind::Text] {
        let bytes = fixture(kind);
        let mut input = Cursor::new(&bytes);
        let index = ArchiveIndex::read(&mut input).unwrap();
        assert_eq!(index.kind(), kind);
        assert_eq!(index.entries().len(), 3);
        assert!(index.get("Video/intro.mp4").is_none());
        let mut scratch = ReadScratch::new();
        for entry in index.entries() {
            let mut payload = vec![0; entry.length as usize];
            entry
                .read_range(&mut input, 0, &mut payload, &mut scratch, || false)
                .unwrap();
            let expected = match entry.path.as_ref() {
                "video/intro.mp4" => &b"video payload"[..],
                "locales/zh-CN/第一章.ftl" => &b"chapter = hello"[..],
                "empty" => &b""[..],
                _ => unreachable!(),
            };
            assert_eq!(payload, expected);
            assert_eq!(entry.version, *blake3::hash(&payload).as_bytes());
        }
        assert_eq!(index.get("empty").unwrap().length, 0);
        assert_eq!(bytes, fixture(kind), "same inputs produce the same archive");
    }
}

#[test]
fn malformed_index_and_nonportable_names_are_rejected_at_mount() {
    let mut writer = ArchiveWriter::new(Cursor::new(Vec::new()), ArchiveKind::Resources).unwrap();
    for name in [
        "", "/a", "../a", "a/../b", "a//b", "./a", "a/", "a\\b", "C:a", "a\0b",
    ] {
        assert!(writer.add(name, &mut io::empty()).is_err(), "{name:?}");
    }
    writer.add("a", &mut &b"one"[..]).unwrap();
    assert!(writer.add("a", &mut io::empty()).is_err());
    let bytes = writer.finish().unwrap().into_inner();
    let index_offset = u64::from_le_bytes(bytes[16..24].try_into().unwrap()) as usize;
    let bad = |mut bytes: Vec<u8>, at: usize, value: &[u8]| {
        bytes[at..at + value.len()].copy_from_slice(value);
        assert!(ArchiveIndex::read(&mut Cursor::new(bytes)).is_err());
    };
    bad(bytes.clone(), index_offset + 4, &0u64.to_le_bytes());
    bad(bytes.clone(), index_offset + 12, &u64::MAX.to_le_bytes());
    bad(bytes.clone(), index_offset, &u32::MAX.to_le_bytes());
    bad(bytes.clone(), index_offset + 72, b"/");
    bad(bytes.clone(), index_offset + 52, &2u64.to_le_bytes());
    bad(bytes.clone(), index_offset + 60, &1u64.to_le_bytes());
    bad(bytes.clone(), index_offset + 68, &2u32.to_le_bytes());
    bad(bytes.clone(), 24, &u64::MAX.to_le_bytes());
    bad(bytes.clone(), 16, &u64::MAX.to_le_bytes());
    let mut duplicate = bytes.clone();
    duplicate.extend_from_slice(&bytes[index_offset..]);
    duplicate[24..32].copy_from_slice(&2u64.to_le_bytes());
    assert!(ArchiveIndex::read(&mut Cursor::new(duplicate)).is_err());
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(ArchiveIndex::read(&mut Cursor::new(trailing)).is_err());
    assert!(ArchiveIndex::read(&mut Cursor::new(&bytes[..bytes.len() - 1])).is_err());
}

// A virtual large file makes touching the multi-gigabyte payload an immediate
// failure, without allocating or creating a large fixture on the filesystem.
struct IndexOnly {
    position: u64,
    header: Vec<u8>,
    index: Vec<u8>,
    offset: u64,
    bytes_read: usize,
}
impl Read for IndexOnly {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let source = if self.position < HEADER_LEN {
            &self.header[self.position as usize..]
        } else {
            assert!(self.position >= self.offset, "mount tried to read payload");
            &self.index[(self.position - self.offset) as usize..]
        };
        let size = out.len().min(source.len());
        out[..size].copy_from_slice(&source[..size]);
        self.position += size as u64;
        self.bytes_read += size;
        Ok(size)
    }
}
impl Seek for IndexOnly {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.position = match position {
            SeekFrom::Start(n) => n,
            SeekFrom::End(n) => {
                (i128::from(self.offset) + self.index.len() as i128 + i128::from(n))
                    .try_into()
                    .unwrap()
            }
            SeekFrom::Current(n) => (i128::from(self.position) + i128::from(n))
                .try_into()
                .unwrap(),
        };
        Ok(self.position)
    }
}
#[test]
fn mount_reads_only_index_and_preserves_offsets_beyond_four_gib() {
    let mut writer = ArchiveWriter::new(Cursor::new(Vec::new()), ArchiveKind::Resources).unwrap();
    writer.add("video/long.mp4", &mut io::empty()).unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    let offset = 1u64 << 34;
    let mut input = IndexOnly {
        position: 0,
        header: bytes[..32].to_vec(),
        index: bytes[32..].to_vec(),
        offset,
        bytes_read: 0,
    };
    input.header[16..24].copy_from_slice(&offset.to_le_bytes());
    input.index[4..12].copy_from_slice(&(offset - 500).to_le_bytes());
    input.index[12..20].copy_from_slice(&500u64.to_le_bytes());
    input.index[52..60].copy_from_slice(&500u64.to_le_bytes());
    let index = ArchiveIndex::read(&mut input).unwrap();
    assert_eq!(index.get("video/long.mp4").unwrap().offset, offset - 500);
    assert_eq!(input.bytes_read, bytes.len());
}

fn mixed_payload() -> Vec<u8> {
    let mut payload = vec![b'A'; BLOCK_BYTES];
    let mut state = 0x173b_419du32;
    for _ in 0..BLOCK_BYTES {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        payload.push(state as u8);
    }
    payload.extend(std::iter::repeat_n(b'Z', 321));
    payload
}

/// Delivers unknown-length input with both short reads and a transient interrupt.
struct ShortReader<R> {
    inner: R,
    interrupted: bool,
}
impl<R: Read> Read for ShortReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if !self.interrupted {
            self.interrupted = true;
            return Err(io::ErrorKind::Interrupted.into());
        }
        let size = output.len().min(37);
        self.inner.read(&mut output[..size])
    }
}
impl<R: Seek> Seek for ShortReader<R> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.inner.seek(from)
    }
}

fn compressed_fixture() -> (Vec<u8>, Vec<u8>) {
    let payload = mixed_payload();
    let mut writer = ArchiveWriter::new(Cursor::new(Vec::new()), ArchiveKind::Resources).unwrap();
    let upper_bound = writer
        .encoded_len_after("mixed", payload.len() as u64)
        .unwrap();
    writer
        .add(
            "mixed",
            &mut ShortReader {
                inner: payload.as_slice(),
                interrupted: false,
            },
        )
        .unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    assert!(bytes.len() as u64 <= upper_bound);
    (bytes, payload)
}

#[test]
fn independently_compressed_blocks_preserve_unknown_length_input_and_misaligned_ranges() {
    let (bytes, payload) = compressed_fixture();
    assert_eq!(
        bytes,
        compressed_fixture().0,
        "short reads preserve deterministic blocks"
    );
    let mut input = ShortReader {
        inner: Cursor::new(&bytes),
        interrupted: false,
    };
    let index = ArchiveIndex::read(&mut input).unwrap();
    let entry = index.get("mixed").unwrap();
    assert_eq!(entry.version, *blake3::hash(&payload).as_bytes());
    assert_eq!(entry.length, payload.len() as u64);
    assert!(entry.stored_length < entry.length);
    let EntryStorage::Blocks { directory_offset } = entry.storage else {
        panic!("expected compression")
    };
    let directory = directory_offset as usize;
    assert_eq!(
        u32::from_le_bytes(bytes[directory + 12..directory + 16].try_into().unwrap()),
        1
    );
    assert_eq!(
        u32::from_le_bytes(bytes[directory + 28..directory + 32].try_into().unwrap()),
        0
    );
    assert_eq!(
        u32::from_le_bytes(bytes[directory + 44..directory + 48].try_into().unwrap()),
        1
    );
    const { assert!(SCRATCH_BYTES <= 128 << 10) };
    let mut scratch = ReadScratch::new();
    for (start, length) in [
        (0, payload.len()),
        (3, 11),
        (BLOCK_BYTES - 7, 31),
        (BLOCK_BYTES + 19, 37),
        (BLOCK_BYTES * 2 - 11, 301),
        (payload.len() - 13, 13),
        (payload.len(), 0),
    ] {
        let mut result = vec![0; length];
        entry
            .read_range(&mut input, start as u64, &mut result, &mut scratch, || {
                false
            })
            .unwrap();
        assert_eq!(
            result,
            payload[start..start + length],
            "range {start}+{length}"
        );
    }
    assert_eq!(
        entry
            .read_range(&mut input, entry.length, &mut [0], &mut scratch, || false)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(
        entry
            .read_range(&mut input, u64::MAX, &mut [0], &mut scratch, || false)
            .is_err()
    );
}

#[test]
fn raw_fallback_and_explicit_bypass_preserve_direct_storage() {
    let payload = mixed_payload();
    let incompressible = &payload[BLOCK_BYTES..BLOCK_BYTES * 2];
    let mut writer = ArchiveWriter::new(Cursor::new(Vec::new()), ArchiveKind::Resources).unwrap();
    writer
        .add("incompressible", &mut &incompressible[..])
        .unwrap();
    writer.add_raw("bypass", &mut payload.as_slice()).unwrap();
    let mut input = writer.finish().unwrap();
    let index = ArchiveIndex::read(&mut input).unwrap();
    let mut scratch = ReadScratch::new();
    for (name, expected) in [
        ("incompressible", incompressible),
        ("bypass", payload.as_slice()),
    ] {
        let entry = index.get(name).unwrap();
        assert_eq!(entry.storage, EntryStorage::Raw);
        assert_eq!(entry.stored_length, expected.len() as u64);
        assert_eq!(
            &input.get_ref()[entry.offset as usize..(entry.offset + entry.length) as usize],
            expected
        );
        let mut bytes = vec![0; expected.len()];
        entry
            .read_range(&mut input, 0, &mut bytes, &mut scratch, || false)
            .unwrap();
        assert_eq!(bytes, expected);
        assert_eq!(entry.version, *blake3::hash(expected).as_bytes());
    }
}

#[test]
fn corrupt_block_metadata_and_codec_output_fail_at_the_addressed_read() {
    let (bytes, _) = compressed_fixture();
    let entry = ArchiveIndex::read(&mut Cursor::new(&bytes))
        .unwrap()
        .get("mixed")
        .unwrap()
        .clone();
    let EntryStorage::Blocks { directory_offset } = entry.storage else {
        unreachable!()
    };
    let directory = directory_offset as usize;
    let check = |at: usize, change: &[u8], logical_offset: u64| {
        let mut bad = bytes.clone();
        bad[at..at + change.len()].copy_from_slice(change);
        // Mount leaves directory and payload IO to the actual range request.
        let entry = ArchiveIndex::read(&mut Cursor::new(&bad))
            .unwrap()
            .get("mixed")
            .unwrap()
            .clone();
        let error = entry
            .read_range(
                &mut Cursor::new(bad),
                logical_offset,
                &mut [0],
                &mut ReadScratch::new(),
                || false,
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    };
    check(directory, &u64::MAX.to_le_bytes(), 0);
    check(directory + 8, &u32::MAX.to_le_bytes(), 0);
    check(directory + 12, &9u32.to_le_bytes(), 0);
    check(directory + 24, &1u32.to_le_bytes(), BLOCK_BYTES as u64);
    check(
        directory + 32,
        &0u64.to_le_bytes(),
        (BLOCK_BYTES * 2) as u64,
    );
    check(entry.offset as usize, &[0, 0, 0], 0); // invalid zero match offset

    let main = u64::from_le_bytes(bytes[16..24].try_into().unwrap()) as usize;
    let mut bad = bytes.clone();
    bad[main + 60..main + 68].copy_from_slice(&directory_offset.saturating_sub(1).to_le_bytes());
    assert!(ArchiveIndex::read(&mut Cursor::new(bad)).is_err());
    let mut truncated = bytes;
    truncated.truncate(entry.offset as usize + 1);
    assert_eq!(
        entry
            .read_range(
                &mut Cursor::new(truncated),
                0,
                &mut [0],
                &mut ReadScratch::new(),
                || false
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::UnexpectedEof
    );
}

struct CancelOnRead<R> {
    inner: R,
    at: u64,
    cancelled: Rc<Cell<bool>>,
}
impl<R: Read + Seek> Read for CancelOnRead<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.inner.stream_position()? == self.at {
            self.cancelled.set(true);
            return Err(io::ErrorKind::Interrupted.into());
        }
        self.inner.read(output)
    }
}
impl<R: Seek> Seek for CancelOnRead<R> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.inner.seek(from)
    }
}

#[test]
fn cancellation_stops_before_the_next_block_and_preserves_the_completed_prefix() {
    let (bytes, payload) = compressed_fixture();
    let index = ArchiveIndex::read(&mut Cursor::new(&bytes)).unwrap();
    let entry = index.get("mixed").unwrap();
    let EntryStorage::Blocks { directory_offset } = entry.storage else {
        unreachable!()
    };
    let cancelled = Rc::new(Cell::new(false));
    let second_payload = entry.offset
        + u64::from_le_bytes(
            bytes[directory_offset as usize + 16..directory_offset as usize + 24]
                .try_into()
                .unwrap(),
        );
    let mut input = CancelOnRead {
        inner: Cursor::new(bytes),
        at: second_payload,
        cancelled: cancelled.clone(),
    };
    let mut result = vec![0; payload.len()];
    let mut scratch = ReadScratch::new();
    let error = entry
        .read_range(&mut input, 0, &mut result, &mut scratch, || cancelled.get())
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(&result[..BLOCK_BYTES], &payload[..BLOCK_BYTES]);
    assert!(result[BLOCK_BYTES..].iter().all(|byte| *byte == 0));
    // The same workspace remains usable after a cancelled request.
    cancelled.set(false);
    entry
        .read_range(
            &mut input,
            (BLOCK_BYTES * 2) as u64,
            &mut result[..17],
            &mut scratch,
            || cancelled.get(),
        )
        .unwrap();
    assert_eq!(
        &result[..17],
        &payload[BLOCK_BYTES * 2..BLOCK_BYTES * 2 + 17]
    );
    result[0] = 0;
    assert_eq!(
        entry
            .read_range(
                &mut input,
                (BLOCK_BYTES * 2) as u64,
                &mut result[..1],
                &mut scratch,
                || true
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(
        result[0], 0,
        "a cached block still checks cancellation before copying"
    );
}

/// Maps a small fixture beyond a multi-gigabyte hole and records actual IO.
/// The hole is inaccessible, so offset truncation fails without a large file.
struct SparseArchive {
    bytes: Vec<u8>,
    position: u64,
    gap: u64,
    reads: Vec<(u64, usize)>,
    seeks: usize,
}
impl Read for SparseArchive {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let logical = if self.position < HEADER_LEN {
            self.position
        } else {
            self.position
                .checked_sub(self.gap)
                .filter(|offset| *offset >= HEADER_LEN)
                .expect("read entered inaccessible sparse hole")
        };
        let available = self.bytes.get(logical as usize..).unwrap_or_default();
        let size = output.len().min(available.len());
        output[..size].copy_from_slice(&available[..size]);
        self.reads.push((self.position, size));
        self.position += size as u64;
        Ok(size)
    }
}
impl Seek for SparseArchive {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.seeks += 1;
        self.position = match from {
            SeekFrom::Start(position) => position,
            SeekFrom::Current(delta) => (i128::from(self.position) + i128::from(delta))
                .try_into()
                .unwrap(),
            SeekFrom::End(delta) => {
                (self.bytes.len() as i128 + i128::from(self.gap) + i128::from(delta))
                    .try_into()
                    .unwrap()
            }
        };
        Ok(self.position)
    }
}

#[test]
fn compressed_sparse_mount_and_ranges_only_read_addressed_records_and_payloads() {
    let (mut bytes, payload) = compressed_fixture();
    let gap = 1u64 << 34;
    let main = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
    let index_at = main as usize;
    let original_offset =
        u64::from_le_bytes(bytes[index_at + 4..index_at + 12].try_into().unwrap());
    let original_directory =
        u64::from_le_bytes(bytes[index_at + 60..index_at + 68].try_into().unwrap());
    let first_stored = u32::from_le_bytes(
        bytes[original_directory as usize + 8..original_directory as usize + 12]
            .try_into()
            .unwrap(),
    );
    let raw_relative = u64::from_le_bytes(
        bytes[original_directory as usize + 16..original_directory as usize + 24]
            .try_into()
            .unwrap(),
    );
    bytes[16..24].copy_from_slice(&(main + gap).to_le_bytes());
    bytes[index_at + 4..index_at + 12].copy_from_slice(&(original_offset + gap).to_le_bytes());
    bytes[index_at + 60..index_at + 68].copy_from_slice(&(original_directory + gap).to_le_bytes());
    let length = bytes.len();
    let mut input = SparseArchive {
        bytes,
        position: 0,
        gap,
        reads: Vec::new(),
        seeks: 0,
    };
    let index = ArchiveIndex::read(&mut input).unwrap();
    assert_eq!(
        input.reads,
        [(0, HEADER_LEN as usize), (main + gap, length - index_at)]
    );
    let entry = index.get("mixed").unwrap();
    assert!(entry.offset > u32::MAX as u64);
    let mut scratch = ReadScratch::new();
    input.reads.clear();
    let mut result = [0; 19];
    entry
        .read_range(&mut input, 3, &mut result, &mut scratch, || false)
        .unwrap();
    assert_eq!(result, payload[3..22]);
    assert_eq!(
        input.reads,
        [
            (original_directory + gap, 16),
            (entry.offset, first_stored as usize),
        ]
    );
    input.reads.clear();
    for start in [22, 41, 60] {
        entry
            .read_range(&mut input, start, &mut result, &mut scratch, || false)
            .unwrap();
        assert_eq!(
            result,
            payload[start as usize..start as usize + result.len()]
        );
    }
    assert!(
        input.reads.is_empty(),
        "successive small requests reuse the decoded block"
    );
    let mut alias = entry.clone();
    alias.path = "same-content-under-another-name".into();
    alias
        .read_range(&mut input, 79, &mut result, &mut scratch, || false)
        .unwrap();
    assert_eq!(result, payload[79..98]);
    assert!(
        input.reads.is_empty(),
        "immutable content is shared across logical paths"
    );
    entry
        .read_range(
            &mut input,
            BLOCK_BYTES as u64 + 123,
            &mut result,
            &mut scratch,
            || false,
        )
        .unwrap();
    assert_eq!(result, payload[BLOCK_BYTES + 123..BLOCK_BYTES + 142]);
    assert_eq!(
        input.reads,
        [
            (original_directory + gap + 16, 16),
            (entry.offset + raw_relative + 123, 19),
        ]
    );
}

#[test]
fn decoded_cache_tracks_content_and_retires_identity_before_failed_decodes() {
    let (bytes, payload) = compressed_fixture();
    let entry = ArchiveIndex::read(&mut Cursor::new(&bytes))
        .unwrap()
        .get("mixed")
        .unwrap()
        .clone();
    let EntryStorage::Blocks { directory_offset } = entry.storage else {
        unreachable!()
    };
    let last_record = directory_offset as usize + 32;
    let last_payload =
        entry.offset + u64::from_le_bytes(bytes[last_record..last_record + 8].try_into().unwrap());
    let mut damaged = bytes.clone();
    // One valid literal overwrites the output, followed by an invalid zero match.
    damaged[last_payload as usize..last_payload as usize + 4].copy_from_slice(&[0x10, b'Q', 0, 0]);
    let mut other_payload = payload.clone();
    other_payload[..BLOCK_BYTES].fill(b'B');
    let mut writer = ArchiveWriter::new(Cursor::new(Vec::new()), ArchiveKind::Resources).unwrap();
    writer.add("other", &mut other_payload.as_slice()).unwrap();
    let other_bytes = writer.finish().unwrap().into_inner();
    let other_entry = ArchiveIndex::read(&mut Cursor::new(&other_bytes))
        .unwrap()
        .get("other")
        .unwrap()
        .clone();
    assert_ne!(entry.version, other_entry.version);
    assert_eq!(entry.length, other_entry.length);
    let mut input = SparseArchive {
        bytes,
        position: 0,
        gap: 0,
        reads: Vec::new(),
        seeks: 0,
    };
    let mut other_input = SparseArchive {
        bytes: other_bytes,
        position: 0,
        gap: 0,
        reads: Vec::new(),
        seeks: 0,
    };
    let mut damaged_input = SparseArchive {
        bytes: damaged,
        position: 0,
        gap: 0,
        reads: Vec::new(),
        seeks: 0,
    };
    let mut scratch = ReadScratch::new();
    let mut result = [0];
    entry
        .read_range(&mut input, 0, &mut result, &mut scratch, || false)
        .unwrap();
    assert_eq!(result, [b'A']);
    other_entry
        .read_range(&mut other_input, 0, &mut result, &mut scratch, || false)
        .unwrap();
    assert_eq!(result, [b'B']);
    input.reads.clear();
    entry
        .read_range(&mut input, 0, &mut result, &mut scratch, || false)
        .unwrap();
    assert_eq!(result, [b'A']);
    assert_eq!(input.reads.len(), 2, "different content replaces the cache");

    for _ in 0..2 {
        assert_eq!(
            entry
                .read_range(
                    &mut damaged_input,
                    (BLOCK_BYTES * 2) as u64,
                    &mut result,
                    &mut scratch,
                    || false
                )
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        input.reads.clear();
        entry
            .read_range(&mut input, 0, &mut result, &mut scratch, || false)
            .unwrap();
        assert_eq!(
            result,
            [b'A'],
            "partially overwritten failed decode cannot reuse the old identity"
        );
        assert_eq!(input.reads.len(), 2);
    }
    let mut wrong_length = entry.clone();
    wrong_length.length += 1;
    assert_eq!(
        wrong_length
            .read_range(
                &mut input,
                (BLOCK_BYTES * 2) as u64,
                &mut result,
                &mut scratch,
                || false
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    input.reads.clear();
    entry
        .read_range(&mut input, 0, &mut result, &mut scratch, || false)
        .unwrap();
    assert_eq!(result, [b'A']);
    assert_eq!(
        input.reads.len(),
        2,
        "decoded length mismatch also retires the previous identity"
    );
}

#[test]
fn sequential_blocks_batch_directory_reads_and_preserve_small_range_cache() {
    let mut payload = Vec::with_capacity(BLOCK_BYTES * 258 + 17);
    for block in 0..258 {
        payload.extend(std::iter::repeat_n((block % 251) as u8, BLOCK_BYTES));
    }
    payload.extend_from_slice(&[42; 17]);
    let mut writer = ArchiveWriter::new(Cursor::new(Vec::new()), ArchiveKind::Resources).unwrap();
    writer.add("large", &mut payload.as_slice()).unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    let entry = ArchiveIndex::read(&mut Cursor::new(&bytes))
        .unwrap()
        .get("large")
        .unwrap()
        .clone();
    let mut input = SparseArchive {
        bytes,
        position: 0,
        gap: 0,
        reads: Vec::new(),
        seeks: 0,
    };
    let mut result = vec![0; payload.len()];
    let mut scratch = ReadScratch::new();
    entry
        .read_range(&mut input, 0, &mut result, &mut scratch, || false)
        .unwrap();
    assert_eq!(result, payload);
    assert_eq!(
        input.seeks, 4,
        "two directory batches and two contiguous payload runs"
    );
    assert_eq!(
        input.reads.len(),
        261,
        "259 payload blocks and two directory reads"
    );
    assert_eq!(input.reads[0].1, 4096);
    assert_eq!(input.reads[257].1, 3 * 16);

    // A small request owns its cached decoded block. Complete block decodes write
    // into the caller's buffer and preserve those still-valid cached bytes.
    let mut tiny = [0; 19];
    entry
        .read_range(&mut input, 3, &mut tiny, &mut scratch, || false)
        .unwrap();
    entry
        .read_range(
            &mut input,
            BLOCK_BYTES as u64,
            &mut result[BLOCK_BYTES..],
            &mut scratch,
            || false,
        )
        .unwrap();
    input.reads.clear();
    entry
        .read_range(&mut input, 22, &mut tiny, &mut scratch, || false)
        .unwrap();
    assert_eq!(tiny, payload[22..41]);
    assert!(
        input.reads.is_empty(),
        "complete reads leave the small-range cache usable"
    );
}
