use super::worker::{Step, Worker, check_completion};
mod directory;
pub(crate) mod file;
mod pack;
use file::File;
use reel_core::{
    budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
    identity::{AssetSource, ContentVersion, IoId},
    io::*,
    queue::{charge, invalid},
    request_ids::RequestIds,
    result::{RejectCode, RejectReason, Rejected, Terminal},
    wake::WakeSignal,
};
use std::{
    collections::BTreeMap,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

enum Request {
    Describe(DescribeRequest),
    Read(Reading),
}
struct Reading {
    request: ReadRequest,
    data: Vec<u8>,
    length: Option<usize>,
    version: Option<ContentVersion>,
    eof: bool,
    cursor: Option<Box<reel_package::archive::ReadCursor>>,
}
impl Reading {
    fn new(request: ReadRequest) -> Self {
        Self {
            request,
            data: Vec::new(),
            length: None,
            version: None,
            eof: false,
            cursor: None,
        }
    }
    fn step(&mut self, files: &mut Files, cancelled: &AtomicBool) -> Result<bool, IoError> {
        let mut file = files.open(&self.request.resource, cancelled)?;
        let length = if let Some(length) = self.length {
            length
        } else {
            let request = &mut self.request;
            if request.offset > file.length {
                return Err(failure("asset read starts after EOF"));
            }
            let length = u64::from(request.len).min(file.length - request.offset) as usize;
            self.data
                .try_reserve_exact(length)
                .map_err(|error| failure(error.to_string()))?;
            self.eof = request.offset + length as u64 == file.length;
            self.version = Some(file.version);
            request.resource.version = Some(file.version);
            if length > reel_package::archive::BLOCK_BYTES
                && let Some((entry, _)) = &file.compressed
            {
                self.cursor = Some(Box::new(
                    entry
                        .read_cursor(request.offset, length)
                        .map_err(io_error)?,
                ));
            }
            self.length = Some(length);
            length
        };
        let start = self.data.len();
        let offset = self.request.offset + start as u64;
        let quantum = reel_package::archive::BLOCK_BYTES
            - offset as usize % reel_package::archive::BLOCK_BYTES;
        let end = length.min(start + quantum);
        // Capacity is reserved once, but only the next quantum is initialized.
        self.data.resize(end, 0);
        file.read_at(
            offset,
            &mut self.data[start..end],
            self.cursor.as_deref_mut(),
            cancelled,
        )?;
        Ok(end == length)
    }
}
pub struct DirectoryIo(FileIo);
/// Indexed archives, mounted in declaration order (last matching entry wins).
pub struct PackIo(FileIo);
#[cfg(test)]
type ReadObserver = std::sync::Arc<dyn Fn(IoId, usize, usize, usize) + Send + Sync>;
struct FileIo {
    worker: Worker<(IoId, Request), IoEvent>,
    ids: RequestIds<IoId>,
    limits: QueueLimits,
}
impl DirectoryIo {
    pub fn new(root: PathBuf, limits: QueueLimits, wake: WakeSignal) -> Self {
        Self(FileIo::new(
            Files::Directory(directory::Files::new(root, limits.max_requests as usize)),
            limits,
            wake,
        ))
    }
    /// Resolves logical paths through the selected resource directory overlay.
    pub fn for_target(
        root: PathBuf,
        target: String,
        limits: QueueLimits,
        wake: WakeSignal,
    ) -> Result<Self, IoError> {
        let files = directory::Files::for_target(root, limits.max_requests as usize, target)?;
        Ok(Self(FileIo::new(Files::Directory(files), limits, wake)))
    }
    /// Reads logical resources from selected physical files, retaining ordinary
    /// range IO, content identity and bounded descriptor caching.
    pub fn selected(
        files: BTreeMap<String, PathBuf>,
        limits: QueueLimits,
        wake: WakeSignal,
    ) -> Self {
        Self(FileIo::new(
            Files::Directory(directory::Files::selected(
                files,
                limits.max_requests as usize,
            )),
            limits,
            wake,
        ))
    }
}
impl PackIo {
    #[cfg(test)]
    pub(super) fn observed(
        paths: impl IntoIterator<Item = PathBuf>,
        temporary: &BudgetPool,
        metadata: &BudgetPool,
        limits: QueueLimits,
        wake: WakeSignal,
        observer: ReadObserver,
    ) -> Result<Self, IoError> {
        Ok(Self(FileIo::build(
            Files::Pack(pack::Files::open(paths, None, temporary, metadata)?),
            limits,
            wake,
            Some(observer),
        )))
    }
    /// Mounts only archive indexes. An optional development overlay takes precedence;
    /// only missing loose files fall through to the selected archive entry.
    /// Compressed entries share one fixed workspace charged to `temporary` until
    /// the IO worker exits. Raw-only mounts allocate no decoding workspace.
    /// `metadata` admits index/mount capacities and parsing peak before allocation;
    /// retained index storage stays charged until the IO worker releases it.
    pub fn open(
        paths: impl IntoIterator<Item = PathBuf>,
        overlay: Option<PathBuf>,
        target: String,
        temporary: &BudgetPool,
        metadata: &BudgetPool,
        limits: QueueLimits,
        wake: WakeSignal,
    ) -> Result<Self, IoError> {
        let overlay = overlay
            .map(|root| directory::Files::for_target(root, limits.max_requests as usize, target))
            .transpose()?;
        let files = pack::Files::open(paths, overlay, temporary, metadata)?;
        Ok(Self(FileIo::new(Files::Pack(files), limits, wake)))
    }
    /// Mounts archives beneath a logical-to-physical resource selection. Archive
    /// entries supply resources absent from the selected file map.
    /// Metadata admission and decoding workspace follow [`Self::open`].
    pub fn selected(
        paths: impl IntoIterator<Item = PathBuf>,
        selected: BTreeMap<String, PathBuf>,
        temporary: &BudgetPool,
        metadata: &BudgetPool,
        limits: QueueLimits,
        wake: WakeSignal,
    ) -> Result<Self, IoError> {
        let overlay = directory::Files::selected(selected, limits.max_requests as usize);
        let files = pack::Files::open(paths, Some(overlay), temporary, metadata)?;
        Ok(Self(FileIo::new(Files::Pack(files), limits, wake)))
    }
}
enum Files {
    Directory(directory::Files),
    Pack(pack::Files),
}
impl Files {
    fn open(
        &mut self,
        source: &AssetSource,
        cancelled: &AtomicBool,
    ) -> Result<ReadFile<'_>, IoError> {
        if cancelled.load(Ordering::Acquire) {
            return Err(failure("read cancelled"));
        }
        reel_package::archive::validate_path(&source.key.0).map_err(io_error)?;
        let file = match self {
            Self::Directory(files) => ReadFile::directory(files.open(source)?),
            Self::Pack(files) => files.entry(source)?,
        };
        if source
            .version
            .is_some_and(|version| version != file.version)
        {
            return Err(directory::changed());
        }
        Ok(file)
    }
}
struct ReadFile<'a> {
    file: &'a mut File,
    offset: u64,
    length: u64,
    version: ContentVersion,
    compressed: Option<(
        reel_package::archive::Entry<'a>,
        &'a mut reel_package::archive::ReadScratch,
    )>,
}
impl<'a> ReadFile<'a> {
    fn directory(file: &'a mut directory::OpenFile) -> Self {
        Self {
            file: &mut file.file,
            offset: 0,
            length: file.length,
            version: file.version,
            compressed: None,
        }
    }
    fn read_at(
        &mut self,
        offset: u64,
        output: &mut [u8],
        cursor: Option<&mut reel_package::archive::ReadCursor>,
        cancelled: &AtomicBool,
    ) -> Result<(), IoError> {
        if let Some((entry, scratch)) = &mut self.compressed {
            if let Some(cursor) = cursor {
                return entry
                    .read_range_with_cursor(self.file, offset, output, scratch, cursor, || {
                        cancelled.load(Ordering::Acquire)
                    })
                    .map_err(io_error);
            }
            return entry
                .read_range(self.file, offset, output, scratch, || {
                    cancelled.load(Ordering::Acquire)
                })
                .map_err(io_error);
        }
        self.file
            .seek(SeekFrom::Start(self.offset + offset))
            .map_err(io_error)?;
        for chunk in output.chunks_mut(reel_package::archive::BLOCK_BYTES) {
            if cancelled.load(Ordering::Acquire) {
                return Err(failure("read cancelled"));
            }
            self.file.read_exact(chunk).map_err(io_error)?;
        }
        Ok(())
    }
}
impl FileIo {
    fn new(files: Files, limits: QueueLimits, wake: WakeSignal) -> Self {
        Self::build(
            files,
            limits,
            wake,
            #[cfg(test)]
            None,
        )
    }
    fn build(
        files: Files,
        limits: QueueLimits,
        wake: WakeSignal,
        #[cfg(test)] observer: Option<ReadObserver>,
    ) -> Self {
        let files = Mutex::new(files);
        Self {
            worker: Worker::stepped(
                limits,
                wake,
                2,
                move |(id, request), cancelled| {
                    let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::Read);
                    let (scope, completion, result) = match request {
                        Request::Describe(r) => {
                            let result =
                                files
                                    .lock()
                                    .unwrap()
                                    .open(&r.resource, cancelled)
                                    .map(|file| {
                                        IoPayload::Described(AssetInfo {
                                            length: Some(file.length),
                                            range_read: true,
                                            seekable: true,
                                            version: Some(file.version),
                                            media_type: None,
                                        })
                                    });
                            (r.scope, r.completion, result)
                        }
                        Request::Read(mut reading) => {
                            let result = reading.step(&mut files.lock().unwrap(), cancelled);
                            #[cfg(test)]
                            if let Some(observer) = &observer {
                                let seeks = match &*files.lock().unwrap() {
                                    Files::Pack(files) => files.seek_calls(),
                                    Files::Directory(_) => 0,
                                };
                                observer(
                                    id,
                                    reading.data.len(),
                                    reading.data.as_ptr() as usize,
                                    seeks,
                                );
                            }
                            if matches!(result, Ok(false)) {
                                return Step::Continue((id, Request::Read(reading)));
                            }
                            let r = reading.request;
                            let scope = r.scope;
                            let completion = r.completion;
                            let result = result.and_then(|_| {
                                let bytes = OwnedBytes::new(reading.data, r.output)
                                    .map_err(|e| failure(e.reason.message))?;
                                Ok(IoPayload::Read(ReadResult {
                                    bytes,
                                    eof: reading.eof,
                                    version: reading.version.expect("completed read has a version"),
                                }))
                            });
                            (scope, completion, result)
                        }
                    };
                    Step::Complete(IoEvent {
                        id,
                        scope,
                        process_request: None,
                        result: match result {
                            Ok(v) => Terminal::Completed(v),
                            Err(e) => Terminal::Failed(e),
                        },
                        completion,
                    })
                },
                |event| event.result = Terminal::Cancelled,
            ),
            ids: RequestIds::default(),
            limits,
        }
    }
}
fn failure(message: impl Into<String>) -> IoError {
    IoError {
        code: IoErrorCode::StorageFailure,
        message: message.into(),
    }
}
fn io_error(error: std::io::Error) -> IoError {
    IoError {
        code: if error.kind() == std::io::ErrorKind::NotFound {
            IoErrorCode::NotFound
        } else {
            IoErrorCode::StorageFailure
        },
        message: error.to_string(),
    }
}
#[allow(
    clippy::result_large_err,
    reason = "rejection returns the owning request without allocating"
)]
impl AssetIo for FileIo {
    fn capabilities(&self) -> IoCaps {
        IoCaps {
            range_read: true,
            read_queue: self.limits,
            save_queue: QueueLimits {
                max_requests: 0,
                max_bytes: 0,
            },
            save_levels: vec![],
        }
    }
    fn describe(&mut self, request: DescribeRequest) -> Result<IoId, Rejected<DescribeRequest>> {
        let checks = check_completion(&request.completion).and_then(|_| {
            charge(&[
                request.completion.bytes(),
                request.resource.key.0.capacity() as u64,
                size_of::<Request>() as u64,
            ])
        });
        let bytes = match checks {
            Ok(bytes) => bytes,
            Err(reason) => return Err(Rejected { request, reason }),
        };
        self.ids.accept(request, |id, request| {
            let priority = request.priority;
            self.worker
                .accept(id.0, (id, Request::Describe(request)), bytes, priority)
                .map_err(|error| {
                    let (_, Request::Describe(request)) = error.request else {
                        unreachable!()
                    };
                    Rejected {
                        request,
                        reason: error.reason,
                    }
                })
        })
    }
    fn read(&mut self, request: ReadRequest) -> Result<IoId, Rejected<ReadRequest>> {
        let checks = check_completion(&request.completion).and_then(|_| {
            if !matches!(
                request.output.category(),
                BudgetCategory::CpuInput | BudgetCategory::CpuOutput
            ) || request.output.bytes() < u64::from(request.len)
            {
                return Err(invalid("read output ticket missing or too small"));
            }
            charge(&[
                request.output.bytes(),
                request.completion.bytes(),
                request.resource.key.0.capacity() as u64,
                size_of::<Request>() as u64,
                if request.len as usize > reel_package::archive::BLOCK_BYTES {
                    size_of::<reel_package::archive::ReadCursor>() as u64
                } else {
                    0
                },
            ])
        });
        let bytes = match checks {
            Ok(bytes) => bytes,
            Err(reason) => return Err(Rejected { request, reason }),
        };
        self.ids.accept(request, |id, request| {
            let priority = request.priority;
            self.worker
                .accept(
                    id.0,
                    (id, Request::Read(Reading::new(request))),
                    bytes,
                    priority,
                )
                .map_err(|error| {
                    let (_, Request::Read(request)) = error.request else {
                        unreachable!()
                    };
                    Rejected {
                        request: request.request,
                        reason: error.reason,
                    }
                })
        })
    }
    fn replace_save(&mut self, request: SaveWrite) -> Result<IoId, Rejected<SaveWrite>> {
        Err(Rejected {
            request,
            reason: RejectReason::new(RejectCode::Unsupported, "asset storage is read-only"),
        })
    }
    fn cancel(&mut self, id: IoId) {
        self.worker.cancel(id.0);
    }
    fn poll(&mut self, out: &mut Vec<IoEvent>, limit: usize) {
        self.worker.drain(out, limit);
    }
}

macro_rules! forward_asset_io {
    ($adapter:ty) => {
        #[allow(
            clippy::result_large_err,
            reason = "rejection returns the owning request without allocating"
        )]
        impl AssetIo for $adapter {
            fn capabilities(&self) -> IoCaps {
                self.0.capabilities()
            }
            fn describe(
                &mut self,
                request: DescribeRequest,
            ) -> Result<IoId, Rejected<DescribeRequest>> {
                self.0.describe(request)
            }
            fn read(&mut self, request: ReadRequest) -> Result<IoId, Rejected<ReadRequest>> {
                self.0.read(request)
            }
            fn replace_save(&mut self, request: SaveWrite) -> Result<IoId, Rejected<SaveWrite>> {
                self.0.replace_save(request)
            }
            fn cancel(&mut self, id: IoId) {
                self.0.cancel(id);
            }
            fn poll(&mut self, out: &mut Vec<IoEvent>, limit: usize) {
                self.0.poll(out, limit);
            }
        }
    };
}
forward_asset_io!(DirectoryIo);
forward_asset_io!(PackIo);
