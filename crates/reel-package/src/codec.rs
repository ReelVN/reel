use super::{
    Budget, CodePackage, Error, ErrorKind, Limits, Metadata, distribution::Distribution, require,
};
use aside_vm::bytecode as bc;
use std::io::{self, Read, Seek, SeekFrom, Write};

pub const MAGIC: &[u8; 8] = b"REELASB\0";
pub const VERSION: u16 = 1;
pub const HEADER_LEN: usize = 72;
pub const INDEX_ENTRY_LEN: usize = 56;
const B01: u32 = 1;
const METADATA: u32 = 2;
const DISTRIBUTION: u32 = 3;
const REQUIRED: u32 = 1;

/// Canonical writer: header, B01, metadata, distribution, index; no padding or timestamps.
pub fn encode(
    bytecode: &[u8],
    metadata: &Metadata,
    distribution: Option<&Distribution>,
    limits: Limits,
) -> Result<Vec<u8>, Error> {
    let mut budget = Budget::new(limits);
    require(
        bytecode.len() <= limits.bytecode.max_bytes,
        ErrorKind::Limit,
        "B01 byte budget exceeded",
    )?;
    let program = bc::VerifiedProgram::decode(bytecode, limits.bytecode)
        .map_err(|e| Error::new(ErrorKind::Bytecode, e.to_string()))?;
    metadata.validate(&program, bytecode, limits, &mut budget)?;
    let json = json_bytes(metadata, limits)?;
    budget.charge(json.len())?;
    let distribution = distribution
        .map(|distribution| {
            distribution
                .validate()
                .map_err(|e| Error::new(ErrorKind::Metadata, e.to_string()))?;
            json_bytes(distribution, limits)
        })
        .transpose()?;
    let distribution_bytes = distribution.as_deref().unwrap_or_default();
    budget.charge(distribution_bytes.len())?;
    let section_count = 2 + usize::from(distribution.is_some());
    require(
        limits.max_sections >= section_count,
        ErrorKind::Limit,
        "section count budget exceeded",
    )?;
    let index_offset = HEADER_LEN
        .checked_add(bytecode.len())
        .and_then(|n| n.checked_add(json.len()))
        .and_then(|n| n.checked_add(distribution_bytes.len()))
        .ok_or_else(|| Error::new(ErrorKind::Limit, "container size overflow"))?;
    let total = index_offset
        .checked_add(section_count * INDEX_ENTRY_LEN)
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
    out.extend_from_slice(distribution_bytes);
    index_entry(&mut out, B01, HEADER_LEN, bytecode);
    index_entry(&mut out, METADATA, HEADER_LEN + bytecode.len(), &json);
    if distribution.is_some() {
        index_entry(
            &mut out,
            DISTRIBUTION,
            HEADER_LEN + bytecode.len() + json.len(),
            distribution_bytes,
        );
    }
    out[..8].copy_from_slice(MAGIC);
    out[8..10].copy_from_slice(&VERSION.to_le_bytes());
    // minor and reserved flags remain zero.
    out[16..24].copy_from_slice(&(total as u64).to_le_bytes());
    out[24..32].copy_from_slice(&(index_offset as u64).to_le_bytes());
    out[32..40].copy_from_slice(&((section_count * INDEX_ENTRY_LEN) as u64).to_le_bytes());
    let index_hash = blake3::hash(&out[index_offset..]);
    out[40..72].copy_from_slice(index_hash.as_bytes());
    Ok(out)
}

fn json_bytes(value: &impl serde::Serialize, limits: Limits) -> Result<Vec<u8>, Error> {
    let mut writer = BoundedWriter {
        bytes: Vec::new(),
        limit: limits.max_metadata_bytes,
    };
    serde_json::to_writer(&mut writer, value)
        .map_err(|e| Error::new(ErrorKind::Limit, e.to_string()))?;
    preflight_json(&writer.bytes, limits)?;
    Ok(writer.bytes)
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
    let mut budget = Budget::new(limits);
    budget.charge(input.len())?;
    let index = parse_header(input, input.len(), limits)?;
    let sections = parse_index(input, &input[index.clone()], index, input.len(), limits)?;
    for section in &sections {
        check_digest(&input[section.range.clone()], &section.digest)?;
    }
    let b01 = payload(input, &sections, B01).expect("required B01");
    let json = payload(input, &sections, METADATA).expect("required metadata");
    let metadata: Metadata = decode_json(json, limits, &mut budget)?;
    let distribution = payload(input, &sections, DISTRIBUTION)
        .map(|json| decode_distribution(json, limits, &mut budget))
        .transpose()?;
    let program = bc::VerifiedProgram::decode(b01, limits.bytecode)
        .map_err(|e| Error::new(ErrorKind::Bytecode, e.to_string()))?;
    metadata.validate(&program, b01, limits, &mut budget)?;
    Ok(CodePackage {
        program,
        metadata,
        distribution,
    })
}

/// Read only the embedded distribution, without decoding bytecode or reading resources.
pub fn read_distribution(
    reader: &mut (impl Read + Seek),
    limits: Limits,
) -> io::Result<Option<Distribution>> {
    let total = usize::try_from(reader.seek(SeekFrom::End(0))?).map_err(io::Error::other)?;
    reader.seek(SeekFrom::Start(0))?;
    let mut header = [0; HEADER_LEN];
    reader.read_exact(&mut header)?;
    let index_range = parse_header(&header, total, limits).map_err(io::Error::other)?;
    let mut budget = Budget::new(limits);
    budget.charge(HEADER_LEN).map_err(io::Error::other)?;
    let index = read_range(reader, index_range.clone(), &mut budget)?;
    let sections =
        parse_index(&header, &index, index_range, total, limits).map_err(io::Error::other)?;
    let Some(section) = sections.iter().find(|section| section.kind == DISTRIBUTION) else {
        return Ok(None);
    };
    let json = read_range(reader, section.range.clone(), &mut budget)?;
    check_digest(&json, &section.digest).map_err(io::Error::other)?;
    decode_distribution(&json, limits, &mut budget)
        .map(Some)
        .map_err(io::Error::other)
}

