use reel_package::archive::{ArchiveIndex, ArchiveKind, ArchiveWriter, HEADER_LEN};
use std::io::{self, Cursor, Read, Seek, SeekFrom};

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
        for entry in index.entries() {
            input.seek(SeekFrom::Start(entry.offset)).unwrap();
            let mut payload = vec![0; entry.length as usize];
            input.read_exact(&mut payload).unwrap();
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
    bad(bytes.clone(), index_offset + 52, b"/");
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
    let index = ArchiveIndex::read(&mut input).unwrap();
    assert_eq!(index.get("video/long.mp4").unwrap().offset, offset - 500);
    assert_eq!(input.bytes_read, bytes.len());
}
