use reel_package::archive::{
    ArchiveIndex, ArchiveKind, ArchiveWriter, BLOCK_BYTES, Entry, EntryStorage, ReadScratch,
};
use std::{
    fs::File,
    io::{self, Cursor, Read, Seek, SeekFrom},
    time::Instant,
};

struct Counted<R> {
    inner: R,
    position: u64,
    reads: usize,
    seeks: usize,
    bytes: u64,
    directory: std::ops::Range<u64>,
    directories: usize,
}
impl<R> Counted<R> {
    fn new(inner: R, entry: &Entry<'_>) -> Self {
        let directory = match entry.storage {
            EntryStorage::Raw => 0..0,
            EntryStorage::Blocks { directory_offset } => {
                directory_offset..directory_offset + entry.length.div_ceil(BLOCK_BYTES as u64) * 16
            }
        };
        Self {
            inner,
            position: 0,
            reads: 0,
            seeks: 0,
            bytes: 0,
            directory,
            directories: 0,
        }
    }
}
impl<R: Read> Read for Counted<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(output)?;
        self.reads += 1;
        self.directories += usize::from(self.directory.contains(&self.position));
        self.position += count as u64;
        self.bytes += count as u64;
        Ok(count)
    }
}
impl<R: Seek> Seek for Counted<R> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.seeks += 1;
        self.position = self.inner.seek(from)?;
        Ok(self.position)
    }
}

#[test]
fn resumable_directory_windows_survive_interleaving_and_validate_cursor_identity() {
    let payload: Vec<u8> = (0..258 * BLOCK_BYTES + 17)
        .map(|i| (i / BLOCK_BYTES % 251) as u8)
        .collect();
    let mut writer = ArchiveWriter::new(Cursor::new(Vec::new()), ArchiveKind::Resources).unwrap();
    writer.add("large", &mut payload.as_slice()).unwrap();
    writer.add("media", &mut &b"music"[..]).unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    let index = ArchiveIndex::read(&mut Cursor::new(&bytes)).unwrap();
    let entry = index.get("large").unwrap();
    let media = index.get("media").unwrap();
    let mut input = Counted::new(Cursor::new(&bytes), &entry);
    let mut scratch = ReadScratch::new();
    let mut cursor = entry.read_cursor(0, payload.len()).unwrap();
    let mut result = vec![0; payload.len()];
    for (block, output) in result.chunks_mut(BLOCK_BYTES).enumerate() {
        entry
            .read_range_with_cursor(
                &mut input,
                (block * BLOCK_BYTES) as u64,
                output,
                &mut scratch,
                &mut cursor,
                || false,
            )
            .unwrap();
        // A different reader changes the shared file offset between every block.
        let mut sound = [0; 5];
        media
            .read_range(&mut input, 0, &mut sound, &mut scratch, || false)
            .unwrap();
        assert_eq!(&sound, b"music");
    }
    assert_eq!(result, payload);
    assert_eq!(
        input.directories, 2,
        "259 quanta preserve the original two directory windows"
    );
    let mut wrong = entry;
    wrong.length += 1;
    assert!(
        wrong
            .read_range_with_cursor(
                &mut input,
                0,
                &mut [0; 1],
                &mut scratch,
                &mut cursor,
                || false
            )
            .is_err()
    );
    assert!(
        entry
            .read_range_with_cursor(
                &mut input,
                0,
                &mut [0; 1],
                &mut scratch,
                &mut cursor,
                || true
            )
            .is_err()
    );
    let mut small = entry.read_cursor(31, 100).unwrap();
    assert!(
        entry
            .read_range_with_cursor(
                &mut input,
                30,
                &mut [0; 1],
                &mut scratch,
                &mut small,
                || false
            )
            .is_err()
    );
    entry
        .read_range_with_cursor(
            &mut input,
            31,
            &mut result[..100],
            &mut scratch,
            &mut small,
            || false,
        )
        .unwrap();
    assert_eq!(result[..100], payload[31..131]);
}

