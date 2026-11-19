use super::{ReadFile, directory, failure, file::File, io_error};
use reel_core::{
    budget::{BudgetCategory, BudgetPool, BudgetTicket},
    identity::{AssetSource, ContentVersion},
    io::{AssetInfo, IoError, IoErrorCode},
};
use reel_package::archive::{self, ArchiveIndex, EntryStorage, ReadScratch};
use std::path::PathBuf;

struct Archive {
    file: File,
    index: ArchiveIndex,
    _metadata: BudgetTicket,
}
struct Workspace {
    scratch: ReadScratch,
    source: Option<usize>,
    _ticket: BudgetTicket,
}
pub(super) struct Files {
    archives: Vec<Archive>,
    overlay: Option<directory::Files>,
    workspace: Option<Box<Workspace>>,
    _storage: BudgetTicket,
}

#[derive(Clone, Copy)]
pub(super) struct Location {
    archive: usize,
    entry: usize,
}
impl Files {
    #[cfg(test)]
    pub(super) fn seek_calls(&self) -> usize {
        self.archives
            .iter()
            .map(|archive| archive.file.seek_calls())
            .sum()
    }
    pub(super) fn open(
        paths: impl IntoIterator<Item = PathBuf>,
        overlay: Option<directory::Files>,
        temporary: &BudgetPool,
        metadata: &BudgetPool,
    ) -> Result<Self, IoError> {
        let mut storage = metadata
            .reserve(0)
            .map_err(|e| failure(format!("archive mount metadata: {}", e.message)))?;
        if storage.category() != BudgetCategory::Metadata {
            return Err(failure("archive indexes require a metadata budget"));
        }
        let mut archives = Vec::new();
        for path in paths {
            if archives.len() == archives.capacity() {
                let capacity = archives.capacity().saturating_mul(2).max(1);
                let bytes = capacity
                    .checked_mul(size_of::<Archive>())
                    .filter(|bytes| *bytes <= isize::MAX as usize)
                    .ok_or_else(|| failure("archive mounts exceed addressable memory"))?
                    as u64;
                let overlap = storage
                    .bytes()
                    .checked_add(bytes)
                    .ok_or_else(|| failure("archive mount metadata overflow"))?;
                storage
                    .resize(overlap)
                    .map_err(|e| failure(format!("archive mount metadata: {}", e.message)))?;
                archives
                    .try_reserve_exact(capacity - archives.len())
                    .map_err(|e| failure(format!("archive mounts: {e}")))?;
                storage
                    .resize((archives.capacity() * size_of::<Archive>()) as u64)
                    .map_err(|e| failure(format!("archive mount metadata: {}", e.message)))?;
            }
            let mut file = File::open(&path).map_err(io_error)?;
            let mut ticket = metadata
                .reserve(0)
                .map_err(|e| failure(format!("archive index metadata: {}", e.message)))?;
            let index = ArchiveIndex::read_with_admission(&mut file, |bytes| {
                ticket.resize(bytes).map_err(|error| {
                    std::io::Error::other(format!(
                        "archive index metadata requires {bytes} bytes: {}",
                        error.message
                    ))
                })
            })
            .map_err(|error| failure(format!("{}: {error}", path.display())))?;
            archives.push(Archive {
                file,
                index,
                _metadata: ticket,
            });
        }
        let workspace = if archives
            .iter()
            .any(|archive| archive.index.has_block_entries())
        {
            let ticket = temporary
                .reserve(archive::SCRATCH_BYTES as u64)
                .map_err(|e| failure(format!("archive workspace: {}", e.message)))?;
            if ticket.category() != BudgetCategory::Temporary {
                return Err(failure("archive workspace requires a temporary budget"));
            }
            storage
                .resize(storage.bytes() + size_of::<Workspace>() as u64)
                .map_err(|error| {
                    failure(format!("archive workspace metadata: {}", error.message))
                })?;
            Some(Box::new(Workspace {
                scratch: ReadScratch::new(),
                source: None,
                _ticket: ticket,
            }))
        } else {
            None
        };
        Ok(Self {
            archives,
            overlay,
            workspace,
            _storage: storage,
        })
    }
    pub(super) fn indexed_info(&self, source: &AssetSource) -> Option<AssetInfo> {
        if self.overlay.is_some() {
            return None;
        }
        let location = Self::locate(&self.archives, source)?;
        let entry = self.archives[location.archive]
            .index
            .entry_at(location.entry)?;
        let version = ContentVersion(entry.version);
        if source.version.is_some_and(|requested| requested != version) {
            return None;
        }
        Some(AssetInfo {
            length: Some(entry.length),
            range_read: true,
            seekable: true,
            version: Some(version),
            media_type: None,
        })
    }
    fn locate(archives: &[Archive], source: &AssetSource) -> Option<Location> {
        archives
            .iter()
            .enumerate()
            .rev()
            .find_map(|(archive, mount)| {
                mount
                    .index
                    .index_of(&source.key.0)
                    .map(|entry| Location { archive, entry })
            })
    }
    pub(super) fn entry(
        &mut self,
        source: &AssetSource,
        location: Option<Location>,
    ) -> Result<ReadFile<'_>, IoError> {
        let immutable = self.overlay.is_none();
        if let Some(overlay) = &mut self.overlay {
            match overlay.open(source) {
                Ok(file) => return Ok(ReadFile::directory(file)),
                Err(error) if error.code == IoErrorCode::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let location = location
            .or_else(|| Self::locate(&self.archives, source))
            .ok_or_else(|| IoError {
                code: IoErrorCode::NotFound,
                message: format!("asset not found in mounted archives: {}", source.key.0),
            })?;
        let archive = &mut self.archives[location.archive];
        let descriptor = archive
            .index
            .entry_at(location.entry)
            .expect("resolved archive entry");
        let compressed = match descriptor.storage {
            EntryStorage::Raw => None,
            EntryStorage::Blocks { .. } => {
                let workspace = self.workspace.as_mut().unwrap();
                if workspace.source != Some(location.archive) {
                    workspace.scratch.clear_encoded();
                    workspace.source = Some(location.archive);
                }
                Some((descriptor, &mut workspace.scratch))
            }
        };
        Ok(ReadFile {
            file: &mut archive.file,
            offset: descriptor.offset,
            length: descriptor.length,
            version: ContentVersion(descriptor.version),
            location: immutable.then_some(location),
            compressed,
        })
    }
}
