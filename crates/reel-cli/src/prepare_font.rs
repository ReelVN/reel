//! Offline font preparation for a project's explicit Unicode set.
use std::{
    borrow::Cow,
    collections::{BTreeSet, HashMap},
    fs,
    io::{self, Write},
    path::Path,
};

use read_fonts::{
    FileRef, FontRef, TableProvider, TopLevelTable,
    collections::IntSet,
    tables::{cmap::PlatformId, os2::Os2},
    types::{GlyphId, Tag},
};
use skera::{FastHashMap, Plan, SubsetFlags};
use write_fonts::{
    FontBuilder,
    tables::cmap::{Cmap, Dmap},
};

const CMAP: Tag = Tag::new(b"cmap");
const DMAP: Tag = Tag::new(b"DMAP");
const GASP: Tag = Tag::new(b"gasp");
const HEAD: Tag = Tag::new(b"head");
const NAME: Tag = Tag::new(b"name");
const DSIG: Tag = Tag::new(b"DSIG");
const OUTLINE_TAGS: [Tag; 4] = [
    Tag::new(b"glyf"),
    Tag::new(b"loca"),
    Tag::new(b"CFF "),
    Tag::new(b"CFF2"),
];

/// Subset TTF, OpenType/CFF, and TTC fonts to the requested Unicode characters.
///
/// The subset keeps the face order, names, style, layout closure, and shared
/// outlines. Complete coverage keeps the original font file byte for byte.
/// The workspace limit is an estimate, checked before the font is read.
pub fn prepare(
    source: &Path,
    output: &Path,
    characters: &BTreeSet<u32>,
    memory_bytes: u64,
) -> io::Result<u64> {
    if output.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("font output already exists: {}", output.display()),
        ));
    }
    if characters
        .iter()
        .any(|&value| char::from_u32(value).is_none())
    {
        return Err(invalid(
            "font character set contains an invalid Unicode scalar value",
        ));
    }
    let workspace = source
        .metadata()?
        .len()
        .checked_mul(12)
        .and_then(|bytes| bytes.checked_add((characters.len() as u64).checked_mul(128)?))
        .and_then(|bytes| bytes.checked_add(16 << 20))
        .ok_or_else(|| invalid("font workspace estimate overflows"))?;
    if workspace > memory_bytes {
        return Err(invalid(format!(
            "font preparation estimates {workspace} workspace bytes; limit is {memory_bytes}"
        )));
    }

    let data = fs::read(source)?;
    let output_bytes = subset(&data, &characters.iter().copied().collect())?;
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::Builder::new()
        .prefix(".reel-font-")
        .tempfile_in(parent)?;
    temporary.write_all(output_bytes.as_deref().unwrap_or(&data))?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(output)
        .map_err(|error| error.error)?;
    Ok(workspace)
}

fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}

fn coverage(font: &FontRef<'_>) -> io::Result<IntSet<u32>> {
    let cmap = font.cmap().map_err(invalid)?;
    let limits = read_fonts::tables::cmap::CmapIterLimits {
        max_char: 0x10ffff,
        glyph_count: u32::from(font.maxp().map_err(invalid)?.num_glyphs()),
    };
    let mut coverage = IntSet::empty();
    for record in cmap.encoding_records() {
        if record.platform_id() == PlatformId::Unicode
            || (record.platform_id() == PlatformId::Windows
                && matches!(record.encoding_id(), 0 | 1 | 10))
        {
            let table = record.subtable(cmap.offset_data()).map_err(invalid)?;
            coverage.extend_unsorted(
                table
                    .iter_with_limits(limits)
                    .filter_map(|(cp, glyph)| (glyph != GlyphId::NOTDEF).then_some(cp)),
            );
        }
    }
    Ok(coverage)
}

