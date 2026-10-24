//! Immutable generation files. A no-replace rename is the logical commit point.
use super::{Published, Staging, destination};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

const FORMAT: &[u8] = b"REELSTOR\x01";
const FIRST: &str = "00000000000000000001";
static OPERATIONS: Mutex<()> = Mutex::new(());

pub(super) trait Kernel {
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn sync_mount(&self, path: &Path) -> io::Result<()>;
}

fn generation(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    (name.len() == 20 && name.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| name.parse().ok())
        .flatten()
}

fn latest(target: &Path) -> io::Result<u64> {
    let mut format = [0; FORMAT.len()];
    fs::File::open(target.join("format"))?.read_exact(&mut format)?;
    if format.as_slice() != FORMAT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported storage format",
        ));
    }
    let mut latest = None;
    for entry in fs::read_dir(target)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && let Some(number) = generation(&entry.path())
        {
            latest = Some(latest.map_or(number, |previous: u64| previous.max(number)));
        }
    }
    latest.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no committed generation"))
}

pub(super) fn open(target: &Path) -> io::Result<fs::File> {
    let _operation = OPERATIONS.lock().expect("storage operation poisoned");
    fs::File::open(target.join(format!("{:020}", latest(target)?)))
}

pub(super) fn entries(
    directory: &Path,
) -> io::Result<impl Iterator<Item = io::Result<PathBuf>> + use<>> {
    Ok(fs::read_dir(directory)?.filter_map(|entry| {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => return Some(Err(error)),
        };
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(error) => return Some(Err(error)),
        };
        if !kind.is_dir()
            || entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(".reel-stage-"))
        {
            return None;
        }
        let _operation = OPERATIONS.lock().expect("storage operation poisoned");
        let path = entry.path();
        match latest(&path) {
            Ok(_) => Some(Ok(path)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => Some(Err(error)),
        }
    }))
}

fn append_warning(warning: &mut Option<String>, message: String) {
    if let Some(warning) = warning {
        warning.push_str("; ");
        warning.push_str(&message);
    } else {
        *warning = Some(message);
    }
}

fn cleanup_old(target: &Path, current: u64) -> io::Result<()> {
    for entry in fs::read_dir(target)? {
        let path = entry?.path();
        if generation(&path).is_some_and(|number| number < current) {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

pub(super) fn publish(
    target: &Path,
    overwrite: bool,
    fill: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    kernel: &impl Kernel,
) -> io::Result<Published> {
    let target = destination(target)?;
    let mut staging = Staging::new(target.parent().unwrap())?;
    let mut file = fs::File::create_new(staging.path.join(FIRST))?;
    fill(&mut file)?;
    file.sync_all()?;
    drop(file);

    // Reads and cleanup share this short commit gate. The author callback runs
    // before locking, so preparation neither blocks readers nor owns publication.
    let _operation = OPERATIONS.lock().expect("storage operation poisoned");
    let current = match latest(&target) {
        Ok(previous) => {
            if !overwrite {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "logical file exists",
                ));
            }
            let next = previous
                .checked_add(1)
                .ok_or_else(|| io::Error::other("storage generation exhausted"))?;
            kernel.rename_no_replace(
                &staging.path.join(FIRST),
                &target.join(format!("{next:020}")),
            )?;
            next
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut marker = fs::File::create_new(staging.path.join("format"))?;
            marker.write_all(FORMAT)?;
            marker.sync_all()?;
            drop(marker);
            kernel.rename_no_replace(&staging.path, &target)?;
            staging.owned = false;
            1
        }
        Err(error) => return Err(error),
    };
    // Every fallible operation below happens after commit. Its outcome must
    // report the published contents, including uncertain power-loss durability.
    let durability_warning = kernel.sync_mount(&target).err().map(|error| {
        format!(
            "file published at {}; device synchronization failed, durability unconfirmed: {error}",
            target.display()
        )
    });
    let mut cleanup_warning = None;
    if durability_warning.is_none()
        && let Err(error) = cleanup_old(&target, current)
    {
        append_warning(
            &mut cleanup_warning,
            format!(
                "file published; old generation cleanup failed at {}: {error}",
                target.display()
            ),
        );
    }
    if staging.owned
        && let Err(error) = staging.cleanup()
    {
        append_warning(
            &mut cleanup_warning,
            format!(
                "file published; staging cleanup failed at {}: {error}",
                staging.path.display()
            ),
        );
    }
    Ok(Published {
        durability_warning,
        cleanup_warning,
    })
}

pub(super) fn remove(target: &Path, kernel: &impl Kernel) -> io::Result<Published> {
    let target = destination(target)?;
    let _operation = OPERATIONS.lock().expect("storage operation poisoned");
    latest(&target)?;
    let mut staging = Staging::new(target.parent().unwrap())?;
    let removed = staging.path.join("removed");
    kernel.rename_no_replace(&target, &removed)?;
    let durability_warning = kernel.sync_mount(&target).err().map(|error| {
        format!("file removed at {}; device synchronization failed, durability unconfirmed; prior contents retained at {}: {error}", target.display(), removed.display())
    });
    let cleanup_warning = if durability_warning.is_some() {
        staging.owned = false;
        None
    } else {
        staging.cleanup().err().map(|error| {
            format!(
                "file removed; retired generation cleanup failed at {}: {error}",
                staging.path.display()
            )
        })
    };
    Ok(Published {
        durability_warning,
        cleanup_warning,
    })
}

#[cfg(test)]
mod tests;
