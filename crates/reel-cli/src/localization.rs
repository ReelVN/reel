//! Publish compiler-generated languages through the ordinary text archive.
mod project;
pub(crate) use project::bundle_project;
pub use project::{PreparedCatalogs, check_project, prepare_project};
