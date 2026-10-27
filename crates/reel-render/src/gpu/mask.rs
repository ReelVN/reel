//! Fixed image-threshold pipelines and live scalar-mask structures per quality.
use super::{Gpu, GpuError, Quality, pipeline, targets::Target};
use reel_core::budget::{BudgetPool, BudgetTicket};
use reel_ui::{
    frame::FrameImageMask,
    kurbo::Affine,
    stage::{MaskChannel, MaskOp, MaskProgram},
};
use std::sync::{Arc, Mutex, Weak};
use wgpu::util::DeviceExt;

#[derive(Default)]
pub(super) struct Cache(Mutex<State>);
#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    storage: Option<BudgetTicket>,
    split: crate::common::mask::split::Cache,
}
struct Entry {
    builtin: bool,
    split: bool,
    hash: u64,
    operations: Vec<MaskOp>,
    images: usize,
    programs: Vec<Weak<MaskProgram>>,
    pipeline: Arc<Compiled>,
    storage: BudgetTicket,
}
pub(super) struct Compiled {
    layout: wgpu::BindGroupLayout,
    normal: wgpu::RenderPipeline,
    blend: wgpu::RenderPipeline,
    extended_blend: wgpu::RenderPipeline,
    split_plan: Option<Arc<crate::common::mask::split::AccountedPlan>>,
}
pub(super) struct Prepared {
    pub(super) pipeline: Arc<Compiled>,
    split: Option<crate::common::mask::split::Prepared>,
}
fn reserve(budget: &BudgetPool, bytes: usize, gpu: &Gpu) -> Result<BudgetTicket, GpuError> {
    budget.reserve(bytes as u64).map_err(|_| GpuError::Budget {
        domain: "mask pipeline metadata",
        requested: bytes as u64,
        available: gpu
            .limits
            .resource_bytes
            .saturating_sub(budget.usage().bytes),
    })
}
fn resize(
    ticket: &mut BudgetTicket,
    bytes: usize,
    gpu: &Gpu,
    budget: &BudgetPool,
) -> Result<(), GpuError> {
    ticket.resize(bytes as u64).map_err(|_| GpuError::Budget {
        domain: "mask pipeline metadata",
        requested: bytes as u64,
        available: gpu
            .limits
            .resource_bytes
            .saturating_sub(budget.usage().bytes),
    })
}
impl Cache {
    pub(super) fn prune(&self) {
        let mut state = self.0.lock().unwrap();
        state.split.prune();
        let previous = state.entries.len();
        state.entries.retain_mut(|entry| {
            entry.programs.retain(|p| p.strong_count() != 0);
            entry.builtin || !entry.programs.is_empty()
        });
        if state.entries.len() != previous || state.entries.is_empty() {
            state.entries.shrink_to_fit();
            let bytes = state.entries.capacity() * size_of::<Entry>();
            if let Some(storage) = &mut state.storage {
                storage.resize(bytes as u64).unwrap();
            }
            if state.entries.is_empty() {
                state.storage = None;
            }
        }
    }
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.0.lock().unwrap().entries.len()
    }
    pub(super) fn get(
        &self,
        gpu: &Gpu,
        quality: Quality,
        program: &Arc<MaskProgram>,
        parameters: [f32; 5],
        budget: &BudgetPool,
    ) -> Result<Prepared, GpuError> {
        self.prune();
        let mut state = self.0.lock().unwrap();
        let hash = program.structure_hash();
        let matches = |entry: &&Entry| {
            entry.hash == hash
                && entry.operations == program.operations()
                && entry.images == program.sources().len()
        };
        let existing_plan = state
            .entries
            .iter()
            .filter(matches)
            .find_map(|entry| entry.pipeline.split_plan.clone());
        let has_general = state
            .entries
            .iter()
            .filter(matches)
            .any(|entry| !entry.split);
        let split = if let Some(plan) = existing_plan {
            // A cache eviction must not change the selected pipeline between
            // frame admission and replay. The compiled pipeline owns its plan.
            state.split.get(program, parameters, budget).or_else(|| {
                plan.plan
                    .evaluate(program, parameters)
                    .map(|values| crate::common::mask::split::Prepared { plan, values })
            })
        } else if !has_general && crate::common::mask::plan(program).is_none() {
            state.split.get(program, parameters, budget)
        } else {
            None
        };
        let split_plan = split.as_ref().map(|split| &split.plan.plan);
        if let Some(entry) = state.entries.iter_mut().find(|entry| {
            entry.hash == hash
                && entry.split == split.is_some()
                && (entry
                    .programs
                    .iter()
                    .any(|p| p.as_ptr() == Arc::as_ptr(program))
                    || entry.operations == program.operations()
                        && entry.images == program.sources().len())
        }) {
            if entry.builtin {
                return Ok(Prepared {
                    pipeline: entry.pipeline.clone(),
                    split,
                });
            }
            if !entry
                .programs
                .iter()
                .any(|p| p.as_ptr() == Arc::as_ptr(program))
            {
                if entry.programs.len() == entry.programs.capacity() {
                    let capacity = entry.programs.len() + 1;
                    let bytes = entry.operations.capacity() * size_of::<MaskOp>()
                        + capacity * size_of::<Weak<MaskProgram>>()
                        + size_of::<Compiled>()
                        + 2 * size_of::<usize>();
                    resize(&mut entry.storage, bytes, gpu, budget)?;
                    entry.programs.reserve_exact(1);
                }
                entry.programs.push(Arc::downgrade(program));
            }
            return Ok(Prepared {
                pipeline: entry.pipeline.clone(),
                split,
            });
        }
        // Count formatting bytes before allocating the source. Driver-private compiler
        // storage is owned by wgpu; generated source and cache metadata are charged here.
        let mut count = Counter(0);
        shader_with_split(program, split_plan, &mut count).unwrap();
        let _construction = reserve(
            budget,
            count.0 + (8 + program.sources().len()) * size_of::<wgpu::BindGroupLayoutEntry>(),
            gpu,
        )?;
        let mut code = String::with_capacity(count.0);
        shader_with_split(program, split_plan, &mut code).unwrap();
        let builtin = program.is_image_threshold();
        let bytes = size_of_val(program.operations())
            + if builtin {
                0
            } else {
                size_of::<Weak<MaskProgram>>()
            }
            + size_of::<Compiled>()
            + 2 * size_of::<usize>();
        let storage = reserve(budget, bytes, gpu)?;
        if state.entries.len() == state.entries.capacity() {
            let bytes = (state.entries.len() + 1) * size_of::<Entry>();
            if let Some(ticket) = &mut state.storage {
                resize(ticket, bytes, gpu, budget)?;
            } else {
                state.storage = Some(reserve(budget, bytes, gpu)?);
            }
            state.entries.reserve_exact(1);
        }
        let compiled = pipeline::validated(gpu, || {
            let mut bindings = Vec::with_capacity(8 + program.sources().len());
            bindings.extend([
                pipeline::texture(0),
                pipeline::texture(1),
                pipeline::sampler(2),
                wgpu::BindGroupLayoutEntry {
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ..pipeline::uniform(3)
                },
                pipeline::texture(4),
                pipeline::sampler(5),
                pipeline::sampler(6),
                pipeline::sampler(7),
            ]);
            bindings.extend((0..program.sources().len()).map(|i| pipeline::texture(i as u32 + 8)));
            let layout = gpu
                .device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("mask expression"),
                    entries: &bindings,
                });
            let normal = pipeline::pipeline(
                &gpu.device,
                quality,
                &layout,
                &code,
                false,
                #[cfg(test)]
                None,
            );
            let blend = pipeline::pipeline_entry(
                &gpu.device,
                quality,
                &layout,
                &code,
                false,
                pipeline::FragmentOutput {
                    entry: "fragment",
                    blend: None,
                },
                #[cfg(test)]
                None,
            );
            let extended_blend = pipeline::pipeline_entry(
                &gpu.device,
                quality,
                &layout,
                &code,
                false,
                pipeline::FragmentOutput {
                    entry: "extended_blend",
                    blend: None,
                },
                #[cfg(test)]
                None,
            );
            Compiled {
                layout,
                normal,
                blend,
                extended_blend,
                split_plan: split.as_ref().map(|split| split.plan.clone()),
            }
        })?;
        let compiled = Arc::new(compiled);
        state.entries.push(Entry {
            builtin,
            split: split.is_some(),
            hash,
            operations: program.operations().to_vec(),
            images: program.sources().len(),
            programs: if builtin {
                Vec::new()
            } else {
                vec![Arc::downgrade(program)]
            },
            pipeline: compiled.clone(),
            storage,
        });
        Ok(Prepared {
            pipeline: compiled,
            split,
        })
    }
    pub(super) fn prepare_builtin(
        &self,
        gpu: &Gpu,
        quality: Quality,
        budget: &BudgetPool,
    ) -> Result<(), GpuError> {
        if self
            .0
            .lock()
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.builtin)
        {
            return Ok(());
        }
        self.get(
            gpu,
            quality,
            &MaskProgram::image("image-threshold".into()),
            [0.; 5],
            budget,
        )?;
        Ok(())
    }
}
struct Counter(usize);
impl std::fmt::Write for Counter {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        self.0 += text.len();
        Ok(())
    }
}

