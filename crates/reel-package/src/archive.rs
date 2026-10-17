//! Seekable release archives. Only the index is resident; payloads stay in storage.
//! Content digests identify immutable entries, not signatures or access control.
use std::{
    collections::BTreeMap,
    io::{self, BufReader, Read, Seek, SeekFrom, Write},
};

pub const MAGIC: [u8; 8] = *b"REELPAK\0";
pub const HEADER_LEN: u64 = 32;
const VERSION: u32 = 1;
const ENTRY_FIXED_LEN: u64 = 52;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ArchiveKind {
    Resources = 1,
    Text = 2,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub offset: u64,
    pub length: u64,
    /// BLAKE3 of this entry's raw payload, computed once when packaging.
    pub version: [u8; 32],
}

#[derive(Debug)]
pub struct ArchiveIndex {
    kind: ArchiveKind,
    entries: BTreeMap<String, Entry>,
}
impl ArchiveIndex {
    pub fn kind(&self) -> ArchiveKind {
        self.kind
    }
    pub fn get(&self, path: &str) -> Option<&Entry> {
        self.entries.get(path)
    }
    pub fn entries(&self) -> impl ExactSizeIterator<Item = &Entry> {
        self.entries.values()
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
        let mut input = BufReader::new(input.take(remaining));
        let mut entries = BTreeMap::new();
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
            if offset < HEADER_LEN
                || offset
                    .checked_add(length)
                    .is_none_or(|end| end > index_offset)
            {
                return Err(invalid(format!(
                    "archive entry lies outside payload: {path}"
                )));
            }
            let entry = Entry {
                path: path.clone(),
                offset,
                length,
                version: fixed[20..52].try_into().unwrap(),
            };
            if entries.insert(path, entry).is_some() {
                return Err(invalid("duplicate archive path"));
            }
        }
        if remaining != 0 {
            return Err(invalid("unexpected bytes after archive index"));
        }
        Ok(Self { kind, entries })
    }
}

/// One writer owns a new, empty output. Entries stream through fixed scratch space;
/// finish writes the sorted index and publishes its location in the header.
pub struct ArchiveWriter<W> {
    output: W,
    index: ArchiveIndex,
}
impl<W: Write + Seek> ArchiveWriter<W> {
    pub fn new(mut output: W, kind: ArchiveKind) -> io::Result<Self> {
        if output.seek(SeekFrom::End(0))? != 0 {
            return Err(invalid("archive output must be empty"));
        }
        output.write_all(&[0; HEADER_LEN as usize])?;
        Ok(Self {
            output,
            index: ArchiveIndex {
                kind,
                entries: BTreeMap::new(),
            },
        })
    }
    pub fn add(&mut self, path: &str, input: &mut impl Read) -> io::Result<&Entry> {
        validate_path(path)?;
        u32::try_from(path.len()).map_err(|_| invalid("archive path exceeds format width"))?;
        if self.index.entries.contains_key(path) {
            return Err(invalid(format!("duplicate archive path: {path}")));
        }
        let offset = self.output.stream_position()?;
        let mut hash = blake3::Hasher::new();
        let mut length = 0u64;
        let mut scratch = [0; 64 << 10];
        loop {
            let size = match input.read(&mut scratch) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if size == 0 {
                break;
            }
            length = length
                .checked_add(size as u64)
                .ok_or_else(|| invalid("archive entry exceeds format width"))?;
            self.output.write_all(&scratch[..size])?;
            hash.update(&scratch[..size]);
        }
        let entry = Entry {
            path: path.into(),
            offset,
            length,
            version: *hash.finalize().as_bytes(),
        };
        self.index.entries.insert(path.into(), entry);
        Ok(self.index.entries.get(path).unwrap())
    }
    pub fn finish(mut self) -> io::Result<W> {
        let index_offset = self.output.stream_position()?;
        for entry in self.index.entries.values() {
            self.output
                .write_all(&(entry.path.len() as u32).to_le_bytes())?;
            self.output.write_all(&entry.offset.to_le_bytes())?;
            self.output.write_all(&entry.length.to_le_bytes())?;
            self.output.write_all(&entry.version)?;
            self.output.write_all(entry.path.as_bytes())?;
        }
        self.output.seek(SeekFrom::Start(0))?;
        self.output.write_all(&MAGIC)?;
        self.output.write_all(&VERSION.to_le_bytes())?;
        self.output
            .write_all(&(self.index.kind as u32).to_le_bytes())?;
        self.output.write_all(&index_offset.to_le_bytes())?;
        self.output
            .write_all(&(self.index.entries.len() as u64).to_le_bytes())?;
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
