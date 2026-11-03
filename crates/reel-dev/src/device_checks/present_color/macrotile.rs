//! Compare ordered macrocell feedback with separate RGBA8 scene writebacks.
use super::{Blend, FragmentOutput, Result, Target, TextureFilter, log, native};

const SOURCE: [u32; 2] = [29, 19];
const SIZE: [u32; 2] = [137, 93];
const MIXED: [[u32; 2]; 4] = [[31, 33], [137, 93], [65, 129], [193, 97]];

fn pattern() -> Vec<u8> {
    let mut bytes = Vec::with_capacity((SOURCE[0] * SOURCE[1] * 4) as usize);
    for y in 0..SOURCE[1] {
        for x in 0..SOURCE[0] {
            let alpha = [0u8, 1, 63, 127, 128, 191, 254, 255][((x + y * 3) % 8) as usize];
            let channels = if x == 0 || y == 0 || x + 1 == SOURCE[0] || y + 1 == SOURCE[1] {
                [255u8, ((x * 17) % 256) as u8, ((y * 29) % 256) as u8]
            } else {
                [
                    ((x * 37 + y * 11) % 256) as u8,
                    ((x * 13 + y * 71) % 256) as u8,
                    ((x * 97 + y * 7) % 256) as u8,
                ]
            };
            bytes.extend(
                channels.map(|channel| ((u32::from(channel) * u32::from(alpha) + 127) / 255) as u8),
            );
            bytes.push(alpha);
        }
    }
    bytes
}

fn stage(
    context: &native::Context,
    target: &native::Framebuffer,
    source: &native::Texture,
    program: &native::Program,
    vertices: &native::VertexBuffer,
    sampling: (Blend, TextureFilter),
    index: usize,
) -> Result {
    let (feedback_blend, filter) = sampling;
    // The first cell of each four-cell producer blends over a nonzero-alpha
    // backdrop. Distinct global clear colors expose incorrect cell offsets.
    context.clear(
        Target::Framebuffer(target),
        [
            0.0625 * (index % 4 + 1) as f32 + 0.0078125 * (index / 4) as f32,
            0.0625,
            0.015625,
            0.375,
        ],
    )?;
    let image = program.uniform("u_image").ok_or("missing RGBA sampler")?;
    context.draw_filtered(
        native::Draw {
            target: Target::Framebuffer(target),
            size: target.size(),
            program,
            vertices,
            instances: None,
            indices: None,
            range: 0..3,
            textures: &[source],
            uniforms: &[native::UniformValue::Int(&image, 0)],
            blend: if index.is_multiple_of(4) {
                Blend::Premultiplied
            } else {
                feedback_blend
            },
            scissor: None,
        },
        filter,
    )?;
    Ok(())
}

fn read(context: &native::Context, target: &native::Framebuffer) -> Result<Vec<u8>> {
    let size = target.size();
    let mut pixels = vec![0; (size[0] * size[1] * 4) as usize];
    context.read_rgba(target, 0, &mut pixels)?;
    Ok(pixels)
}

