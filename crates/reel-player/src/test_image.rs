//! Shared test helper for an owning image pixel reader.
pub(crate) fn data(lease: &reel_ui::resources::ResourceLease) -> reel_ui::peniko::ImageData {
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
