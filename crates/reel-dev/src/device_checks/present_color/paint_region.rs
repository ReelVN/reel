//! Exact RGBA comparison of original regional draws and disjoint direct interiors.
//! Shader binaries come from the renderer's production variant definitions.
use super::{Blend, FragmentOutput, Result, Target, TextureFilter, log, native};

const SIZE: [u32; 2] = [640, 480];
const SOURCE: [u32; 2] = [64, 32];
const CLIPS: [[u32; 4]; 5] = [
    [32, 32, 576, 416],
    [0, 0, 640, 32],
    [0, 32, 32, 416],
    [608, 32, 32, 416],
    [0, 448, 640, 32],
];

macro_rules! code {
    ($name:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/paint-region-", $name, ".gxp")).as_slice()
    };
}

fn attributes() -> [native::VertexAttribute; 4] {
    std::array::from_fn(|index| native::VertexAttribute {
        location: index as u32,
        components: [2, 2, 4, 4][index],
        offset: [0, 2, 4, 8][index],
    })
}

fn program(context: &native::Context, fragment: &[u8], bounded: bool) -> Result<native::Program> {
    Ok(context.create_program(native::ProgramDescriptor {
        output: FragmentOutput::Patched,
        vertex: native::ShaderCode::GxpBinary(if bounded {
            code!("vertex_bounded")
        } else {
            code!("vertex")
        }),
        fragment: native::ShaderCode::GxpBinary(fragment),
        bindings: &std::array::from_fn::<_, 4, _>(|index| native::AttributeBinding {
            location: index as u32,
            name: ["a_position", "a_uv", "a_color", "a_window"][index],
        }),
        stride: 12,
        attributes: &attributes(),
        instance: None,
        // All fixture clips follow complete hardware tiles, as selected by the
        // production draw path for these exact rectangles.
        clip_uniform: None,
    })?)
}

fn vertices(context: &native::Context, mirrored: bool) -> Result<native::VertexBuffer> {
    let mut data = Vec::new();
    for [x, y] in [
        [-1., 1.],
        [-1., -1.],
        [1., -1.],
        [-1., 1.],
        [1., -1.],
        [1., 1.],
    ] {
        let u = if (x < 0.) ^ mirrored { 0.125 } else { 0.875 };
        let v = if (y > 0.) ^ mirrored { 0.25 } else { 0.75 };
        data.extend([x, y, u, v, 0.125, 0.25, 0.5, 0.75, 0.125, 0.25, 0.75, 0.5]);
    }
    Ok(context.vertex_buffer(&data, 12, &attributes())?)
}

struct Draw<'a> {
    target: &'a native::Framebuffer,
    source: &'a native::Texture,
    vertices: &'a native::VertexBuffer,
    appearance: [[f32; 4]; 10],
    filter: TextureFilter,
    blend: Blend,
}

impl Draw<'_> {
    fn issue(
        &self,
        context: &native::Context,
        program: &native::Program,
        scissor: [u32; 4],
    ) -> Result {
        let image = program.uniform("u_image");
        let dimensions = program.uniform("u_dimensions");
        let appearance = program.uniform("u_appearance[0]");
        let mut values = Vec::new();
        if let Some(image) = &image {
            values.push(native::UniformValue::Int(image, 0));
        }
        if let Some(dimensions) = &dimensions {
            values.push(native::UniformValue::Vec2(
                dimensions,
                SOURCE.map(|n| n as f32),
            ));
        }
        if let Some(appearance) = &appearance {
            values.push(native::UniformValue::Vec4Array(
                appearance,
                &self.appearance,
            ));
        }
        context.draw_filtered(
            native::Draw {
                target: Target::Framebuffer(self.target),
                size: SIZE,
                program,
                vertices: self.vertices,
                instances: None,
                indices: None,
                range: 0..6,
                textures: &[self.source],
                uniforms: &values,
                blend: self.blend,
                scissor: Some(scissor),
            },
            self.filter,
        )?;
        Ok(())
    }
}

