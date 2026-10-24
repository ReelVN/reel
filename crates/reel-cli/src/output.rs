//! Author build path validation; atomic publication is shared with player storage.
pub use reel_storage::Published;
use reel_storage::publish;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Both paths are explicit. Validation and compilation happen before this call.
pub fn write(path: &Path, source: &Path, bytes: &[u8], overwrite: bool) -> io::Result<Published> {
    let target = target(path, source)?;
    publish(&target, overwrite, |file| file.write_all(bytes))
}

fn target(path: &Path, source: &Path) -> io::Result<PathBuf> {
    if path.extension().is_none_or(|extension| extension != "asb") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "output must be an explicit .asb file path",
        ));
    }
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "output filename is missing"))?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = parent.canonicalize()?;
    if !parent.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "output parent must be a directory",
        ));
    }
    let target = parent.join(name);
    let canonical_source = source.canonicalize()?;
    if canonical_source == target {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "output must not replace its source file",
        ));
    }
    match fs::symlink_metadata(&target) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "output must not be a symlink, directory or special file",
            ));
        }
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        Ok(_) if target.canonicalize()? == canonical_source => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "output must not replace its source file",
            ));
        }
        _ => {}
    }
    Ok(target)
}
