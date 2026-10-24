use super::{Published, Staging, destination};
use atomic_write_file::AtomicWriteFile;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub(super) fn publish(
    target: &Path,
    overwrite: bool,
    fill: impl FnOnce(&mut dyn io::Write) -> io::Result<()>,
) -> io::Result<Published> {
    let target = destination(target)?;
    let mut staging = Staging::new(target.parent().unwrap())?;
    let candidate = staging.path.join("candidate");
    let mut file = AtomicWriteFile::open(&candidate)?;
    fill(&mut file)?;
    // Directory sync can fail after the dependency's rename. Its commit stays
    // inside private staging, so that error still preserves the destination.
    file.commit()?;
    if overwrite {
        fs::rename(&candidate, &target)?;
    } else {
        fs::hard_link(&candidate, &target)?;
    }
    let cleanup_warning = staging.cleanup().err().map(|error| {
        format!(
            "artifact published; staging cleanup failed at {}: {error}",
            staging.path.display()
        )
    });
    Ok(Published { cleanup_warning })
}

pub(super) fn open(target: &Path) -> io::Result<fs::File> {
    fs::File::open(target)
}

pub(super) fn remove(target: &Path) -> io::Result<Published> {
    fs::remove_file(target)?;
    Ok(Published {
        cleanup_warning: None,
    })
}

pub(super) fn entries(
    directory: &Path,
) -> io::Result<impl Iterator<Item = io::Result<PathBuf>> + use<>> {
    Ok(fs::read_dir(directory)?.filter_map(|entry| {
        match entry.and_then(|entry| Ok(entry.file_type()?.is_file().then(|| entry.path()))) {
            Ok(Some(path)) => Some(Ok(path)),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        }
    }))
}
