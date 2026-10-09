//! Blocking diagnostic readback, kept out of the interactive render path.
use super::{GpuError, Prepared, Renderer};
impl Renderer {
    pub fn readback(&mut self, mut prepared: Prepared) -> Result<Vec<[f32; 4]>, GpuError> {
        self.validate_prepared(&prepared)?;
        let bytes_per_pixel = if prepared.quality.format == wgpu::TextureFormat::Rgba16Float {
            8
        } else {
            4
        };
        let row = (prepared.size[0] * bytes_per_pixel).div_ceil(256) * 256;
        let size = prepared.size;
        let bytes = u64::from(row) * u64::from(size[1]);
        let output_bytes = u64::from(size[0]) * u64::from(size[1]) * 16;
        let _storage = self.reserve_readback(bytes * 2 + output_bytes)?;
        let buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("diagnostic readback"),
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        prepared.encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &prepared.texture,
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
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
        );
        self.submit(prepared)?;
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        self.gpu
            .device
            .poll(wgpu::PollType::Wait)
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        receiver
            .recv()
            .map_err(|e| GpuError::Readback(e.to_string()))?
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let mapped = buffer.slice(..).get_mapped_range();
        let mut out = Vec::with_capacity(size[0] as usize * size[1] as usize);
        for y in 0..size[1] as usize {
            for x in 0..size[0] as usize {
                let offset = y * row as usize + x * bytes_per_pixel as usize;
                out.push(std::array::from_fn(|c| {
                    if bytes_per_pixel == 8 {
                        half::f16::from_le_bytes([
                            mapped[offset + c * 2],
                            mapped[offset + c * 2 + 1],
                        ])
                        .to_f32()
                    } else {
                        f32::from(mapped[offset + c]) / 255.
                    }
                }));
            }
        }
        drop(mapped);
        buffer.unmap();
        self.poll()?;
        Ok(out)
    }
}
