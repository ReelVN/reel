//! Same-filesystem publication shared by package builds and player storage.
//! Files are synced before publication. Vita retains immutable generations in a
//! private v1 directory; native no-replace renames commit their logical contents.
#[cfg(not(target_os = "vita"))]
#[path = "desktop.rs"]
mod backend;
#[cfg(target_os = "vita")]
#[path = "vita.rs"]
mod backend;
#[cfg(any(target_os = "vita", test))]
mod generations;
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// A committed logical operation with separate durability and cleanup outcomes.
pub struct Published {
    /// Publication or deletion committed, but subsequent device synchronization
    /// failed. The new logical state applies; power-loss durability is unconfirmed.
    pub durability_warning: Option<String>,
    /// The logical operation committed, but reclaiming retired storage failed.
    pub cleanup_warning: Option<String>,
}

/// Fill and sync a temporary file, then publish it on the destination filesystem.
/// The writer is borrowed for the callback; a callback error preserves the destination.
pub fn publish(
    target: &Path,
    overwrite: bool,
    fill: impl FnOnce(&mut dyn io::Write) -> io::Result<()>,
) -> io::Result<Published> {
    backend::publish(target, overwrite, fill)
}

/// Open the current committed contents as a standard file. Existing readers keep
/// their immutable generation while a later publication replaces the logical file.
pub fn open(target: &Path) -> io::Result<fs::File> {
    backend::open(target)
}

/// Remove the logical file. Warnings describe failures after deletion committed;
/// an error means the logical file was not removed.
pub fn remove(target: &Path) -> io::Result<Published> {
    backend::remove(target)
}

/// Enumerate committed logical files in a directory, excluding private staging.
/// Contents returned by `open` retain their original application format.
pub fn entries(directory: &Path) -> io::Result<impl Iterator<Item = io::Result<PathBuf>> + use<>> {
    backend::entries(directory)
}

fn destination(target: &Path) -> io::Result<PathBuf> {
    // Resolve the existing directory before constructing either rename path.
    // Windows app-data virtualization may redirect file creation to another
    // volume while resolving a not-yet-existing rename destination literally.
    let parent = target
        .parent()
        .expect("normalized target parent")
        .canonicalize()?;
    Ok(parent.join(target.file_name().expect("normalized target filename")))
}

struct Staging {
    path: PathBuf,
    owned: bool,
}
impl Staging {
    fn new(parent: &Path) -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..128 {
            let path = parent.join(format!(
                ".reel-stage-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let builder = fs::DirBuilder::new();
            #[cfg(unix)]
            let builder = {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = builder;
                builder.mode(0o700);
                builder
            };
            match builder.create(&path) {
                Ok(()) => return Ok(Self { path, owned: true }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "cannot allocate unique staging directory",
        ))
    }
    fn cleanup(&mut self) -> io::Result<()> {
        fs::remove_dir_all(&self.path)?;
        self.owned = false;
        Ok(())
    }
}
impl Drop for Staging {
    fn drop(&mut self) {
        if self.owned {
            let _ = self.cleanup();
        }
    }
}

#[cfg(all(test, not(target_os = "vita")))]
mod tests {
    use super::*;
    fn directory() -> Staging {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/reel-artifacts/storage-tests");
        fs::create_dir_all(&root).unwrap();
        Staging::new(&root).unwrap()
    }
    #[test]
    fn failed_write_preserves_old_file_and_cleans_own_staging() {
        let dir = directory();
        let target = dir.path.join("old.asb");
        fs::write(&target, b"original").unwrap();
        let result = publish(&target, true, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("injected write failure"))
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&target).unwrap(), b"original");
        assert_eq!(fs::read_dir(&dir.path).unwrap().count(), 1);
    }
    #[test]
    fn target_created_during_write_is_never_overwritten() {
        let dir = directory();
        let target = dir.path.join("race.asb");
        let result = publish(&target, false, |file| {
            file.write_all(b"candidate")?;
            fs::write(&target, b"other writer")
        });
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&target).unwrap(), b"other writer");
        assert_eq!(fs::read_dir(&dir.path).unwrap().count(), 1);
    }
    #[test]
    fn overwrite_replaces_only_after_complete_write() {
        for dir in [directory(), Staging::new(&std::env::temp_dir()).unwrap()] {
            let target = dir.path.join("replace.asb");
            fs::write(&target, b"old").unwrap();
            let result = publish(&target, true, |file| {
                file.write_all(b"new")?;
                assert_eq!(fs::read(&target)?, b"old");
                Ok(())
            })
            .unwrap();
            assert!(result.cleanup_warning.is_none());
            assert_eq!(fs::read(&target).unwrap(), b"new");
            assert_eq!(fs::read_dir(&dir.path).unwrap().count(), 1);
        }
    }
}
