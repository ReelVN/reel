//! One host declaration for source analysis, bytecode linking and VM admission.
#![forbid(unsafe_code)]
extern crate self as aside_host;

mod binding;
mod declaration;
mod property;
pub mod source;
pub use aside_compiler;
pub use aside_host_macros::*;
pub use aside_vm;
pub use binding::*;
pub use declaration::*;
pub use property::*;

/// Store Markdown beside a handwritten native declaration.
///
/// `#[native]` extracts function documentation automatically. Resumable natives
/// can use this macro and assign the resulting constant to their binding.
#[macro_export]
macro_rules! native_documentation {
    ($vis:vis fn $name:ident { $($(#[doc = $line:expr])+ $($symbol:literal)|+;)* }) => {
        $vis fn $name(symbol: &str) -> Option<&'static str> {
            match symbol {
                $($($symbol)|+ => Some(concat!($($line, "\n",)+)),)*
                _ => None,
            }
        }
    };
    ($(#[doc = $line:expr])* $vis:vis const $name:ident;) => {
        $(#[doc = $line])*
        $vis const $name: &str = concat!($($line, "\n",)*);
    };
}
