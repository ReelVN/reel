use super::queue::{Queue, QueueUsage, charge, invalid};
use crate::{
    budget::{BudgetCategory, BudgetTicket, OwnedBytes, QueueLimits},
    identity::{IoId, ResourceVersion, Scope},
    io::*,
    result::{RejectCode, Rejected, Terminal},
    wake::WakeSignal,
};
use std::collections::BTreeMap;

/// Conservative record/diagnostic/metadata reservation for this fixture only.
pub const COMPLETION_BYTES: u64 = 512;

#[derive(Debug)]
pub struct MemoryAsset {
    pub bytes: OwnedBytes,
    pub media_type: Option<String>,
}

#[derive(Debug)]
pub struct StoredSave {
    pub data: OwnedBytes,
    pub commit: SaveCommit,
}

enum ReadOp {
    Describe(DescribeRequest),
    Read(ReadRequest),
}

/// Reads exact immutable versions; saves implement only in-process visible replacement.
/// Driver methods perform memory operations; `poll` itself only drains completions.
pub struct MemoryAssetIo {
    caps: IoCaps,
    assets: BTreeMap<ResourceVersion, MemoryAsset>,
    saves: BTreeMap<SaveSlot, StoredSave>,
    reads: Queue<ReadOp, IoEvent>,
    writes: Queue<SaveWrite, IoEvent>,
    next_id: Option<u64>,
    wake: WakeSignal,
    fail_next_save: bool,
}

impl MemoryAssetIo {
    pub fn new(read_queue: QueueLimits, save_queue: QueueLimits, wake: WakeSignal) -> Self {
        Self {
            caps: IoCaps {
                range_read: true,
                read_queue,
                save_queue,
                save_levels: vec![SaveDurability::Visible],
            },
            assets: BTreeMap::new(),
            saves: BTreeMap::new(),
            reads: Queue::new(read_queue),
            writes: Queue::new(save_queue),
            next_id: Some(1),
            wake,
            fail_next_save: false,
        }
    }
    pub fn insert_asset(
        &mut self,
        version: ResourceVersion,
        asset: MemoryAsset,
    ) -> Result<(), Rejected<(ResourceVersion, MemoryAsset)>> {
        if self.assets.contains_key(&version)
            || asset
                .media_type
                .as_ref()
                .is_some_and(|t| t.capacity() > 128)
        {
            return Err(Rejected {
                request: (version, asset),
                reason: invalid("version already fixed or media type exceeds fixture bound"),
            });
        }
        self.assets.insert(version, asset);
        Ok(())
    }
    pub fn saved(&self, slot: &SaveSlot) -> Option<&StoredSave> {
        self.saves.get(slot)
    }
    pub fn fail_next_save(&mut self) {
        self.fail_next_save = true;
    }
    pub fn read_usage(&self) -> QueueUsage {
        self.reads.usage()
    }
    pub fn save_usage(&self) -> QueueUsage {
        self.writes.usage()
    }

    pub fn complete_next_read(&mut self) -> bool {
        let Some(entry) = self.reads.pop_priority(|r| match r {
            ReadOp::Describe(r) => r.priority,
            ReadOp::Read(r) => r.priority,
        }) else {
            return false;
        };
        let (scope, completion, result) = match entry.request {
            ReadOp::Describe(r) => {
                let result = self.find(&r.resource).map(|a| {
                    IoPayload::Described(AssetInfo {
                        length: Some(a.bytes.as_slice().len() as u64),
                        range_read: true,
                        seekable: true,
                        version: r.resource.version,
                        media_type: a.media_type.clone(),
                    })
                });
                (r.scope, r.completion, terminal(result))
            }
            ReadOp::Read(r) => {
                let result = self.find(&r.resource).and_then(|a| {
                    let source = a.bytes.as_slice();
                    let start = r.offset.min(source.len() as u64) as usize;
                    let count = (source.len() - start).min(r.len as usize);
                    let mut bytes = Vec::new();
                    bytes.try_reserve_exact(count).map_err(|_| IoError {
                        code: IoErrorCode::OutOfMemory,
                        message: "read allocation failed".into(),
                    })?;
                    if bytes.capacity() as u64 > r.output.bytes() {
                        return Err(IoError {
                            code: IoErrorCode::OutOfMemory,
                            message: "allocator capacity exceeds reservation".into(),
                        });
                    }
                    bytes.extend_from_slice(&source[start..start + count]);
                    Ok(IoPayload::Read(ReadResult {
                        bytes: OwnedBytes::new(bytes, r.output).expect("capacity checked"),
                        eof: start + count == source.len(),
                        version: r.resource.version,
                    }))
                });
                (r.scope, r.completion, terminal(result))
            }
        };
        self.reads.finish(
            entry.bytes,
            IoEvent {
                id: IoId(entry.id),
                scope,
                process_request: None,
                result,
                completion,
            },
        );
        self.wake.wake();
        true
    }

    pub fn complete_next_save(&mut self) -> bool {
        let Some(entry) = self.writes.pop() else {
            return false;
        };
        let r = entry.request;
        let result = if std::mem::take(&mut self.fail_next_save) {
            Terminal::Failed(IoError {
                code: IoErrorCode::StorageFailure,
                message: "injected failure before replacement".into(),
            })
        } else {
            let commit = SaveCommit {
                identity: r.identity,
                commit_sequence: r.commit_sequence,
                durability: SaveDurability::Visible,
            };
            self.saves.insert(
                r.slot,
                StoredSave {
                    data: r.data,
                    commit,
                },
            );
            Terminal::Completed(IoPayload::Saved(commit))
        };
        self.writes.finish(
            entry.bytes,
            IoEvent {
                id: IoId(entry.id),
                scope: Scope::Process,
                process_request: Some(r.identity),
                result,
                completion: r.completion,
            },
        );
        self.wake.wake();
        true
    }