pub(super) fn run(context: &native::Context) -> Result {
    let source = context.create_target_texture(SOURCE, native::TargetFormat::Rgba8)?;
    let pattern: Vec<u8> = (0..SOURCE[0] * SOURCE[1])
        .flat_map(|index| {
            [
                (index * 37) as u8,
                (index * 13 + 71) as u8,
                (index * 97 + 7) as u8,
                index as u8,
            ]
        })
        .collect();
    context.upload(&source, [0; 2], SOURCE, &pattern, SOURCE[0] as usize * 4)?;
    let mut targets = Vec::new();
    for _ in 0..2 {
        let storage = context.create_target_texture(SIZE, native::TargetFormat::Rgba8)?;
        targets.push(context.framebuffer(&storage)?);
    }
    let mut pixels = [
        vec![0u8; (SIZE[0] * SIZE[1] * 4) as usize],
        vec![0u8; (SIZE[0] * SIZE[1] * 4) as usize],
    ];
    let mut cases = 0;
    let mut changed = 0usize;
    let mut maximum = 0u8;
    let mut negative = 0usize;
    let mut blend_negative = 0usize;
    let mut first = None;
    for (name, regional, direct, bounded) in [
        (
            "rgba",
            code!("paint_regional_unclipped"),
            code!("paint_direct_unclipped"),
            false,
        ),
        (
            "bc3",
            code!("paint_regional_bc3_unclipped"),
            code!("paint_direct_bc3_unclipped"),
            false,
        ),
        (
            "rgba_bounded",
            code!("paint_regional_bounded_unclipped"),
            code!("paint_direct_bounded_unclipped"),
            true,
        ),
        (
            "bc1_bounded",
            code!("paint_regional_bc1_bounded_unclipped"),
            code!("paint_direct_bc1_bounded_unclipped"),
            true,
        ),
        (
            "bc3_bounded",
            code!("paint_regional_bc3_bounded_unclipped"),
            code!("paint_direct_bc3_bounded_unclipped"),
            true,
        ),
        (
            "alpha",
            code!("paint_alpha_region_unclipped"),
            code!("paint_alpha_direct_unclipped"),
            true,
        ),
        (
            "adjust",
            code!("paint_adjust_region_unclipped"),
            code!("paint_adjust_direct_unclipped"),
            false,
        ),
        (
            "monochrome",
            code!("paint_monochrome_region_unclipped"),
            code!("paint_monochrome_direct_unclipped"),
            false,
        ),
    ] {
        let regional = program(context, regional, bounded)?;
        let direct = program(context, direct, bounded)?;
        for mirrored in [false, true] {
            let vertices = vertices(context, mirrored)?;
            for filter in [TextureFilter::Nearest, TextureFilter::Linear] {
                // Also compare first source-over on transparent storage with
                // replacement. The final mode overlaps a second translucent draw.
                for mode in 0..4 {
                    let blend = if mode == 0 {
                        Blend::Replace
                    } else {
                        Blend::Premultiplied
                    };
                    let mut appearance = [[0.; 4]; 10];
                    appearance[6] = if name == "alpha" {
                        [0.; 4]
                    } else {
                        [0.7, 0.8, 0.9, 0.6]
                    };
                    appearance[7] = [0.1, 0.2, 0.3, 1.];
                    appearance[8][2] = 2.;
                    for index in 0..2 {
                        context.clear(
                            Target::Framebuffer(&targets[index]),
                            if mode >= 2 {
                                [0.; 4]
                            } else {
                                [0.125, 0.25, 0.375, 0.5]
                            },
                        )?;
                        let draw = Draw {
                            target: &targets[index],
                            source: &source,
                            vertices: &vertices,
                            appearance,
                            filter,
                            blend: if index == 1 && mode >= 2 {
                                Blend::Replace
                            } else {
                                blend
                            },
                        };
                        if index == 0 {
                            draw.issue(context, &regional, [0, 0, SIZE[0], SIZE[1]])?;
                        } else {
                            for (part, scissor) in CLIPS.into_iter().enumerate() {
                                draw.issue(
                                    context,
                                    if part == 0 { &direct } else { &regional },
                                    scissor,
                                )?;
                            }
                        }
                        if mode == 3 {
                            Draw {
                                blend: Blend::Premultiplied,
                                ..draw
                            }
                            .issue(context, &regional, CLIPS[0])?;
                        }
                        context.finish()?;
                        context.read_rgba(&targets[index], 0, &mut pixels[index])?;
                    }
                    if pixels[0]
                        .chunks_exact(4)
                        .all(|pixel| pixel == &pixels[0][..4])
                    {
                        return Err(format!(
                            "paint region {name} baseline did not draw the varying source"
                        )
                        .into());
                    }
                    for (index, (left, right)) in pixels[0].iter().zip(&pixels[1]).enumerate() {
                        changed += usize::from(left != right);
                        maximum = maximum.max(left.abs_diff(*right));
                        if left != right && first.is_none() {
                            first = Some((
                                name,
                                mirrored,
                                filter,
                                blend,
                                index / 4 % SIZE[0] as usize,
                                index / 4 / SIZE[0] as usize,
                                index % 4,
                                *left,
                                *right,
                            ));
                        }
                    }
                    if cases == 4 {
                        context
                            .clear(Target::Framebuffer(&targets[1]), [0.125, 0.25, 0.375, 0.5])?;
                        Draw {
                            target: &targets[1],
                            source: &source,
                            vertices: &vertices,
                            appearance,
                            filter,
                            blend,
                        }
                        .issue(
                            context,
                            &direct,
                            [0, 0, SIZE[0], SIZE[1]],
                        )?;
                        context.finish()?;
                        context.read_rgba(&targets[1], 0, &mut pixels[1])?;
                        negative = pixels[0]
                            .iter()
                            .zip(&pixels[1])
                            .filter(|(a, b)| a != b)
                            .count();
                    }
                    if cases == 7 {
                        context.clear(Target::Framebuffer(&targets[1]), [0.; 4])?;
                        let wrong = Draw {
                            target: &targets[1],
                            source: &source,
                            vertices: &vertices,
                            appearance,
                            filter,
                            blend: Blend::Replace,
                        };
                        wrong.issue(context, &regional, [0, 0, SIZE[0], SIZE[1]])?;
                        wrong.issue(context, &regional, CLIPS[0])?;
                        context.finish()?;
                        context.read_rgba(&targets[1], 0, &mut pixels[1])?;
                        blend_negative = pixels[0]
                            .iter()
                            .zip(&pixels[1])
                            .filter(|(a, b)| a != b)
                            .count();
                    }
                    cases += 1;
                }
            }
        }
        log(format_args!(
            "PAINT REGION family={name} cases={cases} changed_bytes={changed} max_error={maximum}"
        ));
    }
    log(format_args!(
        "PAINT REGION cases={cases} pixels={} changed_bytes={changed} max_error={maximum} negative_control_changed_bytes={negative} blend_negative_changed_bytes={blend_negative} first_diff={first:?} tolerance=0",
        cases as u64 * u64::from(SIZE[0]) * u64::from(SIZE[1])
    ));
    if changed != 0 || negative == 0 || blend_negative == 0 {
        return Err(
            "regional paint split differs from original draw or negative control was not detected"
                .into(),
        );
    }
    Ok(())
}