fn subset(source: &[u8], characters: &IntSet<u32>) -> io::Result<Option<Vec<u8>>> {
    let file = FileRef::new(source).map_err(invalid)?;
    let collection = matches!(file, FileRef::Collection(_));
    let faces = file
        .fonts()
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid)?;
    if faces.is_empty() {
        return Err(invalid("font collection is empty"));
    }
    let source_coverage = faces.iter().map(coverage).collect::<io::Result<Vec<_>>>()?;
    let source_styles = faces.iter().map(style).collect::<io::Result<Vec<_>>>()?;
    if source_coverage
        .iter()
        .all(|face| face.is_subset(characters))
    {
        return Ok(None);
    }
    let groups = shared_outline_groups(&faces)?;
    let mut shared_glyphs = vec![IntSet::empty(); faces.len()];
    for group in &groups {
        unify_outlines(&faces, group, characters, &mut shared_glyphs);
    }

    let mut subsets = Vec::with_capacity(faces.len());
    for (index, (face, gids)) in faces.iter().zip(&shared_glyphs).enumerate() {
        let prepared = subset_face(face, gids, characters)
            .map_err(|error| invalid(format!("font face {index}: {error}")))?;
        let after = FontRef::new(&prepared).map_err(invalid)?;
        let mut expected_coverage = source_coverage[index].clone();
        expected_coverage.intersect(characters);
        let actual_coverage = coverage(&after)?;
        if actual_coverage != expected_coverage {
            let missing = expected_coverage
                .iter()
                .find(|&cp| !actual_coverage.contains(cp));
            let extra = actual_coverage
                .iter()
                .find(|&cp| !expected_coverage.contains(cp));
            return Err(invalid(format!(
                "font face {index}: Unicode coverage changed outside the requested intersection (first missing: {missing:X?}, first extra: {extra:X?})"
            )));
        }
        if after.table_directory().sfnt_version() != face.table_directory().sfnt_version()
            || table_bytes(&after, NAME) != table_bytes(face, NAME)
            || style(&after)? != source_styles[index]
        {
            return Err(invalid(format!("font face {index}: name or style changed")));
        }
        subsets.push(prepared);
    }

    if !collection {
        return Ok(subsets.pop());
    }

    let faces = subsets
        .iter()
        .map(|bytes| FontRef::new(bytes))
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid)?;
    for group in &groups {
        for tag in OUTLINE_TAGS {
            let first = table_bytes(&faces[group[0]], tag);
            if group
                .iter()
                .any(|&index| table_bytes(&faces[index], tag) != first)
            {
                return Err(invalid(format!(
                    "font collection lost shared {tag} outlines"
                )));
            }
        }
    }
    pack_collection(&faces).map(Some)
}

fn table_bytes<'a>(font: &FontRef<'a>, tag: Tag) -> Option<&'a [u8]> {
    font.table_data(tag).map(|data| data.as_bytes())
}

#[derive(Debug, PartialEq, Eq)]
struct FontStyle {
    mac_style: u16,
    os2: Option<Os2Style>,
}

#[derive(Debug, PartialEq, Eq)]
struct Os2Style {
    weight_class: u16,
    width_class: u16,
    selection: u16,
}

fn style(font: &FontRef<'_>) -> io::Result<FontStyle> {
    let head = font.head().map_err(invalid)?;
    let os2 = font
        .table_data(Os2::TAG)
        .map(|_| {
            font.os2().map(|table| Os2Style {
                weight_class: table.us_weight_class(),
                width_class: table.us_width_class(),
                selection: table.fs_selection().bits(),
            })
        })
        .transpose()
        .map_err(invalid)?;
    Ok(FontStyle {
        mac_style: head.mac_style().bits(),
        os2,
    })
}

type OutlineTables = [Option<(u32, u32)>; OUTLINE_TAGS.len()];

