//! Embed the main icon in a staged Windows release executable.

use std::{io, path::Path};

pub(super) fn write_icon(executable: &Path, icon: image::DynamicImage) -> io::Result<()> {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut player = editpe::Image::parse_file(executable)?;
        let mut resources = player.resource_directory().cloned().unwrap_or_default();
        resources.remove_main_icon()?;
        resources.set_main_icon(icon)?;
        player.set_resource_directory(resources)?;
        player.write_file(executable)?;
        Ok(())
    })();
    result.map_err(|error| {
        io::Error::other(format!(
            "write Windows icon to {}: {error}",
            executable.display()
        ))
    })
}
