use super::{ReadFile, directory, failure, file::File, io_error};
use reel_core::{
    budget::{BudgetCategory, BudgetPool, BudgetTicket},
    identity::{AssetSource, ContentVersion},
    io::{IoError, IoErrorCode},
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
    _ticket: BudgetTicket,
}
pub(super) struct Files {
    archives: Vec<Archive>,
    overlay: Option<directory::Files>,
    workspace: Option<Workspace>,
    _storage: BudgetTicket,
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
            .flat_map(|archive| archive.index.entries())
            .any(|e| matches!(e.storage, EntryStorage::Blocks { .. }))
        {
            let ticket = temporary
                .reserve(archive::SCRATCH_BYTES as u64)
                .map_err(|e| failure(format!("archive workspace: {}", e.message)))?;
            if ticket.category() != BudgetCategory::Temporary {
                return Err(failure("archive workspace requires a temporary budget"));
            }
            Some(Workspace {
                scratch: ReadScratch::new(),
                _ticket: ticket,
            })
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
    pub(super) fn entry(&mut self, source: &AssetSource) -> Result<ReadFile<'_>, IoError> {
        if let Some(overlay) = &mut self.overlay {
            match overlay.open(source) {
                Ok(file) => return Ok(ReadFile::directory(file)),
                Err(error) if error.code == IoErrorCode::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let (file, descriptor) = self
            .archives
            .iter_mut()
            .rev()
            .find_map(|archive| {
                archive
                    .index
                    .get(&source.key.0)
                    .map(|entry| (&mut archive.file, entry))
            })
            .ok_or_else(|| IoError {
                code: IoErrorCode::NotFound,
                message: format!("asset not found in mounted archives: {}", source.key.0),
            })?;
        let compressed = match descriptor.storage {
            EntryStorage::Raw => None,
            EntryStorage::Blocks { .. } => {
                Some((descriptor, &mut self.workspace.as_mut().unwrap().scratch))
            }
        };
        Ok(ReadFile {
            file,
            offset: descriptor.offset,
            length: descriptor.length,
            version: ContentVersion(descriptor.version),
            compressed,
        })
    }
}
