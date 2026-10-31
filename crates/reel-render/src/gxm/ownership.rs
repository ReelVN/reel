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
    pub fn clear(&self, target: native::Target<'_>, color: [f32; 4]) -> Result<(), native::Error> {
        #[cfg(feature = "profiling")]
        if crate::profiling::serial_effect_timing() {
            let size = self.attachment_size(target);
            return self.serial_pass(false, size, size, |context| context.clear(target, color));
        }
        self.native.clear(target, color)
    }

    #[inline]
    pub fn draw_filtered(
        &self,
        draw: native::Draw<'_>,
        filter: native::TextureFilter,
    ) -> Result<(), native::Error> {
        #[cfg(feature = "profiling")]
        if crate::profiling::serial_effect_timing() {
            let storage = self.attachment_size(draw.target);
            return self.serial_pass(true, draw.size, storage, |context| {
                context.draw_filtered(draw, filter)
            });
        }
        self.native.draw_filtered(draw, filter)
    }

    #[cfg(feature = "profiling")]
    fn attachment_size(&self, target: native::Target<'_>) -> [u32; 2] {
        match target {
            native::Target::Screen => self.native.output_size(),
            native::Target::Framebuffer(framebuffer) => framebuffer.size(),
        }
    }

    #[cfg(feature = "profiling")]
    fn serial_pass(
        &self,
        paint: bool,
        viewport: [u32; 2],
        storage: [u32; 2],
        operation: impl FnOnce(&native::Context) -> Result<(), native::Error>,
    ) -> Result<(), native::Error> {
        use std::time::Instant;
        let prior = Instant::now();
        self.native.finish()?;
        let prior_drain_us = prior.elapsed().as_micros() as u64;
        let submit = Instant::now();
        operation(&self.native)?;
        let submit_wall_us = submit.elapsed().as_micros() as u64;
        let completion = Instant::now();
        self.native.finish()?;
        let completion_drain_us = completion.elapsed().as_micros() as u64;
        crate::profiling::record_serial_pass(
            paint,
            crate::profiling::SerialPassTiming {
                calls: 1,
                viewport_pixels: u64::from(viewport[0]) * u64::from(viewport[1]),
                storage_pixels: u64::from(storage[0]) * u64::from(storage[1]),
                prior_drain_us,
                submit_wall_us,
                completion_drain_us,
                max_completion_drain_us: completion_drain_us,
            },
        );
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
