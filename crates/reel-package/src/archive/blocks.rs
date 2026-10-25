use super::{Entry, EntryStorage, invalid};
use std::io::{self, Read, Seek, SeekFrom, Write};

/// Maximum logical size of an independently decoded archive block.
pub const BLOCK_BYTES: usize = 64 << 10;
/// Heap payload capacity of one range-read workspace. Reserve this amount before
/// constructing [`ReadScratch`] and release that reservation after dropping it.
pub const SCRATCH_BYTES: usize = BLOCK_BYTES * 2;
const RECORD_BYTES: u64 = 16;

/// Reusable workspace for archive range reads. Decoding allocates no additional
/// block buffers; raw entries and complete compressed blocks write directly into
/// the destination. The last partially requested decoded block remains available,
/// identified by the immutable logical content version, length, and block index.
#[derive(Debug)]
pub struct ReadScratch {
    bytes: Box<[u8]>,
    cached: Option<DecodedBlock>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DecodedBlock {
    version: [u8; 32],
    length: u64,
    block: u64,
}
impl ReadScratch {
    /// Allocates exactly [`SCRATCH_BYTES`] bytes of reusable payload storage.
    pub fn new() -> Self {
        Self {
            bytes: vec![0; SCRATCH_BYTES].into_boxed_slice(),
            cached: None,
        }
    }
}
impl Default for ReadScratch {
    fn default() -> Self {
        Self::new()
    }
}

pub(super) fn directory_bytes(length: u64) -> Option<u64> {
    length
        .div_ceil(BLOCK_BYTES as u64)
        .checked_mul(RECORD_BYTES)
}

impl Entry {
    /// Reads exactly `output.len()` logical bytes beginning at `offset`.
    /// Only the addressed directory records and payload blocks are read. Records
    /// are batched in 4 KiB of stack storage; adjacent payloads read sequentially.
    /// Cancellation returns [`io::ErrorKind::Interrupted`] and may leave a written
    /// prefix in `output`; callers publish their result only after success.
    /// Content digests identify bytes at packaging time; range reads validate
    /// storage structure and codec output, without hashing the whole entry.
    pub fn read_range(
        &self,
        input: &mut (impl Read + Seek),
        offset: u64,
        mut output: &mut [u8],
        scratch: &mut ReadScratch,
        mut cancelled: impl FnMut() -> bool,
    ) -> io::Result<()> {
        if offset
            .checked_add(output.len() as u64)
            .is_none_or(|end| end > self.length)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "archive read lies outside logical entry",
            ));
        }
        check_cancelled(&mut cancelled)?;
        if output.is_empty() {
            return Ok(());
        }
        let EntryStorage::Blocks { directory_offset } = self.storage else {
            let position = self
                .offset
                .checked_add(offset)
                .ok_or_else(|| invalid("archive raw offset overflow"))?;
            input.seek(SeekFrom::Start(position))?;
            for chunk in output.chunks_mut(BLOCK_BYTES) {
                read_exact(input, chunk, &mut cancelled)?;
            }
            return check_cancelled(&mut cancelled);
        };
        let (encoded, decoded) = scratch.bytes.split_at_mut(BLOCK_BYTES);
        let last_block = (offset + output.len() as u64 - 1) / BLOCK_BYTES as u64;
        let mut records = [0; 4096];
        let mut first_record = 0;
        let mut record_count = 0;
        let mut next_payload = None;
        let mut logical_offset = offset;
        while !output.is_empty() {
            check_cancelled(&mut cancelled)?;
            let block = logical_offset / BLOCK_BYTES as u64;
            let block_start = block * BLOCK_BYTES as u64;
            let logical_length = (self.length - block_start).min(BLOCK_BYTES as u64) as usize;
            let within = (logical_offset - block_start) as usize;
            let size = output.len().min(logical_length - within);
            let key = DecodedBlock {
                version: self.version,
                length: self.length,
                block,
            };
            if scratch.cached == Some(key) {
                output[..size].copy_from_slice(&decoded[within..within + size]);
                logical_offset += size as u64;
                output = &mut output[size..];
                continue;
            }
            if block < first_record || block - first_record >= record_count {
                let record_offset = block
                    .checked_mul(RECORD_BYTES)
                    .and_then(|relative| directory_offset.checked_add(relative))
                    .ok_or_else(|| invalid("archive block record offset overflow"))?;
                first_record = block;
                record_count = (last_block - block + 1).min(records.len() as u64 / RECORD_BYTES);
                input.seek(SeekFrom::Start(record_offset))?;
                read_exact(
                    input,
                    &mut records[..(record_count * RECORD_BYTES) as usize],
                    &mut cancelled,
                )?;
                next_payload = None;
            }
            let record_at = ((block - first_record) * RECORD_BYTES) as usize;
            let record = &records[record_at..record_at + RECORD_BYTES as usize];
            let relative = u64::from_le_bytes(record[..8].try_into().unwrap());
            let stored = u32::from_le_bytes(record[8..12].try_into().unwrap()) as usize;
            let codec = u32::from_le_bytes(record[12..16].try_into().unwrap());
            let end = relative.checked_add(stored as u64);
            if stored == 0
                || end.is_none_or(|end| end > self.stored_length)
                || (block == 0 && relative != 0)
                || (block_start + logical_length as u64 == self.length
                    && end != Some(self.stored_length))
                || !matches!((codec, stored), (0, n) if n == logical_length)
                    && !matches!((codec, stored), (1, n) if n < logical_length)
            {
                return Err(invalid("invalid archive block record"));
            }
            let payload_offset = self
                .offset
                .checked_add(relative)
                .ok_or_else(|| invalid("archive block payload offset overflow"))?;
            if codec == 0 {
                // Raw blocks require neither a decode nor a full-block read.
                let position = payload_offset + within as u64;
                if next_payload != Some(position) {
                    input.seek(SeekFrom::Start(position))?;
                }
                read_exact(input, &mut output[..size], &mut cancelled)?;
                next_payload = Some(position + size as u64);
            } else {
                if next_payload != Some(payload_offset) {
                    input.seek(SeekFrom::Start(payload_offset))?;
                }
                read_exact(input, &mut encoded[..stored], &mut cancelled)?;
                next_payload = Some(payload_offset + stored as u64);
                check_cancelled(&mut cancelled)?;
                let direct = within == 0 && size == logical_length;
                let destination = if direct {
                    &mut output[..size]
                } else {
                    // A failed decode may mutate scratch; retire its old identity first.
                    scratch.cached = None;
                    &mut decoded[..logical_length]
                };
                let written = lz4_flex::block::decompress_into(&encoded[..stored], destination)
                    .map_err(|error| invalid(format!("invalid archive LZ4 block: {error}")))?;
                if written != logical_length {
                    return Err(invalid("archive block decoded length mismatch"));
                }
                if !direct {
                    scratch.cached = Some(key);
                    output[..size].copy_from_slice(&decoded[within..within + size]);
                }
            }
            logical_offset += size as u64;
            output = &mut output[size..];
        }
        check_cancelled(&mut cancelled)
    }
}

