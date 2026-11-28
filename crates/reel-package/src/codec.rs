use super::{
    Budget, CodePackage, EffectProducts, Error, ErrorKind, Limits, MAX_VISUAL_PLAN_BYTES, Metadata,
    Products, distribution::Distribution, require,
};
use aside_vm::bytecode as bc;
use aside_vm::resume::{ResumeLimits, ResumeTable, VerifiedResumeTable};
use std::borrow::Cow;
use std::io::{self, Read, Seek, SeekFrom, Write};

pub const MAGIC: &[u8; 8] = b"REELASB\0";
pub const VERSION: u16 = 1;
pub const HEADER_LEN: usize = 72;
pub const INDEX_ENTRY_LEN: usize = 56;
const B01: u32 = 1;
const METADATA: u32 = 2;
const DISTRIBUTION: u32 = 3;
const EFFECTS: u32 = 4;
const RESUME: u32 = 5;
const VISUAL_PLAN: u32 = 6;
const REQUIRED: u32 = 1;

/// Canonical writer: header, B01, metadata, distribution, effects, resume, visual plan, index.
/// Reuses the serialization's verified program for metadata validation.
/// Host registrations validate nominal method targets in continuation metadata.
pub fn encode(
    encoded: &bc::EncodedProgram,
    metadata: &Metadata,
    distribution: Option<&Distribution>,
    products: Products<'_>,
    host_types: &[bc::HostTypeSupport],
    limits: Limits,
) -> Result<Vec<u8>, Error> {
    let Products {
        effects,
        resume,
        visual_plan,
    } = products;
    let bytecode = encoded.as_bytes();
    let mut budget = Budget::new(limits);
    require(
        bytecode.len() <= limits.bytecode.max_bytes,
        ErrorKind::Limit,
        "B01 byte budget exceeded",
    )?;
    require_visual_plan_size(visual_plan.len(), limits)?;
    budget.charge(visual_plan.len())?;
    budget.charge(bytecode.len())?;
    let bytecode_hash = blake3::hash(bytecode);
    metadata.validate(encoded.program(), bytecode_hash, limits, &mut budget)?;
    effects.validate(encoded.program(), limits, &mut budget)?;
    verify_resume(encoded.program(), resume.clone(), host_types, limits)?;
    let resume_bytes = if resume.is_empty() {
        Vec::new()
    } else {
        json_bytes(resume, limits)?
    };
    budget.charge(resume_bytes.len())?;
    let effect_bytes = if effects.is_empty() {
        Vec::new()
    } else {
        effects.encode(limits)?
    };
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
    let sections = [
        (B01, bytecode),
        (METADATA, json.as_slice()),
        (DISTRIBUTION, distribution_bytes),
        (EFFECTS, effect_bytes.as_slice()),
        (RESUME, resume_bytes.as_slice()),
        (VISUAL_PLAN, visual_plan),
    ]
    .into_iter()
    .filter(|(_, bytes)| !bytes.is_empty());
    let section_count = sections.clone().count();
    require(
        limits.max_sections >= section_count,
        ErrorKind::Limit,
        "section count budget exceeded",
    )?;
    let index_offset = sections
        .clone()
        .try_fold(HEADER_LEN, |offset, (_, bytes)| {
            offset.checked_add(bytes.len())
        })
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
    for (_, bytes) in sections.clone() {
        out.extend_from_slice(bytes);
    }
    let mut offset = HEADER_LEN;
    for (kind, bytes) in sections {
        let hash = if kind == B01 {
            bytecode_hash
        } else {
            blake3::hash(bytes)
        };
        index_entry(&mut out, kind, offset, bytes.len(), hash);
        offset += bytes.len();
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

fn index_entry(out: &mut Vec<u8>, kind: u32, offset: usize, length: usize, hash: blake3::Hash) {
    let flags = if kind == VISUAL_PLAN { 0 } else { REQUIRED };
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&(offset as u64).to_le_bytes());
    out.extend_from_slice(&(length as u64).to_le_bytes());
    out.extend_from_slice(hash.as_bytes());
}

/// Bounded static validation only. Unknown optional sections are hash-checked
/// and ignored; they cannot affect entry, linking, or host authorization.
/// Verify one package against the host's independent nominal method declarations.
pub fn decode(
    input: &[u8],
    host_types: &[bc::HostTypeSupport],
    limits: Limits,
) -> Result<CodePackage, Error> {
    let mut budget = Budget::new(limits);
    budget.charge(input.len())?;
    let index = parse_header(input, input.len(), limits)?;
    let sections = parse_index(input, &input[index.clone()], index, input.len(), limits)?;
    decode_sections(&sections, host_types, limits, &mut budget, |section| {
        Ok(Cow::Borrowed(&input[section.range.clone()]))
    })
}

/// Decode a file-backed package with one encoded section resident at a time.
/// This applies the same limits and verification as decoding a byte slice.
pub fn decode_reader(
    reader: &mut (impl Read + Seek),
    host_types: &[bc::HostTypeSupport],
    limits: Limits,
) -> io::Result<CodePackage> {
    let mut budget = Budget::new(limits);
    let (total, sections) = read_index(reader, limits, &mut budget)?;
    budget
        .charge(total - HEADER_LEN - sections.len() * INDEX_ENTRY_LEN)
        .map_err(io::Error::other)?;
    decode_sections(&sections, host_types, limits, &mut budget, |section| {
        read_range(reader, section.range.clone())
            .map(Cow::Owned)
            .map_err(|error| Error::new(ErrorKind::Encoding, error.to_string()))
    })
    .map_err(io::Error::other)
}

fn decode_sections<'a>(
    sections: &[Section],
    host_types: &[bc::HostTypeSupport],
    limits: Limits,
    budget: &mut Budget,
    mut read: impl FnMut(&Section) -> Result<Cow<'a, [u8]>, Error>,
) -> Result<CodePackage, Error> {
    let mut program = None;
    let mut metadata = None;
    let mut distribution = None;
    let mut effects = EffectProducts::default();
    let mut resume = ResumeTable::default();
    let mut visual_plan = Vec::new().into_boxed_slice();
    // The index is sorted by file offset. Verified code and metadata can be
    // decoded independently and linked after all the encoded buffers are gone.
    for section in sections {
        let bytes = read(section)?;
        check_digest(&bytes, &section.digest)?;
        match section.kind {
            B01 => {
                let decoded = bc::VerifiedProgram::decode(&bytes, limits.bytecode)
                    .map_err(|e| Error::new(ErrorKind::Bytecode, e.to_string()))?;
                budget.charge(bytes.len())?;
                program = Some((decoded, section.digest));
            }
            METADATA => metadata = Some(decode_json::<Metadata>(&bytes, limits, budget)?),
            DISTRIBUTION => distribution = Some(decode_distribution(&bytes, limits, budget)?),
            EFFECTS => effects = EffectProducts::decode(&bytes, limits, budget)?,
            RESUME => resume = decode_json(&bytes, limits, budget)?,
            VISUAL_PLAN => {
                budget.charge(bytes.len())?;
                visual_plan = match bytes {
                    Cow::Owned(bytes) => bytes.into_boxed_slice(),
                    Cow::Borrowed(bytes) => {
                        let mut owned = Vec::new();
                        owned
                            .try_reserve_exact(bytes.len())
                            .map_err(|e| Error::new(ErrorKind::Limit, e.to_string()))?;
                        owned.extend_from_slice(bytes);
                        owned.into_boxed_slice()
                    }
                };
            }
            _ => (),
        }
    }
    let (program, digest) = program.expect("required B01");
    let metadata = metadata.expect("required metadata");
    metadata.validate(&program, digest.into(), limits, budget)?;
    effects.validate(&program, limits, budget)?;
    let resume = verify_resume(&program, resume, host_types, limits)?;
    Ok(CodePackage {
        metadata,
        distribution,
        effects,
        resume,
        visual_plan,
    })
}