pub(super) fn run(context: &native::Context) -> Result {
    context.finish()?;
    let program = super::sampling_program(
        context,
        FragmentOutput::Patched,
        include_bytes!(concat!(env!("OUT_DIR"), "/present-color-rgba.gxp")),
    )?;
    let source = context.create_target_texture(SOURCE, native::TargetFormat::Rgba8)?;
    context.upload(&source, [0; 2], SOURCE, &pattern(), SOURCE[0] as usize * 4)?;
    // UVs deliberately cross all four texture edges. CLAMP must use each cell's
    // own view dimensions and stride, including the last partial row and column.
    let vertices = context.vertex_buffer(
        &[
            -1., 1., -0.25, -0.25, 3., 1., 2.75, -0.25, -1., -3., -0.25, 2.75,
        ],
        4,
        &super::attributes(),
    )?;
    let mut cases = 0;
    let mut cells_checked = 0;
    let mut pixels_checked = 0;
    let mut changed = 0;
    let mut maximum = 0;
    let mut nonopaque = 0;
    let mut structural_errors = 0;
    let mut extent_errors = 0;
    // Retain every atlas so each cold case starts with distinct storage.
    let mut retained_atlases = Vec::new();
    let directory = super::super::directory();
    std::fs::create_dir_all(&directory)?;
    for (mixed, filter) in [
        (false, TextureFilter::Nearest),
        (false, TextureFilter::Linear),
        (true, TextureFilter::Nearest),
        (true, TextureFilter::Linear),
    ] {
        let family = if mixed { "mixed" } else { "uniform" };
        let filter_name = if filter == TextureFilter::Nearest {
            "nearest"
        } else {
            "linear"
        };
        for count in [2, 3, 4, 8, 12, 16] {
            // Existing mixed cases prove nonuniform aliases. Larger uniform
            // cases cover two, three and four independent four-cell producers
            // without retaining another complete large mixed-size matrix.
            if mixed && count > 4 {
                continue;
            }
            let sizes = (0..count)
                .map(|index| if mixed { MIXED[index] } else { SIZE })
                .collect::<Vec<_>>();
            let sizes = sizes.as_slice();
            let cell = [0, 1].map(|axis| {
                sizes
                    .iter()
                    .map(|size| size[axis])
                    .max()
                    .unwrap()
                    .next_multiple_of(128)
            });
            let grid = match count {
                2 => [1, 2],
                3 | 4 => [2, 2],
                8 => [2, 4],
                12 => [3, 4],
                16 => [4, 4],
                _ => unreachable!(),
            };
            // Allocate every target before the dependent draw sequences, so
            // allocation or program setup cannot accidentally end a macro scene.
            let layout = native::MacrotileLayout::new(cell, grid)?;
            // Keep both allocations alive: each blend starts with independent
            // color storage, then repeats against its own already-written atlas.
            let macro_targets = [
                context.create_macrotile_target(layout, sizes)?,
                context.create_macrotile_target(layout, sizes)?,
            ];
            retained_atlases.extend(macro_targets.iter().cloned());
            let mut ordinary = Vec::new();
            for &size in sizes {
                let texture = context.create_target_texture(size, native::TargetFormat::Rgba8)?;
                ordinary.push(context.framebuffer(&texture)?);
            }
            for (blend_index, feedback_blend) in [Blend::Premultiplied, Blend::Replace]
                .into_iter()
                .enumerate()
            {
                let macro_target = &macro_targets[blend_index];
                for repetition in 0..2 {
                    context.finish()?;
                    let baseline_before = context.memory_usage()?.scene_submissions;
                    for (index, target) in ordinary.iter().enumerate() {
                        let input = if index.is_multiple_of(4) {
                            &source
                        } else {
                            ordinary[index - 1].texture()
                        };
                        stage(
                            context,
                            target,
                            input,
                            &program,
                            &vertices,
                            (feedback_blend, filter),
                            index,
                        )?;
                    }
                    context.finish()?;
                    let baseline_scenes =
                        context.memory_usage()?.scene_submissions - baseline_before;
                    if baseline_scenes != count as u64 {
                        structural_errors += 1;
                    }
                    let expected = ordinary
                        .iter()
                        .map(|target| read(context, target))
                        .collect::<Result<Vec<_>>>()?;
                    let before = context.memory_usage()?.scene_submissions;
                    for index in 0..count {
                        let input = if index.is_multiple_of(4) {
                            &source
                        } else {
                            macro_target.cell(index - 1).unwrap().texture()
                        };
                        stage(
                            context,
                            macro_target.cell(index).unwrap(),
                            input,
                            &program,
                            &vertices,
                            (feedback_blend, filter),
                            index,
                        )?;
                    }
                    // Do not finish or read an intermediate cell: these assertions
                    // make a serial-per-stage implementation fail this very oracle.
                    let early = context.memory_usage()?.scene_submissions - before;
                    if early != 0 {
                        structural_errors += 1;
                    }
                    context.finish()?;
                    let submitted = context.memory_usage()?.scene_submissions - before;
                    if submitted != 1 {
                        structural_errors += 1;
                    }
                    log(format_args!(
                        "MACROTILE SCENES case={cases} family={family} cells={count} chains={} grid={grid:?} filter={filter:?} source_blend=Premultiplied feedback_blend={feedback_blend:?} repetition={repetition} baseline={baseline_scenes} early={early} macro={submitted} expected_baseline={count} expected_early=0 expected_macro=1",
                        count.div_ceil(4),
                    ));
                    for (index, reference) in expected.iter().enumerate() {
                        let actual = read(context, macro_target.cell(index).unwrap())?;
                        if macro_target.cell(index).unwrap().size() != sizes[index]
                            || actual.len() != reference.len()
                        {
                            extent_errors += 1;
                        }
                        let size = sizes[index];
                        let mut local_changed = 0usize;
                        let mut local_maximum = 0u8;
                        let mut channel_changed = [0usize; 4];
                        let mut channel_maximum = [0u8; 4];
                        let mut first = None;
                        for (byte, (&wanted, &got)) in reference.iter().zip(&actual).enumerate() {
                            let error = wanted.abs_diff(got);
                            maximum = maximum.max(error);
                            changed += usize::from(error != 0);
                            local_maximum = local_maximum.max(error);
                            channel_maximum[byte % 4] = channel_maximum[byte % 4].max(error);
                            if error != 0 {
                                local_changed += 1;
                                channel_changed[byte % 4] += 1;
                                first.get_or_insert((
                                    byte / 4 % size[0] as usize,
                                    byte / 4 / size[0] as usize,
                                    byte % 4,
                                    wanted,
                                    got,
                                ));
                            }
                        }
                        let stem = format!(
                            "macrotile-matrix-c{cases:02}-{family}-n{count}-{filter_name}-{}-r{repetition}-cell{index}-{}x{}",
                            match feedback_blend {
                                Blend::Premultiplied => "premult",
                                Blend::Replace => "replace",
                            },
                            size[0],
                            size[1]
                        );
                        std::fs::write(directory.join(format!("{stem}-expected.rgba")), reference)?;
                        std::fs::write(directory.join(format!("{stem}-actual.rgba")), &actual)?;
                        let blend = if index.is_multiple_of(4) {
                            Blend::Premultiplied
                        } else {
                            feedback_blend
                        };
                        log(format_args!(
                            "MACROTILE CELL case={cases} family={family} cells={count} cell={index} chain={} chain_cell={} filter={filter:?} source_blend=Premultiplied feedback_blend={feedback_blend:?} cell_blend={blend:?} repetition={repetition} width={} height={} expected_bytes={} actual_bytes={} changed_bytes={local_changed} max_error={local_maximum} channel_changed={channel_changed:?} channel_max={channel_maximum:?} first_xy_channel_expected_actual={first:?} raw_prefix={stem} tolerance=0",
                            index / 4,
                            index % 4,
                            size[0],
                            size[1],
                            reference.len(),
                            actual.len()
                        ));
                        nonopaque += actual
                            .chunks_exact(4)
                            .filter(|pixel| pixel[3] != 255)
                            .count();
                        pixels_checked += actual.len() / 4;
                        cells_checked += 1;
                    }
                    cases += 1;
                }
            }
        }
    }
    log(format_args!(
        "MACROTILE COLOR cases={cases} cells={cells_checked} pixels={pixels_checked} changed_bytes={changed} max_error={maximum} nonopaque_pixels={nonopaque} structural_errors={structural_errors} extent_errors={extent_errors} tolerance=0 macro_scenes=one_per_case reference_scenes=one_per_cell"
    ));
    if changed != 0 || nonopaque == 0 || structural_errors != 0 || extent_errors != 0 {
        return Err(format!("MACROTILE COLOR completed {cases} cases, {cells_checked} cells with changed_bytes={changed} max_error={maximum} structural_errors={structural_errors} extent_errors={extent_errors}; tolerance=0").into());
    }
    queued_reuse(context, &source, &program, &vertices)
}

