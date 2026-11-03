//! Native storage retains its budget ownership after a terminal driver failure.
use super::{GpuError, native, native_error};
use reel_core::budget::BudgetTicket;
use std::{cell::RefCell, ops::Deref, rc::Rc, sync::Arc};

#[derive(Clone)]
pub(super) struct Device {
    native: native::Context,
    fault: Rc<RefCell<Option<Arc<native::DeviceFault>>>>,
}
impl Device {
    #[inline]
    pub fn draw(&self, draw: native::Draw<'_>) -> Result<(), native::Error> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::DriverDraw);
        #[cfg(feature = "profiling")]
        {
            let data = self.draw_trace_data(&draw);
            return self.observe("native-draw", data, || self.native.draw(draw));
        }
        #[cfg(not(feature = "profiling"))]
        self.native.draw(draw)
    }

    #[inline]
    pub fn clear(&self, target: native::Target<'_>, color: [f32; 4]) -> Result<(), native::Error> {
        #[cfg(feature = "profiling")]
        if crate::profiling::serial_effect_timing() {
            let size = self.attachment_size(target);
            let pixels = u64::from(size[0]) * u64::from(size[1]);
            return self.serial_pass("clear", size, size, pixels, 3, |context| {
                context.clear(target, color)
            });
        }
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::DriverClear);
        #[cfg(feature = "profiling")]
        {
            let data = crate::profiling::TraceData {
                output_extent: self.attachment_size(target),
                known: crate::profiling::KNOWN_OUTPUT_EXTENT,
                count: 1,
                ..Default::default()
            };
            return self.observe("native-clear", data, || self.native.clear(target, color));
        }
        #[cfg(not(feature = "profiling"))]
        self.native.clear(target, color)
    }

    #[inline]
    pub fn draw_filtered(
        &self,
        draw: native::Draw<'_>,
        filter: native::TextureFilter,
        #[cfg(feature = "profiling")] label: &'static str,
    ) -> Result<(), native::Error> {
        #[cfg(feature = "profiling")]
        if crate::profiling::serial_effect_timing() {
            let storage = self.attachment_size(draw.target);
            let scissor = draw.scissor.unwrap_or([0, 0, draw.size[0], draw.size[1]]);
            let extent = [0, 1].map(|axis| draw.size[axis].min(storage[axis]));
            let covered = [0, 1].map(|axis| {
                scissor[axis]
                    .saturating_add(scissor[axis + 2])
                    .min(extent[axis])
                    .saturating_sub(scissor[axis].min(extent[axis]))
            });
            let pixels = u64::from(covered[0]) * u64::from(covered[1]);
            let instances = draw.instances.as_ref().map_or(1, |instances| {
                instances.range.end.saturating_sub(instances.range.start)
            });
            let references =
                u64::from(draw.range.end.saturating_sub(draw.range.start)) * u64::from(instances);
            return self.serial_pass(label, draw.size, storage, pixels, references, |context| {
                context.draw_filtered(draw, filter)
            });
        }
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::DriverDraw);
        #[cfg(feature = "profiling")]
        {
            let data = self.draw_trace_data(&draw);
            return self.observe(label, data, || self.native.draw_filtered(draw, filter));
        }
        #[cfg(not(feature = "profiling"))]
        self.native.draw_filtered(draw, filter)
    }

    #[cfg(feature = "profiling")]
    fn draw_trace_data(&self, draw: &native::Draw<'_>) -> crate::profiling::TraceData {
        use crate::profiling::*;
        let storage = self.attachment_size(draw.target);
        let extent = [0, 1].map(|axis| draw.size[axis].min(storage[axis]));
        let rect = draw.scissor.unwrap_or([0, 0, extent[0], extent[1]]);
        let low = [0, 1].map(|axis| rect[axis].min(extent[axis]));
        let high = [0, 1].map(|axis| rect[axis].saturating_add(rect[axis + 2]).min(extent[axis]));
        let mut data = TraceData {
            logical: draw.size,
            output_extent: storage,
            scissor: [
                low[0],
                low[1],
                high[0].saturating_sub(low[0]),
                high[1].saturating_sub(low[1]),
            ],
            known: KNOWN_LOGICAL | KNOWN_OUTPUT_EXTENT | KNOWN_SCISSOR | KNOWN_TEXTURE_BINDINGS,
            texture_bindings: draw.textures.len() as u32,
            count: 1,
            ..Default::default()
        };
        if let Some(source) = draw.textures.first() {
            data.source_extent = source.size();
            data.known |= KNOWN_SOURCE_EXTENT;
        }
        data
    }

    #[cfg(feature = "profiling")]
    fn observe<T>(
        &self,
        label: &'static str,
        data: crate::profiling::TraceData,
        operation: impl FnOnce() -> Result<T, native::Error>,
    ) -> Result<T, native::Error> {
        use crate::profiling::{KNOWN_ERROR_CODE, KNOWN_WALL, TracePass, TraceStatus};
        let trace = TracePass::begin(label, data);
        let started = trace.as_ref().map(|_| std::time::Instant::now());
        let result = operation();
        if let Some((trace, started)) = trace.zip(started) {
            let mut data = data;
            data.wall_ns = started.elapsed().as_nanos() as u64;
            data.known |= KNOWN_WALL;
            let status = match &result {
                Ok(_) => TraceStatus::Ok,
                Err(error) => {
                    let code = match error {
                        native::Error::Sdk(_, code) | native::Error::Allocation { code, .. } => {
                            Some(*code)
                        }
                        native::Error::Terminal(fault) => Some(fault.code),
                        _ => None,
                    };
                    if let Some(code) = code {
                        data.error_code = i64::from(code);
                        data.known |= KNOWN_ERROR_CODE;
                    }
                    TraceStatus::Error
                }
            };
            trace.finish(status, data);
        }
        result
    }

    #[cfg(feature = "profiling")]
    fn attachment_size(&self, target: native::Target<'_>) -> [u32; 2] {
        match target {
            native::Target::Screen => self.native.output_size(),
            native::Target::Framebuffer(framebuffer) => framebuffer.size(),
        }
    }

    #[cfg(feature = "profiling")]
    pub(super) fn finish_observed(&self, label: &'static str) -> Result<(), native::Error> {
        self.observe(label, Default::default(), || self.native.finish())
    }

    #[cfg(feature = "profiling")]
    fn serial_pass(
        &self,
        label: &'static str,
        viewport: [u32; 2],
        storage: [u32; 2],
        scissor_pixels: u64,
        geometry_references: u64,
        operation: impl FnOnce(&native::Context) -> Result<(), native::Error>,
    ) -> Result<(), native::Error> {
        use std::time::Instant;
        let prior = Instant::now();
        self.observe("serial-drain-before", Default::default(), || {
            self.native.finish()
        })?;
        let prior_drain_us = prior.elapsed().as_micros() as u64;
        let submit = Instant::now();
        self.observe(
            label,
            crate::profiling::TraceData {
                logical: viewport,
                output_extent: storage,
                known: crate::profiling::KNOWN_LOGICAL | crate::profiling::KNOWN_OUTPUT_EXTENT,
                count: 1,
                ..Default::default()
            },
            || operation(&self.native),
        )?;
        let submit_wall_us = submit.elapsed().as_micros() as u64;
        let completion = Instant::now();
        self.observe("serial-drain-after", Default::default(), || {
            self.native.finish()
        })?;
        let completion_drain_us = completion.elapsed().as_micros() as u64;
        crate::profiling::record_serial_pass(crate::profiling::SerialPassTiming {
            label,
            calls: 1,
            viewport_pixels: u64::from(viewport[0]) * u64::from(viewport[1]),
            storage_pixels: u64::from(storage[0]) * u64::from(storage[1]),
            scissor_pixels,
            geometry_references,
            prior_drain_us,
            submit_wall_us,
            completion_drain_us,
            max_completion_drain_us: completion_drain_us,
        });
        Ok(())
    }

    pub fn new(native: native::Context) -> Self {
        Self {
            native,
            fault: Default::default(),
        }
    }
    pub fn check(&self) -> Result<(), GpuError> {
        self.fault.borrow().as_ref().map_or(Ok(()), |fault| {
            Err(native_error(native::Error::Terminal(fault.clone())))
        })
    }
    pub fn refresh(&self) -> bool {
        if self.fault.borrow().is_none() {
            *self.fault.borrow_mut() = self.native.terminal_fault();
        }
        self.fault.borrow().is_some()
    }
    pub fn ticket(&self, ticket: BudgetTicket) -> NativeTicket {
        NativeTicket {
            ticket: Some(ticket),
            device: self.clone(),
        }
    }
    pub fn close(self) -> Result<(), (native::Context, native::Error)> {
        self.native.close()
    }
}
impl Deref for Device {
    type Target = native::Context;
    fn deref(&self) -> &Self::Target {
        &self.native
    }
}

/// Place this after native fields so their actual retirement can latch failure
/// before budget capacity is returned. Failed constructors use the same guard.
pub(super) struct NativeTicket {
    ticket: Option<BudgetTicket>,
    device: Device,
}
impl NativeTicket {
    pub fn bytes(&self) -> u64 {
        <Self as std::borrow::Borrow<BudgetTicket>>::borrow(self).bytes()
    }
}
impl std::borrow::Borrow<BudgetTicket> for NativeTicket {
    fn borrow(&self) -> &BudgetTicket {
        self.ticket.as_ref().expect("live native allocation ticket")
    }
}
impl Drop for NativeTicket {
    fn drop(&mut self) {
        if self.device.refresh() {
            // The native owner is retained until process exit. Its admission
            // capacity must remain occupied in every surviving pool observer.
            std::mem::forget(self.ticket.take());
        }
    }
}
