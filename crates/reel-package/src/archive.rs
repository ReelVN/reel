//! Seekable release archives. Only the index is resident; payloads stay in storage.
//! Content digests identify immutable entries, not signatures or access control.
use std::{
    collections::BTreeMap,
    io::{self, BufReader, Read, Seek, SeekFrom, Write},
    ops::Range,
};

mod blocks;
pub use blocks::{BLOCK_BYTES, ReadCursor, ReadScratch, SCRATCH_BYTES};

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

/// Borrowed immutable descriptor. Its path belongs to the index or writer that
/// returned it; copying the view does not extend that owner's storage lifetime.
/// ```compile_fail,E0515
/// use reel_package::archive::{ArchiveIndex, Entry};
/// fn escape(index: ArchiveIndex) -> Option<Entry<'static>> {
///     index.get("image.png")
/// }
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    /// Immutable portable logical path relative to the archive's resource root.
    pub path: &'a str,
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

#[derive(Clone, Copy, Debug)]
struct EntryData {
    offset: u64,
    length: u64,
    version: [u8; 32],
    stored_length: u64,
    // Valid compressed directories follow a nonempty payload at offset >=32.
    // Zero therefore encodes Raw without retaining another enum discriminant.
    directory_offset: u64,
}
impl EntryData {
    #[inline]
    fn view<'a>(&self, path: &'a str) -> Entry<'a> {
        Entry {
            path,
            offset: self.offset,
            length: self.length,
            version: self.version,
            stored_length: self.stored_length,
            storage: if self.directory_offset == 0 {
                EntryStorage::Raw
            } else {
                EntryStorage::Blocks {
                    directory_offset: self.directory_offset,
                }
            },
        }
    }
}
#[derive(Debug)]
struct IndexRecord {
    path: Range<usize>,
    data: EntryData,
}

#[derive(Debug)]
pub struct ArchiveIndex {
    kind: ArchiveKind,
    entries: Vec<IndexRecord>,
    paths: String,
}
impl ArchiveIndex {
    pub fn kind(&self) -> ArchiveKind {
        self.kind
    }
    /// Find a logical path without allocating or copying path storage.
    #[inline]
    pub fn get(&self, path: &str) -> Option<Entry<'_>> {
        let paths = self.paths.as_bytes();
        self.entries
            .binary_search_by(|entry| paths[entry.path.clone()].cmp(path.as_bytes()))
            .ok()
            .map(|index| self.view(&self.entries[index]))
    }
    /// Iterate sorted borrowed views. The index retains every path allocation.
    #[inline]
    pub fn entries(&self) -> impl ExactSizeIterator<Item = Entry<'_>> {
        self.entries.iter().map(|entry| self.view(entry))
    }
    #[inline]
    fn view(&self, entry: &IndexRecord) -> Entry<'_> {
        entry.data.view(&self.paths[entry.path.clone()])
    }
    /// Heap payload bytes retained by record and path capacities. Inline owner
    /// metadata and the caller's collection of index owners are excluded.
    pub fn capacity_bytes(&self) -> u64 {
        (self.entries.capacity() * size_of::<IndexRecord>() + self.paths.capacity()) as u64
    }
    /// Reads the header and index only. Payloads are neither loaded nor hashed.
    /// Runtime owners use [`Self::read_with_admission`] to retain a capacity reservation.
    pub fn read(input: &mut (impl Read + Seek)) -> io::Result<Self> {
        Self::read_with_admission(input, |_| Ok(()))
    }
    /// Read through the same bounded parser, admitting heap capacity before
    /// allocation. The callback receives this index's total live requirement,
    /// including its temporary parsing buffer. On success the final call reports
    /// [`Self::capacity_bytes`]. The caller retains its reservation until the
    /// returned owner drops and releases it on any read/admission failure.
    pub fn read_with_admission(
        input: &mut (impl Read + Seek),
        mut admit: impl FnMut(u64) -> io::Result<()>,
    ) -> io::Result<Self> {
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
        let path_bytes = usize::try_from(remaining - count * ENTRY_FIXED_LEN)
            .map_err(|_| invalid("archive paths exceed addressable memory"))?;
        let count = usize::try_from(count)
            .map_err(|_| invalid("archive index exceeds addressable memory"))?;
        let record_bytes = count
            .checked_mul(size_of::<IndexRecord>())
            .filter(|bytes| *bytes <= isize::MAX as usize)
            .ok_or_else(|| invalid("archive index exceeds addressable memory"))?;
        if path_bytes > isize::MAX as usize {
            return Err(invalid("archive paths exceed addressable memory"));
        }
        let parsing_bytes = remaining.min(64 << 10);
        let capacity = (record_bytes as u64)
            .checked_add(path_bytes as u64)
            .ok_or_else(|| invalid("archive index capacity overflow"))?;
        admit(
            capacity
                .checked_add(parsing_bytes)
                .ok_or_else(|| invalid("archive index capacity overflow"))?,
        )?;
        input.seek(SeekFrom::Start(index_offset))?;
        let mut input = BufReader::with_capacity(parsing_bytes as usize, input.take(remaining));
        let mut entries = Vec::new();
        entries.try_reserve_exact(count).map_err(io::Error::other)?;
        let mut paths = Vec::new();
        paths
            .try_reserve_exact(path_bytes)
            .map_err(io::Error::other)?;
        paths.resize(path_bytes, 0);
        let mut path_end = 0_usize;
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
            let path_len = usize::try_from(path_len)
                .map_err(|_| invalid("archive path exceeds addressable memory"))?;
            let start = path_end;
            path_end = path_end
                .checked_add(path_len)
                .filter(|end| *end <= paths.len())
                .ok_or_else(|| invalid("archive path exceeds index length"))?;
            input.read_exact(&mut paths[start..path_end])?;
            let path = std::str::from_utf8(&paths[start..path_end])
                .map_err(|_| invalid("archive path is not UTF-8"))?;
            validate_path(path)?;
            let offset = u64::from_le_bytes(fixed[4..12].try_into().unwrap());
            let length = u64::from_le_bytes(fixed[12..20].try_into().unwrap());
            let stored_length = u64::from_le_bytes(fixed[52..60].try_into().unwrap());
            let directory_offset = u64::from_le_bytes(fixed[60..68].try_into().unwrap());
            match u32::from_le_bytes(fixed[68..72].try_into().unwrap()) {
                0 if stored_length == length && directory_offset == 0 => {}
                1 if length != 0 && stored_length != 0 && stored_length <= length => {
                    let payload_end = offset.checked_add(stored_length);
                    let directory_end = blocks::directory_bytes(length)
                        .and_then(|bytes| directory_offset.checked_add(bytes));
                    if payload_end != Some(directory_offset)
                        || directory_end.is_none_or(|end| end > index_offset)
                    {
                        return Err(invalid("archive block directory lies outside payload"));
                    }
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
            entries.push(IndexRecord {
                path: start..path_end,
                data: EntryData {
                    offset,
                    length,
                    version: fixed[20..52].try_into().unwrap(),
                    stored_length,
                    directory_offset,
                },
            });
        }
        if remaining != 0 {
            return Err(invalid("unexpected bytes after archive index"));
        }
        let paths = String::from_utf8(paths).map_err(|_| invalid("archive path is not UTF-8"))?;
        // UTF-8 string ordering is byte ordering. Internal comparisons do not
        // need to recheck the already-validated character boundaries per probe.
        let encoded = paths.as_bytes();
        entries.sort_unstable_by(|a, b| encoded[a.path.clone()].cmp(&encoded[b.path.clone()]));
        if entries
            .windows(2)
            .any(|pair| encoded[pair[0].path.clone()] == encoded[pair[1].path.clone()])
        {
            return Err(invalid("duplicate archive path"));
        }
        drop(input);
        let index = Self {
            kind,
            entries,
            paths,
        };
        admit(index.capacity_bytes())?;
        Ok(index)
    }
}