/// Keep earlier submitted readers alive while rewriting the same complete atlas.
/// Distinct clear colors make a premature overwrite observable in the snapshots.
fn queued_reuse(
    context: &native::Context,
    source: &native::Texture,
    program: &native::Program,
    vertices: &native::VertexBuffer,
) -> Result {
    let sizes = &MIXED;
    let layout = native::MacrotileLayout::new([256, 256], [2, 2])?;
    let atlas = context.create_macrotile_target(layout, sizes)?;
    let ordinary = sizes
        .iter()
        .map(|&size| {
            let texture = context.create_target_texture(size, native::TargetFormat::Rgba8)?;
            context.framebuffer(&texture)
        })
        .collect::<std::result::Result<Vec<_>, native::Error>>()?;
    let snapshots = (0..4)
        .map(|_| {
            let texture = context.create_target_texture(sizes[3], native::TargetFormat::Rgba8)?;
            context.framebuffer(&texture)
        })
        .collect::<std::result::Result<Vec<_>, native::Error>>()?;
    let sampling = (Blend::Premultiplied, TextureFilter::Linear);
    let mut flights = Vec::new();
    for macrocell in [false, true] {
        for repetition in 0..2 {
            for index in 0..sizes.len() {
                let target = if macrocell {
                    atlas.cell(index).unwrap()
                } else {
                    &ordinary[index]
                };
                let input = if index == 0 {
                    source
                } else if macrocell {
                    atlas.cell(index - 1).unwrap().texture()
                } else {
                    ordinary[index - 1].texture()
                };
                stage(
                    context,
                    target,
                    input,
                    program,
                    vertices,
                    sampling,
                    index + repetition * 4,
                )?;
            }
            let output = if macrocell {
                atlas.cell(3).unwrap()
            } else {
                &ordinary[3]
            };
            stage(
                context,
                &snapshots[usize::from(macrocell) * 2 + repetition],
                output.texture(),
                program,
                vertices,
                (Blend::Replace, TextureFilter::Linear),
                1,
            )?;
            flights.push(context.fence()?);
        }
    }
    context.finish()?;
    let mut expected = Vec::new();
    for repetition in 0..2 {
        let reference = read(context, &snapshots[repetition])?;
        let actual = read(context, &snapshots[2 + repetition])?;
        let changed = reference
            .iter()
            .zip(&actual)
            .filter(|(a, b)| a != b)
            .count();
        log(format_args!(
            "MACROTILE QUEUED REUSE repetition={repetition} changed_bytes={changed} tolerance=0"
        ));
        if reference != actual {
            return Err("queued macrotile reuse changed a submitted reader".into());
        }
        expected.push(reference);
    }
    if expected[0] == expected[1] {
        return Err("queued macrotile reuse requires distinct frame pixels".into());
    }
    drop(flights);
    Ok(())
}
