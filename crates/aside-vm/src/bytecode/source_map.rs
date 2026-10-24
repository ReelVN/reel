use super::{Error, ErrorKind, Pc, SourceId, SourceMapEntry, SourceSpan};

const RUNS_PER_BLOCK: usize = 32;

/// Source runs use ordinal PCs while editing and execution-word PCs at runtime.
pub trait SourceMapSequence {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn entries(&self) -> impl ExactSizeIterator<Item = SourceMapEntry>;
    fn source_at(&self, pc: Pc) -> Option<SourceSpan>;
    /// Allocation owned by the sequence, excluding its inline handle.
    fn capacity_bytes(&self) -> usize;
    /// Canonical packed runs, when already stored in execution form.
    fn packed(&self) -> Option<&[u8]> {
        None
    }
    /// Work bound for one location lookup, including its short delta block.
    fn lookup_work(&self) -> u64 {
        2 + self.len().checked_ilog2().unwrap_or(0) as u64
    }
}

impl SourceMapSequence for Vec<SourceMapEntry> {
    fn len(&self) -> usize {
        Vec::len(self)
    }
    fn entries(&self) -> impl ExactSizeIterator<Item = SourceMapEntry> {
        self.as_slice().iter().cloned()
    }
    fn source_at(&self, pc: Pc) -> Option<SourceSpan> {
        self.partition_point(|entry| entry.pc <= pc)
            .checked_sub(1)
            .map(|i| self[i].span)
    }
    fn capacity_bytes(&self) -> usize {
        self.capacity() * size_of::<SourceMapEntry>()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Checkpoint {
    pc: u32,
    offset: u32,
}

/// Immutable delta-coded source runs. Location lookup uses a sparse PC index
/// and decodes at most 32 runs without allocation. Wire loading retains these
/// bytes directly instead of expanding one record per instruction location.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompactSourceMap {
    bytes: Box<[u8]>,
    checkpoints: Box<[Checkpoint]>,
    len: u32,
}

impl CompactSourceMap {
    pub(crate) fn from_entries(entries: &[SourceMapEntry]) -> Result<Self, Error> {
        let size = Self::encoded_size(entries.iter().cloned())?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(size)
            .map_err(|_| Error::new(ErrorKind::Allocation, "packed source map"))?;
        Self::encode(entries.iter().cloned(), |part| {
            bytes.extend_from_slice(part);
            Ok(())
        })?;
        Self::from_owned(
            bytes,
            u32::try_from(entries.len()).map_err(|_| error("source map count"))?,
        )
    }
    pub(crate) fn encoded_size(
        entries: impl ExactSizeIterator<Item = SourceMapEntry>,
    ) -> Result<usize, Error> {
        let mut size = 0usize;
        Self::encode(entries, |bytes| {
            size = size
                .checked_add(bytes.len())
                .ok_or_else(|| error("source map size"))?;
            Ok(())
        })?;
        Ok(size)
    }
    pub(crate) fn planned_capacity(count: usize, bytes: usize) -> Result<usize, Error> {
        count
            .div_ceil(RUNS_PER_BLOCK)
            .checked_mul(size_of::<Checkpoint>())
            .and_then(|n| n.checked_add(bytes))
            .ok_or_else(|| error("source map capacity"))
    }

    pub(crate) fn from_encoded(bytes: &[u8], count: u32) -> Result<Self, Error> {
        let mut owned = Vec::new();
        owned
            .try_reserve_exact(bytes.len())
            .map_err(|_| Error::new(ErrorKind::Allocation, "packed source map"))?;
        owned.extend_from_slice(bytes);
        Self::from_owned(owned, count)
    }
    fn from_owned(bytes: Vec<u8>, count: u32) -> Result<Self, Error> {
        let mut checkpoints = Vec::new();
        checkpoints
            .try_reserve_exact((count as usize).div_ceil(RUNS_PER_BLOCK))
            .map_err(|_| Error::new(ErrorKind::Allocation, "source map checkpoints"))?;
        scan(&bytes, count, |pc, offset| {
            checkpoints.push(Checkpoint { pc, offset });
        })?;
        Ok(Self {
            bytes: bytes.into_boxed_slice(),
            checkpoints: checkpoints.into_boxed_slice(),
            len: count,
        })
    }

    pub(crate) fn scan(bytes: &[u8], count: u32) -> Result<(), Error> {
        scan(bytes, count, |_, _| {})
    }