pub(super) struct Inputs<'a> {
    pub source: &'a wgpu::TextureView,
    pub mask: &'a wgpu::TextureView,
    pub backdrop: &'a wgpu::TextureView,
    pub images: Vec<&'a super::resources::Image>,
}
/// GPU parameter storage plus a spill allocation only beyond the four-image inline capacity.
impl Prepared {
    pub(super) fn parameter_cost(&self, images: usize) -> u64 {
        let bytes = pipeline::COMPOSITE_BYTES
            + 64
            + images as u64 * 16
            + if self.split.is_some() { 64 } else { 0 };
        bytes * if images > 4 { 2 } else { 1 }
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "the draw call borrows GPU resources and receives per-node mask parameters"
    )]
    pub(super) fn draw(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        target: &Target,
        inputs: Inputs<'_>,
        params: pipeline::CompositeParams,
        mask: &FrameImageMask,
        transform: Affine,
        samplers: &[wgpu::Sampler; 4],
    ) {
        let matrix = transform.as_coeffs();
        let header = pipeline::COMPOSITE_WORDS;
        let base_words = header + 16 + inputs.images.len() * 4;
        let mut words = smallvec::SmallVec::<[u32; 60]>::with_capacity(
            base_words + if self.split.is_some() { 16 } else { 0 },
        );
        words.resize(base_words, 0);
        words[..header].copy_from_slice(&params.uniform_words(target, inputs.source));
        words[header..header + 8].copy_from_slice(
            &[
                matrix[0] as f32,
                matrix[2] as f32,
                matrix[4] as f32,
                0.,
                matrix[1] as f32,
                matrix[3] as f32,
                matrix[5] as f32,
                0.,
            ]
            .map(f32::to_bits),
        );
        for (word, value) in words[header + 8..header + 13]
            .iter_mut()
            .zip(mask.parameters)
        {
            *word = value.to_bits();
        }
        for (parameters, image) in words[header + 16..]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(&inputs.images)
        {
            let l = image.layout;
            parameters.copy_from_slice(
                &[
                    l.content_size[0] as f32 / l.storage_size[0] as f32,
                    l.content_size[1] as f32 / l.storage_size[1] as f32,
                    f32::from(image.encoding == reel_ui::resources::ColorEncoding::Linear),
                    match image.texture_format {
                        Some(reel_core::texture::TextureFormat::Bc1Rgb) => 1.,
                        Some(reel_core::texture::TextureFormat::Bc3Rgba) => 2.,
                        _ => 0.,
                    },
                ]
                .map(f32::to_bits),
            );
        }
        if let Some(split) = &self.split {
            words.extend(split.values.seeds.map(f32::to_bits));
            words.extend(
                split
                    .values
                    .coordinates
                    .into_iter()
                    .flatten()
                    .map(f32::to_bits),
            );
        }
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mask parameters"),
            contents: bytemuck::cast_slice(&words),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let mut bindings = Vec::with_capacity(8 + inputs.images.len());
        bindings.extend([
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(inputs.source),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(inputs.mask),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(&samplers[0]),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(inputs.backdrop),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::Sampler(&samplers[1]),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::Sampler(&samplers[2]),
            },
            wgpu::BindGroupEntry {
                binding: 7,
                resource: wgpu::BindingResource::Sampler(&samplers[3]),
            },
        ]);
        bindings.extend(
            inputs
                .images
                .iter()
                .enumerate()
                .map(|(i, image)| wgpu::BindGroupEntry {
                    binding: i as u32 + 8,
                    resource: wgpu::BindingResource::TextureView(&image.view),
                }),
        );
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mask expression"),
            layout: &self.pipeline.layout,
            entries: &bindings,
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("masked subtree"),
            color_attachments: &[Some(target.attachment(params.clear))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            multiview_mask: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(if params.blend == 0 && !params.operation.replaces() {
            &self.pipeline.normal
        } else if params.blend > 4 {
            &self.pipeline.extended_blend
        } else {
            &self.pipeline.blend
        });
        params.scissor.apply(&mut pass);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
fn shader(program: &MaskProgram, code: &mut impl std::fmt::Write) -> std::fmt::Result {
    shader_with_split(program, None, code)
}
fn shader_with_split(
    program: &MaskProgram,
    split: Option<&crate::common::mask::split::SplitPlan>,
    code: &mut impl std::fmt::Write,
) -> std::fmt::Result {
    let shared_threshold_sample = match crate::common::mask::plan(program) {
        Some(crate::common::mask::Plan::ImageThreshold { alpha, gray }) => Some((alpha, gray)),
        _ => None,
    };
    let number = |bits| format!("{:.9e}", f32::from_bits(bits));
    let (before_fields, rest) = include_str!("composite.wgsl")
        .split_once("/*MASK_FIELDS*/")
        .unwrap();
    let (before_coverage, rest) = rest.split_once("/*MASK_COVERAGE*/").unwrap();
    let (_, after_coverage) = rest.split_once("/*END_MASK_COVERAGE*/").unwrap();
    code.write_str(before_fields)?;
    code.write_str(
        ", mask_uv0: vec4<f32>, mask_uv1: vec4<f32>, mask_values: vec4<f32>, mask_flags: vec4<f32>",
    )?;
    if !program.sources().is_empty() {
        write!(
            code,
            ", mask_texels: array<vec4<f32>,{}>",
            program.sources().len()
        )?;
    }
    if split.is_some() {
        code.write_str(", split_seeds: array<vec4<f32>,2>, split_coordinates: array<vec4<f32>,2>")?;
    }
    code.write_str(before_coverage)?;
    code.write_str(after_coverage)?;
    code.write_str("\n@group(0) @binding(5) var repeat_x: sampler;\n@group(0) @binding(6) var repeat_y: sampler;\n@group(0) @binding(7) var repeat_xy: sampler;\n")?;
    for i in 0..program.sources().len() {
        writeln!(
            code,
            "@group(0) @binding({}) var image_{i}: texture_2d<f32>;",
            i + 8
        )
        .unwrap();
    }
    code.write_str("\nfn mask_smooth(a:f32,b:f32,x:f32)->f32 { if a==b { return select(0.0,1.0,x>=a); } let t=clamp((x-a)/(b-a),0.0,1.0); return t*t*(3.0-2.0*t); }\nfn mask_gray(s:vec4<f32>,encoding:f32)->f32 { if s.a<=0.0 { return 0.0; } var rgb=clamp(s.rgb/s.a,vec3(0.0),vec3(1.0)); if encoding==0.0 { rgb=encoded_srgb(rgb); } return dot(rgb,vec3(0.2126,0.7152,0.0722)); }\nfn expression_coverage(position:vec2<f32>)->f32 { let uv=vec2(dot(params.mask_uv0.xy,position)+params.mask_uv0.z,dot(params.mask_uv1.xy,position)+params.mask_uv1.z); if any(uv<vec2(0.0)) || any(uv>vec2(1.0)) { return 0.0; }\n")?;
    if let Some(paired) = crate::common::mask::paired::Plan::new(program) {
        return paired_expression(program, &paired, code);
    }
    if let Some(split) = split {
        return split_expression(split, code);
    }
    let registers = crate::common::mask::RegisterPlan::new(program);
    if let Some(plan) = &registers {
        for index in 0..plan.register_count() {
            writeln!(code, "var n{index}:f32=0.0;")?;
        }
    }
    for (i, original) in program.operations().iter().enumerate() {
        let remapped = registers.as_ref().map(|plan| plan.operation(original));
        let operation = remapped.as_ref().unwrap_or(original);
        let destination = registers
            .as_ref()
            .map_or(i as u32, |plan| plan.destination(i));
        let declaration = if registers.is_some() { "" } else { "let " };
        let expression = match *operation {
            MaskOp::Constant(bits) => number(bits),
            MaskOp::Parameter(p) => format!("params.mask_values[{p}]"),
            MaskOp::Sample {
                image,
                channel,
                coordinates: [x, y, sx, sy],
                repeat,
            } => {
                // The common classifier proves that alpha and gray use the
                // same image, coordinates and repeat. Reuse the sampled color.
                if let Some((alpha, gray)) = shared_threshold_sample
                    && i == gray
                {
                    writeln!(
                        code,
                        "{declaration}n{destination}=mask_gray(sample_{alpha},texels_{alpha}.z);"
                    )?;
                    continue;
                }
                writeln!(code, "var coords_{i}=uv*vec2(n{sx},n{sy})+vec2(n{x},n{y});")?;
                if repeat & 1 != 0 {
                    writeln!(code, "coords_{i}.x=fract(coords_{i}.x);")?;
                }
                if repeat & 2 != 0 {
                    writeln!(code, "coords_{i}.y=fract(coords_{i}.y);")?;
                }
                writeln!(
                    code,
                    "let texels_{i}=params.mask_texels[{image}];let half_{i}=0.5/vec2<f32>(textureDimensions(image_{image}));var sample_{i}=textureSampleLevel(image_{image},linear_sampler,clamp(coords_{i}*texels_{i}.xy,half_{i},texels_{i}.xy-half_{i}),0.0);if texels_{i}.w==1.0{{sample_{i}.a=1.0;}}if texels_{i}.w==2.0{{sample_{i}=vec4<f32>(min(sample_{i}.rgb,vec3<f32>(sample_{i}.a)),sample_{i}.a);}}"
                )?;
                match channel {
                    MaskChannel::Alpha => format!("sample_{i}.a"),
                    MaskChannel::Gray => format!("mask_gray(sample_{i},texels_{i}.z)"),
                }
            }
            MaskOp::Add(a, b) => format!("n{a}+n{b}"),
            MaskOp::Subtract(a, b) => format!("n{a}-n{b}"),
            MaskOp::Multiply(a, b) => format!("n{a}*n{b}"),
            MaskOp::Pow(a, p) => {
                if f32::from_bits(p) == 0. {
                    "1.0".into()
                } else {
                    format!("pow(max(n{a},0.0),{})", number(p))
                }
            }
            MaskOp::Clamp(a, min, max) => format!("clamp(n{a},{},{})", number(min), number(max)),
            MaskOp::Smoothstep(a, b, x) => format!("mask_smooth(n{a},n{b},n{x})"),
            MaskOp::ImageThreshold { alpha, gray } => {
                writeln!(code, "if params.mask_values.x<=0.0 {{ return 0.0; }} if params.mask_values.x>=1.0 {{ return 1.0; }} let gray_{i}=select(n{gray},1.0-n{gray},params.mask_flags.x!=0.0); var threshold_{i}=select(0.0,1.0,gray_{i}<=params.mask_values.x); if params.mask_values.y>0.0 {{ threshold_{i}=clamp((params.mask_values.x*(1.0+params.mask_values.y)-gray_{i})/params.mask_values.y,0.0,1.0); }}").unwrap();
                format!("n{alpha}*threshold_{i}")
            }
        };
        writeln!(code, "{declaration}n{destination}={expression};").unwrap();
    }
    let last = program.operations().len() - 1;
    let result = registers
        .as_ref()
        .map_or(last as u32, |plan| plan.destination(last));
    writeln!(code, "return clamp(n{result},0.0,1.0); }}").unwrap();
    Ok(())
}

fn paired_expression(
    program: &MaskProgram,
    plan: &crate::common::mask::paired::Plan,
    code: &mut impl std::fmt::Write,
) -> std::fmt::Result {
    use crate::common::mask::paired::Step;
    let number = |bits| format!("{:.9e}", f32::from_bits(bits));
    for (index, operation) in program.operations().iter().enumerate() {
        if !plan.is_uniform(index) {
            continue;
        }
        let value = match *operation {
            MaskOp::Constant(bits) => number(bits),
            MaskOp::Parameter(p) => format!("params.mask_values[{p}]"),
            MaskOp::Add(a, b) => format!("u{a}+u{b}"),
            MaskOp::Subtract(a, b) => format!("u{a}-u{b}"),
            MaskOp::Multiply(a, b) => format!("u{a}*u{b}"),
            MaskOp::Clamp(a, low, high) => format!("clamp(u{a},{},{})", number(low), number(high)),
            _ => unreachable!("paired uniforms contain only basic scalar arithmetic"),
        };
        writeln!(code, "let u{index}={value};")?;
    }
    for (index, sample) in plan.samples.iter().enumerate() {
        let image = sample.image;
        let [x, y, sx, sy] = sample.coordinates;
        writeln!(code, "var c{index}=uv*vec2(u{sx},u{sy})+vec2(u{x},u{y});")?;
        if sample.repeat & 1 != 0 {
            writeln!(code, "c{index}.x=fract(c{index}.x);")?;
        }
        if sample.repeat & 2 != 0 {
            writeln!(code, "c{index}.y=fract(c{index}.y);")?;
        }
        writeln!(
            code,
            "let t{index}=params.mask_texels[{image}];let h{index}=0.5/vec2<f32>(textureDimensions(image_{image}));var s{index}=textureSampleLevel(image_{image},linear_sampler,clamp(c{index}*t{index}.xy,h{index},t{index}.xy-h{index}),0.0);if t{index}.w==1.0{{s{index}.a=1.0;}}"
        )?;
        if sample.channel == MaskChannel::Gray {
            writeln!(
                code,
                "if t{index}.w==2.0{{s{index}=vec4(min(s{index}.rgb,vec3(s{index}.a)),s{index}.a);}}var p{index}=mask_gray(s{index},t{index}.z);"
            )?;
        } else {
            writeln!(code, "var p{index}=s{index}.a;")?;
        }
    }
    for (index, chain) in plan.chains.iter().enumerate() {
        if index == 2 {
            let operator = match plan.join {
                1 => "+",
                2 => "-",
                3 => "*",
                _ => unreachable!(),
            };
            writeln!(code, "var p2=p0{operator}p1;")?;
        }
        for step in chain.iter().flatten() {
            let value = format!("p{index}");
            let expression = match *step {
                Step::Add(a) => format!("{value}+u{a}"),
                Step::Subtract(a) => format!("{value}-u{a}"),
                Step::SubtractFrom(a) => format!("u{a}-{value}"),
                Step::Multiply(a) => format!("{value}*u{a}"),
                Step::Pow(bits) if f32::from_bits(bits) == 0. => "1.0".into(),
                Step::Pow(bits) => format!("pow(max({value},0.0),{})", number(bits)),
                Step::Clamp(low, high) => {
                    format!("clamp({value},{},{})", number(low), number(high))
                }
                Step::Smoothstep(a, b) => format!("mask_smooth(u{a},u{b},{value})"),
            };
            writeln!(code, "{value}={expression};")?;
        }
    }
    code.write_str("return clamp(p2,0.0,1.0); }\n")
}

fn split_expression(
    plan: &crate::common::mask::split::SplitPlan,
    code: &mut impl std::fmt::Write,
) -> std::fmt::Result {
    for slot in 0..8 {
        writeln!(
            code,
            "var n{slot}=params.split_seeds[{}][{}];",
            slot / 4,
            slot % 4
        )?;
    }
    for (i, fetch) in plan.fetches().enumerate() {
        let image = fetch.image;
        writeln!(
            code,
            "let c{i}=params.split_coordinates[{i}];var at{i}=uv*c{i}.zw+c{i}.xy;"
        )?;
        if fetch.repeat & 1 != 0 {
            writeln!(code, "at{i}.x=fract(at{i}.x);")?;
        }
        if fetch.repeat & 2 != 0 {
            writeln!(code, "at{i}.y=fract(at{i}.y);")?;
        }
        writeln!(
            code,
            "let t{i}=params.mask_texels[{image}];let h{i}=0.5/vec2<f32>(textureDimensions(image_{image}));var s{i}=textureSampleLevel(image_{image},linear_sampler,clamp(at{i}*t{i}.xy,h{i},t{i}.xy-h{i}),0.0);if t{i}.w==1.0{{s{i}.a=1.0;}}if t{i}.w==2.0{{s{i}=vec4<f32>(min(s{i}.rgb,vec3<f32>(s{i}.a)),s{i}.a);}}"
        )?;
        if let Some(slot) = fetch.alpha {
            writeln!(code, "n{slot}=s{i}.a;")?;
        }
        if let Some(slot) = fetch.gray {
            writeln!(code, "n{slot}=mask_gray(s{i},t{i}.z);")?;
        }
    }
    let number = |bits| format!("{:.9e}", f32::from_bits(bits));
    for instruction in plan.instructions() {
        let expression = match instruction.operation {
            MaskOp::Add(a, b) => format!("n{a}+n{b}"),
            MaskOp::Subtract(a, b) => format!("n{a}-n{b}"),
            MaskOp::Multiply(a, b) => format!("n{a}*n{b}"),
            MaskOp::Pow(_, p) if f32::from_bits(p) == 0. => "1.0".into(),
            MaskOp::Pow(a, p) => format!("pow(max(n{a},0.0),{})", number(p)),
            MaskOp::Clamp(a, low, high) => format!("clamp(n{a},{},{})", number(low), number(high)),
            MaskOp::Smoothstep(a, b, x) => format!("mask_smooth(n{a},n{b},n{x})"),
            _ => unreachable!("split arithmetic contains no sampling or uniform leaf"),
        };
        writeln!(code, "n{}={expression};", instruction.destination)?;
    }
    writeln!(code, "return clamp(n{},0.0,1.0); }}", plan.result())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_threshold_reuses_one_sample_without_changing_composition() {
        let mut source = String::new();
        shader(&MaskProgram::image("mask".into()), &mut source).unwrap();
        assert_eq!(source.matches("textureSampleLevel(image_").count(), 1);
        assert!(source.contains("n2=sample_2.a;"));
        assert!(source.contains("n0=mask_gray(sample_2,texels_2.z)"));
        assert!(source.contains("n0=n2*threshold_4;"));
    }

    mod dag_fixture {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/mask_dag.rs"
        ));
    }

    #[test]
    fn scalar_register_masks_validate_as_static_wgsl() {
        for operations in [
            dag_fixture::operations(),
            dag_fixture::compact_operations(),
            vec![MaskOp::Constant(1_f32.to_bits()); 33],
        ] {
            let program = MaskProgram::new(
                operations,
                vec!["a".into(), "b".into(), "c".into(), "d".into()],
            )
            .unwrap();
            let mut source = String::new();
            shader(&program, &mut source).unwrap();
            let module = wgpu::naga::front::wgsl::parse_str(&source).unwrap();
            wgpu::naga::valid::Validator::new(
                wgpu::naga::valid::ValidationFlags::all(),
                wgpu::naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap();
            if let Some(plan) = crate::common::mask::RegisterPlan::new(&program) {
                assert_eq!(source.matches(":f32=0.0;").count(), plan.register_count());
                assert!(source.contains("coords_4=uv*vec2("));
            } else {
                assert!(source.contains("let n32="));
            }
        }
    }

    #[test]
    fn split_masks_validate_static_samples_and_parameter_layout() {
        for length in [8, 12, 13] {
            let program = MaskProgram::new(
                dag_fixture::operations()[..length].to_vec(),
                vec!["a".into(), "b".into(), "c".into(), "d".into()],
            )
            .unwrap();
            let split = crate::common::mask::split::SplitPlan::new(&program).unwrap();
            let mut source = String::new();
            shader_with_split(&program, Some(&split), &mut source).unwrap();
            assert_eq!(
                source.matches("textureSampleLevel(image_").count(),
                split.fetch_count()
            );
            let module = wgpu::naga::front::wgsl::parse_str(&source).unwrap();
            wgpu::naga::valid::Validator::new(
                wgpu::naga::valid::ValidationFlags::all(),
                wgpu::naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap();
        }
    }

    #[test]
    fn paired_masks_emit_two_static_channels_and_keep_each_scalar_chain() {
        for first in [MaskChannel::Alpha, MaskChannel::Gray] {
            for second in [MaskChannel::Alpha, MaskChannel::Gray] {
                for repeat in 0..4 {
                    let mut operations = dag_fixture::operations()[..13].to_vec();
                    for (index, channel) in [(3, first), (4, second)] {
                        if let MaskOp::Sample {
                            channel: output,
                            repeat: output_repeat,
                            ..
                        } = &mut operations[index]
                        {
                            *output = channel;
                            *output_repeat = repeat;
                        }
                    }
                    let program =
                        MaskProgram::new(operations, vec!["a".into(), "b".into()]).unwrap();
                    assert!(crate::common::mask::paired::Plan::new(&program).is_some());
                    let mut source = String::new();
                    shader(&program, &mut source).unwrap();
                    assert_eq!(source.matches("textureSampleLevel(image_").count(), 2);
                    assert_eq!(
                        source.matches("=mask_gray(s").count(),
                        usize::from(first == MaskChannel::Gray)
                            + usize::from(second == MaskChannel::Gray)
                    );
                    assert!(source.contains("var p2=p0-p1;"));
                    assert!(source.contains("p2=mask_smooth("));
                    assert!(!source.contains("read_node"));
                    let module = wgpu::naga::front::wgsl::parse_str(&source).unwrap();
                    wgpu::naga::valid::Validator::new(
                        wgpu::naga::valid::ValidationFlags::all(),
                        wgpu::naga::valid::Capabilities::all(),
                    )
                    .validate(&module)
                    .unwrap();
                }
            }
        }
    }
}
