use super::worker::{Worker, check_completion};
use reel_core::{
    budget::{BudgetCategory, OwnedBytes, QueueLimits},
    identity::{ContentVersion, IoId},
    io::*,
    queue::{charge, invalid},
    request_ids::RequestIds,
    result::{RejectCode, RejectReason, Rejected, Terminal},
    wake::WakeSignal,
};
use std::{io::Read, path::PathBuf, sync::atomic::Ordering};

enum Request {
    Describe(DescribeRequest),
    Read(ReadRequest),
}
pub struct DirectoryIo {
    worker: Worker<(IoId, Request), IoEvent>,
    ids: RequestIds<IoId>,
    limits: QueueLimits,
}
impl DirectoryIo {
    pub fn new(root: PathBuf, limits: QueueLimits, wake: WakeSignal) -> Self {
        Self {
            worker: Worker::new(
                limits,
                wake,
                move |(id, request), cancelled| {
                    let (scope, completion, result) = match request {
                        Request::Describe(r) => {
                            let result = std::fs::metadata(root.join(&r.resource.key.0))
                                .map(|m| {
                                    IoPayload::Described(AssetInfo {
                                        length: Some(m.len()),
                                        range_read: false,
                                        seekable: false,
                                        version: None,
                                        media_type: None,
                                    })
                                })
                                .map_err(io_error);
                            (r.scope, r.completion, result)
                        }
                        Request::Read(r) => {
                            let scope = r.scope;
                            let completion = r.completion;
                            let result = (|| {
                                let mut file = std::fs::File::open(root.join(&r.resource.key.0))
                                    .map_err(io_error)?;
                                let length = file.metadata().map_err(io_error)?.len();
                                if length != u64::from(r.len) {
                                    return Err(failure("asset size changed before reading"));
                                }
                                let mut data = Vec::new();
                                data.try_reserve_exact(r.len as usize)
                                    .map_err(|e| failure(e.to_string()))?;
                                data.resize(r.len as usize, 0);
                                for chunk in data.chunks_mut(64 << 10) {
                                    if cancelled.load(Ordering::Acquire) {
                                        return Err(failure("read cancelled"));
                                    }
                                    file.read_exact(chunk).map_err(io_error)?;
                                }
                                if file.read(&mut [0]).map_err(io_error)? != 0 {
                                    return Err(failure("asset grew during reading"));
                                }
                                let version = ContentVersion(*blake3::hash(&data).as_bytes());
                                if r.resource
                                    .version
                                    .is_some_and(|expected| expected != version)
                                {
                                    return Err(IoError {
                                        code: IoErrorCode::VersionUnavailable,
                                        message: "requested asset version unavailable".into(),
                                    });
                                }
                                let bytes = OwnedBytes::new(data, r.output)
                                    .map_err(|e| failure(e.reason.message))?;
                                Ok(IoPayload::Read(ReadResult {
                                    bytes,
                                    eof: true,
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
impl AssetIo for DirectoryIo {
    fn capabilities(&self) -> IoCaps {
        IoCaps {
            range_read: false,
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
        if request.offset != 0 {
            return Err(Rejected {
                request,
                reason: RejectReason::new(
                    RejectCode::Unsupported,
                    "directory IO accepts complete bounded reads",
                ),
            });
        }
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
            reason: RejectReason::new(RejectCode::Unsupported, "asset directory is read-only"),
        })
    }
    fn cancel(&mut self, id: IoId) {
        self.worker.cancel(id.0);
    }
    fn poll(&mut self, out: &mut Vec<IoEvent>, limit: usize) {
        self.worker.drain(out, limit);
    }
}
