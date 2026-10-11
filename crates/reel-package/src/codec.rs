use super::{Budget, CodePackage, Error, ErrorKind, Limits, Metadata, require};
use aside_vm::bytecode as bc;
use std::io::Write;

pub const MAGIC: &[u8; 8] = b"REELASB\0";
pub const HEADER_LEN: usize = 72;
pub const INDEX_ENTRY_LEN: usize = 56;
const B01: u32 = 1;
const METADATA: u32 = 2;
const REQUIRED: u32 = 1;

/// Canonical writer: header, B01, metadata, index; no padding or timestamps.
pub fn encode(bytecode: &[u8], metadata: &Metadata, limits: Limits) -> Result<Vec<u8>, Error> {
    let mut budget = Budget::new(limits);
    require(
        bytecode.len() <= limits.bytecode.max_bytes,
        ErrorKind::Limit,
        "B01 byte budget exceeded",
    )?;
    let program = bc::VerifiedProgram::decode(bytecode, limits.bytecode)
        .map_err(|e| Error::new(ErrorKind::Bytecode, e.to_string()))?;
    metadata.validate(&program, bytecode, limits, &mut budget)?;
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        limit: limits.max_metadata_bytes,
    };
    serde_json::to_writer(&mut writer, metadata)
        .map_err(|e| Error::new(ErrorKind::Limit, e.to_string()))?;
    let json = writer.bytes;
    budget.charge(json.len())?;
    preflight_json(&json, limits)?;
    require(
        limits.max_sections >= 2,
        ErrorKind::Limit,
        "section count budget exceeded",
    )?;
    let index_offset = HEADER_LEN
        .checked_add(bytecode.len())
        .and_then(|n| n.checked_add(json.len()))
        .ok_or_else(|| Error::new(ErrorKind::Limit, "container size overflow"))?;
    let total = index_offset
        .checked_add(2 * INDEX_ENTRY_LEN)
        .ok_or_else(|| Error::new(ErrorKind::Limit, "container size overflow"))?;
    require(
        total <= limits.max_file_bytes,
        ErrorKind::Limit,
        "container byte budget exceeded",
    )?;
    budget.charge(total)?;
    let mut out = Vec::new();
    out.try_reserve_exact(total)
        .map_err(|e| Error::new(ErrorKind::Limit, e.to_string()))?;
    out.resize(HEADER_LEN, 0);
    out.extend_from_slice(bytecode);
    out.extend_from_slice(&json);
    index_entry(&mut out, B01, HEADER_LEN, bytecode);
    index_entry(&mut out, METADATA, HEADER_LEN + bytecode.len(), &json);
    out[..8].copy_from_slice(MAGIC);
    out[8..10].copy_from_slice(&1u16.to_le_bytes());
    // minor and reserved flags remain zero.
    out[16..24].copy_from_slice(&(total as u64).to_le_bytes());
    out[24..32].copy_from_slice(&(index_offset as u64).to_le_bytes());
    out[32..40].copy_from_slice(&(2u64 * INDEX_ENTRY_LEN as u64).to_le_bytes());
    let index_hash = blake3::hash(&out[index_offset..]);
    out[40..72].copy_from_slice(index_hash.as_bytes());
    Ok(out)
}

fn index_entry(out: &mut Vec<u8>, kind: u32, offset: usize, bytes: &[u8]) {
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&REQUIRED.to_le_bytes());
    out.extend_from_slice(&(offset as u64).to_le_bytes());
    out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    out.extend_from_slice(blake3::hash(bytes).as_bytes());
}

