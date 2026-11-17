//! Same-filesystem publication shared by package builds and player storage.
//! Files are synced before publication, and failed writes preserve the destination.
#[cfg(not(target_os = "vita"))]
mod desktop;
#[cfg(any(target_os = "vita", test))]
mod generation;
#[cfg(not(target_os = "vita"))]
use desktop as platform;
#[cfg(target_os = "vita")]
use generation as platform;
#[cfg(target_os = "vita")]
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// A committed operation with any subsequent staging cleanup warning.
pub struct Published {
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
    platform::publish(target, overwrite, fill)
}

/// Open the published contents as a standard file.
pub fn open(target: &Path) -> io::Result<fs::File> {
    platform::open(target)
}

/// Remove the logical file. Warnings describe failures after deletion committed;
/// an error means the logical file was not removed.
pub fn remove(target: &Path) -> io::Result<Published> {
    platform::remove(target)
}

/// Enumerate selected published logical files, excluding directories and staging.
/// Each returned file owns one committed version; later replacement or removal
/// leaves that open reader attached to the selected version. The selector runs
/// before opening data, and dropping the iterator stops enumeration.
pub fn entries<F>(
    directory: &Path,
    select: F,
) -> io::Result<impl Iterator<Item = io::Result<(PathBuf, fs::File)>> + use<F>>
where
    F: Fn(&Path) -> bool,
{
    platform::entries(directory, select)
}

#[cfg(not(target_os = "vita"))]
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

#[cfg(not(target_os = "vita"))]
struct Staging(tempfile::TempDir);
#[cfg(not(target_os = "vita"))]
impl Staging {
    fn new(parent: &Path) -> io::Result<Self> {
        tempfile::Builder::new()
            .prefix(".reel-stage-")
            .tempdir_in(parent)
            .map(Self)
    }
    fn path(&self) -> &Path {
        self.0.path()
    }
    fn close(self) -> io::Result<()> {
        self.0.close()
    }
}

#[cfg(target_os = "vita")]
struct Staging {
    path: PathBuf,
    owned: bool,
}
#[cfg(target_os = "vita")]
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
    fn path(&self) -> &Path {
        &self.path
    }
    fn close(mut self) -> io::Result<()> {
        fs::remove_dir_all(&self.path)?;
        self.owned = false;
        Ok(())
    }
}
#[cfg(target_os = "vita")]
impl Drop for Staging {
    fn drop(&mut self) {
        if self.owned {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    fn read(path: &Path) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        open(path)?.read_to_end(&mut bytes)?;
        Ok(bytes)
    }
    fn directory() -> Staging {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/reel-artifacts/storage-tests");
        fs::create_dir_all(&root).unwrap();
        Staging::new(&root).unwrap()
    }
    #[test]
    fn failed_write_preserves_old_file_and_cleans_own_staging() {
        let dir = directory();
        let target = dir.path().join("old.asb");
        publish(&target, false, |file| file.write_all(b"original")).unwrap();
        let result = publish(&target, true, |file| {
            file.write_all(b"partial")?;
            Err(io::Error::other("injected write failure"))
        });
        assert!(result.is_err());
        assert_eq!(read(&target).unwrap(), b"original");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    #[test]
    fn target_created_during_write_is_never_overwritten() {
        let dir = directory();
        let target = dir.path().join("race.asb");
        let result = publish(&target, false, |file| {
            file.write_all(b"candidate")?;
            publish(&target, false, |file| file.write_all(b"other writer")).map(|_| ())
        });
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(read(&target).unwrap(), b"other writer");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    #[test]
    fn overwrite_replaces_only_after_complete_write() {
        for dir in [directory(), Staging::new(&std::env::temp_dir()).unwrap()] {
            let target = dir.path().join("replace.asb");
            publish(&target, false, |file| file.write_all(b"old")).unwrap();
            let result = publish(&target, true, |file| {
                file.write_all(b"new")?;
                assert_eq!(read(&target)?, b"old");
                Ok(())
            })
            .unwrap();
            assert!(result.cleanup_warning.is_none());
            assert_eq!(read(&target).unwrap(), b"new");
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        }
    }
}
