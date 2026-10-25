//! Atomic logical publication on filesystems whose rename cannot replace a file.
//! Immutable data and a synced commit record are prepared separately. Publishing
//! the record with native no-replace rename is the single visibility boundary.
use super::{Published, Staging};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

const PREFIX: &str = ".reel-store-";
static COMMIT: Mutex<()> = Mutex::new(());
#[derive(Clone, Copy)]
struct Commit {
    generation: u64,
    present: bool,
}
fn store(target: &Path) -> io::Result<PathBuf> {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "storage requires a UTF-8 filename",
            )
        })?;
    Ok(target
        .parent()
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "storage target needs a parent")
        })?
        .join(format!("{PREFIX}{name}")))
}
fn record(root: &Path, generation: u64) -> PathBuf {
    root.join(format!("{generation:016x}.commit"))
}
fn data(root: &Path, generation: u64) -> PathBuf {
    root.join(format!("{generation:016x}.data"))
}
fn current(root: &Path) -> io::Result<Option<Commit>> {
    // Vita newlib's opendir reports ENOTDIR for missing paths as well. Stat
    // first so a missing store stays distinct from an actual file in its place.
    match fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                root.display().to_string(),
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut latest = None;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str().and_then(|name| name.strip_suffix(".commit")) else {
            continue;
        };
        if name.len() != 16 {
            continue;
        }
        let Ok(generation) = u64::from_str_radix(name, 16) else {
            continue;
        };
        if latest.is_some_and(|old| old >= generation) {
            continue;
        }
        latest = Some(generation);
    }
    latest
        .map(|generation| {
            let mut bytes = [0; 2];
            let count = fs::File::open(record(root, generation))?.read(&mut bytes)?;
            if count != 1 || !matches!(bytes[0], b'P' | b'D') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid storage commit record",
                ));
            }
            Ok(Commit {
                generation,
                present: bytes[0] == b'P',
            })
        })
        .transpose()
}
fn next(root: &Path, old: Option<Commit>) -> io::Result<u64> {
    let mut generation = old.map_or(0, |old| old.generation);
    loop {
        generation = generation
            .checked_add(1)
            .ok_or_else(|| io::Error::other("storage generation exhausted"))?;
        // Interrupted publication can leave an uncommitted immutable data file. Its identity
        // is never reused; the next successful commit reclaims it.
        if !data(root, generation).try_exists()? && !record(root, generation).try_exists()? {
            return Ok(generation);
        }
    }
}
fn write_synced(
    path: &Path,
    fill: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    #[cfg(target_os = "vita")]
    let mut file = pvr_psp2_sys::platform::fs::File::create_new(path)?;
    #[cfg(not(target_os = "vita"))]
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    fill(&mut file)?;
    #[cfg(target_os = "vita")]
    {
        file.sync()?;
        file.close()?;
    }
    #[cfg(not(target_os = "vita"))]
    {
        file.sync_all()?;
        drop(file);
    }
    Ok(())
}
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(target_os = "vita")]
    {
        pvr_psp2_sys::platform::fs::rename_no_replace(from, to)
    }
    #[cfg(not(target_os = "vita"))]
    {
        // Host oracle for the kernel primitive. Only the protocol is exercised
        // here; Vita's native atomic move is validated on the target.
        fs::hard_link(from, to)?;
        fs::remove_file(from)
    }
}
fn cleanup(root: &Path, keep: Commit) -> Option<String> {
    let mut warning = None;
    let result = (|| -> io::Result<()> {
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            if entry.path() == record(root, keep.generation)
                || (keep.present && entry.path() == data(root, keep.generation))
            {
                continue;
            }
            if let Err(error) = fs::remove_file(entry.path()) {
                warning.get_or_insert_with(|| {
                    format!("storage published; retired generation cleanup failed: {error}")
                });
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        warning.get_or_insert_with(|| {
            format!("storage published; generation cleanup failed: {error}")
        });
    }
    warning
}
pub(super) fn publish(
    target: &Path,
    overwrite: bool,
    fill: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<Published> {
    let root = store(target)?;
    fs::create_dir_all(&root)?;
    let mut staging = Staging::new(&root)?;
    let candidate = staging.path.join("data");
    write_synced(&candidate, fill)?;
    // Filling performs no store lock: readers retain the preceding committed
    // version, and another authoring request may publish while this IO runs.
    let _commit = COMMIT
        .lock()
        .map_err(|_| io::Error::other("storage commit poisoned"))?;
    let old = current(&root)?;
    if !overwrite && old.is_some_and(|old| old.present) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "storage destination exists",
        ));
    }
    let generation = next(&root, old)?;
    let payload = data(&root, generation);
    rename_no_replace(&candidate, &payload)?;
    let commit = staging.path.join("commit");
    if let Err(error) = write_synced(&commit, |file| file.write_all(b"P"))
        .and_then(|()| rename_no_replace(&commit, &record(&root, generation)))
    {
        let _ = fs::remove_file(payload);
        return Err(error);
    }
    let keep = Commit {
        generation,
        present: true,
    };
    let warning = cleanup(&root, keep).or_else(|| {
        staging
            .cleanup()
            .err()
            .map(|e| format!("storage published; staging cleanup failed: {e}"))
    });
    Ok(Published {
        cleanup_warning: warning,
    })
}
pub(super) fn open(target: &Path) -> io::Result<fs::File> {
    let _commit = COMMIT
        .lock()
        .map_err(|_| io::Error::other("storage commit poisoned"))?;
    let root = store(target)?;
    let latest = current(&root)?
        .filter(|latest| latest.present)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "storage destination is absent"))?;
    fs::File::open(data(&root, latest.generation))
}
pub(super) fn remove(target: &Path) -> io::Result<Published> {
    let _commit = COMMIT
        .lock()
        .map_err(|_| io::Error::other("storage commit poisoned"))?;
    let root = store(target)?;
    let old = current(&root)?
        .filter(|latest| latest.present)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "storage destination is absent"))?;
    let generation = next(&root, Some(old))?;
    let mut staging = Staging::new(&root)?;
    let commit = staging.path.join("commit");
    write_synced(&commit, |file| file.write_all(b"D"))?;
    rename_no_replace(&commit, &record(&root, generation))?;
    let warning = cleanup(
        &root,
        Commit {
            generation,
            present: false,
        },
    )
    .or_else(|| {
        staging
            .cleanup()
            .err()
            .map(|e| format!("storage removed; staging cleanup failed: {e}"))
    });
    Ok(Published {
        cleanup_warning: warning,
    })
}
pub(super) fn entries(
    directory: &Path,
) -> io::Result<impl Iterator<Item = io::Result<PathBuf>> + use<>> {
    // Preserve NotFound before crossing newlib's directory-open boundary.
    fs::metadata(directory)?;
    let parent = directory.to_path_buf();
    Ok(fs::read_dir(directory)?.filter_map(move |entry| {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => return Some(Err(error)),
        };
        let name = entry.file_name();
        let logical = name.to_str()?.strip_prefix(PREFIX)?;
        let path = parent.join(logical);
        let _commit = match COMMIT.lock() {
            Ok(lock) => lock,
            Err(_) => return Some(Err(io::Error::other("storage commit poisoned"))),
        };
        match current(&entry.path()) {
            Ok(Some(commit)) if commit.present => Some(Ok(path)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn directory() -> Staging {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/reel-artifacts/storage-generations");
        fs::create_dir_all(&root).unwrap();
        Staging::new(&root).unwrap()
    }
    fn read(path: &Path) -> Vec<u8> {
        let mut bytes = Vec::new();
        open(path).unwrap().read_to_end(&mut bytes).unwrap();
        bytes
    }
    #[test]
    fn missing_store_is_absent_and_a_file_in_its_place_is_an_error() {
        let dir = directory();
        let target = dir.path.join("new.data");
        assert_eq!(open(&target).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(
            entries(&dir.path.join("missing")).err().unwrap().kind(),
            io::ErrorKind::NotFound
        );
        fs::write(store(&target).unwrap(), b"not a directory").unwrap();
        assert_eq!(
            open(&target).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
    }
    #[test]
    fn committed_generations_preserve_reader_ownership_failures_and_tombstones() {
        let dir = directory();
        let path = dir.path.join("slot.save");
        publish(&path, false, |file| file.write_all(b"old")).unwrap();
        let mut retained = open(&path).unwrap();
        let failed = publish(&path, true, |file| {
            file.write_all(b"partial")?;
            assert_eq!(read(&path), b"old");
            Err(io::Error::other("injected"))
        });
        assert!(failed.is_err());
        assert_eq!(read(&path), b"old");
        publish(&path, true, |file| file.write_all(b"new")).unwrap();
        assert_eq!(read(&path), b"new");
        let mut old = Vec::new();
        retained.read_to_end(&mut old).unwrap();
        assert_eq!(old, b"old");
        assert_eq!(
            entries(&dir.path)
                .unwrap()
                .collect::<io::Result<Vec<_>>>()
                .unwrap(),
            vec![path.clone()]
        );
        remove(&path).unwrap();
        assert_eq!(open(&path).err().unwrap().kind(), io::ErrorKind::NotFound);
        assert_eq!(entries(&dir.path).unwrap().count(), 0);
        publish(&path, false, |file| file.write_all(b"again")).unwrap();
        assert_eq!(read(&path), b"again");
    }
    #[test]
    fn incomplete_generations_are_invisible_and_create_race_preserves_winner() {
        let dir = directory();
        let path = dir.path.join("slot.save");
        let root = store(&path).unwrap();
        fs::create_dir_all(&root).unwrap();
        fs::write(data(&root, 1), b"uncommitted").unwrap();
        assert_eq!(open(&path).err().unwrap().kind(), io::ErrorKind::NotFound);
        let result = publish(&path, false, |file| {
            file.write_all(b"loser")?;
            publish(&path, false, |file| file.write_all(b"winner"))?;
            Ok(())
        });
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(read(&path), b"winner");
    }
}