fn read_range(
    reader: &mut (impl Read + Seek),
    range: std::ops::Range<usize>,
    budget: &mut Budget,
) -> io::Result<Vec<u8>> {
    budget.charge(range.len()).map_err(io::Error::other)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(range.len())
        .map_err(io::Error::other)?;
    bytes.resize(range.len(), 0);
    reader.seek(SeekFrom::Start(range.start as u64))?;
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

struct Section {
    kind: u32,
    range: std::ops::Range<usize>,
    digest: [u8; 32],
}

fn parse_header(
    input: &[u8],
    total: usize,
    limits: Limits,
) -> Result<std::ops::Range<usize>, Error> {
    use ErrorKind::{Encoding, Limit, Version};
    require(
        total <= limits.max_file_bytes,
        Limit,
        "container byte budget exceeded",
    )?;
    require(input.len() >= HEADER_LEN, Encoding, "truncated ASB header")?;
    require(&input[..8] == MAGIC, Encoding, "invalid ASB magic")?;
    require(
        input[8..10] == VERSION.to_le_bytes() && input[10..12] == [0, 0],
        Version,
        "unsupported ASB version",
    )?;
    require(u32_at(input, 12) == 0, Version, "unknown header flags")?;
    require(
        u64_at(input, 16) == total as u64,
        Encoding,
        "total length mismatch",
    )?;
    let index = range(u64_at(input, 24), u64_at(input, 32), total)?;
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
    Ok(index)
}

fn parse_index(
    header: &[u8],
    bytes: &[u8],
    index: std::ops::Range<usize>,
    total: usize,
    limits: Limits,
) -> Result<Vec<Section>, Error> {
    use ErrorKind::{Encoding, Integrity, Limit, Version};
    require(
        blake3::hash(bytes).as_bytes() == &header[40..72],
        Integrity,
        "index digest mismatch",
    )?;
    let count = bytes.len() / INDEX_ENTRY_LEN;
    let mut sections: Vec<Section> = Vec::new();
    sections
        .try_reserve_exact(count)
        .map_err(|e| Error::new(Limit, e.to_string()))?;
    let mut kinds = std::collections::BTreeSet::new();
    for entry in bytes.chunks_exact(INDEX_ENTRY_LEN) {
        let kind = u32_at(entry, 0);
        let flags = u32_at(entry, 4);
        require(kinds.insert(kind), Encoding, "duplicate section kind")?;
        require(flags & !REQUIRED == 0, Version, "unknown section flags")?;
        let known = matches!(kind, B01 | METADATA | DISTRIBUTION);
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
        let section = range(u64_at(entry, 8), u64_at(entry, 16), total)?;
        require(
            !section.is_empty() && section.start >= HEADER_LEN && !overlaps(&section, &index),
            Encoding,
            "section overlaps header/index or is empty",
        )?;
        if kind == B01 {
            require(
                section.len() <= limits.bytecode.max_bytes,
                Limit,
                "B01 byte budget exceeded",
            )?;
        } else if matches!(kind, METADATA | DISTRIBUTION) {
            require(
                section.len() <= limits.max_metadata_bytes,
                Limit,
                "metadata byte budget exceeded",
            )?;
        }
        sections.push(Section {
            kind,
            range: section,
            digest: entry[24..56].try_into().unwrap(),
        });
    }
    require(kinds.contains(&B01), Encoding, "missing B01 section")?;
    require(
        kinds.contains(&METADATA),
        Encoding,
        "missing metadata section",
    )?;
    sections.sort_unstable_by_key(|section| section.range.start);
    let mut end = HEADER_LEN;
    for section in &sections {
        require(section.range.start >= end, Encoding, "overlapping sections")?;
        end = section.range.end;
    }
    Ok(sections)
}

fn payload<'a>(input: &'a [u8], sections: &[Section], kind: u32) -> Option<&'a [u8]> {
    sections
        .iter()
        .find(|section| section.kind == kind)
        .map(|section| &input[section.range.clone()])
}

fn check_digest(bytes: &[u8], digest: &[u8; 32]) -> Result<(), Error> {
    require(
        blake3::hash(bytes).as_bytes() == digest,
        ErrorKind::Integrity,
        "section digest mismatch",
    )
}

fn decode_json<T: serde::de::DeserializeOwned>(
    json: &[u8],
    limits: Limits,
    budget: &mut Budget,
) -> Result<T, Error> {
    budget.charge(
        json.len()
            .checked_mul(2)
            .ok_or_else(|| Error::new(ErrorKind::Limit, "metadata work overflow"))?,
    )?;
    preflight_json(json, limits)?;
    // Deserialize straight to bounded, closed DTOs. Derived struct visitors
    // reject duplicate/unknown fields; no unbounded serde_json::Value tree.
    serde_json::from_slice(json).map_err(|e| Error::new(ErrorKind::Metadata, e.to_string()))
}

fn decode_distribution(
    json: &[u8],
    limits: Limits,
    budget: &mut Budget,
) -> Result<Distribution, Error> {
    let distribution: Distribution = decode_json(json, limits, budget)?;
    distribution
        .validate()
        .map_err(|e| Error::new(ErrorKind::Metadata, e.to_string()))?;
    Ok(distribution)
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
