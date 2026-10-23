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
        storage
            .buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
                wake.wake();
            });
        Ok(GpuCapture {
            storage,
            ready,
            size,
            row,
            output: Some(output),
            generation: self.gpu.generation(),
        })
    }
    pub fn poll_capture(
        &mut self,
        capture: &mut GpuCapture,
    ) -> Result<Option<CapturedPixels>, GpuError> {
        self.gpu.check()?;
        if capture.generation != self.gpu.generation() {
            return Err(GpuError::WrongDevice);
        }
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
        let mapped = capture
            .storage
            .buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let width = capture.size[0] as usize * 4;
        let mut rgba = Vec::with_capacity(width * capture.size[1] as usize);
        for row in mapped.chunks_exact(capture.row as usize) {
            rgba.extend_from_slice(&row[..width]);
        }
        drop(mapped);
        capture.storage.buffer.unmap();
        let rgba = OwnedBytes::new(rgba, capture.output.take().unwrap())
            .map_err(|e| GpuError::Readback(e.reason.message))?;
        Ok(Some(CapturedPixels {
            size: capture.size,
            rgba,
        }))
    }
}
impl CaptureBackend for Renderer {
    type Error = GpuError;
    type Capture = GpuCapture;
    fn capture(
        &mut self,
        frame: &RenderFrame,
        size: [u32; 2],
        output: &BudgetPool,
        wake: WakeSignal,
    ) -> Result<GpuCapture, GpuError> {
        Renderer::capture(self, frame, size, output, wake)
    }
    fn poll_capture(
        &mut self,
        capture: &mut GpuCapture,
    ) -> Result<Option<CapturedPixels>, GpuError> {
        Renderer::poll_capture(self, capture)
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
    ) -> Result<Option<CapturedPixels>, GpuError> {
        self.renderer_mut().poll_capture(capture)
    }
}
