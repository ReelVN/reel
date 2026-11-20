//! Deterministic bounded offline atlas preparation for one explicit cohort.
use super::*;
use image::{DynamicImage, Rgba32FImage};
use reel_core::{
    prepared_atlas::PreparedAtlasView,
    texture::{
        ColorEncoding, ImageView, TextureDescriptor, TextureFormat, TextureHeader, TextureLayout,
        TextureStorageLayout,
    },
};
use std::io::Write;

struct Source {
    name: String,
    path: PathBuf,
    hash: blake3::Hash,
    bytes: u64,
}

struct Item<'a> {
    source: &'a Source,
    logical: [u32; 2],
    content: [u32; 2],
    origin: [u32; 2],
}

/// Publish one page and one view file per original path in an immutable object.
pub(super) fn prepare(
    root: &Path,
    target: &ResourceTarget,
    recipe: &Recipe,
    members: Vec<(String, PathBuf)>,
    memory_bytes: u64,
    identity: &str,
    prepared: &mut Prepared,
) -> io::Result<()> {
    let Recipe::Atlas {
        scale,
        format,
        dither,
        page_size,
        fallback,
    } = *recipe
    else {
        unreachable!()
    };
    let mut hash = blake3::Hasher::new();
    hash.update(b"Reel atlas cohort v1\0");
    hash.update(identity.as_bytes());
    hash.update(target.as_str().as_bytes());
    hash.update(&serde_json::to_vec(recipe).map_err(io::Error::other)?);
    let mut sources = Vec::with_capacity(members.len());
    for (name, path) in members {
        let path = path.canonicalize()?;
        let bytes = fs::metadata(&path)?.len();
        let source_hash = hash_file(&path)?;
        hash.update(&(name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update(source_hash.as_bytes());
        sources.push(Source {
            name,
            path,
            hash: source_hash,
            bytes,
        });
    }
    let key = hash.finalize().to_hex().to_string();
    let object = root.join("build/resources/objects").join(&key);
    let page = object.join("page.reel-texture");
    let page_name = format!("__reel/atlas/{key}/page.reel-texture");
    let present = page.is_file()
        && (0..sources.len()).all(|i| object.join(format!("{i}.reel-atlas")).is_file());
    if present {
        prepared.report.reused += sources.len();
    } else {
        let mut items = Vec::with_capacity(sources.len());
        let mut max_source = 0u128;
        for source in &sources {
            let (decoder, codec_workspace) = reel_player::artwork::open(&source.path, memory_bytes)
                .map_err(|error| invalid(format!("atlas image {:?}: {error}", source.name)))?;
            let (w, h) = decoder.dimensions();
            let logical = [w, h];
            let content =
                prepare_texture::scaled_content_size(logical, None, scale).map_err(invalid)?;
            if content
                .iter()
                .zip(page_size)
                .any(|(&n, page)| n > page.saturating_sub(8))
            {
                return Err(invalid(format!(
                    "atlas image {:?} exceeds page_size including gutter",
                    source.name
                )));
            }
            let decoded = u128::from(decoder.total_bytes());
            max_source = max_source.max(
                (u128::from(w) * u128::from(h) * 48 + decoded)
                    .max(decoded + u128::from(codec_workspace)),
            );
            items.push(Item {
                source,
                logical,
                content,
                origin: [0; 2],
            });
        }
        let used = pack(&mut items, page_size)?;
        let vita = target.as_str() == "vita";
        let rgba_size = if vita {
            [used[0].next_multiple_of(8), used[1]]
        } else {
            used
        };
        let bc_size = if vita {
            used.map(u32::next_power_of_two)
        } else {
            used
        };
        let fits = |storage: [u32; 2]| {
            storage
                .into_iter()
                .zip(page_size)
                .all(|(used, limit)| used <= limit)
        };
        let try_bc = !matches!(format, prepare_texture::Format::Rgba8) && fits(bc_size);
        if !fits(rgba_size)
            || (!matches!(format, prepare_texture::Format::Rgba8) && !try_bc && fallback.is_none())
        {
            return Err(invalid("aligned atlas storage exceeds page_size"));
        }
        let workspace_size = if try_bc { bc_size } else { rgba_size };
        let workspace = u128::from(workspace_size[0]) * u128::from(workspace_size[1]) * 80
            + max_source
            + (items.len() as u128 * 2048)
            + (1 << 20);
        let workspace =
            u64::try_from(workspace).map_err(|_| invalid("atlas workspace overflow"))?;
        if workspace > memory_bytes {
            return Err(invalid(format!(
                "atlas needs {workspace} workspace bytes; limit is {memory_bytes}"
            )));
        }
        let objects = object.parent().unwrap();
        fs::create_dir_all(objects)?;
        let staging = tempfile::Builder::new()
            .prefix(".atlas-")
            .tempdir_in(objects)?;
        let mut atlas = Rgba32FImage::from_pixel(used[0], used[1], image::Rgba([0., 0., 0., 1.]));
        let mut opaque = true;
        for item in &items {
            let (decoder, _) =
                reel_player::artwork::open(&item.source.path, memory_bytes).map_err(invalid)?;
            let pixels = DynamicImage::from_decoder(decoder).map_err(invalid)?;
            let options = prepare_texture::Options {
                format: prepare_texture::Format::Rgba8,
                ..Default::default()
            };
            let (descriptor, linear) =
                prepare_texture::prepare_linear(pixels, options, scale).map_err(invalid)?;
            if descriptor.layout.logical_size != item.logical
                || descriptor.layout.content_size != item.content
            {
                return Err(invalid(format!(
                    "atlas image changed during preparation: {}",
                    item.source.path.display()
                )));
            }
            opaque &= linear.pixels().all(|p| p[3] == 1.);
            let padded = item.content.map(|n| n.next_multiple_of(4) + 8);
            for y in 0..padded[1] {
                for x in 0..padded[0] {
                    let sx = x.saturating_sub(4).min(item.content[0] - 1);
                    let sy = y.saturating_sub(4).min(item.content[1] - 1);
                    atlas.put_pixel(
                        item.origin[0] - 4 + x,
                        item.origin[1] - 4 + y,
                        *linear.get_pixel(sx, sy),
                    );
                }
            }
            if hash_file(&item.source.path)? != item.source.hash {
                return Err(invalid(format!(
                    "atlas image changed during preparation: {}",
                    item.source.path.display()
                )));
            }
        }
        let format = match format {
            prepare_texture::Format::Auto if opaque => TextureFormat::Bc1Rgb,
            prepare_texture::Format::Auto | prepare_texture::Format::Bc3 => TextureFormat::Bc3Rgba,
            prepare_texture::Format::Bc1 if opaque => TextureFormat::Bc1Rgb,
            prepare_texture::Format::Bc1 => {
                return Err(invalid("atlas BC1 requires opaque images"));
            }
            prepare_texture::Format::Rgba8 => TextureFormat::Rgba8,
        };
        let mut descriptor = TextureDescriptor {
            storage_layout: if vita && try_bc {
                TextureStorageLayout::GxmBcMorton
            } else {
                TextureStorageLayout::Linear
            },
            mip_count: 1,
            format,
            layout: TextureLayout {
                storage_size: if try_bc { bc_size } else { rgba_size },
                ..TextureLayout::identity(used)
            },
            source_encoding: ColorEncoding::Srgb,
        };
        let blocks = if try_bc {
            let (blocks, meets_quality) = encode_independent_blocks(&atlas, descriptor, &items)?;
            (meets_quality || fallback.is_none()).then_some(blocks)
        } else {
            None
        };
        let payload = match blocks {
            Some(blocks) => blocks,
            None => {
                descriptor.format = TextureFormat::Rgba8;
                descriptor.storage_layout = TextureStorageLayout::Linear;
                descriptor.layout.storage_size = rgba_size;
                prepare_texture::validate_target_extent(descriptor, vita).map_err(invalid)?;
                prepare_texture::quantized_pixels(&atlas, descriptor, dither).into_raw()
            }
        };
        let header = TextureHeader::new(descriptor, payload.len() as u64).map_err(invalid)?;
        let mut file = File::create(staging.path().join("page.reel-texture"))?;
        file.write_all(&header.encode())?;
        file.write_all(&payload)?;
        drop(file);
        for (i, item) in items.iter().enumerate() {
            let view = PreparedAtlasView {
                page: page_name.clone(),
                image: ImageView {
                    logical_size: item.logical,
                    content_origin: item.origin,
                    content_size: item.content,
                },
                encoding: ColorEncoding::Srgb,
            };
            fs::write(
                staging.path().join(format!("{i}.reel-atlas")),
                view.encode().map_err(invalid)?,
            )?;
        }
        match fs::rename(staging.path(), &object) {
            Ok(()) => {}
            Err(_) if page.is_file() => {}
            Err(error) => return Err(error),
        }
        prepared.report.converted += items.len();
        prepared.report.workspace_bytes = prepared.report.workspace_bytes.max(workspace);
    }
    for (i, source) in sources.into_iter().enumerate() {
        prepared.report.source_bytes += source.bytes;
        insert_prepared(
            prepared,
            source.name,
            object.join(format!("{i}.reel-atlas")),
        )?;
    }
    insert_prepared(prepared, page_name, page)?;
    Ok(())
}

/// Fit each image against its own visible pixels, then copy its original blocks
/// into the page. Gutter blocks are encoded separately and never share a block
/// with another image.
fn encode_independent_blocks(
    atlas: &Rgba32FImage,
    page: TextureDescriptor,
    items: &[Item<'_>],
) -> io::Result<(Vec<u8>, bool)> {
    let size = page.layout.storage_size;
    let block_bytes = page.format.block_bytes() as usize;
    let mut result = vec![0; page.byte_len().map_err(invalid)? as usize];
    let mut meets_quality = true;
    let format = match page.format {
        TextureFormat::Bc1Rgb => rgbcx::Format::Bc1,
        TextureFormat::Bc3Rgba => rgbcx::Format::Bc3,
        _ => unreachable!(),
    };
    for item in items {
        let padded = item.content.map(|n| n.next_multiple_of(4) + 8);
        let region = Rgba32FImage::from_fn(padded[0], padded[1], |x, y| {
            *atlas.get_pixel(item.origin[0] - 4 + x, item.origin[1] - 4 + y)
        });
        let region_descriptor = TextureDescriptor {
            storage_layout: TextureStorageLayout::Linear,
            mip_count: 1,
            layout: TextureLayout::identity(padded),
            ..page
        };
        let quantized = prepare_texture::quantized_pixels(
            &region,
            region_descriptor,
            prepare_texture::Dither::None,
        );
        let gutters = rgbcx::encode(
            padded[0],
            padded[1],
            quantized.as_raw(),
            format,
            rgbcx::Quality::Balanced,
        )
        .map_err(invalid)?;
        for y in 0..padded[1] / 4 {
            for x in 0..padded[0] / 4 {
                let from = ((y * (padded[0] / 4) + x) as usize) * block_bytes;
                let to = page.storage_layout.block_offset(
                    size,
                    block_bytes,
                    (item.origin[0] - 4) / 4 + x,
                    (item.origin[1] - 4) / 4 + y,
                );
                result[to..to + block_bytes].copy_from_slice(&gutters[from..from + block_bytes]);
            }
        }
        let visible = Rgba32FImage::from_fn(item.content[0], item.content[1], |x, y| {
            *atlas.get_pixel(item.origin[0] + x, item.origin[1] + y)
        });
        let storage = item.content.map(|n| n.next_multiple_of(4));
        let descriptor = TextureDescriptor {
            storage_layout: TextureStorageLayout::Linear,
            mip_count: 1,
            layout: TextureLayout {
                logical_size: item.logical,
                content_size: item.content,
                storage_size: storage,
            },
            ..page
        };
        let (blocks, acceptable) =
            prepare_texture::quality::encode(&visible, descriptor).map_err(invalid)?;
        meets_quality &= acceptable;
        for y in 0..storage[1] / 4 {
            for x in 0..storage[0] / 4 {
                let from = ((y * (storage[0] / 4) + x) as usize) * block_bytes;
                let to = page.storage_layout.block_offset(
                    size,
                    block_bytes,
                    item.origin[0] / 4 + x,
                    item.origin[1] / 4 + y,
                );
                result[to..to + block_bytes].copy_from_slice(&blocks[from..from + block_bytes]);
            }
        }
    }
    Ok((result, meets_quality))
}

/// Deterministic guillotiere allocation. Every rectangle has a
/// four-texel edge gutter and starts on a compression-block boundary.
fn pack(items: &mut [Item<'_>], size: [u32; 2]) -> io::Result<[u32; 2]> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by_key(|&i| {
        let [w, h] = items[i].content.map(|n| n.next_multiple_of(4) + 8);
        (
            std::cmp::Reverse(w.max(h)),
            std::cmp::Reverse(u64::from(w) * u64::from(h)),
            items[i].source.name.as_str(),
        )
    });
    let mut allocator =
        guillotiere::SimpleAtlasAllocator::new(guillotiere::size2(size[0] as i32, size[1] as i32));
    for i in order {
        let [w, h] = items[i].content.map(|n| n.next_multiple_of(4) + 8);
        let rectangle = allocator
            .allocate(guillotiere::size2(w as i32, h as i32))
            .ok_or_else(|| {
                invalid(format!(
                    "atlas page cannot fit {:?}; enlarge page_size or split the cohort",
                    items[i].source.name
                ))
            })?;
        items[i].origin = [rectangle.min.x as u32 + 4, rectangle.min.y as u32 + 4];
    }
    let mut used = [8, 8];
    for item in items {
        for (axis, end) in used.iter_mut().enumerate() {
            *end = (*end).max(item.origin[axis] + item.content[axis].next_multiple_of(4) + 4);
        }
    }
    Ok(used)
}
