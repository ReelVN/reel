#![cfg(feature = "wgpu")]
//! Compare numeric coverage samples, without capturing or displaying a frame.
use reel_render::gpu::{DeviceGeneration, Gpu, GpuLimits};

#[test]
#[ignore = "requires a WGPU adapter; reads only numeric mismatch counters"]
fn single_channel_samples_match_alpha_at_centers_and_linear_intervals() {
    let gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        DeviceGeneration(1),
        GpuLimits::default(),
    ))
    .unwrap();
    let shader = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("coverage channel numerical comparison"),
            source: wgpu::ShaderSource::Wgsl(
                r#"
@group(0) @binding(0) var single: texture_2d<f32>;
@group(0) @binding(1) var rgba: texture_2d<f32>;
@group(0) @binding(2) var linear_sampler: sampler;
@group(0) @binding(3) var<storage, read_write> mismatches: array<atomic<u32>, 3>;
@group(0) @binding(4) var expected: texture_2d<f32>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = f32(id.x) * 0.5;
    let uv = vec2<f32>((x + 0.5) / 256.0, 0.5);
    let red = textureSampleLevel(single, linear_sampler, uv, 0.0).r;
    let alpha = textureSampleLevel(rgba, linear_sampler, uv, 0.0).a;
    if red != alpha { atomicAdd(&mismatches[id.x & 1u], 1u); }
    if (id.x & 1u) == 0u && red != textureLoad(expected, vec2<i32>(i32(id.x / 2u), 0), 0).r {
        atomicAdd(&mismatches[2], 1u);
    }
}
"#
                .into(),
            ),
        });
    let pipeline = gpu
        .device
        .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("coverage compare"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
    let writer = gpu
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("coverage numeric store"),
            source: wgpu::ShaderSource::Wgsl(
                r#"
@group(0) @binding(0) var input: texture_2d<f32>;
@vertex fn vertex(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
}
@fragment fn fragment(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    return vec4<f32>(textureLoad(input, vec2<i32>(i32(p.x), 0), 0).r);
}
"#
                .into(),
            ),
        });
    let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let boundary = [
        0x0000u16, 0x0001, 0x0002, 0x03fe, 0x03ff, 0x0400, 0x0401, 0x0fff, 0x1000, 0x1001, 0x2fff,
        0x3000, 0x3001, 0x3554, 0x3555, 0x3556, 0x37ff, 0x3800, 0x3801, 0x3bfe, 0x3bff, 0x3c00,
    ];
    for half in [false, true] {
        let (one, four) = if half {
            (
                wgpu::TextureFormat::R16Float,
                wgpu::TextureFormat::Rgba16Float,
            )
        } else {
            (
                wgpu::TextureFormat::R8Unorm,
                wgpu::TextureFormat::Rgba8Unorm,
            )
        };
        for format in [one, four] {
            let f = gpu.adapter.get_texture_format_features(format);
            assert!(f.allowed_usages.contains(
                wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT
            ));
            assert!(
                f.flags
                    .contains(wgpu::TextureFormatFeatureFlags::FILTERABLE)
            );
        }
        let extent = wgpu::Extent3d {
            width: 256,
            height: 1,
            depth_or_array_layers: 1,
        };
        let create = |format| {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("numeric coverage values"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
        };
        let single = create(one);
        let rgba = create(four);
        let written_single = create(one);
        let written_rgba = create(four);
        let mut single_data = Vec::new();
        let mut rgba_data = Vec::new();
        for i in 0..256usize {
            if half {
                let bits = if i < boundary.len() {
                    boundary[i]
                } else {
                    half::f16::from_f32(i as f32 / 255.).to_bits()
                };
                single_data.extend(bits.to_le_bytes());
                rgba_data.extend([0u8; 6]);
                rgba_data.extend(bits.to_le_bytes());
            } else {
                single_data.push(i as u8);
                rgba_data.extend([0, 0, 0, i as u8]);
            }
        }
        for (texture, bytes, stride) in [
            (&single, &single_data, if half { 512 } else { 256 }),
            (&rgba, &rgba_data, if half { 2048 } else { 1024 }),
        ] {
            gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: None,
                },
                extent,
            );
        }
        let output = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("coverage mismatch counts"),
            size: 12,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("coverage numeric result"),
            size: 12,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let input_view = single.create_view(&Default::default());
        let single_view = written_single.create_view(&Default::default());
        let rgba_view = written_rgba.create_view(&Default::default());
        let bindings = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("coverage numeric samples"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&single_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&rgba_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: output.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
            ],
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        for (format, target) in [(one, &single_view), (four, &rgba_view)] {
            let writer_pipeline =
                gpu.device
                    .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                        label: Some("coverage numeric store"),
                        layout: None,
                        vertex: wgpu::VertexState {
                            module: &writer,
                            entry_point: Some("vertex"),
                            compilation_options: Default::default(),
                            buffers: &[],
                        },
                        fragment: Some(wgpu::FragmentState {
                            module: &writer,
                            entry_point: Some("fragment"),
                            compilation_options: Default::default(),
                            targets: &[Some(wgpu::ColorTargetState {
                                format,
                                blend: None,
                                write_mask: wgpu::ColorWrites::ALL,
                            })],
                        }),
                        primitive: Default::default(),
                        depth_stencil: None,
                        multisample: Default::default(),
                        multiview_mask: None,
                        cache: None,
                    });
            let input = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("coverage numeric source"),
                layout: &writer_pipeline.get_bind_group_layout(0),
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                }],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("coverage numeric store"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                multiview_mask: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&writer_pipeline);
            pass.set_bind_group(0, &input, &[]);
            pass.draw(0..3, 0..1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("numeric coverage check"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bindings, &[]);
            pass.dispatch_workgroups(8, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output, 0, &staging, 0, 12);
        gpu.queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        receiver.recv().unwrap().unwrap();
        let mapped = staging.slice(..).get_mapped_range().unwrap();
        let counts: &[u32] = bytemuck::cast_slice(&mapped);
        assert_eq!(
            counts,
            &[0, 0, 0],
            "{one:?}: center and bilinear interval mismatch counts"
        );
        drop(mapped);
        staging.unmap();
        eprintln!("{one:?} versus {four:?}: 256 centers and 256 linear intervals agree");
    }
}
