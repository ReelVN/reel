//! Fixed simple-mask shaders and an interpreter for the remaining scalar DAGs.
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

pub(super) const OPERATION_CAPACITY: usize = 32;

const fn operation_capacity(count: usize) -> usize {
    if count <= 8 {
        8
    } else if count <= 16 {
        16
    } else {
        OPERATION_CAPACITY
    }
}

fn encode_operation(operation: &MaskOp) -> [f32; 4] {
    match *operation {
        MaskOp::Constant(bits) => [0., f32::from_bits(bits), 0., 0.],
        MaskOp::Parameter(p) => [1., f32::from(p), 0., 0.],
        MaskOp::Sample {
            image,
            channel,
            coordinates: [x, y, sx, sy],
            repeat,
        } => [
            if channel == MaskChannel::Alpha {
                2.
            } else {
                3.
            },
            image as f32,
            // Validated references fit in five bits. These small integers are
            // exact in 32-bit floats and share the otherwise spare lanes.
            x as f32 + 32. * y as f32,
            sx as f32 + 32. * sy as f32 + 1024. * f32::from(repeat),
        ],
        MaskOp::Add(a, b) => [4., a as f32, b as f32, 0.],
        MaskOp::Subtract(a, b) => [5., a as f32, b as f32, 0.],
        MaskOp::Multiply(a, b) => [6., a as f32, b as f32, 0.],
        MaskOp::Pow(a, p) => [7., a as f32, f32::from_bits(p), 0.],
        MaskOp::Clamp(a, min, max) => [8., a as f32, f32::from_bits(min), f32::from_bits(max)],
        MaskOp::Smoothstep(a, b, x) => [9., a as f32, b as f32, x as f32],
        MaskOp::ImageThreshold { alpha, gray } => [10., alpha as f32, gray as f32, 0.],
    }
}
fn encode_compact(operation: &MaskOp, destination: u32) -> [f32; 4] {
    let mut encoded = encode_operation(operation);
    encoded[0] += 16. * destination as f32;
    encoded
}
impl Context<'_> {
    /// Apply a sampled mask while compositing the already isolated source.
    /// Coverage stays highp through the final blend instead of visiting RGBA8.
    pub fn composite_sample_mask(
        &mut self,
        target: &Target,
        source: &Target,
        definition: &FrameImageMask,
        transform: Affine,
        alpha: f32,
    ) -> Result<bool, GpuError> {
        let simple = crate::common::mask::plan(&definition.program)
            .and_then(|plan| plan.evaluate(&definition.program, definition.parameters));
        let paired = if simple.is_none() {
            crate::common::mask::paired::Plan::new(&definition.program).and_then(|plan| {
                plan.evaluate(&definition.program, definition.parameters)
                    .map(|values| (plan, values))
            })
        } else {
            None
        };
        if simple.is_none() && paired.is_none() {
            return Ok(false);
        }
        let Some(uv) = mask_uv(definition, transform) else {
            return Ok(true);
        };
        self.ensure_sampled(&source.view)?;
        self.ensure_target(target)?;
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Composite);
        let _kind = reel_core::profile::Scope::new(reel_core::profile::Phase::EffectComposite);
        if let Some(simple) = simple {
            self.mask_sample(target, definition, uv, simple, Some((source, alpha)))?;
        } else if let Some((plan, values)) = paired {
            self.mask_paired(
                target,
                definition,
                uv,
                &plan,
                &values,
                Some((source, alpha)),
            )?;
        }
        Ok(true)
    }
    pub fn mask_expression(
        &mut self,
        definition: &FrameImageMask,
        transform: Affine,
    ) -> Result<Target, GpuError> {
        let at = self.at;
        let reject = move |record: &'static str| {
            at.map_or_else(
                || GpuError::Device(record.into()),
                |at| GpuError::Unsupported { at, record },
            )
        };
        let target = self.target()?;
        let Some(uv) = mask_uv(definition, transform) else {
            return Ok(target);
        };
        self.ensure_target(&target)?;
        if let Some(simple) = crate::common::mask::plan(&definition.program)
            .and_then(|plan| plan.evaluate(&definition.program, definition.parameters))
        {
            self.mask_sample(&target, definition, uv, simple, None)?;
            return Ok(target);
        }
        if let Some((plan, values)) = crate::common::mask::paired::Plan::new(&definition.program)
            .and_then(|plan| {
                plan.evaluate(&definition.program, definition.parameters)
                    .map(|values| (plan, values))
            })
        {
            self.mask_paired(&target, definition, uv, &plan, &values, None)?;
            return Ok(target);
        }
        if let Some(split) = self.mask_cache.get(
            &definition.program,
            definition.parameters,
            &self.budgets.resources,
        ) {
            self.mask_split(&target, definition, uv, split)?;
            return Ok(target);
        }
        let capacity = operation_capacity(definition.program.operations().len());
        let registers = (capacity > 8)
            .then(|| crate::common::mask::RegisterPlan::new(&definition.program))
            .flatten()
            .filter(|plan| plan.register_count() <= 8);
        let mut operations = [[0.; 4]; OPERATION_CAPACITY];
        for (index, operation) in definition.program.operations().iter().enumerate() {
            operations[index] = if let Some(plan) = &registers {
                encode_compact(&plan.operation(operation), plan.destination(index))
            } else {
                encode_operation(operation)
            };
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
        let shader = match (capacity, registers.is_some()) {
            (8, _) => &self.programs.mask_8,
            (16, true) => &self.programs.mask_16_compact,
            (32, true) => &self.programs.mask_32_compact,
            (16, false) => &self.programs.mask_16,
            _ => &self.programs.mask,
        };
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
        values.extend(shader.array("u_ops[0]", &operations[..capacity]));
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
            programs::Quad {
                target: native::Target::Framebuffer(&target.framebuffer),
                size: self.size,
                shader,
                textures: &[
                    &images[0].texture,
                    &images[1].texture,
                    &images[2].texture,
                    &images[3].texture,
                ],
                values: &values,
                blend: native::Blend::Replace,
                scissor: self.native_scissor(),
            },
        )?;
        Ok(target)
    }

    fn mask_paired(
        &self,
        target: &Target,
        definition: &FrameImageMask,
        uv: Affine,
        plan: &crate::common::mask::paired::Plan,
        input: &crate::common::mask::paired::Values,
        composite: Option<(&Target, f32)>,
    ) -> Result<(), GpuError> {
        let channels = usize::from(plan.samples[0].channel == MaskChannel::Gray) * 2
            + usize::from(plan.samples[1].channel == MaskChannel::Gray);
        let shader = if composite.is_some() {
            &self.programs.mask_paired_composite[channels]
        } else {
            &self.programs.mask_paired[channels]
        };
        let mut textures = smallvec::SmallVec::<[&native::Texture; 3]>::new();
        let mut texels = [[0.; 4]; 2];
        let mut dimensions = [[0.; 4]; 2];
        for (index, sample) in plan.samples.iter().enumerate() {
            let image = self
                .images
                .get(&definition.images[sample.image as usize])
                .ok_or_else(|| GpuError::Device("mask image is not resident".into()))?;
            textures.push(&image.texture);
            texels[index] = crate::common::effect_plan::texture_metadata(
                image.layout,
                image.encoding,
                image.format,
            )[0];
            dimensions[index] = [
                image.layout.storage_size[0] as f32,
                image.layout.storage_size[1] as f32,
                f32::from(sample.repeat),
                0.,
            ];
        }
        let stream = input.stream(plan.join);
        let mut values = Values::new();
        values.extend(shader.int("u_image0", 0));
        values.extend(shader.int("u_image1", 1));
        if let Some((source, alpha)) = composite {
            textures.push(&source.view);
            values.extend(shader.int("u_source", 2));
            values.extend(shader.vec4("u_params", [alpha, 0., 0., 0.]));
            values.extend(shader.vec4("u_source_uv", source.source_uv(self.size, [0; 2])));
        }
        values.extend(shader.array("u_coordinates[0]", &input.coordinates));
        values.extend(shader.array("u_texels[0]", &texels));
        values.extend(shader.array("u_dimensions[0]", &dimensions));
        values.extend(shader.array("u_ops[0]", &stream.operations));
        values.extend(shader.vec4("u_flags", stream.phases));
        let [a, b, c, d, e, f] = uv.as_coeffs().map(|v| v as f32);
        values.extend(shader.vec4("u_uv0", [a, c, e, 0.]));
        values.extend(shader.vec4("u_uv1", [b, d, f, 0.]));
        values.extend(shader.vec2("u_output_size", self.size.map(|v| v as f32)));
        self.programs.quad(
            self.gpu,
            programs::Quad {
                target: native::Target::Framebuffer(&target.framebuffer),
                size: self.size,
                shader,
                textures: &textures,
                values: &values,
                blend: if composite.is_some() {
                    native::Blend::Premultiplied
                } else {
                    native::Blend::Replace
                },
                scissor: self.native_scissor(),
            },
        )
    }

    fn mask_split(
        &self,
        target: &Target,
        definition: &FrameImageMask,
        uv: Affine,
        split: crate::common::mask::split::Prepared,
    ) -> Result<(), GpuError> {
        let plan = &split.plan.plan;
        let count = plan.fetch_count();
        let shader = if count == 1 {
            &self.programs.mask_split_1
        } else if plan.fetches().all(|fetch| fetch.gray.is_none()) {
            &self.programs.mask_split_2_alpha
        } else {
            &self.programs.mask_split_2
        };
        let mut images = smallvec::SmallVec::<[&native::Texture; 2]>::new();
        let mut texels = [[0.; 4]; 2];
        let mut dimensions = [[0.; 4]; 2];
        let mut fetches = [[0.; 4]; 2];
        for (index, fetch) in plan.fetches().enumerate() {
            let image = self
                .images
                .get(&definition.images[fetch.image as usize])
                .ok_or_else(|| GpuError::Device("mask image is not resident".into()))?;
            images.push(&image.texture);
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
            fetches[index] = [
                f32::from(fetch.repeat),
                fetch.alpha.map_or(-1., f32::from),
                fetch.gray.map_or(-1., f32::from),
                0.,
            ];
        }
        let mut operations = [[0.; 4]; 16];
        for (output, instruction) in operations.iter_mut().zip(plan.instructions()) {
            *output = encode_compact(&instruction.operation, u32::from(instruction.destination));
        }
        let mut values = Values::new();
        values.extend(shader.int("u_image0", 0));
        if count == 2 {
            values.extend(shader.int("u_image1", 1));
        }
        values.extend(shader.array("u_ops[0]", &operations[..if count == 1 { 8 } else { 16 }]));
        values.extend(shader.array("u_coordinates[0]", &split.values.coordinates[..count]));
        values.extend(shader.array("u_fetches[0]", &fetches[..count]));
        values.extend(shader.array("u_texels[0]", &texels[..count]));
        values.extend(shader.array("u_dimensions[0]", &dimensions[..count]));
        values.extend(shader.vec4("u_seed0", split.values.seeds[..4].try_into().unwrap()));
        values.extend(shader.vec4("u_seed1", split.values.seeds[4..].try_into().unwrap()));
        values.extend(shader.vec4(
            "u_flags",
            [
                plan.instructions().count() as f32,
                f32::from(plan.result()),
                0.,
                0.,
            ],
        ));
        let [a, b, c, d, e, f] = uv.as_coeffs().map(|v| v as f32);
        values.extend(shader.vec4("u_uv0", [a, c, e, 0.]));
        values.extend(shader.vec4("u_uv1", [b, d, f, 0.]));
        values.extend(shader.vec2("u_output_size", self.size.map(|v| v as f32)));
        self.programs.quad(
            self.gpu,
            programs::Quad {
                target: native::Target::Framebuffer(&target.framebuffer),
                size: self.size,
                shader,
                textures: &images,
                values: &values,
                blend: native::Blend::Replace,
                scissor: self.native_scissor(),
            },
        )
    }

    fn mask_sample(
        &self,
        target: &Target,
        definition: &FrameImageMask,
        uv: Affine,
        simple: crate::common::mask::Evaluated,
        composite: Option<(&Target, f32)>,
    ) -> Result<(), GpuError> {
        let image = self
            .images
            .get(&definition.images[simple.image])
            .ok_or_else(|| {
                self.at.map_or_else(
                    || GpuError::Device("mask image is not resident".into()),
                    |at| GpuError::Unsupported {
                        at,
                        record: "mask image is not resident",
                    },
                )
            })?;
        let shader = match (simple.threshold, composite.is_some()) {
            (true, true) => &self.programs.mask_threshold_composite,
            (false, true) => &self.programs.mask_sample_chain_composite,
            (true, false) => &self.programs.mask_threshold,
            (false, false) => &self.programs.mask_sample_chain,
        };
        let [a, b, c, d, e, f] = uv.as_coeffs().map(|v| v as f32);
        let mut values = Values::new();
        values.extend(shader.int("u_image", 0));
        if let Some((source, alpha)) = composite {
            values.extend(shader.int("u_source", 1));
            values.extend(shader.vec4("u_params", [alpha, 0., 0., 0.]));
            values.extend(shader.vec4("u_source_uv", source.source_uv(self.size, [0; 2])));
        }
        values.extend(shader.vec4("u_uv0", [a, c, e, 0.]));
        values.extend(shader.vec4("u_uv1", [b, d, f, 0.]));
        values.extend(shader.vec4("u_values", definition.parameters[..4].try_into().unwrap()));
        values.extend(shader.vec4("u_flags", [definition.parameters[4], 0., 0., 0.]));
        values.extend(shader.vec4("u_coordinates", simple.coordinates));
        values.extend(shader.vec2(
            "u_sample_flags",
            [
                f32::from(simple.channel == MaskChannel::Gray),
                f32::from(simple.repeat),
            ],
        ));
        values.extend(shader.vec2("u_dimensions", image.layout.storage_size.map(|v| v as f32)));
        values.extend(shader.vec2("u_output_size", self.size.map(|v| v as f32)));
        values.extend(shader.vec4(
            "u_texels",
            [
                image.layout.content_size[0] as f32 / image.layout.storage_size[0] as f32,
                image.layout.content_size[1] as f32 / image.layout.storage_size[1] as f32,
                f32::from(image.encoding == ColorEncoding::Linear),
                match image.format {
                    Some(PixelFormat::Bc1Rgb) => 1.,
                    Some(PixelFormat::Bc3Rgba) => 2.,
                    _ => 0.,
                },
            ],
        ));
        for (name, step) in [
            "u_step0", "u_step1", "u_step2", "u_step3", "u_step4", "u_step5", "u_step6", "u_step7",
        ]
        .into_iter()
        .zip(simple.steps)
        {
            values.extend(shader.vec4(name, step));
        }
        let textures = [
            &image.texture,
            composite.map_or(&image.texture, |(source, _)| &source.view),
        ];
        self.programs.quad(
            self.gpu,
            programs::Quad {
                target: native::Target::Framebuffer(&target.framebuffer),
                size: self.size,
                shader,
                textures: &textures[..if composite.is_some() { 2 } else { 1 }],
                values: &values,
                blend: if composite.is_some() {
                    native::Blend::Premultiplied
                } else {
                    native::Blend::Replace
                },
                scissor: self.native_scissor(),
            },
        )
    }
}

