use super::worker::{Worker, check_completion};
mod directory;
mod pack;
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
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};

enum Request {
    Describe(DescribeRequest),
    Read(ReadRequest),
}
pub struct DirectoryIo(FileIo);
/// Indexed archives, mounted in declaration order (last matching entry wins).
pub struct PackIo(FileIo);
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
}
impl PackIo {
    /// Mounts only archive indexes. An optional development overlay takes precedence;
    /// only missing loose files fall through to the selected archive entry.
    /// Compressed entries share one fixed workspace charged to `temporary` until
    /// the IO worker exits. Raw-only mounts allocate no decoding workspace.
    pub fn open(
        paths: impl IntoIterator<Item = PathBuf>,
        overlay: Option<PathBuf>,
        target: String,
        temporary: &BudgetPool,
        limits: QueueLimits,
        wake: WakeSignal,
    ) -> Result<Self, IoError> {
        let files = pack::Files::open(
            paths,
            overlay,
            target,
            temporary,
            limits.max_requests as usize,
        )?;
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
        reel_package::archive::validate_path(&source.key.0).map_err(io_error)?;
        let file = match self {
            Self::Directory(files) => ReadFile::directory(files.open(source, cancelled)?),
            Self::Pack(files) => files.entry(source, cancelled)?,
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
    stamp: Option<SystemTime>,
    compressed: Option<(
        &'a reel_package::archive::Entry,
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
            stamp: Some(file.modified),
            compressed: None,
        }
    }
    fn read_at(
        &mut self,
        offset: u64,
        output: &mut [u8],
        cancelled: &AtomicBool,
    ) -> Result<(), IoError> {
        if let Some((entry, scratch)) = &mut self.compressed {
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
    fn verify(&self) -> Result<(), IoError> {
        if let Some(modified) = self.stamp
            && !directory::same(
                &self.file.metadata().map_err(io_error)?,
                self.length,
                modified,
            )?
        {
            return Err(directory::changed());
        }
        Ok(())
    }
}
impl FileIo {
    fn new(files: Files, limits: QueueLimits, wake: WakeSignal) -> Self {
        let files = Mutex::new(files);
        Self {
            worker: Worker::new(
                limits,
                wake,
                move |(id, request), cancelled| {
                    let (scope, completion, result) =
                        match request {
                            Request::Describe(r) => {
                                let result =
                                    files.lock().unwrap().open(&r.resource, cancelled).map(
                                        |file| {
                                            IoPayload::Described(AssetInfo {
                                                length: Some(file.length),
                                                range_read: true,
                                                seekable: true,
                                                version: Some(file.version),
                                                media_type: None,
                                            })
                                        },
                                    );
                                (r.scope, r.completion, result)
                            }
                            Request::Read(r) => {
                                let scope = r.scope;
                                let completion = r.completion;
                                let result = (|| {
                                    let mut files = files.lock().unwrap();
                                    let mut file = files.open(&r.resource, cancelled)?;
                                    if r.offset > file.length {
                                        return Err(failure("asset read starts after EOF"));
                                    }
                                    let len = u64::from(r.len).min(file.length - r.offset) as usize;
                                    let mut data = Vec::new();
                                    data.try_reserve_exact(len)
                                        .map_err(|e| failure(e.to_string()))?;
                                    data.resize(len, 0);
                                    file.read_at(r.offset, &mut data, cancelled)?;
                                    file.verify()?;
                                    let version = file.version;
                                    let eof = r.offset + len as u64 == file.length;
                                    let bytes = OwnedBytes::new(data, r.output)
                                        .map_err(|e| failure(e.reason.message))?;
                                    Ok(IoPayload::Read(ReadResult {
                                        bytes,
                                        eof,
                                        version,
                                    }))
                                })();
                                (scope, completion, result)
                            }
                        };
                    IoEvent {
                        id,
                        scope,
                        process_request: None,
                        result: match result {
                            Ok(v) => Terminal::Completed(v),
                            Err(e) => Terminal::Failed(e),
                        },
                        completion,
                    }
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
                size_of::<DescribeRequest>() as u64,
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
                size_of::<ReadRequest>() as u64,
            ])
        });
        let bytes = match checks {
            Ok(bytes) => bytes,
            Err(reason) => return Err(Rejected { request, reason }),
        };
        self.ids.accept(request, |id, request| {
            let priority = request.priority;
            self.worker
                .accept(id.0, (id, Request::Read(request)), bytes, priority)
                .map_err(|error| {
                    let (_, Request::Read(request)) = error.request else {
                        unreachable!()
                    };
                    Rejected {
                        request,
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
