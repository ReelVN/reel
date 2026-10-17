//! Open descriptors pin the selected file while range reads share its digest.
//! Hashing happens once on the IO worker, using fixed scratch space. Packaged
//! sources can supply the release digest directly without this directory cost.
use super::{failure, io_error};
use reel_core::{
    identity::{AssetSource, ContentVersion},
    io::{IoError, IoErrorCode},
};
use std::{
    collections::HashMap,
    fs::{File, Metadata},
    io::Read,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::SystemTime,
};

pub(super) struct OpenFile {
    pub file: File,
    pub length: u64,
    pub version: ContentVersion,
    modified: SystemTime,
    used: u64,
}
impl OpenFile {
    pub(super) fn verify(&self) -> Result<(), IoError> {
        if same(
            &self.file.metadata().map_err(io_error)?,
            self.length,
            self.modified,
        )? {
            Ok(())
        } else {
            Err(changed())
        }
    }
}

pub(super) struct Files {
    root: PathBuf,
    capacity: usize,
    sequence: u64,
    open: HashMap<String, OpenFile>,
}
impl Files {
    pub(super) fn new(root: PathBuf, capacity: usize) -> Self {
        Self {
            root,
            capacity,
            sequence: 0,
            open: HashMap::new(),
        }
    }
    pub(super) fn open(
        &mut self,
        source: &AssetSource,
        cancelled: &AtomicBool,
    ) -> Result<&mut OpenFile, IoError> {
        self.sequence += 1;
        let path = self.root.join(&source.key.0);
        let metadata = std::fs::metadata(&path).map_err(io_error)?;
        let cached = self
            .open
            .get(&source.key.0)
            .is_some_and(|file| same(&metadata, file.length, file.modified).unwrap_or(false));
        if !cached {
            let mut file = File::open(path).map_err(io_error)?;
            let metadata = file.metadata().map_err(io_error)?;
            let length = metadata.len();
            let modified = metadata.modified().map_err(io_error)?;
            let mut hash = blake3::Hasher::new();
            let mut scratch = [0u8; 64 << 10];
            loop {
                if cancelled.load(Ordering::Acquire) {
                    return Err(failure("read cancelled"));
                }
                let read = file.read(&mut scratch).map_err(io_error)?;
                if read == 0 {
                    break;
                }
                hash.update(&scratch[..read]);
            }
            let file = OpenFile {
                file,
                length,
                modified,
                version: ContentVersion(*hash.finalize().as_bytes()),
                used: self.sequence,
            };
            file.verify()?;
            if self.open.len() >= self.capacity
                && !self.open.contains_key(&source.key.0)
                && let Some(oldest) = self
                    .open
                    .iter()
                    .min_by_key(|(_, v)| v.used)
                    .map(|(k, _)| k.clone())
            {
                self.open.remove(&oldest);
            }
            self.open.insert(source.key.0.clone(), file);
        }
        let file = self.open.get_mut(&source.key.0).unwrap();
        file.used = self.sequence;
        if source
            .version
            .is_some_and(|version| version != file.version)
        {
            return Err(changed());
        }
        Ok(file)
    }
}
fn same(metadata: &Metadata, length: u64, modified: SystemTime) -> Result<bool, IoError> {
    Ok(metadata.len() == length && metadata.modified().map_err(io_error)? == modified)
}
fn changed() -> IoError {
    IoError {
        code: IoErrorCode::VersionUnavailable,
        message: "requested asset version unavailable".into(),
    }
}
