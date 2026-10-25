//! Open descriptors pin the selected file while range reads share its digest.
//! Hashing happens once on the IO worker, using fixed scratch space. Packaged
//! sources can supply the release digest directly without this directory cost.
use super::{
    failure,
    file::{self, File, Metadata},
    io_error,
};
use reel_core::{
    identity::{AssetSource, ContentVersion},
    io::{IoError, IoErrorCode},
};
use reel_package::resource_target::ResourceTarget;
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::SystemTime,
};

pub(super) struct OpenFile {
    pub file: File,
    pub length: u64,
    pub version: ContentVersion,
    pub(super) modified: SystemTime,
    used: u64,
    path: PathBuf,
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
    source: Source,
    capacity: usize,
    sequence: u64,
    open: HashMap<String, OpenFile>,
}
enum Source {
    Directory {
        root: PathBuf,
        target: ResourceTarget,
    },
    Selected(BTreeMap<String, PathBuf>),
}
impl Source {
    fn resolve(&self, logical: &str) -> Result<Cow<'_, Path>, IoError> {
        match self {
            Self::Directory { root, target } => target
                .resolve(root, logical)
                .map(Cow::Owned)
                .map_err(io_error),
            Self::Selected(files) => files
                .get(logical)
                .map(|path| Cow::Borrowed(path.as_path()))
                .ok_or_else(|| IoError {
                    code: IoErrorCode::NotFound,
                    message: format!("asset not found in selected resources: {logical}"),
                }),
        }
    }
}
impl Files {
    pub(super) fn new(root: PathBuf, capacity: usize) -> Self {
        Self::with_target(root, capacity, ResourceTarget::default())
    }
    pub(super) fn for_target(
        root: PathBuf,
        capacity: usize,
        target: String,
    ) -> Result<Self, IoError> {
        Ok(Self::with_target(
            root,
            capacity,
            ResourceTarget::parse(&target).map_err(io_error)?,
        ))
    }
    fn with_target(root: PathBuf, capacity: usize, target: ResourceTarget) -> Self {
        Self::from_source(Source::Directory { root, target }, capacity)
    }
    pub(super) fn selected(files: BTreeMap<String, PathBuf>, capacity: usize) -> Self {
        Self::from_source(Source::Selected(files), capacity)
    }
    fn from_source(source: Source, capacity: usize) -> Self {
        Self {
            source,
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
        let path = self.source.resolve(&source.key.0)?;
        let metadata = file::metadata(&path).map_err(io_error)?;
        let cached = self.open.get(&source.key.0).is_some_and(|file| {
            file.path.as_path() == path.as_ref()
                && same(&metadata, file.length, file.modified).unwrap_or(false)
        });
        if !cached {
            let mut file = File::open(&path).map_err(io_error)?;
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
                path: path.into_owned(),
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
        Ok(file)
    }
}
pub(super) fn same(
    metadata: &Metadata,
    length: u64,
    modified: SystemTime,
) -> Result<bool, IoError> {
    Ok(metadata.len() == length && metadata.modified().map_err(io_error)? == modified)
}
pub(super) fn changed() -> IoError {
    IoError {
        code: IoErrorCode::VersionUnavailable,
        message: "requested asset version unavailable".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reel_core::identity::AssetKey;
    use std::{
        fs,
        io::{Seek, SeekFrom},
    };

    #[test]
    fn target_reads_follow_live_source_paths_and_reselect_cached_files() {
        let root =
            std::env::temp_dir().join(format!("reel-target-directory-{}", std::process::id()));
        fs::create_dir_all(root.join("bg")).unwrap();
        fs::create_dir_all(root.join("@vita/bg")).unwrap();
        fs::create_dir_all(root.join("@other")).unwrap();
        fs::write(root.join("bg/room.bin"), b"common").unwrap();
        fs::write(root.join("@vita/only.bin"), b"target only").unwrap();
        fs::write(root.join("@other/secret.bin"), b"excluded").unwrap();
        let modified = fs::metadata(root.join("bg/room.bin"))
            .unwrap()
            .modified()
            .unwrap();
        let mut files = Files::for_target(root.clone(), 4, "vita".into()).unwrap();
        let read = |files: &mut Files, key: &str| {
            let source = AssetSource {
                key: AssetKey(key.into()),
                version: None,
            };
            let opened = files.open(&source, &AtomicBool::new(false))?;
            opened.file.seek(SeekFrom::Start(0)).map_err(io_error)?;
            let mut bytes = Vec::new();
            opened.file.read_to_end(&mut bytes).map_err(io_error)?;
            Ok::<_, IoError>(bytes)
        };
        assert_eq!(read(&mut files, "bg/room.bin").unwrap(), b"common");
        let variant = root.join("@vita/bg/room.bin");
        fs::write(&variant, b"target").unwrap();
        fs::File::options()
            .write(true)
            .open(&variant)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert_eq!(read(&mut files, "bg/room.bin").unwrap(), b"target");
        fs::remove_file(variant).unwrap();
        assert_eq!(read(&mut files, "bg/room.bin").unwrap(), b"common");
        assert_eq!(read(&mut files, "only.bin").unwrap(), b"target only");
        assert_eq!(
            read(&mut files, "secret.bin").unwrap_err().code,
            IoErrorCode::NotFound
        );
        assert_eq!(
            read(&mut files, "@other/secret.bin").unwrap_err().code,
            IoErrorCode::NotFound
        );
        drop(files);
        fs::remove_dir_all(root).unwrap();
    }
}
