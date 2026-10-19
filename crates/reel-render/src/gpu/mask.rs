//! One compiled coverage function per live scalar-mask structure and quality.
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
}
struct Entry {
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
        let previous = state.entries.len();
        state.entries.retain_mut(|entry| {
            entry.programs.retain(|p| p.strong_count() != 0);
            !entry.programs.is_empty()
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
        budget: &BudgetPool,
    ) -> Result<Arc<Compiled>, GpuError> {
        self.prune();
        let mut state = self.0.lock().unwrap();
        let hash = program.structure_hash();
        if let Some(entry) = state.entries.iter_mut().find(|entry| {
            entry.hash == hash
                && (entry
                    .programs
                    .iter()
                    .any(|p| p.as_ptr() == Arc::as_ptr(program))
                    || entry.operations == program.operations()
                        && entry.images == program.sources().len())
        }) {
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
            return Ok(entry.pipeline.clone());
        }
        let limits = gpu.device.limits();
        if program.sources().len() + 3 > limits.max_sampled_textures_per_shader_stage as usize {
            return Err(GpuError::Device(format!(
                "mask needs {} sampled textures; device supports {}",
                program.sources().len() + 3,
                limits.max_sampled_textures_per_shader_stage
            )));
        }
        // Count formatting bytes before allocating the source. Driver-private compiler
        // storage is owned by wgpu; generated source and cache metadata are charged here.
        let mut count = Counter(0);
        shader(program, &mut count).unwrap();
        let _construction = reserve(
            budget,
            count.0 + (8 + program.sources().len()) * size_of::<wgpu::BindGroupLayoutEntry>(),
            gpu,
        )?;
        let mut code = String::with_capacity(count.0);
        shader(program, &mut code).unwrap();
        let bytes = program.operations().len() * size_of::<MaskOp>()
            + size_of::<Weak<MaskProgram>>()
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
                pipeline::uniform(3),
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
                "fragment",
                None,
                #[cfg(test)]
                None,
            );
            Compiled {
                layout,
                normal,
                blend,
            }
        })?;
        let compiled = Arc::new(compiled);
        state.entries.push(Entry {
            hash,
            operations: program.operations().to_vec(),
            images: program.sources().len(),
            programs: vec![Arc::downgrade(program)],
            pipeline: compiled.clone(),
            storage,
        });
        Ok(compiled)
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
    pub images: Vec<&'a wgpu::TextureView>,
}
impl Compiled {
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
        let (mode, progress, softness) = pipeline::operation_words(params.operation);
        let mut words = [0_u32; 24];
        words[..8].copy_from_slice(&[
            params.opacity.to_bits(),
            mode,
            progress.to_bits(),
            softness.to_bits(),
            params.invert.to_bits(),
            params.blend,
            0,
            0,
        ]);
        words[8..16].copy_from_slice(
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
        for (word, value) in words[16..21].iter_mut().zip(mask.parameters) {
            *word = value.to_bits();
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
                    resource: wgpu::BindingResource::TextureView(image),
                }),
        );
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mask expression"),
            layout: &self.layout,
            entries: &bindings,
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("masked subtree"),
            color_attachments: &[Some(target.attachment(false))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            multiview_mask: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(if params.blend == 0 {
            &self.normal
        } else {
            &self.blend
        });
        params.scissor.apply(&mut pass);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
    }
}

fn shader(program: &MaskProgram, code: &mut impl std::fmt::Write) -> std::fmt::Result {
    let number = |bits| format!("{:.9e}", f32::from_bits(bits));
    let (before_fields, rest) = include_str!("composite.wgsl")
        .split_once("/*MASK_FIELDS*/")
        .unwrap();
    let (before_coverage, rest) = rest.split_once("/*MASK_COVERAGE*/").unwrap();
    let (_, after_coverage) = rest.split_once("/*END_MASK_COVERAGE*/").unwrap();
    code.write_str(before_fields)?;
    code.write_str(", padding: vec2<f32>, mask_uv0: vec4<f32>, mask_uv1: vec4<f32>, mask_values: vec4<f32>, mask_flags: vec4<f32>")?;
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
    code.write_str("\nfn mask_smooth(a:f32,b:f32,x:f32)->f32 { if a==b { return select(0.0,1.0,x>=a); } let t=clamp((x-a)/(b-a),0.0,1.0); return t*t*(3.0-2.0*t); }\nfn mask_gray(s:vec4<f32>)->f32 { if s.a<=0.0 { return 0.0; } return dot(encoded_srgb(clamp(s.rgb/s.a,vec3(0.0),vec3(1.0))),vec3(0.2126,0.7152,0.0722)); }\nfn expression_coverage(position:vec2<f32>)->f32 { let uv=vec2(dot(params.mask_uv0.xy,position)+params.mask_uv0.z,dot(params.mask_uv1.xy,position)+params.mask_uv1.z); if any(uv<vec2(0.0)) || any(uv>vec2(1.0)) { return 0.0; }\n")?;
    for (i, operation) in program.operations().iter().enumerate() {
        let expression = match *operation {
            MaskOp::Constant(bits) => number(bits),
            MaskOp::Parameter(p) => format!("params.mask_values[{p}]"),
            MaskOp::Sample {
                image,
                channel,
                coordinates: [x, y, sx, sy],
                repeat,
            } => {
                let sampler =
                    ["linear_sampler", "repeat_x", "repeat_y", "repeat_xy"][repeat as usize];
                writeln!(code, "let sample_{i}=textureSampleLevel(image_{image},{sampler},uv*vec2(n{sx},n{sy})+vec2(n{x},n{y}),0.0);").unwrap();
                match channel {
                    MaskChannel::Alpha => format!("sample_{i}.a"),
                    MaskChannel::Gray => format!("mask_gray(sample_{i})"),
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
        writeln!(code, "let n{i}={expression};").unwrap();
    }
    let result = program.operations().len() - 1;
    writeln!(code, "return clamp(n{result},0.0,1.0); }}").unwrap();
    Ok(())
}