fn verify_resume(
    program: &bc::VerifiedProgram,
    table: ResumeTable,
    host_types: &[bc::HostTypeSupport],
    limits: Limits,
) -> Result<VerifiedResumeTable, Error> {
    let records = table.flows.iter().try_fold(
        table.states.len() + usize::from(table.root.is_some()),
        |count, flow| {
            count
                .checked_add(1)?
                .checked_add(flow.layouts.len())?
                .checked_add(flow.points.len())?
                .checked_add(flow.returns.len())
        },
    );
    require(
        records.is_some_and(|count| count <= limits.max_metadata_records),
        ErrorKind::Limit,
        "resume metadata record budget exceeded",
    )?;
    program
        .verify_resume(
            table,
            host_types,
            ResumeLimits {
                work: limits.max_work,
                allocation_bytes: limits.max_metadata_bytes,
            },
        )
        .map_err(|error| Error::new(ErrorKind::Metadata, error.to_string()))
}

/// Read only the embedded distribution, without decoding bytecode or reading resources.
pub fn read_distribution(
    reader: &mut (impl Read + Seek),
    limits: Limits,
) -> io::Result<Option<Distribution>> {
    let mut budget = Budget::new(limits);
    let (_, sections) = read_index(reader, limits, &mut budget)?;
    let Some(section) = sections.iter().find(|section| section.kind == DISTRIBUTION) else {
        return Ok(None);
    };
    budget
        .charge(section.range.len())
        .map_err(io::Error::other)?;
    let json = read_range(reader, section.range.clone())?;
    check_digest(&json, &section.digest).map_err(io::Error::other)?;
    decode_distribution(&json, limits, &mut budget)
        .map(Some)
        .map_err(io::Error::other)
}

