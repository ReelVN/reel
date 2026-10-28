//! Shared recording fixtures for backend and frame-planning unit tests.
#[allow(dead_code, reason = "shared UI recording fixture")]
#[path = "../../reel-ui/tests/support/fixture.rs"]
pub(crate) mod fixture;
#[allow(dead_code, reason = "shared frame recording fixture")]
#[path = "../../reel-ui/tests/support/r01_fixture.rs"]
pub(crate) mod r01_fixture;

/// Build the legacy inline test brush from an explicit owning pixel reader.
pub(crate) fn image_data(lease: &reel_ui::resources::ResourceLease) -> reel_ui::peniko::ImageData {
    let image = lease.image_descriptor().expect("test image descriptor");
    let pixels = lease.read_pixels().expect("test image pixels");
    reel_ui::peniko::ImageData {
        data: pixels.blob(),
        format: image.format,
        alpha_type: image.alpha_type,
        width: image.width,
        height: image.height,
    }
}