    pub(crate) fn encode(
        entries: impl ExactSizeIterator<Item = SourceMapEntry>,
        write: impl FnMut(&[u8]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        Self::encode_results(entries.map(Ok), write)
    }

    pub(crate) fn encode_results(
        entries: impl ExactSizeIterator<Item = Result<SourceMapEntry, Error>>,
        mut write: impl FnMut(&[u8]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let mut previous = empty();
        for (i, entry) in entries.enumerate() {
            let entry = entry?;
            if i % RUNS_PER_BLOCK == 0 {
                previous = empty();
            }
            // Signed differences preserve arbitrary editable u32 fields. The
            // semantic verifier diagnoses invalid spans and non-increasing PCs.
            for delta in [
                entry.pc.0 as i64 - previous.pc.0 as i64,
                entry.span.file.0 as i64 - previous.span.file.0 as i64,
                entry.span.start as i64 - previous.span.start as i64,
                entry.span.end as i64 - entry.span.start as i64,
            ] {
                let mut value = ((delta << 1) ^ (delta >> 63)) as u64;
                let mut bytes = [0; 5];
                let mut len = 0;
                loop {
                    bytes[len] = value as u8 & 0x7f;
                    value >>= 7;
                    len += 1;
                    if value == 0 {
                        break;
                    }
                    bytes[len - 1] |= 0x80;
                }
                write(&bytes[..len])?;
            }
            previous = entry;
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn iter(&self) -> SourceMapIter<'_> {
        SourceMapIter {
            bytes: &self.bytes,
            offset: 0,
            index: 0,
            remaining: self.len,
            previous: empty(),
        }
    }
    /// Returns the run active at `pc`; PCs before the first run have no source.
    pub fn source_at(&self, pc: Pc) -> Option<SourceSpan> {
        let block = self.checkpoints.partition_point(|entry| entry.pc <= pc.0);
        let block = block.checked_sub(1)?;
        let mut iter = self.iter();
        iter.offset = self.checkpoints[block].offset as usize;
        iter.remaining = (self.len as usize - block * RUNS_PER_BLOCK).min(RUNS_PER_BLOCK) as u32;
        let mut found = None;
        for entry in iter {
            if entry.pc > pc {
                break;
            }
            found = Some(entry.span);
        }
        found
    }
    pub fn capacity_bytes(&self) -> usize {
        self.bytes.len() + self.checkpoints.len() * size_of::<Checkpoint>()
    }
}

impl SourceMapSequence for CompactSourceMap {
    fn len(&self) -> usize {
        self.len()
    }
    fn entries(&self) -> impl ExactSizeIterator<Item = SourceMapEntry> {
        self.iter()
    }
    fn source_at(&self, pc: Pc) -> Option<SourceSpan> {
        self.source_at(pc)
    }
    fn capacity_bytes(&self) -> usize {
        self.capacity_bytes()
    }
    fn packed(&self) -> Option<&[u8]> {
        Some(&self.bytes)
    }
    fn lookup_work(&self) -> u64 {
        2 + self.checkpoints.len().checked_ilog2().unwrap_or(0) as u64
            + self.len().min(RUNS_PER_BLOCK) as u64 * 20
    }
}

/// Sequential, allocation-free traversal of validated packed source runs.
pub struct SourceMapIter<'a> {
    bytes: &'a [u8],
    offset: usize,
    index: usize,
    remaining: u32,
    previous: SourceMapEntry,
}
impl Iterator for SourceMapIter<'_> {
    type Item = SourceMapEntry;
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        if self.index.is_multiple_of(RUNS_PER_BLOCK) {
            self.previous = empty();
        }
        let entry = read_entry(self.bytes, &mut self.offset, &self.previous)
            .expect("validated packed source map");
        self.previous = entry.clone();
        self.remaining -= 1;
        self.index += 1;
        Some(entry)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining as usize, Some(self.remaining as usize))
    }
}
impl ExactSizeIterator for SourceMapIter<'_> {}
impl<'a> IntoIterator for &'a CompactSourceMap {
    type Item = SourceMapEntry;
    type IntoIter = SourceMapIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

fn empty() -> SourceMapEntry {
    SourceMapEntry {
        pc: Pc(0),
        span: SourceSpan {
            file: SourceId(0),
            start: 0,
            end: 0,
        },
    }
}
fn error(detail: &str) -> Error {
    Error::new(ErrorKind::Source, detail)
}
fn delta(bytes: &[u8], offset: &mut usize, base: u32) -> Result<u32, Error> {
    let mut value = 0u64;
    for shift in (0..35).step_by(7) {
        let byte = *bytes
            .get(*offset)
            .ok_or_else(|| error("truncated source map delta"))?;
        *offset += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            if shift > 0 && byte == 0 || value > (u64::from(u32::MAX) << 1) {
                return Err(error("non-canonical source map delta"));
            }
            let signed = (value >> 1) as i64 ^ -((value & 1) as i64);
            return u32::try_from(i64::from(base) + signed)
                .map_err(|_| error("source map delta overflow"));
        }
    }
    Err(error("source map delta width"))
}
fn read_entry(
    bytes: &[u8],
    offset: &mut usize,
    previous: &SourceMapEntry,
) -> Result<SourceMapEntry, Error> {
    let pc = Pc(delta(bytes, offset, previous.pc.0)?);
    let file = SourceId(delta(bytes, offset, previous.span.file.0)?);
    let start = delta(bytes, offset, previous.span.start)?;
    let end = delta(bytes, offset, start)?;
    Ok(SourceMapEntry {
        pc,
        span: SourceSpan { file, start, end },
    })
}
fn scan(bytes: &[u8], count: u32, mut checkpoint: impl FnMut(u32, u32)) -> Result<(), Error> {
    if count as usize > bytes.len() / 4 {
        return Err(error("source map count exceeds packed bytes"));
    }
    let mut previous = empty();
    let mut last_pc = None;
    let mut offset = 0;
    for i in 0..count as usize {
        let start = offset;
        if i % RUNS_PER_BLOCK == 0 {
            previous = empty();
        }
        let entry = read_entry(bytes, &mut offset, &previous)?;
        if last_pc.is_some_and(|pc| pc >= entry.pc) {
            return Err(error("source map PCs must increase"));
        }
        if i % RUNS_PER_BLOCK == 0 {
            checkpoint(
                entry.pc.0,
                u32::try_from(start).map_err(|_| error("source map offset"))?,
            );
        }
        last_pc = Some(entry.pc);
        previous = entry;
    }
    if offset != bytes.len() {
        return Err(error("trailing source map bytes"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_runs_preserve_every_lookup_across_restart_blocks_without_allocation() {
        let entries: Vec<_> = (0..1031u32)
            .map(|i| SourceMapEntry {
                pc: Pc(7 + i * 3),
                span: SourceSpan {
                    file: SourceId(i / 61 % 3),
                    start: i % 17 * 11,
                    end: i % 17 * 11 + i % 9,
                },
            })
            .collect();
        let packed = CompactSourceMap::from_entries(&entries).unwrap();
        assert_eq!(packed.iter().collect::<Vec<_>>(), entries);
        assert!(packed.capacity_bytes() < entries.len() * 8);
        let allocations = allocation_counter::measure(|| {
            for pc in 0..entries.last().unwrap().pc.0 + 5 {
                assert_eq!(
                    packed.source_at(Pc(pc)),
                    entries.source_at(Pc(pc)),
                    "PC {pc}"
                );
            }
        });
        assert_eq!(allocations.count_total, 0);
        assert_eq!(CompactSourceMap::default().source_at(Pc(u32::MAX)), None);
    }

    #[test]
    fn packed_deltas_cover_full_u32_fields_and_reject_corrupt_encodings() {
        let entries = [
            SourceMapEntry {
                pc: Pc(0),
                span: SourceSpan {
                    file: SourceId(u32::MAX),
                    start: u32::MAX,
                    end: 0,
                },
            },
            SourceMapEntry {
                pc: Pc(u32::MAX),
                span: SourceSpan {
                    file: SourceId(0),
                    start: 0,
                    end: u32::MAX,
                },
            },
        ];
        let packed = CompactSourceMap::from_entries(&entries).unwrap();
        assert_eq!(packed.iter().collect::<Vec<_>>(), entries);
        for end in 0..packed.bytes.len() {
            assert!(CompactSourceMap::from_encoded(&packed.bytes[..end], 2).is_err());
        }
        assert!(CompactSourceMap::from_encoded(&[0, 0, 0, 0, 0], 1).is_err());
        assert!(CompactSourceMap::from_encoded(&[0x80, 0, 0, 0, 0], 1).is_err());
        assert!(
            CompactSourceMap::from_encoded(&[0xff, 0xff, 0xff, 0xff, 0x7f, 0, 0, 0], 1).is_err()
        );
        assert!(CompactSourceMap::from_encoded(&[1, 0, 0, 0], 1).is_err());
        assert!(CompactSourceMap::from_encoded(&[0; 8], 2).is_err());
        assert!(CompactSourceMap::from_encoded(&[0; 4], u32::MAX).is_err());
    }
}
