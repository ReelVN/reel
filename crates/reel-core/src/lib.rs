//! Shared identities, owned bytes, budgets, resource IO and background work.
//!
//! [`budget::OwnedBytes`] retains its reservation through physical release.
//! Service results transfer the original request or a completed owned result.
//! Acknowledge [`wake::WakeSignal`] before draining its sources, then recheck
//! pending work before sleeping. The `test-support` feature provides in-memory
//! IO and a cooperative executor for exercising these contracts.
// Returning a large *original* request is an intentional ownership contract.
#![allow(clippy::result_large_err)]

pub mod audio;
pub mod budget;
pub mod clock;
pub mod identity;
pub mod image;
pub mod image_coverage;
pub mod io;
pub mod prepared_atlas;
pub mod prepared_image;
pub mod profile;
pub mod queue;
pub mod request_ids;
pub mod result;
pub mod text;
pub mod texture;
pub mod wake;
pub mod window;
pub mod work;

#[cfg(feature = "test-support")]
pub mod test_support;