fn read_index(
    reader: &mut (impl Read + Seek),
    limits: Limits,
    budget: &mut Budget,
) -> io::Result<(usize, Vec<Section>)> {
    let total = usize::try_from(reader.seek(SeekFrom::End(0))?).map_err(io::Error::other)?;
    reader.seek(SeekFrom::Start(0))?;
    let mut header = [0; HEADER_LEN];
    budget.charge(HEADER_LEN).map_err(io::Error::other)?;
    reader.read_exact(&mut header)?;
    let index_range = parse_header(&header, total, limits).map_err(io::Error::other)?;
    budget.charge(index_range.len()).map_err(io::Error::other)?;
    let index = read_range(reader, index_range.clone())?;
    parse_index(&header, &index, index_range, total, limits)
        .map(|sections| (total, sections))
        .map_err(io::Error::other)
}

fn read_range(
    reader: &mut (impl Read + Seek),
    range: std::ops::Range<usize>,
) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(range.len())
        .map_err(io::Error::other)?;
    reader.seek(SeekFrom::Start(range.start as u64))?;
    reader.take(range.len() as u64).read_to_end(&mut bytes)?;
    if bytes.len() != range.len() {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
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
    for entry in bytes.as_chunks::<INDEX_ENTRY_LEN>().0 {
        let kind = u32_at(entry, 0);
        let flags = u32_at(entry, 4);
        require(kinds.insert(kind), Encoding, "duplicate section kind")?;
        require(flags & !REQUIRED == 0, Version, "unknown section flags")?;
        if matches!(kind, B01 | METADATA | DISTRIBUTION | EFFECTS | RESUME) {
            require(flags == REQUIRED, Encoding, "core section must be required")?;
        } else if kind == VISUAL_PLAN {
            require(flags == 0, Encoding, "visual plan section must be optional")?;
        } else {
            require(flags == 0, Version, "unknown required section")?;
        }
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
        } else if matches!(kind, METADATA | DISTRIBUTION | EFFECTS | RESUME) {
            require(
                section.len() <= limits.max_metadata_bytes,
                Limit,
                "metadata byte budget exceeded",
            )?;
        } else if kind == VISUAL_PLAN {
            require_visual_plan_size(section.len(), limits)?;
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

fn require_visual_plan_size(length: usize, limits: Limits) -> Result<(), Error> {
    require(
        length <= limits.max_metadata_bytes.min(MAX_VISUAL_PLAN_BYTES),
        ErrorKind::Limit,
        "visual plan byte budget exceeded",
    )
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
