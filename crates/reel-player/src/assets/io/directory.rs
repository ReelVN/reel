//! Fixed loose resources share open descriptors and source-path identities.
//! Content preparation and changes belong to the next game launch.
use super::{file::File, io_error};
use reel_core::{
    identity::{AssetSource, ContentVersion},
    io::{IoError, IoErrorCode},
    profile::{Phase, Scope},
};
use reel_package::resource_target::{ResourceDirectory, ResourceTarget};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

pub(super) struct OpenFile {
    pub file: File,
    pub length: u64,
    pub version: ContentVersion,
}

pub(super) struct Files {
    source: Source,
    open: lru::LruCache<String, OpenFile>,
}
enum Source {
    Directory(ResourceDirectory),
    Selected(BTreeMap<String, PathBuf>),
}
impl Source {
    fn resolve(&self, logical: &str) -> Result<Cow<'_, Path>, IoError> {
        match self {
            Self::Directory(directory) => {
                directory.resolve(logical).map(Cow::Owned).map_err(io_error)
            }
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
        Self::from_source(Source::Directory(target.directory(root)), capacity)
    }
    pub(super) fn selected(files: BTreeMap<String, PathBuf>, capacity: usize) -> Self {
        Self::from_source(Source::Selected(files), capacity)
    }
    fn from_source(source: Source, capacity: usize) -> Self {
        Self {
            source,
            open: lru::LruCache::new(NonZeroUsize::new(capacity.max(1)).unwrap()),
        }
    }
    pub(super) fn open(&mut self, source: &AssetSource) -> Result<&mut OpenFile, IoError> {
        self.open
            .try_get_or_insert_mut_ref(source.key.0.as_str(), || {
                let path = self.source.resolve(&source.key.0)?;
                let file = {
                    let _timing = Scope::new(Phase::FileOpen);
                    File::open(&path).map_err(io_error)?
                };
                let length = {
                    let _timing = Scope::new(Phase::FileMetadata);
                    file.metadata().map_err(io_error)?.len()
                };
                // Only the path is hashed. Opening a stream reads no payload bytes.
                let mut hash = blake3::Hasher::new();
                hash.update(b"reel/fixed-file/v1\0");
                hash.update(path.as_os_str().as_encoded_bytes());
                let file = OpenFile {
                    file,
                    length,
                    version: ContentVersion(*hash.finalize().as_bytes()),
                };
                Ok(file)
            })
    }
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
        io::{Read, Seek, SeekFrom},
    };

    #[test]
    fn fixed_target_selection_opens_without_reading_content_and_survives_eviction() {
        let root =
            std::env::temp_dir().join(format!("reel-target-directory-{}", std::process::id()));
        fs::create_dir_all(root.join("bg")).unwrap();
        fs::create_dir_all(root.join("@vita/bg")).unwrap();
        fs::create_dir_all(root.join("@other")).unwrap();
        fs::write(root.join("bg/room.bin"), b"common").unwrap();
        fs::write(root.join("@vita/bg/room.bin"), b"target").unwrap();
        fs::write(root.join("@vita/only.bin"), b"target only").unwrap();
        fs::write(root.join("@other/secret.bin"), b"excluded").unwrap();
        fs::File::create(root.join("large.bin"))
            .unwrap()
            .set_len(8 << 20)
            .unwrap();
        let mut files = Files::for_target(root.clone(), 1, "vita".into()).unwrap();
        let source = |key: &str| AssetSource {
            key: AssetKey(key.into()),
            version: None,
        };
        let large = files.open(&source("large.bin")).unwrap();
        assert_eq!(large.length, 8 << 20);
        assert_eq!(
            large.file.stream_position().unwrap(),
            0,
            "describe reads no payload"
        );
        let read = |files: &mut Files, key: &str| {
            let opened = files.open(&source(key))?;
            assert_eq!(opened.file.stream_position().map_err(io_error)?, 0);
            opened.file.seek(SeekFrom::Start(0)).map_err(io_error)?;
            let mut bytes = Vec::new();
            opened.file.read_to_end(&mut bytes).map_err(io_error)?;
            Ok::<_, IoError>((bytes, opened.version))
        };
        let (room, identity) = read(&mut files, "bg/room.bin").unwrap();
        assert_eq!(room, b"target");
        assert_eq!(read(&mut files, "only.bin").unwrap().0, b"target only");
        assert_eq!(read(&mut files, "bg/room.bin").unwrap(), (room, identity));
        assert_eq!(
            read(&mut files, "secret.bin").unwrap_err().code,
            IoErrorCode::NotFound
        );
        assert_eq!(
            read(&mut files, "@other/secret.bin").unwrap_err().code,
            IoErrorCode::NotFound
        );
        drop(files);
        let mut common = Files::for_target(root.clone(), 1, "windows".into()).unwrap();
        let (room, common_identity) = read(&mut common, "bg/room.bin").unwrap();
        assert_eq!(room, b"common");
        assert_ne!(identity, common_identity);
        fs::create_dir_all(root.join("@desktop/bg")).unwrap();
        fs::write(root.join("@desktop/bg/room.bin"), b"shared").unwrap();
        common.open(&source("large.bin")).unwrap(); // evict the room descriptor
        assert_eq!(read(&mut common, "bg/room.bin").unwrap().0, b"common");
        drop(common);
        let mut reopened = Files::for_target(root.clone(), 1, "windows".into()).unwrap();
        assert_eq!(read(&mut reopened, "bg/room.bin").unwrap().0, b"shared");
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }
}