fn shared_outline_groups(faces: &[FontRef<'_>]) -> io::Result<Vec<Vec<usize>>> {
    let mut groups: HashMap<(OutlineTables, u16, i16), Vec<usize>> = HashMap::new();
    for (index, face) in faces.iter().enumerate() {
        let records = face.table_directory().table_records();
        let tables = OUTLINE_TAGS.map(|tag| {
            records
                .iter()
                .find(|record| record.tag() == tag)
                .map(|record| (record.offset(), record.length()))
        });
        if tables.iter().all(Option::is_none) {
            continue;
        }
        let glyph_count = face.maxp().map_err(invalid)?.num_glyphs();
        let loca_format = face.head().map_err(invalid)?.index_to_loc_format();
        groups
            .entry((tables, glyph_count, loca_format))
            .or_default()
            .push(index);
    }
    Ok(groups
        .into_values()
        .filter(|indices| indices.len() > 1)
        .collect())
}

fn subset_flags() -> SubsetFlags {
    SubsetFlags::SUBSET_FLAGS_NOTDEF_OUTLINE
        | SubsetFlags::SUBSET_FLAGS_GLYPH_NAMES
        | SubsetFlags::SUBSET_FLAGS_NO_BIDI_CLOSURE
}

fn font_plan(font: &FontRef<'_>, glyphs: &IntSet<GlyphId>, characters: &IntSet<u32>) -> Plan {
    let mut plan = Plan::new(
        glyphs,
        characters,
        font,
        subset_flags(),
        &IntSet::from([DSIG]),
        &IntSet::all(),
        &IntSet::all(),
        &IntSet::all(),
        &IntSet::all(),
    );
    plan.set_no_subset_tables(
        &[
            NAME,
            GASP,
            Tag::new(b"fpgm"),
            Tag::new(b"prep"),
            Tag::new(b"VDMX"),
        ]
        .into(),
    );
    plan
}

fn unify_outlines(
    faces: &[FontRef<'_>],
    indices: &[usize],
    characters: &IntSet<u32>,
    shared: &mut [IntSet<GlyphId>],
) {
    let mut union = IntSet::empty();
    let mut sets = Vec::with_capacity(indices.len());
    let retained_glyphs = |index: usize, glyphs: &IntSet<GlyphId>| {
        font_plan(&faces[index], glyphs, characters)
            .old_to_new_glyph_mapping()
            .map(|(original, _)| original)
            .collect::<IntSet<_>>()
    };
    for &index in indices {
        let glyphs = retained_glyphs(index, &IntSet::empty());
        union.union(&glyphs);
        sets.push(glyphs);
    }
    if sets.iter().all(|glyphs| *glyphs == union) {
        return;
    }

    // The shared outlines require the same compact glyph IDs in every face.
    // Extra glyphs from a sibling may themselves trigger layout closure.
    loop {
        let count = union.len();
        for &index in indices {
            let glyphs = retained_glyphs(index, &union);
            union.union(&glyphs);
        }
        if union.len() == count {
            break;
        }
    }
    for (&index, original) in indices.iter().zip(sets) {
        if original != union {
            shared[index] = union.clone();
        }
    }
}

fn subset_face(
    font: &FontRef<'_>,
    glyphs: &IntSet<GlyphId>,
    characters: &IntSet<u32>,
) -> io::Result<Vec<u8>> {
    let gasp = font
        .table_data(GASP)
        .map(normalize_gasp)
        .transpose()?
        .flatten();
    let full_plan = font_plan(font, glyphs, characters);
    let prepared = skera::subset_font(font, &full_plan).map_err(invalid)?;
    let prepared_font = FontRef::new(&prepared).map_err(invalid)?;
    if glyphs.is_empty() && gasp.is_none() && prepared_font.table_data(CMAP).is_some() {
        return Ok(prepared);
    }

    let mut builder = FontBuilder::new();
    if let Some(gasp) = gasp {
        builder.add_raw(GASP, gasp);
    }
    let subset_cmap;
    let cmap_font = if glyphs.is_empty() {
        prepared_font.clone()
    } else {
        // Skera's GID selection also keeps every cmap alias of selected GIDs.
        // Give it the final glyph map separately when selecting just the cmap.
        let mut drop_tables = IntSet::all();
        drop_tables.remove(CMAP);
        drop_tables.remove(DMAP);
        let glyph_map: FastHashMap<_, _> = full_plan.old_to_new_glyph_mapping().collect();
        let cmap_plan = Plan::new_with_glyph_map(
            &IntSet::empty(),
            characters,
            font,
            subset_flags(),
            &drop_tables,
            &IntSet::all(),
            &IntSet::all(),
            &IntSet::all(),
            &IntSet::all(),
            &glyph_map,
        )
        .map_err(invalid)?;
        subset_cmap = skera::subset_font(font, &cmap_plan).map_err(invalid)?;
        FontRef::new(&subset_cmap).map_err(invalid)?
    };
    if let Some(cmap) = table_bytes(&cmap_font, CMAP) {
        builder.add_raw(CMAP, cmap);
    } else {
        builder.add_table(&Cmap::new(Vec::new())).map_err(invalid)?;
    }
    if let Some(dmap) = table_bytes(&cmap_font, DMAP) {
        builder.add_raw(DMAP, dmap);
    } else if prepared_font.table_data(DMAP).is_some() {
        builder.add_table(&Dmap::new(Vec::new())).map_err(invalid)?;
    }
    builder.copy_missing_tables(prepared_font);
    Ok(builder.build())
}

fn normalize_gasp(data: read_fonts::FontData<'_>) -> io::Result<Option<&[u8]>> {
    let version = data.read_at::<u16>(0).map_err(invalid)?;
    let count = data.read_at::<u16>(2).map_err(invalid)?;
    let length = 4 + usize::from(count) * 4;
    let bytes = data.as_bytes();
    if version > 1 || length > bytes.len() || bytes[length..].iter().any(|&byte| byte != 0) {
        return Err(invalid("invalid gasp table"));
    }
    Ok((length < bytes.len()).then(|| &bytes[..length]))
}

// OpenType collection v1.0 places face directories and shared, aligned tables
// after the header. Table offsets are absolute from the start of the collection.
fn pack_collection(faces: &[FontRef<'_>]) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"ttcf");
    bytes.extend_from_slice(&1u16.to_be_bytes());
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&collection_offset(faces.len())?.to_be_bytes());
    bytes.resize(12 + 4 * faces.len(), 0);
    let mut tables = HashMap::<(Tag, Cow<'_, [u8]>), u32>::new();
    for (index, face) in faces.iter().enumerate() {
        let face_offset = collection_offset(bytes.len())?;
        bytes[12 + index * 4..16 + index * 4].copy_from_slice(&face_offset.to_be_bytes());
        let directory = face.table_directory();
        bytes.extend_from_slice(&directory.sfnt_version().to_be_bytes());
        bytes.extend_from_slice(&directory.num_tables().to_be_bytes());
        bytes.extend_from_slice(&directory.search_range().to_be_bytes());
        bytes.extend_from_slice(&directory.entry_selector().to_be_bytes());
        bytes.extend_from_slice(&directory.range_shift().to_be_bytes());
        let mut record_start = bytes.len();
        bytes.resize(record_start + directory.table_records().len() * 16, 0);
        for record in directory.table_records() {
            let tag = record.tag();
            let mut data = Cow::Borrowed(
                face.table_data(tag)
                    .ok_or_else(|| invalid(format!("font face {index}: missing {tag} table data")))?
                    .as_bytes(),
            );
            if tag == HEAD {
                // The whole-font adjustment is unused in a TTC.
                data.to_mut()[8..12].fill(0);
            }
            let offset = match tables.entry((tag, data)) {
                std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let offset = collection_offset(bytes.len())?;
                    bytes.extend_from_slice(&entry.key().1);
                    bytes.resize(bytes.len().next_multiple_of(4), 0);
                    entry.insert(offset);
                    offset
                }
            };
            let fields = [
                tag.to_be_bytes(),
                record.checksum().to_be_bytes(),
                offset.to_be_bytes(),
                record.length().to_be_bytes(),
            ];
            for field in fields {
                bytes[record_start..record_start + 4].copy_from_slice(&field);
                record_start += 4;
            }
        }
    }
    Ok(bytes)
}

fn collection_offset(value: usize) -> io::Result<u32> {
    u32::try_from(value).map_err(|_| invalid("font collection exceeds OpenType's 32-bit offsets"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGULAR: &[u8] = include_bytes!("../../reel-player/tests/fixtures/font-weight/400.ttf");
    const BOLD: &[u8] = include_bytes!("../../reel-player/tests/fixtures/font-weight/700.ttf");

    #[test]
    fn invalid_unicode_and_low_budget_preserve_output() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("input.ttf");
        let output = directory.path().join("output.ttf");
        fs::write(&source, b"source").unwrap();
        assert!(prepare(&source, &output, &BTreeSet::from([0xd800]), 128 << 20).is_err());
        assert!(prepare(&source, &output, &BTreeSet::from([65]), 1 << 20).is_err());
        assert!(!output.exists());
    }

    #[test]
    fn retains_collection_styles_and_only_requested_cmap() {
        let fonts = [REGULAR, BOLD].map(|data| FontRef::new(data).unwrap());
        let collection = pack_collection(&fonts).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("collection.ttc");
        let output = directory.path().join("subset.ttc");
        fs::write(&source, &collection).unwrap();
        prepare(&source, &output, &BTreeSet::from([65]), 128 << 20).unwrap();
        let bytes = fs::read(&output).unwrap();
        let FileRef::Collection(collection) = FileRef::new(&bytes).unwrap() else {
            panic!("subset should preserve the collection");
        };
        assert_eq!(collection.len(), 2);
        for (before, after) in fonts.iter().zip(collection.iter()) {
            let face = after.unwrap();
            assert_eq!(coverage(&face).unwrap(), [65].into());
            assert_eq!(style(before).unwrap(), style(&face).unwrap());
            assert_eq!(table_bytes(before, NAME), table_bytes(&face, NAME));
        }
        assert_eq!(
            prepare(&source, &output, &BTreeSet::from([65]), 128 << 20)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
    }

    #[test]
    fn shares_outlines_with_distinct_collection_cmaps() {
        let font = FontRef::new(REGULAR).unwrap();
        let space = font.cmap().unwrap().map_codepoint(32u32).unwrap();
        let mut builder = FontBuilder::new();
        builder
            .add_table(&Cmap::from_mappings([(' ', space), ('A', space), ('B', space)]).unwrap())
            .unwrap();
        builder.copy_missing_tables(font.clone());
        let variant = builder.build();
        let collection = pack_collection(&[font, FontRef::new(&variant).unwrap()]).unwrap();
        let characters = IntSet::from([65]);
        let bytes = subset(&collection, &characters).unwrap().unwrap();
        let FileRef::Collection(subsets) = FileRef::new(&bytes).unwrap() else {
            panic!("subset should preserve the collection");
        };
        assert_eq!(subsets.len(), 2);
        let first = subsets.get(0).unwrap();
        let second = subsets.get(1).unwrap();
        let glyph = |font: &FontRef<'_>| font.cmap().unwrap().map_codepoint(65u32).unwrap();
        assert_ne!(glyph(&first), glyph(&second));
        assert_eq!(coverage(&first).unwrap(), characters);
        assert_eq!(coverage(&second).unwrap(), characters);
        for tag in [Tag::new(b"glyf"), Tag::new(b"loca")] {
            let offset = |font: &FontRef<'_>| {
                font.table_directory()
                    .table_records()
                    .iter()
                    .find(|record| record.tag() == tag)
                    .unwrap()
                    .offset()
            };
            assert_eq!(offset(&first), offset(&second));
        }
        let bytes = subset(&collection, &[66].into()).unwrap().unwrap();
        let FileRef::Collection(subsets) = FileRef::new(&bytes).unwrap() else {
            panic!("subset should preserve the collection");
        };
        assert!(coverage(&subsets.get(0).unwrap()).unwrap().is_empty());
        assert_eq!(coverage(&subsets.get(1).unwrap()).unwrap(), [66].into());
        assert!(subset(&collection, &[32, 65, 66].into()).unwrap().is_none());
    }

    #[test]
    fn normalizes_only_zero_gasp_padding() {
        let source = FontRef::new(REGULAR).unwrap();
        let normal = [0, 0, 0, 2, 0, 24, 0, 1, 255, 255, 0, 3];
        let with_gasp = |gasp: &[u8]| {
            let mut builder = FontBuilder::new();
            builder
                .add_raw(GASP, gasp)
                .copy_missing_tables(source.clone());
            builder.build()
        };
        let mut padded = normal.to_vec();
        padded.extend([0; 4]);
        let characters = IntSet::from([65]);
        let result = subset(&with_gasp(&padded), &characters).unwrap().unwrap();
        let face = FontRef::new(&result).unwrap();
        assert_eq!(face.table_data(GASP).unwrap().as_bytes(), &normal);
        assert_eq!(coverage(&face).unwrap(), characters);

        *padded.last_mut().unwrap() = 1;
        assert!(subset(&with_gasp(&padded), &characters).is_err());
        assert!(subset(&with_gasp(&normal[..10]), &characters).is_err());
    }
}
