use super::*;
use std::cell::Cell;

#[derive(Default)]
struct Calls {
    renames: Cell<usize>,
    fail_rename: Cell<Option<usize>>,
    fail_sync: Cell<bool>,
}
impl Kernel for Calls {
    fn rename_no_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        let call = self.renames.get() + 1;
        self.renames.set(call);
        if self.fail_rename.get() == Some(call) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected native rename failure",
            ));
        }
        // Protocol tests run on the host. Commit calls share OPERATIONS; this
        // models the Vita no-replace result without claiming host rename behavior.
        if to.try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "destination exists",
            ));
        }
        fs::rename(from, to)
    }
    fn sync_mount(&self, _: &Path) -> io::Result<()> {
        if self.fail_sync.get() {
            Err(io::Error::other("injected mount sync failure"))
        } else {
            Ok(())
        }
    }
}
fn directory() -> Staging {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/reel-artifacts/storage-tests");
    fs::create_dir_all(&root).unwrap();
    Staging::new(&root.canonicalize().unwrap()).unwrap()
}
fn put(path: &Path, bytes: &[u8], calls: &Calls) -> io::Result<Published> {
    publish(path, true, |file| file.write_all(bytes), calls)
}
fn contents(path: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    open(path).unwrap().read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn complete_generations_replace_contents_while_existing_readers_keep_their_file() {
    let root = directory();
    let path = root.path.join("slot.save");
    let calls = Calls::default();
    put(&path, b"original", &calls).unwrap();
    let mut retained = open(&path).unwrap();
    publish(
        &path,
        true,
        |file| {
            file.write_all(b"replacement")?;
            assert_eq!(contents(&path), b"original");
            Ok(())
        },
        &calls,
    )
    .unwrap();
    assert_eq!(contents(&path), b"replacement");
    let mut previous = Vec::new();
    retained.read_to_end(&mut previous).unwrap();
    assert_eq!(previous, b"original");
    assert_eq!(fs::read_dir(&path).unwrap().count(), 2); // Format + latest payload.
    assert_eq!(
        entries(&root.path)
            .unwrap()
            .collect::<io::Result<Vec<_>>>()
            .unwrap(),
        vec![path]
    );
}

#[test]
fn failed_writer_and_failed_commit_preserve_old_data_and_reclaim_staging() {
    let root = directory();
    let path = root.path.join("slot.save");
    let calls = Calls::default();
    put(&path, b"original", &calls).unwrap();
    assert!(
        publish(
            &path,
            true,
            |file| {
                file.write_all(b"partial")?;
                Err(io::Error::other("injected writer failure"))
            },
            &calls
        )
        .is_err()
    );
    assert_eq!(contents(&path), b"original");
    calls.fail_rename.set(Some(calls.renames.get() + 1));
    let error = put(&path, b"replacement", &calls).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(error.to_string().contains("native rename"));
    assert_eq!(contents(&path), b"original");
    assert_eq!(fs::read_dir(&root.path).unwrap().count(), 1);
}

#[test]
fn no_overwrite_admission_preserves_a_file_published_during_preparation() {
    let root = directory();
    let path = root.path.join("slot.save");
    let calls = Calls::default();
    let result = publish(
        &path,
        false,
        |file| {
            file.write_all(b"candidate")?;
            put(&path, b"other writer", &Calls::default()).map(|_| ())
        },
        &calls,
    );
    assert_eq!(result.err().unwrap().kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(contents(&path), b"other writer");
    assert_eq!(fs::read_dir(&root.path).unwrap().count(), 1);
}

#[test]
fn recovery_ignores_uncommitted_work_and_observes_a_committed_generation() {
    let root = directory();
    let path = root.path.join("slot.save");
    let calls = Calls::default();
    put(&path, b"original", &calls).unwrap();
    let interrupted = Staging::new(&root.path).unwrap();
    fs::write(interrupted.path.join(FIRST), b"unfinished").unwrap();
    fs::write(interrupted.path.join("format"), FORMAT).unwrap();
    fs::write(path.join(".uncommitted"), b"partial").unwrap();
    assert_eq!(contents(&path), b"original");
    assert_eq!(entries(&root.path).unwrap().count(), 1);
    // A process may stop after the kernel commits and before mount sync/cleanup.
    let committed = path.join("00000000000000000002");
    calls
        .rename_no_replace(&interrupted.path.join(FIRST), &committed)
        .unwrap();
    assert_eq!(contents(&path), b"unfinished");
    assert!(path.join(FIRST).exists());
    assert_eq!(entries(&root.path).unwrap().count(), 1);
}

#[test]
fn post_commit_sync_and_cleanup_failures_report_committed_state_separately() {
    let root = directory();
    let path = root.path.join("slot.save");
    let calls = Calls::default();
    put(&path, b"original", &calls).unwrap();
    calls.fail_sync.set(true);
    let published = put(&path, b"committed", &calls).unwrap();
    assert!(
        published
            .durability_warning
            .unwrap()
            .contains("durability unconfirmed")
    );
    assert!(published.cleanup_warning.is_none());
    assert_eq!(contents(&path), b"committed");
    assert!(path.join(FIRST).exists());
    calls.fail_sync.set(false);
    put(&path, b"synced", &calls).unwrap();
    assert!(!path.join(FIRST).exists());
    // Force a real filesystem cleanup failure after a successful commit.
    fs::create_dir(path.join("00000000000000000000")).unwrap();
    let published = put(&path, b"latest", &calls).unwrap();
    assert!(published.durability_warning.is_none());
    assert!(published.cleanup_warning.is_some());
    assert_eq!(contents(&path), b"latest");
}

#[test]
fn deletion_commit_failure_and_post_commit_sync_keep_their_distinct_outcomes() {
    let root = directory();
    let path = root.path.join("slot.save");
    let calls = Calls::default();
    put(&path, b"original", &calls).unwrap();
    calls.fail_rename.set(Some(calls.renames.get() + 1));
    assert_eq!(
        remove(&path, &calls).err().unwrap().kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(contents(&path), b"original");
    calls.fail_sync.set(true);
    let removed = remove(&path, &calls).unwrap();
    assert!(removed.durability_warning.is_some());
    assert_eq!(open(&path).err().unwrap().kind(), io::ErrorKind::NotFound);
    assert_eq!(entries(&root.path).unwrap().count(), 0);
    let retained = fs::read_dir(&root.path)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("removed")
        .join(FIRST);
    assert_eq!(fs::read(retained).unwrap(), b"original");

    let second = root.path.join("other.save");
    calls.fail_sync.set(false);
    put(&second, b"remove me", &calls).unwrap();
    let removed = remove(&second, &calls).unwrap();
    assert!(removed.durability_warning.is_none());
    assert!(removed.cleanup_warning.is_none());
    assert!(!second.exists());
}
