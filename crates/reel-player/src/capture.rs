//! Host-independent frame capture and background PNG encoding.
use crate::assets::worker::Worker;
use image::ImageEncoder;
use reel_core::{
    budget::{BudgetCategory, BudgetPool, BudgetTicket, OwnedBytes, QueueLimits},
    identity::Priority,
    wake::WakeSignal,
};
use reel_engine::aside::{
    AdapterError, SessionVm,
    capture::{CaptureId, CaptureRequest},
};
use reel_render::capture::{CaptureBackend, CapturedPixels};
use reel_ui::frame::RenderFrame;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

struct PngOutput<'a> {
    bytes: Vec<u8>,
    storage: BudgetTicket,
    cancelled: &'a AtomicBool,
}
impl Write for PngOutput<'_> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::other("capture cancelled"));
        }
        let needed = self
            .bytes
            .len()
            .checked_add(input.len())
            .ok_or_else(|| io::Error::other("capture encoding size overflow"))?;
        if needed > self.bytes.capacity() {
            let capacity = needed.max(self.bytes.capacity().max(8192).saturating_mul(2));
            // Account for both buffers during reallocation, then only the retained capacity.
            self.storage
                .resize(self.bytes.capacity() as u64 + capacity as u64)
                .map_err(|e| io::Error::other(e.message))?;
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(io::Error::other)?;
            self.storage
                .resize(self.bytes.capacity() as u64)
                .map_err(|e| io::Error::other(e.message))?;
        }
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct EncodeResult {
    id: CaptureId,
    result: Result<OwnedBytes, String>,
}
enum Phase<C> {
    Waiting,
    Reading(C),
    Encoding(u64),
}
struct Pending<C> {
    request: CaptureRequest,
    frame: RenderFrame,
    size: [u32; 2],
    phase: Phase<C>,
}
pub struct CaptureService<C> {
    active: Option<Pending<C>>,
    worker: Worker<(CaptureId, CapturedPixels), EncodeResult>,
    pool: BudgetPool,
    wake: WakeSignal,
    serial: u64,
}
impl<C> CaptureService<C> {
    pub fn new(max_bytes: u64, wake: WakeSignal) -> Self {
        let pool = BudgetPool::new(
            BudgetCategory::Temporary,
            QueueLimits {
                max_requests: u32::MAX,
                max_bytes,
            },
        );
        let encoding_pool = pool.clone();
        let worker = Worker::new(
            QueueLimits {
                max_requests: 1,
                max_bytes,
            },
            wake.clone(),
            move |(id, pixels): (CaptureId, CapturedPixels), cancelled| {
                let result = (|| {
                    let storage = encoding_pool.reserve(0).map_err(|e| e.message)?;
                    let mut output = PngOutput {
                        bytes: Vec::new(),
                        storage,
                        cancelled,
                    };
                    image::codecs::png::PngEncoder::new(&mut output)
                        .write_image(
                            pixels.rgba.as_slice(),
                            pixels.size[0],
                            pixels.size[1],
                            image::ExtendedColorType::Rgba8,
                        )
                        .map_err(|e| e.to_string())?;
                    OwnedBytes::new(output.bytes, output.storage).map_err(|e| e.reason.message)
                })();
                EncodeResult { id, result }
            },
            |event| event.result = Err("capture cancelled".into()),
        );
        Self {
            active: None,
            worker,
            pool,
            wake,
            serial: 0,
        }
    }
    pub fn busy(&self) -> bool {
        self.active.is_some() || !self.worker.is_idle()
    }
    pub fn drive<B: CaptureBackend<Capture = C>>(
        &mut self,
        backend: &mut B,
        adapter: &mut SessionVm,
        frame: Option<&RenderFrame>,
    ) -> Result<bool, AdapterError> {
        self.wake.acknowledge();
        let mut changed = false;
        let mut events = Vec::new();
        self.worker.drain(&mut events, 1);
        for event in events {
            if self
                .active
                .as_ref()
                .is_some_and(|p| p.request.id == event.id)
            {
                changed |= adapter.complete_capture_request(
                    event.id,
                    event.result.map(|bytes| bytes.into_parts().0),
                )?;
                self.active = None;
            }
        }
        if let Some(active) = &self.active
            && !adapter.capture_pending(active.request.id)?
        {
            if let Phase::Encoding(id) = active.phase {
                self.worker.cancel(id);
            }
            self.active = None;
        }
        if self.active.is_none()
            && self.worker.is_idle()
            && let Some(request) = adapter.take_capture_request()?
        {
            if let Some(frame) = frame {
                let original = frame.info().physical_size;
                let size = match (request.width, request.height) {
                    (Some(w), Some(h)) => [w, h],
                    (Some(w), None) => [
                        w,
                        ((u64::from(w) * u64::from(original[1])) / u64::from(original[0]))
                            .max(1)
                            .try_into()
                            .unwrap_or(u32::MAX),
                    ],
                    (None, Some(h)) => [
                        ((u64::from(h) * u64::from(original[0])) / u64::from(original[1]))
                            .max(1)
                            .try_into()
                            .unwrap_or(u32::MAX),
                        h,
                    ],
                    _ => original,
                };
                self.active = Some(Pending {
                    request,
                    frame: frame.clone(),
                    size,
                    phase: Phase::Waiting,
                });
            } else {
                changed |= adapter.complete_capture_request(
                    request.id,
                    Err("capture requires an actually presented frame".into()),
                )?;
            }
        }
        let Some(active) = &mut self.active else {
            return Ok(changed);
        };
        let result = match &mut active.phase {
            Phase::Waiting => {
                match backend.capture(&active.frame, active.size, &self.pool, self.wake.clone()) {
                    Ok(capture) => {
                        active.phase = Phase::Reading(capture);
                        Ok(())
                    }
                    Err(error) => Err(error.to_string()),
                }
            }
            Phase::Reading(capture) => match backend.poll_capture(capture) {
                Ok(Some(pixels)) => {
                    self.serial += 1;
                    let bytes = pixels.rgba.ticket().bytes();
                    match self.worker.accept(
                        self.serial,
                        (active.request.id, pixels),
                        bytes,
                        Priority::Visible,
                    ) {
                        Ok(()) => {
                            active.phase = Phase::Encoding(self.serial);
                            Ok(())
                        }
                        Err(error) => Err(error.reason.message),
                    }
                }
                Ok(None) => Ok(()),
                Err(error) => Err(error.to_string()),
            },
            Phase::Encoding(_) => Ok(()),
        };
        if let Err(error) = result {
            changed |= adapter.complete_capture_request(active.request.id, Err(error))?;
            self.active = None;
        }
        Ok(changed)
    }
}
