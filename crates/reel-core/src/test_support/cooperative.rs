use super::queue::{Queue, QueueUsage, charge, invalid};
use crate::{
    budget::{BudgetCategory, OwnedBytes, QueueLimits},
    clock::Clock,
    identity::WorkId,
    result::{RejectCode, RejectReason, Rejected, Terminal},
    wake::WakeSignal,
    work::*,
};

pub const COMPLETION_BYTES: u64 = 512;
const CHUNK_BYTES: usize = 256;

struct Job {
    request: WorkRequest,
    output: Vec<u8>,
    scratch: Vec<u8>,
    cursor: usize,
    xor: u8,
}

/// Real cursor-based copy encoding and XOR verification for contract checks.
/// XOR is deliberately a fixture operation, not a package integrity algorithm.
pub struct CooperativeExecutor<C> {
    clock: C,
    wake: WakeSignal,
    queue: Queue<Job, WorkEvent>,
    caps: WorkCaps,
    next_id: Option<u64>,
}

impl<C: Clock> CooperativeExecutor<C> {
    pub fn copy_kind() -> WorkKind {
        WorkKind {
            name: "fixture.copy".into(),
            version: 1,
            operation: WorkOperation::Encode,
        }
    }
    pub fn xor_kind() -> WorkKind {
        WorkKind {
            name: "fixture.xor8".into(),
            version: 1,
            operation: WorkOperation::Verify,
        }
    }
    pub fn new(clock: C, limits: QueueLimits, wake: WakeSignal) -> Self {
        Self {
            clock,
            wake,
            queue: Queue::new(limits),
            caps: WorkCaps {
                mode: WorkMode::Cooperative,
                kinds: vec![Self::copy_kind(), Self::xor_kind()],
                queue: limits,
            },
            next_id: Some(1),
        }
    }
    pub fn usage(&self) -> QueueUsage {
        self.queue.usage()
    }
}

impl<C: Clock> WorkExecutor for CooperativeExecutor<C> {
    fn capabilities(&self) -> WorkCaps {
        self.caps.clone()
    }
    fn submit(&mut self, request: WorkRequest) -> Result<WorkId, Rejected<WorkRequest>> {
        if !self.caps.kinds.contains(&request.kind) {
            return Err(Rejected {
                request,
                reason: RejectReason::new(
                    RejectCode::Unsupported,
                    "work kind/version not registered",
                ),
            });
        }
        let is_copy = request.kind.operation == WorkOperation::Encode;
        let output_size = if is_copy {
            request.input.as_slice().len()
        } else {
            1
        };
        let scratch_size = CHUNK_BYTES.min(request.input.as_slice().len());
        let r = &request.reservations;
        let checked = if request.input.ticket().category() != BudgetCategory::CpuInput
            || r.output.category() != BudgetCategory::CpuOutput
            || r.output.bytes() < output_size as u64
            || r.temporary.category() != BudgetCategory::Temporary
            || r.temporary.bytes() < scratch_size as u64
            || r.completion.category() != BudgetCategory::Completion
            || r.completion.bytes() < COMPLETION_BYTES
        {
            Err(invalid(
                "work input/output/temporary/completion reservations invalid",
            ))
        } else {
            charge(&[
                request.input.ticket().bytes(),
                r.output.bytes(),
                r.temporary.bytes(),
                r.completion.bytes(),
                request.kind.name.capacity() as u64,
                size_of::<Job>() as u64,
            ])
        };
        let bytes = match checked {
            Ok(n) => n,
            Err(reason) => return Err(Rejected { request, reason }),
        };
        let Some(id) = self.next_id else {
            return Err(Rejected {
                request,
                reason: invalid("work identity space exhausted"),
            });
        };
        if let Err(reason) = self.queue.check_capacity(bytes) {
            return Err(Rejected { request, reason });
        }
        let mut output = Vec::new();
        let mut scratch = Vec::new();
        if output.try_reserve_exact(output_size).is_err()
            || scratch.try_reserve_exact(scratch_size).is_err()
            || output.capacity() as u64 > r.output.bytes()
            || scratch.capacity() as u64 > r.temporary.bytes()
        {
            return Err(Rejected {
                request,
                reason: RejectReason::new(
                    RejectCode::OutOfMemory,
                    "work allocation exceeds available reservation",
                ),
            });
        }
        let job = Job {
            request,
            output,
            scratch,
            cursor: 0,
            xor: 0,
        };
        if let Err(e) = self.queue.accept(id, job, bytes) {
            return Err(Rejected {
                request: e.request.request,
                reason: e.reason,
            });
        }
        self.next_id = id.checked_add(1);
        Ok(WorkId(id))
    }
    fn cancel(&mut self, id: WorkId) {
        if let Some(entry) = self.queue.take(id.0) {
            let Job {
                request: r,
                output,
                scratch,
                ..
            } = entry.request;
            // Release buffers before returning the output/temporary tickets.
            drop(output);
            drop(scratch);
            self.queue.finish(
                entry.bytes,
                WorkEvent {
                    id,
                    scope: r.scope,
                    result: Terminal::Cancelled,
                    completion: r.reservations.completion,
                },
            );
            self.wake.wake();
        }
    }
    fn advance(&mut self, budget: WorkBudget) -> WorkProgress {
        let mut progress = WorkProgress::default();
        while progress.units < budget.max_units {
            if budget
                .deadline
                .is_some_and(|d| self.clock.monotonic_now() >= d)
            {
                break;
            }
            let Some(mut entry) = self.queue.pop_priority(|j| j.request.priority) else {
                break;
            };
            let job = &mut entry.request;
            let remaining = job.request.input.as_slice().len() - job.cursor;
            let count = remaining
                .min(CHUNK_BYTES)
                .min((budget.max_bytes - progress.bytes).min(CHUNK_BYTES as u64) as usize);
            if remaining > 0 && count == 0 {
                self.queue.requeue(entry);
                break;
            }
            job.scratch.clear();
            job.scratch
                .extend_from_slice(&job.request.input.as_slice()[job.cursor..job.cursor + count]);
            if job.request.kind.operation == WorkOperation::Encode {
                job.output.extend_from_slice(&job.scratch);
            } else {
                for b in &job.scratch {
                    job.xor ^= *b;
                }
            }
            job.cursor += count;
            progress.units += 1;
            progress.bytes += count as u64;
            if job.cursor == job.request.input.as_slice().len() {
                let Job {
                    request: r,
                    mut output,
                    xor,
                    ..
                } = entry.request;
                let result = if r.kind.operation == WorkOperation::Verify && xor != 0 {
                    Terminal::Failed(WorkError {
                        code: WorkErrorCode::InvalidInput,
                        message: "XOR packet checksum mismatch".into(),
                    })
                } else {
                    if r.kind.operation == WorkOperation::Verify {
                        output.push(xor);
                    }
                    Terminal::Completed(
                        OwnedBytes::new(output, r.reservations.output)
                            .expect("preallocated output fits ticket"),
                    )
                };
                self.queue.finish(
                    entry.bytes,
                    WorkEvent {
                        id: WorkId(entry.id),
                        scope: r.scope,
                        result,
                        completion: r.reservations.completion,
                    },
                );
                self.wake.wake();
            } else {
                self.queue.requeue(entry);
            }
        }
        progress.has_work = self.queue.usage().pending > 0;
        progress
    }
    fn drain(&mut self, out: &mut Vec<WorkEvent>, limit: usize) {
        self.queue.drain(out, limit);
    }
}
