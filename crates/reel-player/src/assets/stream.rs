//! Decoder workers submit and drain readers on the shared bounded IO lane.
use reel_core::{
    budget::{BudgetPool, BudgetTicket, OwnedBytes},
    identity::{AssetKey, AssetSource, Priority, Scope},
    io::{AssetIo, DescribeRequest, IoEvent, IoPayload, ReadDeadline, ReadRequest},
    result::{RejectCode, RejectReason, Terminal},
    wake::WakeSignal,
};
#[cfg(not(target_os = "vita"))]
use std::sync::Condvar;
use std::{
    collections::VecDeque,
    io::{self, Read, Seek, SeekFrom},
    sync::{Arc, Mutex, MutexGuard, Weak},
};
#[cfg(target_os = "vita")]
use vita_native::app::wake::Wake;

#[derive(Clone, Copy)]
enum Request {
    Describe,
    Read { offset: u64, len: u32 },
}
enum Response {
    Info(u64),
    Bytes(OwnedBytes),
}
struct State {
    source: AssetSource,
    completion: Option<BudgetTicket>,
    closed: bool,
}
struct Shared {
    deadline: ReadDeadline,
    input: BudgetPool,
    state: Mutex<State>,
    reader: Mutex<Option<Box<dyn AssetIo + Send>>>,
    admission: Arc<Mutex<Admission>>,
    #[cfg(not(target_os = "vita"))]
    ready: Condvar,
    #[cfg(target_os = "vita")]
    ready: Arc<Wake>,
    wake: WakeSignal,
}
impl Shared {
    fn recycle_completion(&self, completion: BudgetTicket) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return false;
        }
        assert!(state.completion.is_none(), "one completion slot per stream");
        state.completion = Some(completion);
        true
    }
    fn notify(&self) {
        // Serialize notification with the predicate check before Condvar::wait.
        let _state = self.state.lock().unwrap();
        #[cfg(not(target_os = "vita"))]
        self.ready.notify_one();
        #[cfg(target_os = "vita")]
        self.ready.signal().expect("signal live asset stream event");
    }
    fn wait<'a>(&'a self, state: MutexGuard<'a, State>) -> io::Result<MutexGuard<'a, State>> {
        #[cfg(not(target_os = "vita"))]
        {
            Ok(self.ready.wait(state).unwrap())
        }
        #[cfg(target_os = "vita")]
        {
            // SDK callbacks lack pthread cancellation state. The kernel event
            // retains notifications across this unlock before waiting.
            drop(state);
            self.ready.wait(None).map_err(io::Error::other)?;
            Ok(self.state.lock().unwrap())
        }
    }
    fn wake_next(&self) {
        let next = self
            .admission
            .lock()
            .unwrap()
            .next(self.input.available_bytes())
            .map(|(_, shared)| shared);
        if let Some(next) = next {
            next.wake.wake();
        }
    }
    fn leave_admission(&self) {
        self.admission.lock().unwrap().waiters.retain(|waiter| {
            !std::ptr::eq(waiter.reader.as_ptr(), self) && waiter.reader.strong_count() != 0
        });
        self.wake_next();
    }
}
/// Cancelling wakes the decoder and cancels its client without a host pump.
#[derive(Clone)]
pub struct StreamControl(Arc<Shared>);
impl StreamControl {
    /// Worker-owned deadline publisher for this stream's actual decoded buffer.
    pub fn read_deadline(&self) -> ReadDeadline {
        self.0.deadline.clone()
    }
    /// Fixed source identity becomes available after the first describe.
    pub fn version(&self) -> Option<reel_core::identity::ResourceVersion> {
        let state = self.0.state.lock().unwrap();
        state
            .source
            .version
            .map(|version| reel_core::identity::ResourceVersion {
                key: state.source.key.clone(),
                version,
            })
    }
    pub fn cancel(&self) {
        self.0.deadline.pause(true);
        let completion = {
            let mut state = self.0.state.lock().unwrap();
            state.closed = true;
            state.completion.take()
        };
        drop(completion);
        let reader = self.0.reader.lock().unwrap().take();
        drop(reader);
        self.0.admission.lock().unwrap().readers.retain(|reader| {
            !std::ptr::eq(reader.as_ptr(), Arc::as_ptr(&self.0)) && reader.strong_count() != 0
        });
        self.0.leave_admission();
        self.0.wake.wake();
    }
}
pub struct AssetStream {
    shared: Arc<Shared>,
    input: BudgetPool,
    events: Vec<IoEvent>,
    position: u64,
    length: Option<u64>,
    chunk: u32,
}
impl AssetStream {
    /// Call on the decoder worker before constructing a demuxer.
    pub fn open(&mut self) -> io::Result<u64> {
        let Response::Info(length) = self.transact(Request::Describe)? else {
            unreachable!()
        };
        self.length = Some(length);
        Ok(length)
    }
    fn submit(&self, request: Request) -> Result<Option<reel_core::identity::IoId>, RejectReason> {
        let mut admission = self.shared.admission.lock().unwrap();
        let selected = if matches!(request, Request::Read { .. }) {
            let Some((index, reader)) = admission.next(self.input.available_bytes()) else {
                return Ok(None);
            };
            if !Arc::ptr_eq(&reader, &self.shared) {
                return Ok(None);
            }
            Some(index)
        } else {
            None
        };
        let output = if let Request::Read { len, .. } = request {
            Some(self.input.reserve(u64::from(len))?)
        } else {
            None
        };
        let (source, completion) = {
            let mut state = self.shared.state.lock().unwrap();
            if state.closed {
                return Err(closed_reason());
            }
            (
                state.source.clone(),
                state
                    .completion
                    .take()
                    .expect("idle reader owns its completion slot"),
            )
        };
        let mut endpoint = self.shared.reader.lock().unwrap();
        let reader = endpoint.as_mut().ok_or_else(closed_reason)?;
        let (result, returned) = match request {
            Request::Describe => match reader.describe(DescribeRequest {
                resource: source,
                scope: Scope::Process,
                priority: Priority::Realtime,
                completion,
            }) {
                Ok(id) => (Ok(id), None),
                Err(error) => (Err(error.reason), Some(error.request.completion)),
            },
            Request::Read { offset, len } => match reader.read(ReadRequest {
                deadline: Some(self.shared.deadline.clone()),
                resource: source,
                offset,
                len,
                scope: Scope::Process,
                priority: Priority::Realtime,
                output: output.unwrap(),
                completion,
            }) {
                Ok(id) => (Ok(id), None),
                Err(error) => (Err(error.reason), Some(error.request.completion)),
            },
        };
        if result.is_ok()
            && let Some(index) = selected
        {
            admission.admitted(index);
        }
        drop(endpoint);
        drop(admission);
        if let Some(completion) = returned {
            self.shared.recycle_completion(completion);
        }
        if result.is_ok() {
            self.shared.wake_next();
        }
        result.map(Some)
    }
    fn transact(&mut self, request: Request) -> io::Result<Response> {
        if let Request::Read { len, .. } = request {
            self.shared
                .admission
                .lock()
                .unwrap()
                .waiters
                .push_back(Waiter {
                    reader: Arc::downgrade(&self.shared),
                    len,
                    bypasses: 0,
                });
            self.shared.wake_next();
        }
        let result = self.transact_inner(request);
        self.shared.leave_admission();
        result
    }
    fn transact_inner(&mut self, request: Request) -> io::Result<Response> {
        let mut active = None;
        loop {
            self.shared.wake.acknowledge();
            if self.shared.state.lock().unwrap().closed {
                return Err(closed());
            }
            if active.is_none() {
                match self.submit(request) {
                    Ok(id) => active = id,
                    Err(reason) if reason.code == RejectCode::Busy => {}
                    Err(reason) => return Err(io::Error::other(reason.message)),
                }
            }
            if active.is_some() {
                {
                    let mut reader = self.shared.reader.lock().unwrap();
                    reader
                        .as_mut()
                        .ok_or_else(closed)?
                        .poll(&mut self.events, 1);
                }
                if let Some(event) = self.events.pop() {
                    assert_eq!(Some(event.id), active, "reader owns one in-flight request");
                    if !self.shared.recycle_completion(event.completion) {
                        return Err(closed());
                    }
                    return match event.result {
                        Terminal::Completed(IoPayload::Described(info)) => {
                            if !info.range_read || !info.seekable || info.version.is_none() {
                                Err(io::Error::other(
                                    "media requires a seekable, versioned asset source",
                                ))
                            } else {
                                self.shared.state.lock().unwrap().source.version = info.version;
                                info.length
                                    .map(Response::Info)
                                    .ok_or_else(|| io::Error::other("media length unavailable"))
                            }
                        }
                        Terminal::Completed(IoPayload::Read(read))
                            if self.shared.state.lock().unwrap().source.version
                                == Some(read.version) =>
                        {
                            Ok(Response::Bytes(read.bytes))
                        }
                        Terminal::Completed(_) => Err(io::Error::other(
                            "unexpected asset stream response or version",
                        )),
                        Terminal::Failed(error) => Err(io::Error::other(error.message)),
                        Terminal::Cancelled => Err(io::Error::other("asset stream cancelled")),
                    };
                }
            }
            let mut state = self.shared.state.lock().unwrap();
            if state.closed {
                return Err(closed());
            }
            if !self.shared.wake.is_pending() {
                state = self.shared.wait(state)?;
            }
            drop(state);
        }
    }
}
impl Read for AssetStream {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let length = match self.length {
            Some(n) => n,
            None => self.open()?,
        };
        if self.position >= length {
            return Ok(0);
        }
        let len = (length - self.position)
            .min(output.len() as u64)
            .min(u64::from(self.chunk)) as u32;
        let Response::Bytes(bytes) = self.transact(Request::Read {
            offset: self.position,
            len,
        })?
        else {
            unreachable!()
        };
        let count = bytes.as_slice().len();
        output[..count].copy_from_slice(bytes.as_slice());
        self.position += count as u64;
        drop(bytes);
        // Completion draining precedes input lease release. Wake the next fitting
        // deadline here so tiny allowances progress without a main-thread turn.
        self.shared.wake_next();
        Ok(count)
    }
}
impl Seek for AssetStream {
    fn seek(&mut self, offset: SeekFrom) -> io::Result<u64> {
        let length = match self.length {
            Some(n) => n,
            None => self.open()?,
        };
        self.position = match offset {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(n) => self.position.checked_add_signed(n),
            SeekFrom::End(n) => length.checked_add_signed(n),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid asset seek"))?;
        Ok(self.position)
    }
}
impl Drop for AssetStream {
    fn drop(&mut self) {
        StreamControl(self.shared.clone()).cancel();
    }
}
fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "asset stream closed")
}
fn closed_reason() -> RejectReason {
    RejectReason::new(RejectCode::Unavailable, "asset stream closed")
}
struct Waiter {
    reader: Weak<Shared>,
    len: u32,
    bypasses: u8,
}
#[derive(Default)]
struct Admission {
    readers: Vec<Weak<Shared>>,
    waiters: VecDeque<Waiter>,
}
impl Admission {
    fn next(&self, available: u64) -> Option<(usize, Arc<Shared>)> {
        let mut chosen = None;
        let mut earliest = None;
        for (index, waiter) in self.waiters.iter().enumerate() {
            let Some(reader) = waiter.reader.upgrade() else {
                continue;
            };
            // Four bypasses reserve the next capacity for the oldest live request.
            // This bounds starvation even when a stream continually refills.
            if chosen.is_none() && waiter.bypasses >= 4 {
                return (u64::from(waiter.len) <= available).then_some((index, reader));
            }
            if u64::from(waiter.len) > available {
                continue;
            }
            let deadline = reader.deadline.get();
            let rank = (deadline.is_none(), deadline);
            if chosen.is_none() || earliest.is_some_and(|old| rank < old) {
                earliest = Some(rank);
                chosen = Some((index, reader));
            }
        }
        chosen
    }
    fn admitted(&mut self, index: usize) {
        for waiter in self.waiters.iter_mut().take(index) {
            waiter.bypasses = waiter.bypasses.saturating_add(1).min(4);
        }
        self.waiters.remove(index);
    }
}
#[derive(Default)]
pub(super) struct Streams(Arc<Mutex<Admission>>);
impl Streams {
    pub(super) fn create(
        &mut self,
        source: String,
        io: &dyn AssetIo,
        input: BudgetPool,
        completion: BudgetPool,
        metadata: BudgetPool,
        chunk: u32,
    ) -> Result<(StreamControl, AssetStream), String> {
        #[cfg(target_os = "vita")]
        let ready = Wake::new().map_err(|error| error.to_string())?;
        // Reserve the reader's one completion before it can wait. The same
        // ticket follows each request/terminal and is recycled after draining.
        let completion = completion.reserve(512).map_err(|reason| reason.message)?;
        let deadline = ReadDeadline::charged(&metadata).map_err(|reason| reason.message)?;
        let shared = Arc::new_cyclic(|weak: &Weak<Shared>| {
            let target = weak.clone();
            Shared {
                deadline,
                input: input.clone(),
                state: Mutex::new(State {
                    source: AssetSource {
                        key: AssetKey(source),
                        version: None,
                    },
                    completion: Some(completion),
                    closed: false,
                }),
                reader: Mutex::new(None),
                admission: self.0.clone(),
                #[cfg(not(target_os = "vita"))]
                ready: Condvar::new(),
                #[cfg(target_os = "vita")]
                ready,
                wake: WakeSignal::new(move || {
                    if let Some(shared) = target.upgrade() {
                        shared.notify();
                    }
                }),
            }
        });
        *shared.reader.lock().unwrap() = Some(
            io.background_reader(shared.wake.clone())
                .map_err(|reason| reason.message)?,
        );
        let mut admission = self.0.lock().unwrap();
        admission
            .readers
            .retain(|reader| reader.strong_count() != 0);
        admission.readers.push(Arc::downgrade(&shared));
        // Reserve one waiting read per admitted client before decoder work.
        let extra = admission.readers.len() - admission.waiters.len();
        admission.waiters.reserve(extra);
        drop(admission);
        Ok((
            StreamControl(shared.clone()),
            AssetStream {
                shared,
                input,
                events: Vec::with_capacity(1),
                position: 0,
                length: None,
                chunk,
            },
        ))
    }
    pub(super) fn close(&mut self) {
        let readers = std::mem::take(&mut self.0.lock().unwrap().readers);
        for shared in readers.into_iter().filter_map(|reader| reader.upgrade()) {
            StreamControl(shared).cancel();
        }
    }
}

