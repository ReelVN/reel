use super::{ReadFile, directory, failure, io_error};
use reel_core::{
    identity::{AssetSource, ContentVersion},
    io::{IoError, IoErrorCode},
};
use reel_package::archive::ArchiveIndex;
use std::{collections::BTreeMap, fs::File, path::PathBuf, sync::atomic::AtomicBool};

struct Entry {
    archive: usize,
    offset: u64,
    length: u64,
    version: ContentVersion,
}
pub(super) struct Files {
    archives: Vec<File>,
    entries: BTreeMap<String, Entry>,
    overlay: Option<directory::Files>,
}
impl Files {
    pub(super) fn open(
        paths: impl IntoIterator<Item = PathBuf>,
        overlay: Option<PathBuf>,
        cache_capacity: usize,
    ) -> Result<Self, IoError> {
        let mut archives = Vec::new();
        let mut entries = BTreeMap::new();
        for path in paths {
            let mut file = File::open(&path).map_err(io_error)?;
            let index = ArchiveIndex::read(&mut file)
                .map_err(|error| failure(format!("{}: {error}", path.display())))?;
            for entry in index.entries() {
                entries.insert(
                    entry.path.clone(),
                    Entry {
                        archive: archives.len(),
                        offset: entry.offset,
                        length: entry.length,
                        version: ContentVersion(entry.version),
                    },
                );
            }
            archives.push(file);
        }
        Ok(Self {
            archives,
            entries,
            overlay: overlay.map(|root| directory::Files::new(root, cache_capacity)),
        })
    }
    pub(super) fn entry(
        &mut self,
        source: &AssetSource,
        cancelled: &AtomicBool,
    ) -> Result<ReadFile<'_>, IoError> {
        if let Some(overlay) = &mut self.overlay {
            match overlay.open(source, cancelled) {
                Ok(file) => return Ok(ReadFile::directory(file)),
                Err(error) if error.code == IoErrorCode::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let entry = self.entries.get(&source.key.0).ok_or_else(|| IoError {
            code: IoErrorCode::NotFound,
            message: format!("asset not found in mounted archives: {}", source.key.0),
        })?;
        Ok(ReadFile {
            file: &mut self.archives[entry.archive],
            offset: entry.offset,
            length: entry.length,
            version: entry.version,
            stamp: None,
        })
    }
}
