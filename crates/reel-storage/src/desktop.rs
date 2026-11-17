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
    let staging = Staging::new(target.parent().unwrap())?;
    let candidate = staging.path().join("candidate");
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
    let cleanup_warning = staging.close().err().map(|error| {
        format!(
            "artifact published; staging cleanup failed at {}: {error}",
            candidate.parent().unwrap().display()
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

pub(super) fn entries<F>(
    directory: &Path,
    select: F,
) -> io::Result<impl Iterator<Item = io::Result<(PathBuf, fs::File)>> + use<F>>
where
    F: Fn(&Path) -> bool,
{
    Ok(fs::read_dir(directory)?.filter_map(move |entry| {
        match entry.and_then(|entry| Ok(entry.file_type()?.is_file().then(|| entry.path()))) {
            Ok(Some(path)) if select(&path) => Some(fs::File::open(&path).map(|file| (path, file))),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        }
    }))
}
