use super::{GpuError, Renderer, SurfacePass, WgpuBackend, renderer::Reservation};
use crate::capture::{CaptureBackend, CapturedPixels};
use reel_core::{
    budget::{BudgetPool, BudgetTicket, OwnedBytes},
    wake::WakeSignal,
};
use reel_ui::frame::RenderFrame;
use std::sync::{
    Arc,
    mpsc::{Receiver, TryRecvError},
};

pub(super) struct ReadbackStorage {
    buffer: wgpu::Buffer,
    _storage: Reservation,
}
pub struct GpuCapture {
    storage: Arc<ReadbackStorage>,
    ready: Receiver<Result<(), wgpu::BufferAsyncError>>,
    size: [u32; 2],
    row: u32,
    output: Option<BudgetTicket>,
    generation: super::DeviceGeneration,
    mapped: bool,
    mapping_live: bool,
    pixels: Vec<u8>,
    wake: WakeSignal,
}
impl Drop for GpuCapture {
    fn drop(&mut self) {
        if self.mapping_live {
            self.storage.buffer.unmap();
        }
    }
}
impl Renderer {
    pub fn capture(
        &mut self,
        frame: &RenderFrame,
        size: [u32; 2],
        output: &BudgetPool,
        wake: WakeSignal,
    ) -> Result<GpuCapture, GpuError> {
        let mut prepared = self.prepare_capture(frame, size)?;
        let row = size[0]
            .checked_mul(4)
            .ok_or(GpuError::InvalidSize)?
            .div_ceil(256)
            * 256;
        let bytes = u64::from(row) * u64::from(size[1]);
        let output = output
            .reserve(u64::from(size[0]) * u64::from(size[1]) * 4)
            .map_err(|e| GpuError::Readback(e.message))?;
        // Use the window's output conversion, so PNG pixels have the same sRGB appearance.
        let target_ticket =
            self.reserve_readback(&prepared, u64::from(size[0]) * u64::from(size[1]) * 4)?;
        let texture = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("capture sRGB"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        if self.capture_output.is_none() {
            self.capture_output = Some(SurfacePass::new(
                &self.gpu.device,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            )?);
        }
        self.capture_output.as_ref().unwrap().encode(
            &self.gpu.device,
            &mut prepared,
            &texture.create_view(&Default::default()),
        );
        let storage = Arc::new(ReadbackStorage {
            _storage: self.reserve_readback(&prepared, bytes)?,
            buffer: self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("capture readback"),
                size: bytes,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
        });
        prepared.encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &storage.buffer,
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
        prepared.capture = Some(storage.clone());
        prepared.retain_capture_target(target_ticket);
        self.submit(prepared)?;
        let (sender, ready) = std::sync::mpsc::sync_channel(1);
        let map_wake = wake.clone();
        storage
            .buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
                map_wake.wake();
            });
        Ok(GpuCapture {
            storage,
            ready,
            size,
            row,
            output: Some(output),
            generation: self.gpu.generation(),
            mapped: false,
            mapping_live: true,
            pixels: Vec::new(),
            wake,
        })
    }
    pub fn poll_capture(
        &mut self,
        capture: &mut GpuCapture,
        max_bytes: u64,
    ) -> Result<Option<CapturedPixels>, GpuError> {
        self.gpu.check()?;
        if !capture.mapping_live {
            return Ok(None);
        }
        if capture.generation != self.gpu.generation() {
            return Err(GpuError::WrongDevice);
        }
        if !capture.mapped {
            self.gpu
                .device
                .poll(wgpu::PollType::Poll)
                .map_err(|e| GpuError::Readback(e.to_string()))?;
            match capture.ready.try_recv() {
                Err(TryRecvError::Empty) => return Ok(None),
                Err(TryRecvError::Disconnected) => {
                    return Err(GpuError::Readback("capture completion disconnected".into()));
                }
                Ok(result) => result.map_err(|e| GpuError::Readback(e.to_string()))?,
            }
            capture.mapped = true;
            let total = (capture.size[0] as usize * 4)
                .checked_mul(capture.size[1] as usize)
                .ok_or(GpuError::InvalidSize)?;
            let pixels = Vec::with_capacity(total);
            capture
                .output
                .as_mut()
                .unwrap()
                .resize(pixels.capacity() as u64)
                .map_err(|e| GpuError::Readback(e.message))?;
            capture.pixels = pixels;
        }
        let width = capture.size[0] as usize * 4;
        let first = capture.pixels.len() / width;
        let rows =
            crate::capture::readback_rows(width, capture.size[1] as usize - first, max_bytes);
        let mapped = capture
            .storage
            .buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        for row in mapped
            .chunks_exact(capture.row as usize)
            .skip(first)
            .take(rows)
        {
            capture.pixels.extend_from_slice(&row[..width]);
        }
        drop(mapped);
        if first + rows < capture.size[1] as usize {
            capture.wake.wake();
            return Ok(None);
        }
        capture.storage.buffer.unmap();
        capture.mapping_live = false;
        let rgba = OwnedBytes::new(
            std::mem::take(&mut capture.pixels),
            capture.output.take().unwrap(),
        )
        .map_err(|e| GpuError::Readback(e.reason.message))?;
        Ok(Some(CapturedPixels {
            size: capture.size,
            rgba,
            encoding: crate::capture::CaptureEncoding::Srgb,
        }))
    }
}
impl CaptureBackend for WgpuBackend {
    type Error = GpuError;
    type Capture = GpuCapture;
    fn capture(
        &mut self,
        frame: &RenderFrame,
        size: [u32; 2],
        output: &BudgetPool,
        wake: WakeSignal,
    ) -> Result<GpuCapture, GpuError> {
        self.renderer_mut().capture(frame, size, output, wake)
    }
    fn poll_capture(
        &mut self,
        capture: &mut GpuCapture,
        max_bytes: u64,
    ) -> Result<Option<CapturedPixels>, GpuError> {
        self.renderer_mut().poll_capture(capture, max_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::r01_fixture::*;
    use reel_core::{
        budget::{BudgetCategory, QueueLimits},
        identity::FrameId,
    };
    use reel_ui::{
        frame::{FrameBuild, FrameInfo},
        imaging::{
            Composite,
            record::{Draw, Geometry, Scene},
        },
        kurbo::{Affine, Rect},
        peniko::{Brush, Color},
    };

    #[test]
    #[ignore = "requires graphics adapter; synthetic numeric buffer comparison"]
    fn sliced_capture_matches_full_and_cancel_releases_output() {
        let mut renderer = Renderer::new(
            pollster::block_on(super::super::Gpu::request(
                &wgpu::Instance::default(),
                None,
                super::super::DeviceGeneration(1),
                Default::default(),
            ))
            .unwrap(),
        );
        let source_pool = pool();
        let mut scene = Scene::new();
        for y in 0..11 {
            scene.draw(Draw::Fill {
                transform: Affine::IDENTITY,
                fill_rule: Default::default(),
                brush: Brush::Solid(Color::from_rgba8(y * 21, 255 - y * 19, y * 13, 255)),
                brush_transform: None,
                shape: Geometry::Rect(Rect::new(0., f64::from(y), 65., f64::from(y + 1))),
                composite: Composite::default(),
            });
        }
        let mut widget = RecordWidget::new(scene);
        widget.clip = false;
        let mut bridge = widget_bridge(widget, Affine::IDENTITY);
        let mut build = FrameBuild::new(
            bridge.record(FrameId(1)).unwrap(),
            empty_snapshot(&source_pool),
            FrameInfo {
                logical_size: [65., 11.],
                physical_size: [65, 11],
            },
            limits(),
            &source_pool,
        )
        .unwrap();
        while !build.step(step_budget()).unwrap().complete {}
        let frame = build.finish().unwrap();
        let output = BudgetPool::new(
            BudgetCategory::Temporary,
            QueueLimits {
                max_requests: 4,
                max_bytes: 1 << 20,
            },
        );
        let wake = WakeSignal::new(|| {});
        let mut full = renderer
            .capture(&frame, [65, 11], &output, wake.clone())
            .unwrap();
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        let reference = renderer.poll_capture(&mut full, u64::MAX).unwrap().unwrap();
        assert_ne!(
            &reference.rgba.as_slice()[0..4],
            &reference.rgba.as_slice()[260..264]
        );
        let mut sliced = renderer
            .capture(&frame, [65, 11], &output, wake.clone())
            .unwrap();
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        let mut previous = 0;
        let result = loop {
            let result = renderer.poll_capture(&mut sliced, 520).unwrap();
            if let Some(result) = result {
                break result;
            }
            assert_eq!(sliced.pixels.len() - previous, 520);
            previous = sliced.pixels.len();
        };
        assert_eq!(result.rgba.as_slice(), reference.rgba.as_slice());
        assert_eq!(result.encoding, reference.encoding);
        drop((result, reference, sliced, full));
        assert_eq!(output.usage().bytes, 0);
        renderer.poll().unwrap();
        let mut cancelled = renderer.capture(&frame, [65, 11], &output, wake).unwrap();
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        assert!(renderer.poll_capture(&mut cancelled, 1).unwrap().is_none());
        assert_eq!(cancelled.pixels.len(), 260);
        drop(cancelled);
        assert_eq!(output.usage().bytes, 0);
        renderer.poll().unwrap();
        let pending = renderer
            .capture(&frame, [65, 11], &output, WakeSignal::new(|| {}))
            .unwrap();
        drop(pending);
        assert_eq!(output.usage().bytes, 0);
        renderer
            .gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        renderer.poll().unwrap();
    }
}