fn check_cancelled(cancelled: &mut impl FnMut() -> bool) -> io::Result<()> {
    if cancelled() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "archive read cancelled",
        ))
    } else {
        Ok(())
    }
}

fn read_exact(
    input: &mut impl Read,
    mut output: &mut [u8],
    cancelled: &mut impl FnMut() -> bool,
) -> io::Result<()> {
    while !output.is_empty() {
        check_cancelled(cancelled)?;
        match input.read(output) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated archive data",
                ));
            }
            Ok(size) => output = &mut output[size..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Packaging retains directory records, never logical file content. Runtime
/// directory reads use a fixed stack batch and do not retain this list.
pub(super) fn write(
    input: &mut impl Read,
    output: &mut impl Write,
    hash: &mut blake3::Hasher,
    offset: u64,
    compress: bool,
) -> io::Result<(u64, u64, EntryStorage)> {
    let mut raw = vec![0; BLOCK_BYTES];
    let mut encoded = if compress {
        vec![0; lz4_flex::block::get_maximum_output_size(BLOCK_BYTES)]
    } else {
        Vec::new()
    };
    let mut records = Vec::new();
    let mut length = 0u64;
    let mut stored_length = 0u64;
    let mut has_compressed = false;
    loop {
        // Fill an independent logical block despite short reads from the source.
        let mut size = 0;
        while size < raw.len() {
            match input.read(&mut raw[size..]) {
                Ok(0) => break,
                Ok(read) => size += read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        if size == 0 {
            break;
        }
        let bytes = &raw[..size];
        hash.update(bytes);
        length = length
            .checked_add(size as u64)
            .ok_or_else(|| invalid("archive entry exceeds format width"))?;
        let encoded_length = if compress {
            lz4_flex::block::compress_into(bytes, &mut encoded).map_err(io::Error::other)?
        } else {
            size
        };
        let (payload, codec) = if encoded_length < size {
            has_compressed = true;
            (&encoded[..encoded_length], 1u32)
        } else {
            (bytes, 0u32)
        };
        if compress {
            let mut record = [0; RECORD_BYTES as usize];
            record[..8].copy_from_slice(&stored_length.to_le_bytes());
            record[8..12].copy_from_slice(&(payload.len() as u32).to_le_bytes());
            record[12..].copy_from_slice(&codec.to_le_bytes());
            records.try_reserve(1).map_err(io::Error::other)?;
            records.push(record);
        }
        output.write_all(payload)?;
        stored_length = stored_length
            .checked_add(payload.len() as u64)
            .ok_or_else(|| invalid("archive payload exceeds format width"))?;
    }
    let storage = if has_compressed {
        let directory_offset = offset
            .checked_add(stored_length)
            .ok_or_else(|| invalid("archive block directory offset overflow"))?;
        directory_bytes(length)
            .and_then(|bytes| directory_offset.checked_add(bytes))
            .ok_or_else(|| invalid("archive block directory exceeds format width"))?;
        for record in records {
            output.write_all(&record)?;
        }
        EntryStorage::Blocks { directory_offset }
    } else {
        EntryStorage::Raw
    };
    Ok((length, stored_length, storage))
}
