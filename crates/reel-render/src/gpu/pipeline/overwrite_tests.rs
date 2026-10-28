//! Numeric coverage checks over recycled, padded and multisampled attachments.
use super::*;
use crate::gpu::{DeviceGeneration, targets::Scissor};

fn clear(encoder: &mut wgpu::CommandEncoder, target: &Target, color: wgpu::Color) {
    let mut attachment = target.attachment(true);
    attachment.ops.load = wgpu::LoadOp::Clear(color);
    drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        color_attachments: &[Some(attachment)],
        ..Default::default()
    }));
}

#[test]
#[ignore = "requires graphics adapter; synthetic numeric attachment comparison"]
fn replacement_overwrites_old_pixels_and_preserves_partial_initialization() {
    let gpu = pollster::block_on(Gpu::request(
        &wgpu::Instance::default(),
        None,
        DeviceGeneration(1),
        Default::default(),
    ))
    .unwrap();
    for samples in [1, 4] {
        let quality = Quality {
            format: wgpu::TextureFormat::Rgba8Unorm,
            samples,
            degraded: false,
        };
        let pipelines = Pipelines::new(&gpu.device, quality, &Creations::default());
        let source = Target::new(
            &gpu.device,
            [65, 11],
            Quality {
                samples: 1,
                ..quality
            },
        );
        let mask = Target::new(
            &gpu.device,
            [65, 11],
            Quality {
                samples: 1,
                ..quality
            },
        );
        for (storage, scissor) in [
            ([65, 11], Scissor::full([65, 11])),
            ([96, 16], Scissor::full([65, 11])),
            (
                [65, 11],
                Scissor {
                    x: 1,
                    y: 1,
                    width: 63,
                    height: 9,
                },
            ),
        ] {
            let mut target = Target::new(&gpu.device, storage, quality);
            target.size = [65, 11];
            // Reuse the same allocation after a nontransparent prior owner.
            for _ in 0..2 {
                let mut encoder = gpu.device.create_command_encoder(&Default::default());
                clear(
                    &mut encoder,
                    &source,
                    wgpu::Color {
                        r: 0.25,
                        g: 0.,
                        b: 0.,
                        a: 0.5,
                    },
                );
                clear(
                    &mut encoder,
                    &mask,
                    wgpu::Color {
                        r: 0.,
                        g: 0.25,
                        b: 0.,
                        a: 0.5,
                    },
                );
                clear(
                    &mut encoder,
                    &target,
                    wgpu::Color {
                        r: 1.,
                        g: 0.,
                        b: 1.,
                        a: 1.,
                    },
                );
                pipelines.composite(
                    &gpu.device,
                    &mut encoder,
                    &target,
                    CompositeInputs {
                        source: &source.view,
                        mask: &mask.view,
                        backdrop: &source.view,
                    },
                    CompositeParams {
                        clear: true,
                        scissor,
                        opacity: 1.,
                        invert: 0.,
                        operation: CompositeOperation::Transition(0.5),
                        blend: 0,
                        source_origin: [0; 2],
                        backdrop_origin: [0; 2],
                    },
                );
                let row = (storage[0] * 4).div_ceil(256) * 256;
                let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("numeric overwrite result"),
                    size: u64::from(row * storage[1]),
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                encoder.copy_texture_to_buffer(
                    wgpu::TexelCopyTextureInfo {
                        texture: &target.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyBufferInfo {
                        buffer: &buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(row),
                            rows_per_image: None,
                        },
                    },
                    wgpu::Extent3d {
                        width: storage[0],
                        height: storage[1],
                        depth_or_array_layers: 1,
                    },
                );
                gpu.queue.submit([encoder.finish()]);
                let (sender, receiver) = std::sync::mpsc::sync_channel(1);
                buffer
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |result| {
                        sender.send(result).unwrap()
                    });
                gpu.device
                    .poll(wgpu::PollType::wait_indefinitely())
                    .unwrap();
                receiver.recv().unwrap().unwrap();
                let mapped = buffer.slice(..).get_mapped_range().unwrap();
                for y in 0..storage[1] {
                    for x in 0..storage[0] {
                        let inside = x >= scissor.x
                            && x < scissor.x + scissor.width
                            && y >= scissor.y
                            && y < scissor.y + scissor.height;
                        let expected = if inside { [32u8, 32, 0, 128] } else { [0; 4] };
                        let offset = (y * row + x * 4) as usize;
                        for channel in 0..4 {
                            assert!(
                                mapped[offset + channel].abs_diff(expected[channel]) <= 1,
                                "samples={samples} storage={storage:?} ({x},{y}) channel={channel}"
                            );
                        }
                    }
                }
                drop(mapped);
                buffer.unmap();
                gpu.check().unwrap();
            }
        }
    }
}