#[cfg(all(test, not(target_os = "vita")))]
mod tests {
    use super::*;
    use reel_core::budget::{BudgetCategory, QueueLimits};
    use std::time::{Duration, Instant};

    fn reader(deadline: ReadDeadline, input: &BudgetPool) -> Arc<Shared> {
        Arc::new(Shared {
            deadline,
            input: input.clone(),
            state: Mutex::new(State {
                source: AssetSource {
                    key: AssetKey("stream".into()),
                    version: None,
                },
                completion: None,
                closed: false,
            }),
            reader: Mutex::new(None),
            admission: Default::default(),
            ready: Condvar::new(),
            wake: WakeSignal::new(|| {}),
        })
    }
    fn queue(admission: &mut Admission, reader: &Arc<Shared>, len: u32) {
        admission.waiters.push_back(Waiter {
            reader: Arc::downgrade(reader),
            len,
            bypasses: 0,
        });
    }
    #[test]
    fn fitting_deadlines_bypass_large_reads_without_starving_them() {
        let input = BudgetPool::new(
            BudgetCategory::CpuInput,
            QueueLimits {
                max_requests: 8,
                max_bytes: 12,
            },
        );
        let occupied = input.reserve(8).unwrap();
        let now = Instant::now();
        let large = reader(ReadDeadline::at(now), &input);
        let late = reader(ReadDeadline::at(now + Duration::from_secs(2)), &input);
        let urgent = reader(ReadDeadline::at(now + Duration::from_secs(1)), &input);
        let mut admission = Admission::default();
        queue(&mut admission, &large, 8);
        queue(&mut admission, &late, 4);
        queue(&mut admission, &urgent, 4);
        for bypass in 0..4 {
            let (index, selected) = admission.next(input.available_bytes()).unwrap();
            assert!(Arc::ptr_eq(&selected, &urgent));
            admission.admitted(index);
            assert_eq!(admission.waiters[0].bypasses, bypass + 1);
            queue(&mut admission, &urgent, 4);
        }
        assert!(admission.next(input.available_bytes()).is_none());
        drop(occupied);
        let (index, selected) = admission.next(input.available_bytes()).unwrap();
        assert!(Arc::ptr_eq(&selected, &large));
        admission.admitted(index);
        urgent.deadline.pause(true);
        assert!(Arc::ptr_eq(
            &admission.next(input.available_bytes()).unwrap().1,
            &late
        ));
        drop(late);
        assert!(Arc::ptr_eq(
            &admission.next(input.available_bytes()).unwrap().1,
            &urgent
        ));
    }
}
