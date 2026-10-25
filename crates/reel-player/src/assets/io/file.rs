//! Native file offsets stay 64-bit on Vita; archives use the ordinary Read/Seek
//! interfaces and share one file choice with distribution loading.
#[cfg(target_os = "vita")]
pub(crate) use pvr_psp2_sys::platform::fs::{File, Metadata, metadata};
#[cfg(not(target_os = "vita"))]
pub(crate) use std::fs::{File, Metadata, metadata};
