//! Remove the current project's generated files at the build boundary.
use std::{fs, io, path::Path};

pub fn run(resources: bool) -> io::Result<u8> {
    let project = reel_cli::project::locate(Path::new("."))?;
    let output = project.root.join(if resources {
        "build/resources"
    } else {
        "build"
    });
    if !output.try_exists()? {
        println!("Already clean: {}", output.display());
        return Ok(0);
    }
    // Resolve the destructive boundary before traversing it. A redirected build
    // directory may belong to another project or hold shared source resources.
    if output.canonicalize()? != output {
        return Err(io::Error::other(format!(
            "refusing to clean a redirected build directory: {}",
            output.display()
        )));
    }
    fs::remove_dir_all(&output)?;
    println!("Removed {}", output.display());
    Ok(0)
}