/// One writer owns a new, empty output. Entries stream through fixed payload
/// scratch space and retain 16-byte block directory records during packaging.
/// Finish writes the sorted index and publishes its location in the header.
pub struct ArchiveWriter<W> {
    output: W,
    kind: ArchiveKind,
    entries: BTreeMap<String, EntryData>,
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
    /// The returned view borrows the writer's existing path key.
    pub fn add(&mut self, path: &str, input: &mut impl Read) -> io::Result<Entry<'_>> {
        self.add_impl(path, input, true)
    }
    /// Streams an entry verbatim, bypassing compression for content whose storage
    /// encoding is already compressed. The logical content digest is unchanged.
    /// The returned view borrows the writer's existing path key.
    pub fn add_raw(&mut self, path: &str, input: &mut impl Read) -> io::Result<Entry<'_>> {
        self.add_impl(path, input, false)
    }
    fn add_impl(
        &mut self,
        path: &str,
        input: &mut impl Read,
        compress: bool,
    ) -> io::Result<Entry<'_>> {
        validate_path(path)?;
        u32::try_from(path.len()).map_err(|_| invalid("archive path exceeds format width"))?;
        if self.entries.contains_key(path) {
            return Err(invalid(format!("duplicate archive path: {path}")));
        }
        let offset = self.output.stream_position()?;
        let mut hash = blake3::Hasher::new();
        let (length, stored_length, storage) =
            blocks::write(input, &mut self.output, &mut hash, offset, compress)?;
        let entry = EntryData {
            offset,
            length,
            version: *hash.finalize().as_bytes(),
            stored_length,
            directory_offset: match storage {
                EntryStorage::Raw => 0,
                EntryStorage::Blocks { directory_offset } => directory_offset,
            },
        };
        self.index_bytes += ENTRY_FIXED_LEN + path.len() as u64;
        self.entries.insert(path.into(), entry);
        let (path, entry) = self.entries.get_key_value(path).unwrap();
        Ok(entry.view(path))
    }
    pub fn finish(mut self) -> io::Result<W> {
        let index_offset = self.output.stream_position()?;
        for (path, entry) in &self.entries {
            self.output.write_all(&(path.len() as u32).to_le_bytes())?;
            self.output.write_all(&entry.offset.to_le_bytes())?;
            self.output.write_all(&entry.length.to_le_bytes())?;
            self.output.write_all(&entry.version)?;
            self.output.write_all(&entry.stored_length.to_le_bytes())?;
            let storage = u32::from(entry.directory_offset != 0);
            self.output
                .write_all(&entry.directory_offset.to_le_bytes())?;
            self.output.write_all(&storage.to_le_bytes())?;
            self.output.write_all(path.as_bytes())?;
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
