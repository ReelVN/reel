//! One host declaration for source analysis, bytecode linking and VM admission.
#![forbid(unsafe_code)]
extern crate self as aside_host;

mod binding;
mod declaration;
mod property;
pub use aside_compiler;
pub use aside_host_macros::*;
pub use aside_vm;
pub use binding::*;
pub use declaration::*;
pub use property::*;
