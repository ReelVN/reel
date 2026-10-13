//! Host-owned, bounded non-RT destruction. PCM reaches only atomic flags.
use crate::{
    AudioError, AudioErrorKind,
    mixer::{Bridge, DecodedSound},
};
use reel_audio::{AudioManager, backend::Renderer, sound::Sound};
use reel_core::wake::WakeSignal;
use rtrb::{Consumer, Producer, RingBuffer};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex, MutexGuard, TryLockError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
#[derive(Clone, Copy, Debug)]
pub struct ReclaimLimits {
    pub max_generations: usize,
    pub queued_work: usize,
}
impl Default for ReclaimLimits {
    fn default() -> Self {
        Self {
            max_generations: 4,
            queued_work: 64,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReclaimStatus {
    pub live_generations: usize,
    pub control_owners: usize,
    pub unsubmitted_teardowns: usize,
    /// Normal queue plus the single worker's in-flight normal object.
    pub outstanding_work: usize,
    /// Completed retired-Sound jobs. Accepted-data aliases may still retain
    /// PCM; actual BudgetPool residency remains authoritative.
    pub completed_sounds: usize,
    pub completed_accepted_data: usize,
    pub completed_teardowns: usize,
    pub worker_errors: usize,
    pub last_job_ns: usize,
    pub max_job_ns: usize,
    pub closing: bool,
}
#[cfg(test)]
type WorkerHook = Arc<dyn Fn(JobKind) + Send + Sync>;
pub(crate) struct ServiceShared {
    limits: ReclaimLimits,
    pub normal: Mutex<Producer<Envelope>>,
    teardown: Mutex<Producer<Envelope>>,
    pub pending: AtomicBool,
    wake: WakeSignal,
    closing: AtomicBool,
    failed: AtomicBool,
    generations: AtomicUsize,
    control_owners: AtomicUsize,
    unsubmitted: AtomicUsize,
    outstanding: AtomicUsize,
    sounds: AtomicUsize,
    accepted: AtomicUsize,
    teardowns: AtomicUsize,
    errors: AtomicUsize,
    last_ns: AtomicUsize,
    max_ns: AtomicUsize,
    #[cfg(test)]
    pub test_hook: Mutex<Option<WorkerHook>>,
    #[cfg(test)]
    pub after_accept: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    pub abort_construction: AtomicBool,
}
impl ServiceShared {
    pub fn accepting(&self) -> bool {
        !self.closing.load(Ordering::Acquire) && !self.failed.load(Ordering::Acquire)
    }
    pub fn notify(&self) {
        self.pending.store(true, Ordering::Release);
    }
    fn status(&self) -> ReclaimStatus {
        ReclaimStatus {
            live_generations: self.generations.load(Ordering::Acquire),
            control_owners: self.control_owners.load(Ordering::Acquire),
            unsubmitted_teardowns: self.unsubmitted.load(Ordering::Acquire),
            outstanding_work: self.outstanding.load(Ordering::Acquire),
            completed_sounds: self.sounds.load(Ordering::Acquire),
            completed_accepted_data: self.accepted.load(Ordering::Acquire),
            completed_teardowns: self.teardowns.load(Ordering::Acquire),
            worker_errors: self.errors.load(Ordering::Acquire),
            last_job_ns: self.last_ns.load(Ordering::Acquire),
            max_job_ns: self.max_ns.load(Ordering::Acquire),
            closing: self.closing.load(Ordering::Acquire),
        }
    }
    fn record_error(&self) {
        self.failed.store(true, Ordering::Release);
        self.errors.fetch_add(1, Ordering::Release);
        self.notify();
    }
}
/// Host owned independently of mixers. Drop detaches a draining worker; only
/// explicit final-host drain_and_join may block on background destruction.
pub struct AudioReclaimService {
    pub(crate) shared: Arc<ServiceShared>,
    worker: Option<JoinHandle<()>>,
}
impl AudioReclaimService {
    pub fn new(limits: ReclaimLimits, wake: WakeSignal) -> Result<Self, AudioError> {
        let teardown_capacity = limits.max_generations.checked_mul(2);
        if limits.max_generations == 0
            || limits.queued_work == 0
            || std::alloc::Layout::array::<Envelope>(limits.queued_work).is_err()
            || teardown_capacity
                .is_none_or(|capacity| std::alloc::Layout::array::<Envelope>(capacity).is_err())
        {
            return Err(AudioError::new(
                AudioErrorKind::InvalidRequest,
                "invalid reclamation limits",
            ));
        }
        let (normal, normal_consumer) = RingBuffer::new(limits.queued_work);
        let (teardown, teardown_consumer) = RingBuffer::new(teardown_capacity.unwrap());
        let shared = Arc::new(ServiceShared {
            limits,
            normal: Mutex::new(normal),
            teardown: Mutex::new(teardown),
            pending: AtomicBool::new(false),
            wake,
            closing: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            generations: AtomicUsize::new(0),
            control_owners: AtomicUsize::new(0),
            unsubmitted: AtomicUsize::new(0),
            outstanding: AtomicUsize::new(0),
            sounds: AtomicUsize::new(0),
            accepted: AtomicUsize::new(0),
            teardowns: AtomicUsize::new(0),
            errors: AtomicUsize::new(0),
            last_ns: AtomicUsize::new(0),
            max_ns: AtomicUsize::new(0),
            #[cfg(test)]
            test_hook: Mutex::new(None),
            #[cfg(test)]
            after_accept: Mutex::new(None),
            #[cfg(test)]
            abort_construction: AtomicBool::new(false),
        });
        let worker_shared = shared.clone();
        let worker = thread::Builder::new()
            .name("reel-audio-reclaim".into())
            .spawn(move || run_worker(worker_shared, normal_consumer, teardown_consumer))
            .map_err(|e| AudioError::new(AudioErrorKind::Unavailable, e.to_string()))?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }
    pub fn status(&self) -> ReclaimStatus {
        self.shared.status()
    }
    pub fn limits(&self) -> ReclaimLimits {
        self.shared.limits
    }
    /// Final host exit only. Drop mixers/fills first; outstanding owner credits
    /// return Busy immediately. Queued/in-flight work may then block.
    pub fn drain_and_join(&mut self) -> Result<ReclaimStatus, AudioError> {
        if self.shared.control_owners.load(Ordering::Acquire) != 0 {
            return Err(AudioError::new(
                AudioErrorKind::Busy,
                "drop all mixers and fills before draining",
            ));
        }
        self.shared.closing.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            if worker.join().is_err() {
                self.shared.record_error();
            }
        }
        let status = self.status();
        if status.worker_errors != 0 {
            Err(AudioError::new(
                AudioErrorKind::DeviceFailure,
                "background reclamation failed; queues drained",
            ))
        } else {
            Ok(status)
        }
    }
    pub(crate) fn reserve_generation(
        &self,
    ) -> Result<(Arc<RetirementSignal>, TeardownCredit, TeardownCredit), AudioError> {
        if !self.shared.accepting() {
            return Err(AudioError::new(
                AudioErrorKind::Busy,
                "reclamation service is closing or failed",
            ));
        }
        self.shared
            .generations
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.shared.limits.max_generations).then_some(n + 1)
            })
            .map_err(|_| {
                AudioError::new(
                    AudioErrorKind::Busy,
                    "old mixer generation credits still resident",
                )
            })?;
        self.shared.unsubmitted.fetch_add(2, Ordering::AcqRel);
        self.shared.control_owners.fetch_add(2, Ordering::AcqRel);
        let generation = Arc::new(Generation {
            shared: self.shared.clone(),
            owner_bits: AtomicUsize::new(3),
        });
        let signal = Arc::new(RetirementSignal {
            observed: AtomicBool::new(false),
            shared: self.shared.clone(),
            generation: generation.clone(),
        });
        Ok((
            signal,
            TeardownCredit {
                generation: generation.clone(),
                submitted: false,
                owner_bit: 1,
            },
            TeardownCredit {
                generation,
                submitted: false,
                owner_bit: 2,
            },
        ))
    }
}
impl Drop for AudioReclaimService {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
        } // detach, no join
    }
}
pub(crate) struct Generation {
    shared: Arc<ServiceShared>,
    owner_bits: AtomicUsize,
}
impl Generation {
    fn release_control(&self, bit: usize) {
        if self.owner_bits.fetch_and(!bit, Ordering::AcqRel) & bit != 0 {
            self.shared.control_owners.fetch_sub(1, Ordering::AcqRel);
        }
    }
}
impl Drop for Generation {
    fn drop(&mut self) {
        self.shared.generations.fetch_sub(1, Ordering::AcqRel);
        self.shared.notify();
    }
}
pub(crate) struct RetirementSignal {
    pub observed: AtomicBool,
    pub shared: Arc<ServiceShared>,
    pub generation: Arc<Generation>,
}
pub(crate) struct TeardownCredit {
    generation: Arc<Generation>,
    submitted: bool,
    owner_bit: usize,
}
impl Drop for TeardownCredit {
    fn drop(&mut self) {
        self.generation.release_control(self.owner_bit);
        if !self.submitted {
            self.generation
                .shared
                .unsubmitted
                .fetch_sub(1, Ordering::AcqRel);
        }
        self.generation.shared.notify();
    }
}
pub(crate) struct NormalCredit {
    generation: Arc<Generation>,
}
impl NormalCredit {
    fn new(signal: &RetirementSignal) -> Self {
        signal.shared.outstanding.fetch_add(1, Ordering::AcqRel);
        Self {
            generation: signal.generation.clone(),
        }
    }
}
impl Drop for NormalCredit {
    fn drop(&mut self) {
        self.generation
            .shared
            .outstanding
            .fetch_sub(1, Ordering::AcqRel);
        self.generation.shared.notify();
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JobKind {
    Sound,
    AcceptedData,
    Renderer,
    Manager,
}
// Inline, preallocated slots deliberately avoid allocating during teardown.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Job {
    Sound(Box<dyn Sound>),
    AcceptedData(DecodedSound),
    Renderer(Renderer),
    Manager(AudioManager<Bridge>),
}
impl Job {
    fn kind(&self) -> JobKind {
        match self {
            Self::Sound(_owned) => JobKind::Sound,
            Self::AcceptedData(_owned) => JobKind::AcceptedData,
            Self::Renderer(_owned) => JobKind::Renderer,
            Self::Manager(_owned) => JobKind::Manager,
        }
    }
}
// Declaration order is essential: payload physically drops before either credit.
pub(crate) struct Envelope {
    job: Job,
    _normal: Option<NormalCredit>,
    _teardown: Option<TeardownCredit>,
}
impl Envelope {
    pub fn normal(job: Job, signal: &RetirementSignal) -> Self {
        Self {
            job,
            _normal: Some(NormalCredit::new(signal)),
            _teardown: None,
        }
    }
}
pub(crate) fn try_producer(shared: &ServiceShared) -> Option<MutexGuard<'_, Producer<Envelope>>> {
    match shared.normal.try_lock() {
        Ok(p) => Some(p),
        Err(TryLockError::Poisoned(e)) => Some(e.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}
fn submit_teardown<T>(
    value: &mut Option<T>,
    credit: &mut Option<TeardownCredit>,
    make: impl FnOnce(T) -> Job,
) {
    if value.is_none() {
        return;
    }
    let shared = credit
        .as_ref()
        .expect("one teardown credit per owner")
        .generation
        .shared
        .clone();
    let mut producer = shared.teardown.lock().unwrap_or_else(|e| e.into_inner());
    // 2G unique credits, 2G slots. This owner's credit is outside the queue,
    // proving capacity. Reserve the chunk BEFORE taking either owned value.
    let chunk = producer
        .write_chunk_uninit(1)
        .expect("reserved teardown capacity invariant");
    let mut credit = credit.take().expect("owned teardown credit");
    credit.submitted = true;
    shared.unsubmitted.fetch_sub(1, Ordering::AcqRel);
    let mut job = make(value.take().expect("owned teardown value"));
    if let Job::Manager(manager) = &mut job
        && manager.backend_mut().has_fill()
    {
        // The untaken fill is now inside a worker-owned Manager; its
        // reserved Renderer credit is still valid but no host owner remains.
        credit.generation.release_control(1);
    }
    credit.generation.release_control(credit.owner_bit);
    chunk.fill_from_iter(std::iter::once(Envelope {
        job,
        _normal: None,
        _teardown: Some(credit),
    }));
    shared.notify();
}
pub(crate) struct DeferredRenderer {
    pub renderer: Option<Renderer>,
    credit: Option<TeardownCredit>,
}
impl DeferredRenderer {
    pub fn new(renderer: Renderer, credit: TeardownCredit) -> Self {
        Self {
            renderer: Some(renderer),
            credit: Some(credit),
        }
    }
    pub fn render(&mut self, out: &mut [f32], signal: &RetirementSignal) {
        let renderer = self.renderer.as_mut().expect("owned Renderer");
        renderer.on_start_processing();
        // Notify AFTER actual unused publication; only atomic flags on PCM.
        if signal.observed.swap(false, Ordering::AcqRel) {
            signal.shared.notify();
        }
        renderer.process(out, 2);
    }
}
impl Drop for DeferredRenderer {
    fn drop(&mut self) {
        submit_teardown(&mut self.renderer, &mut self.credit, Job::Renderer);
    }
}
pub(crate) struct DeferredManager {
    pub manager: Option<AudioManager<Bridge>>,
    credit: Option<TeardownCredit>,
}
impl DeferredManager {
    pub fn new(manager: AudioManager<Bridge>, credit: TeardownCredit) -> Self {
        Self {
            manager: Some(manager),
            credit: Some(credit),
        }
    }
}
impl Drop for DeferredManager {
    fn drop(&mut self) {
        submit_teardown(&mut self.manager, &mut self.credit, Job::Manager);
    }
}
fn run_worker(
    shared: Arc<ServiceShared>,
    mut normal: Consumer<Envelope>,
    mut teardown: Consumer<Envelope>,
) {
    let mut prefer_teardown = false;
    loop {
        // Bounded fairness between ordinary work and reserved teardown.
        let work = if prefer_teardown {
            teardown.pop().or_else(|_| normal.pop())
        } else {
            normal.pop().or_else(|_| teardown.pop())
        };
        prefer_teardown = !prefer_teardown;
        if let Ok(work) = work {
            let kind = work.job.kind();
            let begin = Instant::now();
            let result = catch_unwind(AssertUnwindSafe(|| {
                #[cfg(test)]
                {
                    let hook = shared
                        .test_hook
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    if let Some(hook) = hook {
                        hook(kind);
                    }
                }
                drop(work);
            }));
            let ns = begin.elapsed().as_nanos().min(usize::MAX as u128) as usize;
            shared.last_ns.store(ns, Ordering::Release);
            shared.max_ns.fetch_max(ns, Ordering::AcqRel);
            if result.is_err() {
                shared.record_error();
            } else {
                match kind {
                    JobKind::Sound => {
                        shared.sounds.fetch_add(1, Ordering::Release);
                    }
                    JobKind::AcceptedData => {
                        shared.accepted.fetch_add(1, Ordering::Release);
                    }
                    JobKind::Renderer | JobKind::Manager => {
                        shared.teardowns.fetch_add(1, Ordering::Release);
                    }
                }
            }
            shared.notify();
        }
        if shared.pending.swap(false, Ordering::AcqRel)
            && catch_unwind(AssertUnwindSafe(|| shared.wake.wake())).is_err()
        {
            shared.record_error();
        }
        if shared.closing.load(Ordering::Acquire)
            && shared.generations.load(Ordering::Acquire) == 0
            && shared.outstanding.load(Ordering::Acquire) == 0
        {
            break;
        }
        if normal.is_empty() && teardown.is_empty() {
            thread::park_timeout(Duration::from_millis(2));
        }
    }
}