#[test]
#[ignore = "reads one selected existing RPK entry; set REEL_RPK_PATH and optionally REEL_RPK_ENTRY"]
fn real_rpk_cursor_reports_io_and_throughput_without_repacking() {
    let path = std::env::var("REEL_RPK_PATH")
        .expect("set REEL_RPK_PATH to an existing game resource archive");
    let index = ArchiveIndex::read(&mut File::open(&path).unwrap()).unwrap();
    let selected = std::env::var("REEL_RPK_ENTRY").ok();
    let entry = selected
        .as_deref()
        .map(|name| {
            if name == "@media" {
                index
                    .entries()
                    .filter(|entry| {
                        [
                            ".mp4", ".ogg", ".opus", ".wav", ".mp3", ".flac", ".webm", ".m4a",
                        ]
                        .iter()
                        .any(|extension| entry.path.ends_with(extension))
                    })
                    .max_by_key(|entry| entry.length)
                    .expect("archive has media")
            } else {
                index.get(name).unwrap()
            }
        })
        .unwrap_or_else(|| {
            index
                .entries()
                .filter(|entry| entry.path.ends_with(".png"))
                .max_by_key(|entry| entry.length)
                .unwrap()
        });
    let mut output = vec![0; entry.length as usize];
    for (continued, read_ahead) in [(false, false), (false, true), (true, false), (true, true)] {
        let mut durations = Vec::new();
        let mut stats = None;
        for _ in 0..5 {
            let mut reader = Counted::new(File::open(&path).unwrap(), &entry);
            let mut scratch = ReadScratch::new();
            let mut cursor = entry.read_cursor(0, output.len()).unwrap();
            cursor.set_read_ahead(read_ahead);
            let start = Instant::now();
            if continued {
                for (block, slice) in output.chunks_mut(BLOCK_BYTES).enumerate() {
                    entry
                        .read_range_with_cursor(
                            &mut reader,
                            (block * BLOCK_BYTES) as u64,
                            slice,
                            &mut scratch,
                            &mut cursor,
                            || false,
                        )
                        .unwrap();
                }
            } else {
                for (block, slice) in output.chunks_mut(BLOCK_BYTES).enumerate() {
                    let offset = (block * BLOCK_BYTES) as u64;
                    let mut cursor = entry.read_cursor(offset, slice.len()).unwrap();
                    cursor.set_read_ahead(read_ahead);
                    entry
                        .read_range_with_cursor(
                            &mut reader,
                            offset,
                            slice,
                            &mut scratch,
                            &mut cursor,
                            || false,
                        )
                        .unwrap();
                }
            }
            durations.push(start.elapsed().as_micros());
            assert_eq!(blake3::hash(&output).as_bytes(), &entry.version);
            let value = (reader.reads, reader.directories, reader.seeks, reader.bytes);
            if let Some(before) = stats {
                assert_eq!(before, value);
            } else {
                stats = Some(value);
            }
        }
        durations.sort_unstable();
        eprintln!(
            "real RPK path={path:?} entry={:?} logical={} stored={} continued={continued} read_ahead={read_ahead} median_us={} reads,directory_reads,seeks,bytes={:?} cursor_bytes={}",
            entry.path,
            entry.length,
            entry.stored_length,
            durations[2],
            stats.unwrap(),
            size_of::<reel_package::archive::ReadCursor>()
        );
    }
}

#[test]
fn adjacent_payload_windows_reduce_physical_reads_for_independent_media_requests() {
    let payload: Vec<u8> = (0..16 * BLOCK_BYTES)
        .map(|i| (i / BLOCK_BYTES) as u8)
        .collect();
    let mut writer = ArchiveWriter::new(Cursor::new(Vec::new()), ArchiveKind::Resources).unwrap();
    writer.add("stream", &mut payload.as_slice()).unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    let index = ArchiveIndex::read(&mut Cursor::new(&bytes)).unwrap();
    let entry = index.get("stream").unwrap();
    let mut results = Vec::new();
    for read_ahead in [false, true] {
        let mut reader = Counted::new(Cursor::new(&bytes), &entry);
        let mut scratch = ReadScratch::new();
        let mut result = vec![0; payload.len()];
        for (block, output) in result.chunks_mut(BLOCK_BYTES).enumerate() {
            let offset = (block * BLOCK_BYTES) as u64;
            let mut cursor = entry.read_cursor(offset, output.len()).unwrap();
            cursor.set_read_ahead(read_ahead);
            entry
                .read_range_with_cursor(
                    &mut reader,
                    offset,
                    output,
                    &mut scratch,
                    &mut cursor,
                    || false,
                )
                .unwrap();
        }
        assert_eq!(result, payload);
        // Cancellation wins even when the requested bytes are already cached.
        let before = reader.reads;
        assert_eq!(
            entry
                .read_range(&mut reader, 0, &mut [0; 1], &mut scratch, || true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(reader.reads, before);
        results.push((reader.reads, reader.directories, reader.bytes));
    }
    assert_eq!(results[0].0, 32);
    assert_eq!(results[1].0, 2);
    assert_eq!(results[0].1, 16);
    assert_eq!(results[1].1, 1);
    println!(
        "16 independent 64KiB media reads: no-ahead={:?}, merged={:?}; scratch={}B",
        results[0],
        results[1],
        reel_package::archive::SCRATCH_BYTES
    );
}
