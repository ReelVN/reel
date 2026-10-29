//! Compare production PBE presentation with the former shader encoding on device.
use super::{Result, log, native};
use native::{Blend, FragmentOutput, SurfaceEncoding, Target, TextureFilter};

const SOURCE: [u32; 2] = [256, 16];
const SIZE: [u32; 2] = [1024, 16];

fn program(context: &native::Context, pbe: bool) -> Result<native::Program> {
    Ok(context.create_program(native::ProgramDescriptor {
        output: if pbe {
            FragmentOutput::Srgb
        } else {
            FragmentOutput::Patched
        },
        vertex: native::ShaderCode::GxpBinary(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/present-color-vertex.gxp"
        ))),
        fragment: native::ShaderCode::GxpBinary(if pbe {
            include_bytes!(concat!(env!("OUT_DIR"), "/present-color-pbe.gxp"))
        } else {
            include_bytes!(concat!(env!("OUT_DIR"), "/present-color-reference.gxp"))
        }),
        bindings: &[
            native::AttributeBinding {
                location: 0,
                name: "a_position",
            },
            native::AttributeBinding {
                location: 1,
                name: "a_uv",
            },
        ],
        stride: 4,
        attributes: &attributes(),
        instance: None,
        clip_uniform: None,
    })?)
}

fn attributes() -> [native::VertexAttribute; 2] {
    [
        native::VertexAttribute {
            location: 0,
            components: 2,
            offset: 0,
        },
        native::VertexAttribute {
            location: 1,
            components: 2,
            offset: 2,
        },
    ]
}

fn draw(
    context: &native::Context,
    target: &native::Framebuffer,
    program: &native::Program,
    vertices: &native::VertexBuffer,
    source: &native::Texture,
    filter: TextureFilter,
) -> std::result::Result<(), native::Error> {
    let image = program
        .uniform("u_image")
        .ok_or(native::Error::Invalid("missing image sampler"))?;
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
            uniforms: &[native::UniformValue::Int(&image, 0)],
            blend: Blend::Replace,
            scissor: None,
        },
        filter,
    )
}

fn encoded(value: f64) -> u8 {
    let srgb = if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1. / 2.4) - 0.055
    };
    (srgb * 255.).round() as u8
}

fn compare(context: &native::Context) -> Result {
    let source = context.create_target_texture(SOURCE, native::TargetFormat::Rgba8)?;
    // Every byte code, reversed channels, dark/bright steps, and varying source
    // alpha. Fourfold scaling also exercises intermediate filtered values.
    let mut data = Vec::with_capacity((SOURCE[0] * SOURCE[1] * 4) as usize);
    for row in 0..SOURCE[1] {
        for x in 0..256u32 {
            let c = x as u8;
            let rgb = match row % 4 {
                0 => [c, c, c],
                1 => [c, 255 - c, c.wrapping_mul(17)],
                2 => [if x % 2 == 0 { 0 } else { 255 }, c % 16, 255 - c % 16],
                _ => [c.wrapping_mul(37), c.wrapping_mul(73), c.wrapping_mul(113)],
            };
            data.extend_from_slice(&[rgb[0], rgb[1], rgb[2], (row * 17) as u8]);
        }
    }
    context.upload(&source, [0; 2], SOURCE, &data, SOURCE[0] as usize * 4)?;
    let vertices = context.vertex_buffer(
        &[-1., 1., 0., 0., 3., 1., 2., 0., -1., -3., 0., 2.],
        4,
        &attributes(),
    )?;
    let mut total = 0;
    let mut changed = 0;
    let mut maximum = 0;
    for filter in [TextureFilter::Nearest, TextureFilter::Linear] {
        let mut outputs = Vec::new();
        for pbe in [false, true] {
            let storage = context.create_target_texture(SIZE, native::TargetFormat::Rgba8)?;
            let target = context.framebuffer_encoded(
                &storage,
                if pbe {
                    SurfaceEncoding::Srgb
                } else {
                    SurfaceEncoding::Linear
                },
            )?;
            let wrong = program(context, !pbe)?;
            if !matches!(
                draw(context, &target, &wrong, &vertices, &source, filter),
                Err(native::Error::Invalid(
                    "fragment output and surface encoding mismatch"
                ))
            ) {
                return Err("mismatched fragment output must be rejected before submission".into());
            }
            drop(wrong);
            if pbe {
                context.clear(Target::Framebuffer(&target), [0.5, 0.25, 0.0031308, 0.37])?;
                context.finish()?;
                let mut row = vec![0; SIZE[0] as usize * 4];
                context.read_rgba(&target, 0, &mut row)?;
                let expected = [encoded(0.5), encoded(0.25), encoded(0.0031308), 94];
                if row
                    .chunks_exact(4)
                    .any(|p| p.iter().zip(expected).any(|(a, b)| a.abs_diff(b) > 1))
                {
                    return Err("sRGB clear must encode RGB and preserve linear alpha".into());
                }
            }
            let program = program(context, pbe)?;
            draw(context, &target, &program, &vertices, &source, filter)?;
            context.finish()?;
            let mut pixels = vec![0; (SIZE[0] * SIZE[1] * 4) as usize];
            context.read_rgba(&target, 0, &mut pixels)?;
            if pixels.chunks_exact(4).any(|pixel| pixel[3] != 255) {
                return Err("presentation alpha must remain opaque".into());
            }
            if filter == TextureFilter::Nearest {
                for (x, pixel) in pixels[..SIZE[0] as usize * 4].chunks_exact(4).enumerate() {
                    let expected = encoded((x / 4) as f64 / 255.);
                    if pixel[..3].iter().any(|c| c.abs_diff(expected) > 1) {
                        return Err("sRGB byte ramp differs from CPU reference".into());
                    }
                }
            }
            outputs.push(pixels);
        }
        for (reference, actual) in outputs[0].iter().zip(&outputs[1]) {
            let error = reference.abs_diff(*actual);
            maximum = maximum.max(error);
            changed += usize::from(error != 0);
        }
        total += (SIZE[0] * SIZE[1]) as usize;
    }
    log(format_args!(
        "PRESENT COLOR pixels={total} changed_bytes={changed} max_error={maximum} alpha=opaque tolerance=1"
    ));
    if maximum > 1 {
        return Err("PBE presentation differs from shader sRGB by more than one byte step".into());
    }
    Ok(())
}

pub(super) fn run() -> Result {
    let context = reel_render::gxm::create_context(&Default::default(), None)?;
    let result = compare(&context);
    context.finish()?;
    context.close().map_err(|(_, error)| error)?;
    result
}