fn mask_uv(definition: &FrameImageMask, transform: Affine) -> Option<Affine> {
    if definition.bounds.width() <= 0.
        || definition.bounds.height() <= 0.
        || transform.determinant() == 0.
    {
        return None;
    }
    Some(
        Affine::scale_non_uniform(
            1. / definition.bounds.width(),
            1. / definition.bounds.height(),
        ) * Affine::translate((-definition.bounds.x0, -definition.bounds.y0))
            * transform.inverse(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_banks_cover_the_exact_program_length() {
        for (count, expected) in [
            (1, 8),
            (8, 8),
            (9, 16),
            (12, 16),
            (16, 16),
            (17, 32),
            (32, 32),
        ] {
            assert_eq!(operation_capacity(count), expected);
        }
    }

    #[test]
    fn packed_sample_references_preserve_coordinates_and_repeat_bits() {
        for repeat in 0..4 {
            for x in 0..32 {
                for y in 0..32 {
                    let sx = 31 - x;
                    let sy = 31 - y;
                    let operation = MaskOp::Sample {
                        image: 3,
                        channel: MaskChannel::Gray,
                        coordinates: [x, y, sx, sy],
                        repeat,
                    };
                    let value = encode_operation(&operation);
                    assert_eq!(value[0..2], [3., 3.]);
                    for destination in [0, 7, 31] {
                        let encoded = encode_compact(&operation, destination);
                        let actual_destination = (encoded[0] / 16.).floor();
                        assert_eq!(actual_destination, destination as f32);
                        assert_eq!(encoded[0] - actual_destination * 16., value[0]);
                        assert_eq!(encoded[1..], value[1..]);
                    }
                    // Decode with the same floating point operations as the Cg shader.
                    assert_eq!(
                        [value[2] % 32., (value[2] / 32.).floor()],
                        [x as f32, y as f32]
                    );
                    assert_eq!(
                        [value[3] % 32., (value[3] / 32.).floor() % 32.],
                        [sx as f32, sy as f32]
                    );
                    assert_eq!((value[3] / 1024.).floor(), f32::from(repeat));
                }
            }
        }
    }
}
