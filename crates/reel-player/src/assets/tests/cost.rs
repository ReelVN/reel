//! Optional measurements over a caller-supplied release archive.
use reel_core::texture::{TextureCodec, TextureFormat, TextureHeader};
use reel_package::archive::{ArchiveIndex, ArchiveKind, ArchiveWriter, ReadScratch};
use std::{fs::File, path::PathBuf, time::Instant};

#[test]
#[ignore = "set REEL_RESOURCE_ARCHIVE and REEL_RESOURCE_COST_DIR to measure prepared texture storage"]
fn prepared_texture_archive_cost() {
    let path = PathBuf::from(std::env::var_os("REEL_RESOURCE_ARCHIVE").unwrap());
    let output = PathBuf::from(std::env::var_os("REEL_RESOURCE_COST_DIR").unwrap());
    std::fs::create_dir_all(&output).unwrap();
    let mut input = File::open(&path).unwrap();
    let index = ArchiveIndex::read(&mut input).unwrap();
    let rewritten = output.join("raw-textures.rpk");
    let mut writer =
        ArchiveWriter::new(File::create(&rewritten).unwrap(), ArchiveKind::Resources).unwrap();
    let mut scratch = ReadScratch::new();
    let mut textures = Vec::new();
    let mut kinds = [0usize; 4];
    let mut original_read_us = 0;
    let mut crunch_us = 0;
    for entry in index.entries() {
        let start = Instant::now();
        let mut bytes = vec![0; entry.length as usize];
        entry
            .read_range(&mut input, 0, &mut bytes, &mut scratch, || false)
            .unwrap();
        let read_us = start.elapsed().as_micros();
        if bytes.starts_with(TextureHeader::MAGIC) {
            let (header, payload) = TextureHeader::parse(&bytes).unwrap();
            let descriptor = header.descriptor();
            let start = Instant::now();
            let blocks = match header.codec() {
                TextureCodec::Raw => {
                    kinds[0] += 1;
                    payload.to_vec()
                }
                TextureCodec::BcCrunch => {
                    kinds[1] += 1;
                    let mut blocks = vec![0; descriptor.byte_len().unwrap() as usize];
                    bc_crunch::decompress_into(
                        descriptor.layout.storage_size[0],
                        descriptor.layout.storage_size[1],
                        match descriptor.format {
                            TextureFormat::Bc1Rgb => bc_crunch::Format::Bc1,
                            TextureFormat::Bc3Rgba => bc_crunch::Format::Bc3,
                            _ => unreachable!(),
                        },
                        payload,
                        &mut blocks,
                    )
                    .unwrap();
                    blocks
                }
            };
            let decode_us = start.elapsed().as_micros();
            original_read_us += read_us;
            crunch_us += decode_us;
            textures.push((
                entry.path.to_string(),
                descriptor,
                *blake3::hash(&blocks).as_bytes(),
            ));
            let mut raw = TextureHeader::new(descriptor, TextureCodec::Raw, blocks.len() as u64)
                .unwrap()
                .encode()
                .to_vec();
            raw.extend(blocks);
            writer.add(&entry.path, &mut raw.as_slice()).unwrap();
            eprintln!(
                "texture\t{}\t{:?}\t{:?}\t{}\t{}\t{}\t{}",
                entry.path,
                descriptor.format,
                header.codec(),
                entry.length,
                descriptor.byte_len().unwrap(),
                read_us,
                decode_us
            );
        } else {
            if bytes.starts_with(b"\x89PNG") {
                kinds[2] += 1;
            }
            if bytes.starts_with(b"\xff\xd8") {
                kinds[3] += 1;
            }
            writer.add(&entry.path, &mut bytes.as_slice()).unwrap();
        }
    }
    writer.finish().unwrap();
    let mut replacement = File::open(&rewritten).unwrap();
    let replacement_index = ArchiveIndex::read(&mut replacement).unwrap();
    let mut raw_read_us = 0;
    for (path, descriptor, expected) in &textures {
        let entry = replacement_index.get(path).unwrap();
        let start = Instant::now();
        let mut bytes = vec![0; entry.length as usize];
        entry
            .read_range(&mut replacement, 0, &mut bytes, &mut scratch, || false)
            .unwrap();
        raw_read_us += start.elapsed().as_micros();
        let (header, blocks) = TextureHeader::parse(&bytes).unwrap();
        assert_eq!(header.codec(), TextureCodec::Raw);
        assert_eq!(header.descriptor(), *descriptor);
        assert_eq!(*blake3::hash(blocks).as_bytes(), *expected);
    }
    if let Some(comparison) = std::env::var_os("REEL_RESOURCE_COMPARE_ARCHIVE") {
        let mut comparison = File::open(comparison).unwrap();
        let comparison_index = ArchiveIndex::read(&mut comparison).unwrap();
        for (path, descriptor, expected) in &textures {
            let entry = comparison_index.get(path).unwrap();
            let mut bytes = vec![0; entry.length as usize];
            entry
                .read_range(&mut comparison, 0, &mut bytes, &mut scratch, || false)
                .unwrap();
            let (header, blocks) = TextureHeader::parse(&bytes).unwrap();
            assert_eq!(header.codec(), TextureCodec::Raw, "{path}");
            assert_eq!(header.descriptor(), *descriptor, "{path}");
            assert_eq!(*blake3::hash(blocks).as_bytes(), *expected, "{path}");
        }
        eprintln!("compared_upload_payloads\t{}", textures.len());
    }
    eprintln!(
        "resource_cost\traw={}\tcrunch={}\tpng={}\tjpeg={}\toriginal_archive={}\traw_archive={}\toriginal_read_us={}\tcrunch_us={}\traw_read_us={}",
        kinds[0],
        kinds[1],
        kinds[2],
        kinds[3],
        input.metadata().unwrap().len(),
        replacement.metadata().unwrap().len(),
        original_read_us,
        crunch_us,
        raw_read_us
    );
}
