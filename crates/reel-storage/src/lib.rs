//! Same-filesystem publication shared by package builds and player storage.
//! Files are synced before atomic replacement; power-loss directory durability is platform-specific.
use atomic_write_file::AtomicWriteFile;
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub struct Published {
    /// Publication is already complete even if private staging cleanup fails.
    pub cleanup_warning: Option<String>,
}

pub fn publish(
    target: &Path,
    overwrite: bool,
    fill: impl FnOnce(&mut AtomicWriteFile) -> io::Result<()>,
) -> io::Result<Published> {
    let mut staging = Staging::new(target.parent().expect("normalized target parent"))?;
    let candidate = staging.path.join("candidate");
    let mut file = AtomicWriteFile::open(&candidate)?;
    fill(&mut file)?;
    // The dependency syncs the file and directory, but directory fsync may fail
    // AFTER its rename. Commit into private staging so any such error still
    // leaves the author's target untouched and our guard can remove leftovers.
    file.commit()?;
    if overwrite {
        fs::rename(&candidate, target)?;
    } else {
        // Atomic no-replace even when another writer creates target concurrently.
        // Unsupported hard links fail safely; never fall back to clobbering rename.
        fs::hard_link(&candidate, target)?;
    }
    // No fallible step after publication is reported as an unpublished failure.
    let cleanup_warning = staging.cleanup().err().map(|e| {
        format!(
            "artifact published; staging cleanup failed at {}: {e}",
            staging.path.display()
        )
    });
    Ok(Published { cleanup_warning })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
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
        let dir = directory();
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
