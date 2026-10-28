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