    fn find(&self, resource: &ResourceVersion) -> Result<&MemoryAsset, IoError> {
        self.assets.get(resource).ok_or_else(|| IoError {
            code: if self.assets.keys().any(|v| v.key == resource.key) {
                IoErrorCode::VersionUnavailable
            } else {
                IoErrorCode::NotFound
            },
            message: "exact resource version unavailable".into(),
        })
    }
}

fn terminal<T>(r: Result<T, IoError>) -> Terminal<T, IoError> {
    match r {
        Ok(p) => Terminal::Completed(p),
        Err(e) => Terminal::Failed(e),
    }
}

fn check_completion(ticket: &BudgetTicket) -> Result<(), crate::result::RejectReason> {
    if ticket.category() != BudgetCategory::Completion || ticket.bytes() < COMPLETION_BYTES {
        return Err(invalid("completion ticket missing or too small"));
    }
    Ok(())
}

impl AssetIo for MemoryAssetIo {
    fn capabilities(&self) -> IoCaps {
        self.caps.clone()
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
            Ok(n) => n,
            Err(reason) => return Err(Rejected { request, reason }),
        };
        let Some(id) = self.next_id else {
            return Err(Rejected {
                request,
                reason: invalid("IO identity space exhausted"),
            });
        };
        if let Err(e) = self.reads.accept(id, ReadOp::Describe(request), bytes) {
            let ReadOp::Describe(request) = e.request else {
                unreachable!()
            };
            return Err(Rejected {
                request,
                reason: e.reason,
            });
        }
        self.next_id = id.checked_add(1);
        Ok(IoId(id))
    }
    fn read(&mut self, request: ReadRequest) -> Result<IoId, Rejected<ReadRequest>> {
        let checks = check_completion(&request.completion).and_then(|_| {
            if request.offset.checked_add(u64::from(request.len)).is_none()
                || request.output.category() != BudgetCategory::CpuOutput
                || request.output.bytes() < u64::from(request.len)
            {
                return Err(invalid("read range or output reservation invalid"));
            }
            charge(&[
                request.completion.bytes(),
                request.output.bytes(),
                request.resource.key.0.capacity() as u64,
                size_of::<ReadRequest>() as u64,
            ])
        });
        let bytes = match checks {
            Ok(n) => n,
            Err(reason) => return Err(Rejected { request, reason }),
        };
        let Some(id) = self.next_id else {
            return Err(Rejected {
                request,
                reason: invalid("IO identity space exhausted"),
            });
        };
        if let Err(e) = self.reads.accept(id, ReadOp::Read(request), bytes) {
            let ReadOp::Read(request) = e.request else {
                unreachable!()
            };
            return Err(Rejected {
                request,
                reason: e.reason,
            });
        }
        self.next_id = id.checked_add(1);
        Ok(IoId(id))
    }
    fn replace_save(&mut self, request: SaveWrite) -> Result<IoId, Rejected<SaveWrite>> {
        if !self.caps.save_levels.contains(&request.minimum_durability) {
            return Err(Rejected {
                request,
                reason: crate::result::RejectReason::new(
                    RejectCode::Unsupported,
                    "memory IO provides visible replacement only",
                ),
            });
        }
        let last = self
            .saves
            .get(&request.slot)
            .map(|s| s.commit.commit_sequence)
            .into_iter()
            .chain(
                self.writes
                    .pending()
                    .filter(|e| e.request.slot == request.slot)
                    .map(|e| e.request.commit_sequence),
            )
            .max();
        let checks = check_completion(&request.completion).and_then(|_| {
            if request.data.ticket().category() != BudgetCategory::CpuInput
                || last.is_some_and(|n| request.commit_sequence <= n)
            {
                return Err(invalid("save input or commit order invalid"));
            }
            charge(&[
                request.data.ticket().bytes(),
                request.completion.bytes(),
                request.slot.namespace.capacity() as u64,
                request.slot.slot.capacity() as u64,
                size_of::<SaveWrite>() as u64,
            ])
        });
        let bytes = match checks {
            Ok(n) => n,
            Err(reason) => return Err(Rejected { request, reason }),
        };
        let Some(id) = self.next_id else {
            return Err(Rejected {
                request,
                reason: invalid("IO identity space exhausted"),
            });
        };
        self.writes.accept(id, request, bytes)?;
        self.next_id = id.checked_add(1);
        Ok(IoId(id))
    }
    fn cancel(&mut self, id: IoId) {
        if let Some(entry) = self.reads.take(id.0) {
            let (scope, completion) = match entry.request {
                ReadOp::Describe(r) => (r.scope, r.completion),
                ReadOp::Read(r) => (r.scope, r.completion),
            };
            self.reads.finish(
                entry.bytes,
                IoEvent {
                    id,
                    scope,
                    process_request: None,
                    result: Terminal::Cancelled,
                    completion,
                },
            );
            self.wake.wake();
        } else if let Some(entry) = self.writes.take(id.0) {
            let r = entry.request;
            self.writes.finish(
                entry.bytes,
                IoEvent {
                    id,
                    scope: Scope::Process,
                    process_request: Some(r.identity),
                    result: Terminal::Cancelled,
                    completion: r.completion,
                },
            );
            self.wake.wake();
        }
    }
    fn poll(&mut self, out: &mut Vec<IoEvent>, limit: usize) {
        let before = out.len();
        self.reads.drain(out, limit);
        self.writes.drain(out, limit - (out.len() - before));
    }
}
