//! Locate Reel projects for the native Aside language server.

use aside_lsp::ServerOptions;

pub use reel_editor::host_context;

/// Use Reel's editor declarations and discover source roots on the local disk.
pub fn options() -> ServerOptions {
    ServerOptions {
        project_prefix: Some("game".into()),
        source_root: Some(|directory, exists| {
            crate::project::locate_with(directory, exists)
                .ok()
                .map(|project| project.root.join("src"))
        }),
        ..reel_editor::options()
    }
}
