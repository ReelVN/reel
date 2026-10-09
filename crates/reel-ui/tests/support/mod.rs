// Migrated ccd0547 real-control regressions; no experiment path or git dependency.
pub mod checks;
pub mod fixture;
pub mod presentation;
pub fn limits() -> reel_ui::app::GeometryLimits {
    reel_ui::app::GeometryLimits {
        max_nodes: 4096,
        max_bytes: 4 * 1024 * 1024,
        max_path: 128,
    }
}
pub mod raw_probe;
