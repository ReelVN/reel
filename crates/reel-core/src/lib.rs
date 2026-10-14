//! Reel's platform-independent shared contracts, design baseline 1.1.
// Returning a large *original* request is an intentional ownership contract.
#![allow(clippy::result_large_err)]

pub mod audio;
pub mod budget;
pub mod clock;
pub mod identity;
pub mod io;
pub mod queue;
pub mod request_ids;
pub mod result;
pub mod text;
pub mod wake;
pub mod work;

#[cfg(feature = "test-support")]
pub mod test_support;
