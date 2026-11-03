//! Compare the production output selector with its retained branching oracle.
use super::{Blend, FragmentOutput, Result, Target, TextureFilter, log, native};

const SOURCE: [u32; 2] = [37, 23];
const SIZE: [u32; 2] = [67, 35];
const CLEAR: [f32; 4] = [0.0625, 0.125, 0.03125, 0.375];

macro_rules! shaders {
    ($name:literal, $coverage:literal) => {
        [
            include_bytes!(concat!(
                env!("OUT_DIR"),
                "/effect-output-",
                $name,
                "-",
                $coverage,
                "-baseline.gxp"
            ))
            .as_slice(),
            include_bytes!(concat!(
                env!("OUT_DIR"),
                "/effect-output-",
                $name,
                "-",
                $coverage,
                "-candidate.gxp"
            ))
            .as_slice(),
        ]
    };
}

fn pattern() -> Vec<u8> {
    (0..SOURCE[0] * SOURCE[1])
        .flat_map(|index| {
            let alpha = (index % 256) as u8;
            let channels = [index * 37, index * 13 + 71, index * 97 + 7];
            let rgb =
                channels.map(|channel| ((channel % 256 * u32::from(alpha) + 127) / 255) as u8);
            [rgb[0], rgb[1], rgb[2], alpha]
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
struct Case {
    mode: u8,
    opacity: f32,
    window: bool,
    blend: Blend,
    filter: TextureFilter,
}

fn parameters(case: Case) -> [[f32; 4]; 10] {
    let mut values = [[0.0; 4]; 10];
    values[0] = [1.0, 0.0, 0.0, 0.0];
    values[1] = [0.0, 1.0, 0.0, f32::from(case.mode)];
    let (offset, scale) = if case.window {
        ([3.0 / 37.0, 2.0 / 23.0], [29.0 / 37.0, 17.0 / 23.0])
    } else {
        ([0.0; 2], [1.0; 2])
    };
    values[2] = [scale[0], 0.0, offset[0], case.opacity];
    values[3] = [0.0, scale[1], offset[1], 0.0];
    values[4] = [0.0, 0.0, SIZE[0] as f32, SIZE[1] as f32];
    values[5] = [1.0, 1.0, SOURCE[0] as f32, SOURCE[1] as f32];
    values[6] = [offset[0], offset[1], scale[0], scale[1]];
    values
}

fn draw(
    context: &native::Context,
    target: &native::Framebuffer,
    program: &native::Program,
    vertices: &native::VertexBuffer,
    source: &native::Texture,
    case: Case,
) -> Result {
    let source_uniform = program
        .uniform("reel_source")
        .ok_or("effect source sampler")?;
    let context_uniform = program.uniform("reel_context[0]").ok_or("effect context")?;
    let clip = program.uniform("reel_clip_rect");
    let values = parameters(case);
    let mut uniforms = vec![
        native::UniformValue::Int(&source_uniform, 0),
        native::UniformValue::Vec4Array(&context_uniform, &values),
    ];
    if let Some(clip) = &clip {
        uniforms.push(native::UniformValue::Vec4(clip, [0.75, 1.25, 65.75, 33.75]));
    }
    context.clear(Target::Framebuffer(target), CLEAR)?;
    context.draw_filtered(
        native::Draw {
            target: Target::Framebuffer(target),
            size: SIZE,
            program,
            vertices,
            instances: None,
            indices: None,
            range: 0..3,
            textures: &[source],
            uniforms: &uniforms,
            blend: case.blend,
            scissor: None,
        },
        case.filter,
    )?;
    Ok(())
}

pub(super) fn run(context: &native::Context) -> Result {
    let source = context.create_target_texture(SOURCE, native::TargetFormat::Rgba8)?;
    context.upload(&source, [0; 2], SOURCE, &pattern(), SOURCE[0] as usize * 4)?;
    // Extending UVs beyond all edges exercises logical clipping independently
    // of the source subwindow clamp and its partial physical rows.
    let vertices = context.vertex_buffer(
        &[
            -1., 1., -0.125, -0.125, 3., 1., 2.375, -0.125, -1., -3., -0.125, 2.375,
        ],
        4,
        &super::attributes(),
    )?;
    let mut targets = Vec::new();
    for _ in 0..2 {
        let storage = context.create_target_texture(SIZE, native::TargetFormat::Rgba8)?;
        targets.push(context.framebuffer(&storage)?);
    }
    let bytes = (SIZE[0] * SIZE[1] * 4) as usize;
    let mut pixels = [vec![0u8; bytes], vec![0u8; bytes]];
    let mut cases = 0usize;
    let mut changed = 0usize;
    let mut maximum = 0u8;
    let mut nonopaque = 0usize;
    let mut first_failure = None;
    for (name, binaries) in [
        ("h3-local", shaders!("h3", "local")),
        ("h3-full", shaders!("h3", "full")),
        ("v3-local", shaders!("v3", "local")),
        ("v3-full", shaders!("v3", "full")),
        ("h17-local", shaders!("h17", "local")),
        ("h17-full", shaders!("h17", "full")),
    ] {
        let programs = binaries
            .map(|binary| super::sampling_program(context, FragmentOutput::Patched, binary));
        let [baseline, candidate] = programs;
        let programs = [baseline?, candidate?];
        for window in [false, true] {
            for filter in [TextureFilter::Nearest, TextureFilter::Linear] {
                for blend in [Blend::Replace, Blend::Premultiplied] {
                    for mode in 0..=2 {
                        for opacity in [0.0, 1.0 / 255.0, 0.375, 1.0] {
                            let case = Case {
                                mode,
                                opacity,
                                window,
                                blend,
                                filter,
                            };
                            for index in 0..2 {
                                draw(
                                    context,
                                    &targets[index],
                                    &programs[index],
                                    &vertices,
                                    &source,
                                    case,
                                )?;
                            }
                            context.finish()?;
                            for index in 0..2 {
                                context.read_rgba(&targets[index], 0, &mut pixels[index])?;
                            }
                            for (index, (&expected, &actual)) in
                                pixels[0].iter().zip(&pixels[1]).enumerate()
                            {
                                let error = expected.abs_diff(actual);
                                maximum = maximum.max(error);
                                changed += usize::from(error != 0);
                                if error != 0 && first_failure.is_none() {
                                    first_failure = Some(format!(
                                        "{name} {case:?} byte={index} expected={expected} actual={actual}"
                                    ));
                                }
                            }
                            nonopaque += pixels[0]
                                .chunks_exact(4)
                                .filter(|pixel| pixel[3] < 255)
                                .count();
                            // A nonzero visible source distinguishes an actual
                            // comparison from two silently omitted effect draws.
                            if mode == 0
                                && pixels[0]
                                    .chunks_exact(4)
                                    .all(|pixel| pixel == [16, 32, 8, 96])
                            {
                                return Err(
                                    format!("effect oracle did not draw: {name} {case:?}").into()
                                );
                            }
                            cases += 1;
                        }
                    }
                }
            }
        }
        log(format_args!(
            "EFFECT OUTPUT family={name} cases={cases} changed_bytes={changed} max_error={maximum}"
        ));
    }
    log(format_args!(
        "EFFECT OUTPUT cases={cases} pixels={} changed_bytes={changed} max_error={maximum} nonopaque={nonopaque} modes=0,1,2 blend=replace,premultiplied filter=nearest,linear coverage=local,full tolerance=0",
        cases * (SIZE[0] * SIZE[1]) as usize,
    ));
    if let Some(failure) = first_failure {
        return Err(format!("effect output differs: {failure}").into());
    }
    Ok(())
}
