//! A fixed precompiled GPU interpreter consumes the validated common scalar DAG.
use super::*;
use super::{
    programs::{Target, Values},
    replay::Context,
};
use reel_ui::{
    frame::FrameImageMask,
    kurbo::Affine,
    stage::{MaskChannel, MaskOp},
};
impl Context<'_> {
    pub fn mask_expression(
        &mut self,
        definition: &FrameImageMask,
        transform: Affine,
    ) -> Result<Target, GlesError> {
        let at = self.at;
        let reject = move |record: &'static str| {
            at.map_or_else(
                || GlesError::Device(record.into()),
                |at| GlesError::Unsupported { at, record },
            )
        };
        let target = self.target()?;
        if definition.bounds.width() <= 0.
            || definition.bounds.height() <= 0.
            || transform.determinant() == 0.
        {
            return Ok(target);
        }
        let uv = Affine::scale_non_uniform(
            1. / definition.bounds.width(),
            1. / definition.bounds.height(),
        ) * Affine::translate((-definition.bounds.x0, -definition.bounds.y0))
            * transform.inverse();
        let mut operations = [[0.; 4]; 64];
        for (index, operation) in definition.program.operations().iter().enumerate() {
            let (a, b) = match *operation {
                MaskOp::Constant(bits) => ([0., f32::from_bits(bits), 0., 0.], [0.; 4]),
                MaskOp::Parameter(p) => ([1., f32::from(p), 0., 0.], [0.; 4]),
                MaskOp::Sample {
                    image,
                    channel,
                    coordinates: [x, y, sx, sy],
                    repeat,
                } => (
                    [
                        if channel == MaskChannel::Alpha {
                            2.
                        } else {
                            3.
                        },
                        image as f32,
                        x as f32,
                        y as f32,
                    ],
                    [sx as f32, sy as f32, f32::from(repeat), 0.],
                ),
                MaskOp::Add(a, b) => ([4., a as f32, b as f32, 0.], [0.; 4]),
                MaskOp::Subtract(a, b) => ([5., a as f32, b as f32, 0.], [0.; 4]),
                MaskOp::Multiply(a, b) => ([6., a as f32, b as f32, 0.], [0.; 4]),
                MaskOp::Pow(a, p) => ([7., a as f32, f32::from_bits(p), 0.], [0.; 4]),
                MaskOp::Clamp(a, min, max) => (
                    [8., a as f32, f32::from_bits(min), f32::from_bits(max)],
                    [0.; 4],
                ),
                MaskOp::Smoothstep(a, b, x) => ([9., a as f32, b as f32, x as f32], [0.; 4]),
                MaskOp::ImageThreshold { alpha, gray } => {
                    ([10., alpha as f32, gray as f32, 0.], [0.; 4])
                }
            };
            operations[index * 2] = a;
            operations[index * 2 + 1] = b;
        }
        let white = self.white()?;
        let mut images = Vec::with_capacity(4);
        let mut texels = [[1., 1., 0., 0.]; 4];
        let mut dimensions = [[1., 1., 0., 0.]; 4];
        for (index, handle) in definition.images.iter().enumerate() {
            let image = self
                .images
                .get(handle)
                .cloned()
                .ok_or_else(|| reject("mask image is not resident"))?;
            texels[index] = [
                image.layout.content_size[0] as f32 / image.layout.storage_size[0] as f32,
                image.layout.content_size[1] as f32 / image.layout.storage_size[1] as f32,
                f32::from(image.encoding == ColorEncoding::Linear),
                match image.format {
                    Some(PixelFormat::Bc1Rgb) => 1.,
                    Some(PixelFormat::Bc3Rgba) => 2.,
                    _ => 0.,
                },
            ];
            dimensions[index] = [
                image.layout.storage_size[0] as f32,
                image.layout.storage_size[1] as f32,
                0.,
                0.,
            ];
            images.push(image);
        }
        while images.len() < 4 {
            images.push(white.clone());
        }
        let shader = &self.programs.mask;
        let mut values = Values::new();
        for (name, unit) in [
            ("u_image0", 0),
            ("u_image1", 1),
            ("u_image2", 2),
            ("u_image3", 3),
        ] {
            values.extend(shader.int(name, unit));
        }
        let [a, b, c, d, e, f] = uv.as_coeffs().map(|v| v as f32);
        values.extend(shader.array("u_ops[0]", &operations));
        values.extend(shader.array("u_texels[0]", &texels));
        values.extend(shader.array("u_dimensions[0]", &dimensions));
        values.extend(shader.vec4("u_uv0", [a, c, e, 0.]));
        values.extend(shader.vec4("u_uv1", [b, d, f, 0.]));
        values.extend(shader.vec4("u_values", definition.parameters[..4].try_into().unwrap()));
        values.extend(shader.vec4("u_flags", [definition.parameters[4], 0., 0., 0.]));
        values.extend(shader.vec2("u_output_size", self.size.map(|v| v as f32)));
        values.extend(shader.int("u_count", definition.program.operations().len() as i32));
        self.programs.quad(
            self.gpu,
            native::Target::Framebuffer(&target.framebuffer),
            self.size,
            shader,
            &[
                &images[0].texture,
                &images[1].texture,
                &images[2].texture,
                &images[3].texture,
            ],
            &values,
            native::Blend::Replace,
            self.gl_scissor(),
        )?;
        Ok(target)
    }
}
