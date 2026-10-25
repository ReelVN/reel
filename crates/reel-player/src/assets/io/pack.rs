use super::{ReadFile, directory, failure, file::File, io_error};
use reel_core::{
    budget::{BudgetCategory, BudgetPool, BudgetTicket},
    identity::{AssetSource, ContentVersion},
    io::{IoError, IoErrorCode},
};
use reel_package::archive::{self, ArchiveIndex, EntryStorage, ReadScratch};
use std::{collections::BTreeMap, path::PathBuf, sync::atomic::AtomicBool};

struct Entry {
    archive: usize,
    descriptor: archive::Entry,
}
struct Workspace {
    scratch: ReadScratch,
    _ticket: BudgetTicket,
}
pub(super) struct Files {
    archives: Vec<File>,
    entries: BTreeMap<String, Entry>,
    overlay: Option<directory::Files>,
    workspace: Option<Workspace>,
}
impl Files {
    pub(super) fn open(
        paths: impl IntoIterator<Item = PathBuf>,
        overlay: Option<directory::Files>,
        temporary: &BudgetPool,
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
                        descriptor: entry.clone(),
                    },
                );
            }
            archives.push(file);
        }
        let workspace = if entries
            .values()
            .any(|e| matches!(e.descriptor.storage, EntryStorage::Blocks { .. }))
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
            entries,
            overlay,
            workspace,
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
        let descriptor = &entry.descriptor;
        let compressed = match descriptor.storage {
            EntryStorage::Raw => None,
            EntryStorage::Blocks { .. } => {
                Some((descriptor, &mut self.workspace.as_mut().unwrap().scratch))
            }
        };
        Ok(ReadFile {
            file: &mut self.archives[entry.archive],
            offset: descriptor.offset,
            length: descriptor.length,
            version: ContentVersion(descriptor.version),
            stamp: None,
            compressed,
        })
    }
}
