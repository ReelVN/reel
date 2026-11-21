//! Browser resources are immutable byte arrays for one mounted story.
use super::{FileIo, ReadFile, ReadHandle, failure, io_error};
use reel_core::{
    budget::QueueLimits,
    identity::{AssetSource, ContentVersion},
    io::{IoError, IoErrorCode},
    wake::WakeSignal,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

pub(in crate::assets) struct MemoryIo(pub(super) FileIo);

impl MemoryIo {
    pub(in crate::assets) fn new(
        files: BTreeMap<String, Arc<[u8]>>,
        input_bytes: u64,
        limits: QueueLimits,
        wake: WakeSignal,
    ) -> Result<Self, IoError> {
        let mut size = 0u64;
        let mut mounted = BTreeMap::new();
        for (key, bytes) in files {
            reel_package::archive::validate_path(&key).map_err(io_error)?;
            size = size
                .checked_add(bytes.len() as u64 + key.capacity() as u64)
                .filter(|bytes| *bytes <= input_bytes)
                .ok_or_else(|| failure("browser resource bytes exceed the input budget"))?;
            mounted.insert(
                key,
                Resource {
                    bytes,
                    version: OnceLock::new(),
                },
            );
        }
        Ok(Self(FileIo::new(
            super::Files::Memory(Files(mounted)),
            limits,
            wake,
        )))
    }
}

struct Resource {
    bytes: Arc<[u8]>,
    version: OnceLock<ContentVersion>,
}

pub(super) struct Files(BTreeMap<String, Resource>);

impl Files {
    pub(super) fn open(&self, source: &AssetSource) -> Result<ReadFile<'_>, IoError> {
        let resource = self.0.get(&source.key.0).ok_or_else(|| IoError {
            code: IoErrorCode::NotFound,
            message: format!("asset not found: {}", source.key.0),
        })?;
        Ok(ReadFile {
            file: ReadHandle::Memory(&resource.bytes),
            offset: 0,
            length: resource.bytes.len() as u64,
            version: *resource
                .version
                .get_or_init(|| ContentVersion(*blake3::hash(&resource.bytes).as_bytes())),
            location: None,
            compressed: None,
        })
    }
}