/// Bounded static validation only. Unknown optional sections are hash-checked
/// and ignored; they cannot affect entry, linking, or host authorization.
pub fn decode(input: &[u8], limits: Limits) -> Result<CodePackage, Error> {
    use ErrorKind::{Encoding, Integrity, Limit, Version};
    require(
        input.len() <= limits.max_file_bytes,
        Limit,
        "container byte budget exceeded",
    )?;
    let mut budget = Budget::new(limits);
    budget.charge(input.len())?;
    require(input.len() >= HEADER_LEN, Encoding, "truncated ASB header")?;
    require(&input[..8] == MAGIC, Encoding, "invalid ASB magic")?;
    require(
        input[8..12] == [1, 0, 0, 0],
        Version,
        "unsupported ASB version",
    )?;
    require(u32_at(input, 12) == 0, Version, "unknown header flags")?;
    require(
        u64_at(input, 16) == input.len() as u64,
        Encoding,
        "total length mismatch",
    )?;
    let index = range(u64_at(input, 24), u64_at(input, 32), input.len())?;
    require(
        index.start >= HEADER_LEN
            && !index.is_empty()
            && index.len().is_multiple_of(INDEX_ENTRY_LEN),
        Encoding,
        "invalid index range/alignment",
    )?;
    let count = index.len() / INDEX_ENTRY_LEN;
    require(
        count <= limits.max_sections,
        Limit,
        "section count budget exceeded",
    )?;
    require(
        blake3::hash(&input[index.clone()]).as_bytes() == &input[40..72],
        Integrity,
        "index digest mismatch",
    )?;
    let mut sections = Vec::new();
    sections
        .try_reserve_exact(count)
        .map_err(|e| Error::new(Limit, e.to_string()))?;
    let mut kinds = std::collections::BTreeSet::new();
    let mut hashes = Vec::new();
    hashes
        .try_reserve_exact(count)
        .map_err(|e| Error::new(Limit, e.to_string()))?;
    let mut bytecode = None;
    let mut metadata = None;
    for entry in input[index.clone()].chunks_exact(INDEX_ENTRY_LEN) {
        let kind = u32_at(entry, 0);
        let flags = u32_at(entry, 4);
        require(kinds.insert(kind), Encoding, "duplicate section kind")?;
        require(flags & !REQUIRED == 0, Version, "unknown section flags")?;
        let known = kind == B01 || kind == METADATA;
        require(
            known || flags & REQUIRED == 0,
            Version,
            "unknown required section",
        )?;
        require(
            !known || flags == REQUIRED,
            Encoding,
            "core section must be required",
        )?;
        let section = range(u64_at(entry, 8), u64_at(entry, 16), input.len())?;
        require(
            !section.is_empty() && section.start >= HEADER_LEN && !overlaps(&section, &index),
            Encoding,
            "section overlaps header/index or is empty",
        )?;
        sections.push(section.clone());
        if kind == B01 {
            require(
                section.len() <= limits.bytecode.max_bytes,
                Limit,
                "B01 byte budget exceeded",
            )?;
            bytecode = Some(section.clone());
        } else if kind == METADATA {
            require(
                section.len() <= limits.max_metadata_bytes,
                Limit,
                "metadata byte budget exceeded",
            )?;
            metadata = Some(section.clone());
        }
        hashes.push((section, &entry[24..56]));
    }
    sections.push(index);
    sections.sort_unstable_by_key(|r| r.start);
    let mut end = HEADER_LEN;
    for section in sections {
        require(section.start >= end, Encoding, "overlapping sections")?;
        end = section.end;
    }
    // Only hash payloads after proving they are disjoint. Their total work
    // cannot exceed the prepaid file byte budget even for malicious indices.
    for (section, expected) in hashes {
        require(
            blake3::hash(&input[section]).as_bytes() == expected,
            Integrity,
            "section digest mismatch",
        )?;
    }
    let b01 = &input[bytecode.ok_or_else(|| Error::new(Encoding, "missing B01 section"))?];
    let json = &input[metadata.ok_or_else(|| Error::new(Encoding, "missing metadata section"))?];
    budget.charge(
        json.len()
            .checked_mul(2)
            .ok_or_else(|| Error::new(Limit, "metadata work overflow"))?,
    )?;
    preflight_json(json, limits)?;
    // Deserialize straight to bounded, closed DTOs. Derived struct visitors
    // reject duplicate/unknown fields; no unbounded serde_json::Value tree.
    let metadata: Metadata =
        serde_json::from_slice(json).map_err(|e| Error::new(ErrorKind::Metadata, e.to_string()))?;
    let program = bc::VerifiedProgram::decode(b01, limits.bytecode)
        .map_err(|e| Error::new(ErrorKind::Bytecode, e.to_string()))?;
    metadata.validate(&program, b01, limits, &mut budget)?;
    Ok(CodePackage { program, metadata })
}
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
fn range(offset: u64, len: u64, total: usize) -> Result<std::ops::Range<usize>, Error> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| Error::new(ErrorKind::Encoding, "offset/length overflow"))?;
    require(
        end <= total as u64,
        ErrorKind::Encoding,
        "section/index out of bounds",
    )?;
    let start = usize::try_from(offset)
        .map_err(|_| Error::new(ErrorKind::Encoding, "offset does not fit usize"))?;
    let end = usize::try_from(end)
        .map_err(|_| Error::new(ErrorKind::Encoding, "end does not fit usize"))?;
    Ok(start..end)
}
fn overlaps(a: &std::ops::Range<usize>, b: &std::ops::Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

/// Allocation-free lexical admission. Byte scan work is at most the explicit
/// metadata-byte limit; tokens/containers/strings are bounded before serde runs.
fn preflight_json(bytes: &[u8], limits: Limits) -> Result<(), Error> {
    require(
        bytes.len() <= limits.max_metadata_bytes,
        ErrorKind::Limit,
        "metadata byte budget exceeded",
    )?;
    let (mut depth, mut objects, mut tokens, mut string_bytes) = (0usize, 0usize, 0usize, 0usize);
    let (mut string, mut escaped, mut scalar) = (false, false, false);
    for &byte in bytes {
        if string {
            if !escaped && byte == b'"' {
                string = false;
            } else {
                string_bytes += 1;
                require(
                    string_bytes <= limits.max_json_string_bytes,
                    ErrorKind::Limit,
                    "JSON string budget exceeded",
                )?;
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                }
            }
            continue;
        }
        match byte {
            b'"' => {
                string = true;
                escaped = false;
                string_bytes = 0;
                scalar = false;
                tokens += 1;
            }
            b'{' | b'[' => {
                depth += 1;
                tokens += 1;
                scalar = false;
                if byte == b'{' {
                    objects += 1;
                }
                require(
                    depth <= limits.max_json_depth && objects <= limits.max_metadata_records,
                    ErrorKind::Limit,
                    "JSON nesting/record budget exceeded",
                )?;
            }
            b'}' | b']' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| Error::new(ErrorKind::Metadata, "unbalanced JSON"))?;
                tokens += 1;
                scalar = false;
            }
            b',' | b':' => {
                tokens += 1;
                scalar = false;
            }
            b' ' | b'\n' | b'\r' | b'\t' => {
                scalar = false;
            }
            _ => {
                if !scalar {
                    tokens += 1;
                    scalar = true;
                }
            }
        }
        require(
            tokens <= limits.max_json_tokens,
            ErrorKind::Limit,
            "JSON token/work budget exceeded",
        )?;
    }
    require(
        depth == 0 && !string,
        ErrorKind::Metadata,
        "unterminated JSON",
    )
}
struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("metadata byte budget exceeded"));
        }
        self.bytes
            .try_reserve(bytes.len())
            .map_err(std::io::Error::other)?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
