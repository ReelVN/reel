//! Seekable release archives. Only the index is resident; payloads stay in storage.
//! Content digests identify immutable entries, not signatures or access control.
use std::{
    collections::BTreeMap,
    io::{self, BufReader, Read, Seek, SeekFrom, Write},
};

mod blocks;
pub use blocks::{BLOCK_BYTES, ReadScratch, SCRATCH_BYTES};

pub const MAGIC: [u8; 8] = *b"REELPAK\0";
pub const HEADER_LEN: u64 = 32;
const VERSION: u32 = 1;
const ENTRY_FIXED_LEN: u64 = 72;

/// Physical representation of one entry's logical bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryStorage {
    /// Contiguous bytes, suitable for direct seek and range reads.
    Raw,
    /// Independent 64 KiB blocks with an on-disk directory immediately after payload.
    Blocks { directory_offset: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ArchiveKind {
    Resources = 1,
    Text = 2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Portable logical path relative to the archive's resource root.
    pub path: String,
    /// Absolute file offset of the physical payload region.
    pub offset: u64,
    /// Decoded content length, independent of physical storage.
    pub length: u64,
    /// BLAKE3 of this entry's logical bytes, computed once when packaging.
    pub version: [u8; 32],
    /// Physical payload bytes, excluding the optional block directory.
    pub stored_length: u64,
    /// Representation used to recover the logical content.
    pub storage: EntryStorage,
}

#[derive(Debug)]
pub struct ArchiveIndex {
    kind: ArchiveKind,
    entries: Vec<Entry>,
}
impl ArchiveIndex {
    pub fn kind(&self) -> ArchiveKind {
        self.kind
    }
    pub fn get(&self, path: &str) -> Option<&Entry> {
        self.entries
            .binary_search_by(|entry| entry.path.as_str().cmp(path))
            .ok()
            .map(|index| &self.entries[index])
    }
    pub fn entries(&self) -> impl ExactSizeIterator<Item = &Entry> {
        self.entries.iter()
    }
    /// Reads the header and index only. Payloads are neither loaded nor hashed.
    pub fn read(input: &mut (impl Read + Seek)) -> io::Result<Self> {
        let length = input.seek(SeekFrom::End(0))?;
        input.seek(SeekFrom::Start(0))?;
        let mut header = [0; HEADER_LEN as usize];
        input.read_exact(&mut header)?;
        if header[..8] != MAGIC || u32::from_le_bytes(header[8..12].try_into().unwrap()) != VERSION
        {
            return Err(invalid("unsupported Reel archive header"));
        }
        let kind = match u32::from_le_bytes(header[12..16].try_into().unwrap()) {
            1 => ArchiveKind::Resources,
            2 => ArchiveKind::Text,
            _ => return Err(invalid("unknown Reel archive kind")),
        };
        let index_offset = u64::from_le_bytes(header[16..24].try_into().unwrap());
        let count = u64::from_le_bytes(header[24..32].try_into().unwrap());
        if index_offset < HEADER_LEN || index_offset > length {
            return Err(invalid("archive index lies outside the file"));
        }
        let mut remaining = length - index_offset;
        if count > remaining / ENTRY_FIXED_LEN {
            return Err(invalid("archive entry count exceeds index length"));
        }
        input.seek(SeekFrom::Start(index_offset))?;
        let mut input = BufReader::with_capacity(64 << 10, input.take(remaining));
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(usize::try_from(count).map_err(io::Error::other)?)
            .map_err(io::Error::other)?;
        for _ in 0..count {
            let mut fixed = [0; ENTRY_FIXED_LEN as usize];
            input.read_exact(&mut fixed)?;
            remaining = remaining
                .checked_sub(ENTRY_FIXED_LEN)
                .ok_or_else(|| invalid("truncated archive index"))?;
            let path_len = u32::from_le_bytes(fixed[..4].try_into().unwrap()) as u64;
            remaining = remaining
                .checked_sub(path_len)
                .ok_or_else(|| invalid("archive path exceeds index length"))?;
            let mut path = Vec::new();
            let path_len = usize::try_from(path_len)
                .map_err(|_| invalid("archive path exceeds addressable memory"))?;
            path.try_reserve_exact(path_len).map_err(io::Error::other)?;
            path.resize(path_len, 0);
            input.read_exact(&mut path)?;
            let path = String::from_utf8(path).map_err(|_| invalid("archive path is not UTF-8"))?;
            validate_path(&path)?;
            let offset = u64::from_le_bytes(fixed[4..12].try_into().unwrap());
            let length = u64::from_le_bytes(fixed[12..20].try_into().unwrap());
            let stored_length = u64::from_le_bytes(fixed[52..60].try_into().unwrap());
            let directory_offset = u64::from_le_bytes(fixed[60..68].try_into().unwrap());
            let storage = match u32::from_le_bytes(fixed[68..72].try_into().unwrap()) {
                0 if stored_length == length && directory_offset == 0 => EntryStorage::Raw,
                1 if length != 0 && stored_length != 0 && stored_length <= length => {
                    let payload_end = offset.checked_add(stored_length);
                    let directory_end = blocks::directory_bytes(length)
                        .and_then(|bytes| directory_offset.checked_add(bytes));
                    if payload_end != Some(directory_offset)
                        || directory_end.is_none_or(|end| end > index_offset)
                    {
                        return Err(invalid("archive block directory lies outside payload"));
                    }
                    EntryStorage::Blocks { directory_offset }
                }
                _ => return Err(invalid("invalid archive entry storage")),
            };
            if offset < HEADER_LEN
                || offset
                    .checked_add(stored_length)
                    .is_none_or(|end| end > index_offset)
            {
                return Err(invalid(format!(
                    "archive entry lies outside payload: {path}"
                )));
            }
            let entry = Entry {
                path,
                offset,
                length,
                version: fixed[20..52].try_into().unwrap(),
                stored_length,
                storage,
            };
            entries.push(entry);
        }
        if remaining != 0 {
            return Err(invalid("unexpected bytes after archive index"));
        }
        entries.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        if entries.windows(2).any(|pair| pair[0].path == pair[1].path) {
            return Err(invalid("duplicate archive path"));
        }
        Ok(Self { kind, entries })
    }
}

/// One writer owns a new, empty output. Entries stream through fixed payload
/// scratch space and retain 16-byte block directory records during packaging.
/// Finish writes the sorted index and publishes its location in the header.
pub struct ArchiveWriter<W> {
    output: W,
    kind: ArchiveKind,
    entries: BTreeMap<String, Entry>,
    index_bytes: u64,
}
impl<W: Write + Seek> ArchiveWriter<W> {
    pub fn new(mut output: W, kind: ArchiveKind) -> io::Result<Self> {
        if output.seek(SeekFrom::End(0))? != 0 {
            return Err(invalid("archive output must be empty"));
        }
        output.write_all(&[0; HEADER_LEN as usize])?;
        Ok(Self {
            output,
            index_bytes: 0,
            kind,
            entries: BTreeMap::new(),
        })
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    /// Conservative final size for an automatically compressed entry, including
    /// its main-index record and worst-case block directory. Raw storage and
    /// compression may make the completed archive smaller.
    pub fn encoded_len_after(&mut self, path: &str, length: u64) -> io::Result<u64> {
        self.output
            .stream_position()?
            .checked_add(self.index_bytes)
            .and_then(|n| n.checked_add(ENTRY_FIXED_LEN + path.len() as u64))
            .and_then(|n| n.checked_add(length))
            .and_then(|n| n.checked_add(blocks::directory_bytes(length)?))
            .ok_or_else(|| invalid("archive exceeds format width"))
    }
    /// Streams an unknown-length input as independent 64 KiB LZ4 blocks, retaining
    /// individual blocks raw when compression does not shrink them. Entries with
    /// no compressed blocks use contiguous raw storage and omit the directory.
    pub fn add(&mut self, path: &str, input: &mut impl Read) -> io::Result<&Entry> {
        self.add_impl(path, input, true)
    }
    /// Streams an entry verbatim, bypassing compression for content whose storage
    /// encoding is already compressed. The logical content digest is unchanged.
    pub fn add_raw(&mut self, path: &str, input: &mut impl Read) -> io::Result<&Entry> {
        self.add_impl(path, input, false)
    }
    fn add_impl(
        &mut self,
        path: &str,
        input: &mut impl Read,
        compress: bool,
    ) -> io::Result<&Entry> {
        validate_path(path)?;
        u32::try_from(path.len()).map_err(|_| invalid("archive path exceeds format width"))?;
        if self.entries.contains_key(path) {
            return Err(invalid(format!("duplicate archive path: {path}")));
        }
        let offset = self.output.stream_position()?;
        let mut hash = blake3::Hasher::new();
        let (length, stored_length, storage) =
            blocks::write(input, &mut self.output, &mut hash, offset, compress)?;
        let entry = Entry {
            path: path.into(),
            offset,
            length,
            version: *hash.finalize().as_bytes(),
            stored_length,
            storage,
        };
        self.index_bytes += ENTRY_FIXED_LEN + path.len() as u64;
        self.entries.insert(path.into(), entry);
        Ok(self.entries.get(path).unwrap())
    }
    pub fn finish(mut self) -> io::Result<W> {
        let index_offset = self.output.stream_position()?;
        for entry in self.entries.values() {
            self.output
                .write_all(&(entry.path.len() as u32).to_le_bytes())?;
            self.output.write_all(&entry.offset.to_le_bytes())?;
            self.output.write_all(&entry.length.to_le_bytes())?;
            self.output.write_all(&entry.version)?;
            self.output.write_all(&entry.stored_length.to_le_bytes())?;
            let (storage, directory_offset) = match entry.storage {
                EntryStorage::Raw => (0u32, 0u64),
                EntryStorage::Blocks { directory_offset } => (1, directory_offset),
            };
            self.output.write_all(&directory_offset.to_le_bytes())?;
            self.output.write_all(&storage.to_le_bytes())?;
            self.output.write_all(entry.path.as_bytes())?;
        }
        self.output.seek(SeekFrom::Start(0))?;
        self.output.write_all(&MAGIC)?;
        self.output.write_all(&VERSION.to_le_bytes())?;
        self.output.write_all(&(self.kind as u32).to_le_bytes())?;
        self.output.write_all(&index_offset.to_le_bytes())?;
        self.output
            .write_all(&(self.entries.len() as u64).to_le_bytes())?;
        self.output.flush()?;
        self.output.seek(SeekFrom::End(0))?;
        Ok(self.output)
    }
}

/// Portable, case-sensitive logical names; never absolute filesystem paths.
pub fn validate_path(path: &str) -> io::Result<()> {
    if path.is_empty()
        || path.contains(['\\', ':', '\0'])
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(invalid(format!("invalid archive logical path: {path:?}")));
    }
    Ok(())
}
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
