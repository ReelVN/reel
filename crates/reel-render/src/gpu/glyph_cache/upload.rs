use super::*;

#[expect(
    clippy::too_many_arguments,
    reason = "one preparation owns the encoder, caches and budget scopes"
)]
pub(super) fn flush(
    gpu: &Gpu,
    encoder: &mut wgpu::CommandEncoder,
    uploads: &mut crate::common::glyph_upload::Batch<Arc<Glyph>>,
    budgets: &Budgets,
    local: &Budgets,
    tickets: &mut Vec<Reservation>,
    images: &super::super::image_cache::ImageCache,
    cache: &GlyphCache,
) -> Result<(), GpuError> {
    let page = |glyph: &Arc<Glyph>| match glyph.as_ref() {
        Glyph::Image { page, .. } => Arc::as_ptr(page),
        Glyph::Empty(_) => unreachable!(),
    };
    uploads.flush(
        |a, b| page(a) == page(b),
        |upload| {
            let Glyph::Image { page, .. } = upload.owner.as_ref() else {
                unreachable!()
            };
            let row = upload.size[0].div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
                * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let cost = u64::from(row) * u64::from(upload.size[1]);
            images.make_room(&budgets.resources, cost, gpu.limits.resource_bytes);
            cache.make_room(&budgets.resources, cost, gpu.limits.resource_bytes);
            tickets.push(reserve(
                &budgets.resources,
                &local.resources,
                cost,
                gpu.limits.resource_bytes,
                "glyph upload",
            )?);
            let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("glyph atlas batch"),
                size: cost,
                usage: wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: true,
            });
            {
                let mut mapping = staging
                    .slice(..)
                    .get_mapped_range_mut()
                    .map_err(|error| GpuError::Device(error.to_string()))?;
                let mut bytes = mapping.slice(..);
                bytes.fill(0);
                upload.write_rows(row as usize, |start, data| {
                    bytes.slice(start..start + data.len()).copy_from_slice(data)
                });
            }
            staging.unmap();
            encoder.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &staging,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(row),
                        rows_per_image: None,
                    },
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &page.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: upload.origin[0],
                        y: upload.origin[1],
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: upload.size[0],
                    height: upload.size[1],
                    depth_or_array_layers: 1,
                },
            );
            #[cfg(test)]
            {
                gpu.upload_creations
                    .staging
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                gpu.upload_creations
                    .copies
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            Ok(())
        },
        |_| (),
    )
}
