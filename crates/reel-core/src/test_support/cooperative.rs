use super::queue::{Queue, QueueUsage, charge, invalid};
use crate::{
    budget::{BudgetCategory, BudgetTicket, OwnedBytes, QueueLimits},
    clock::Clock,
    identity::{Priority, Scope, WorkId},
    request_ids::RequestIds,
    result::{RejectCode, RejectReason, Rejected, Terminal},
    wake::WakeSignal,
    work::*,
};

pub const COMPLETION_BYTES: u64 = 512;
/// Fixture starting configuration; production adapters choose their own limits.
pub const DEFAULT_QUEUE_LIMITS: QueueLimits = QueueLimits {
    max_requests: 32,
    max_bytes: 4 << 20,
};
const CHUNK_BYTES: usize = 256;

/// Field order is the residency invariant: deallocate data before returning its
/// ticket, including whole-Job destruction and partially moved completion paths.
struct WorkBuffer {
    data: Vec<u8>,
    ticket: BudgetTicket,
}

impl WorkBuffer {
    fn into_ticket(self) -> BudgetTicket {
        let Self { data, ticket } = self;
        drop(data);
        ticket
    }

    fn into_bytes(self) -> OwnedBytes {
        OwnedBytes::new(self.data, self.ticket).expect("preallocated output fits ticket")
    }
}

struct Job {
    output: WorkBuffer,
    scratch: WorkBuffer,
    input: std::sync::Arc<OwnedBytes>,
    kind: WorkKind,
    scope: Scope,
    priority: Priority,
    completion: BudgetTicket,
    cursor: usize,
    xor: u8,
}

impl Job {
    /// Return the original request and tickets after releasing admission buffers.
    fn into_request(self) -> WorkRequest {
        let Self {
            output,
            scratch,
            input,
            kind,
            scope,
            priority,
            completion,
            ..
        } = self;
        WorkRequest {
            kind,
            input,
            scope,
            priority,
            reservations: WorkReservations {
                output: output.into_ticket(),
                temporary: scratch.into_ticket(),
                completion,
            },
        }
    }
}

/// Real cursor-based copy encoding and XOR verification for contract checks.
/// XOR is deliberately a fixture operation, not a package integrity algorithm.
pub struct CooperativeExecutor<C> {
    clock: C,
    wake: WakeSignal,
    queue: Queue<Job, WorkEvent>,
    caps: WorkCaps,
    ids: RequestIds<WorkId>,
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
        Self::with_ids(clock, limits, wake, RequestIds::default())
    }
    pub fn with_ids(
        clock: C,
        limits: QueueLimits,
        wake: WakeSignal,
        ids: RequestIds<WorkId>,
    ) -> Self {
        Self {
            clock,
            wake,
            queue: Queue::new(limits),
            caps: WorkCaps {
                mode: WorkMode::Cooperative,
                kinds: vec![Self::copy_kind(), Self::xor_kind()],
                queue: limits,
            },
            ids,
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
        let WorkRequest {
            kind,
            input,
            scope,
            priority,
            reservations,
        } = request;
        let job = Job {
            output: WorkBuffer {
                data: output,
                ticket: reservations.output,
            },
            scratch: WorkBuffer {
                data: scratch,
                ticket: reservations.temporary,
            },
            input,
            kind,
            scope,
            priority,
            completion: reservations.completion,
            cursor: 0,
            xor: 0,
        };
        let accepted = self.ids.accept(job, |id, job| {
            let priority = job.priority;
            self.queue.accept(id.0, job, bytes, priority)
        });
        match accepted {
            Ok(id) => Ok(id),
            Err(e) => Err(Rejected {
                request: e.request.into_request(),
                reason: e.reason,
            }),
        }
    }
    fn cancel(&mut self, id: WorkId) {
        if let Some(entry) = self.queue.take(id.0) {
            let Job {
                output,
                scratch,
                input,
                scope,
                completion,
                ..
            } = entry.request;
            // Release buffers before returning the output/temporary tickets.
            drop(output);
            drop(scratch);
            drop(input);
            self.queue.finish(
                WorkEvent {
                    id,
                    scope,
                    result: Terminal::Cancelled,
                    completion,
                },
                entry.slot,
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
            let Some(mut entry) = self.queue.pop() else {
                break;
            };
            let job = &mut entry.request;
            let remaining = job.input.as_slice().len() - job.cursor;
            let count = remaining
                .min(CHUNK_BYTES)
                .min((budget.max_bytes - progress.bytes).min(CHUNK_BYTES as u64) as usize);
            if remaining > 0 && count == 0 {
                self.queue.requeue(entry);
                break;
            }
            job.scratch.data.clear();
            job.scratch
                .data
                .extend_from_slice(&job.input.as_slice()[job.cursor..job.cursor + count]);
            if job.kind.operation == WorkOperation::Encode {
                job.output.data.extend_from_slice(&job.scratch.data);
            } else {
                for b in &job.scratch.data {
                    job.xor ^= *b;
                }
            }
            job.cursor += count;
            progress.units += 1;
            progress.bytes += count as u64;
            if job.cursor == job.input.as_slice().len() {
                let Job {
                    mut output,
                    scratch,
                    input,
                    kind,
                    scope,
                    completion,
                    xor,
                    ..
                } = entry.request;
                drop(scratch);
                drop(input);
                let result = if kind.operation == WorkOperation::Verify && xor != 0 {
                    drop(output);
                    Terminal::Failed(WorkError {
                        code: WorkErrorCode::InvalidInput,
                        message: "XOR packet checksum mismatch".into(),
                    })
                } else {
                    if kind.operation == WorkOperation::Verify {
                        output.data.push(xor);
                    }
                    Terminal::Completed(output.into_bytes())
                };
                self.queue.finish(
                    WorkEvent {
                        id: WorkId(entry.slot.id),
                        scope,
                        result,
                        completion,
                    },
                    entry.slot,
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
