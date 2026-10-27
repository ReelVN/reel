//! Blocking Read/Seek views for decoder workers and native media callbacks.
//! The shared resource service owns and polls its ordinary AssetIo lane.
#[cfg(target_os = "vita")]
use pvr_psp2_sys::platform::wake::Wake;
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, AssetSource, IoId, Priority, Scope},
    io::{DescribeRequest, IoPayload, ReadRequest},
    result::Terminal,
    wake::WakeSignal,
};
#[cfg(not(target_os = "vita"))]
use std::sync::Condvar;
use std::{
    io::{self, Read, Seek, SeekFrom},
    sync::{Arc, Mutex, MutexGuard, Weak},
};

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
    request: Option<Request>,
    active: Option<IoId>,
    response: Option<Result<Response, String>>,
    closed: bool,
}
struct Shared {
    state: Mutex<State>,
    #[cfg(not(target_os = "vita"))]
    ready: Condvar,
    #[cfg(target_os = "vita")]
    ready: Arc<Wake>,
    wake: WakeSignal,
}
impl Shared {
    fn notify(&self) {
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
            // AvPlayer calls Read on an SDK thread without pthread cancellation
            // state. The kernel event retains notifications across this unlock;
            // the caller rechecks the response/closed predicate after each wake.
            drop(state);
            self.ready.wait(None).map_err(io::Error::other)?;
            Ok(self.state.lock().unwrap())
        }
    }
}

/// Cancelling wakes a decoder even while its IO completion is still in flight.
#[derive(Clone)]
pub struct StreamControl(Arc<Shared>);
impl StreamControl {
    /// Fixed source identity becomes available after the worker's first describe.
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
        self.0.state.lock().unwrap().closed = true;
        self.0.notify();
        self.0.wake.wake();
    }
}

pub struct AssetStream {
    shared: Arc<Shared>,
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
    fn transact(&mut self, request: Request) -> io::Result<Response> {
        let mut state = self.shared.state.lock().unwrap();
        if state.closed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "asset stream closed",
            ));
        }
        state.request = Some(request);
        self.shared.wake.wake();
        loop {
            if state.closed {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "asset stream closed",
                ));
            }
            if let Some(result) = state.response.take() {
                return result.map_err(io::Error::other);
            }
            state = self.shared.wait(state)?;
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
        Ok(count)
    }
}
impl Seek for AssetStream {
    fn seek(&mut self, offset: SeekFrom) -> io::Result<u64> {
        let length = match self.length {
            Some(n) => n,
            None => self.open()?,
        };
        let position = match offset {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(n) => self.position.checked_add_signed(n),
            SeekFrom::End(n) => length.checked_add_signed(n),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid asset seek"))?;
        self.position = position;
        Ok(position)
    }
}
impl Drop for AssetStream {
    fn drop(&mut self) {
        StreamControl(self.shared.clone()).cancel();
    }
}

#[derive(Default)]
pub(super) struct Streams(Vec<Weak<Shared>>);
impl Streams {
    pub(super) fn create(
        &mut self,
        source: String,
        wake: WakeSignal,
        chunk: u32,
    ) -> Result<(StreamControl, AssetStream), String> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                source: AssetSource {
                    key: AssetKey(source),
                    version: None,
                },
                request: None,
                active: None,
                response: None,
                closed: false,
            }),
            #[cfg(not(target_os = "vita"))]
            ready: Condvar::new(),
            #[cfg(target_os = "vita")]
            ready: Wake::new().map_err(|error| error.to_string())?,
            wake,
        });
        self.0.push(Arc::downgrade(&shared));
        Ok((
            StreamControl(shared.clone()),
            AssetStream {
                shared,
                position: 0,
                length: None,
                chunk,
            },
        ))
    }
    pub(super) fn pending(&self) -> bool {
        self.0.iter().filter_map(Weak::upgrade).any(|s| {
            let state = s.state.lock().unwrap();
            state.request.is_some() || state.active.is_some()
        })
    }
    pub(super) fn close(&mut self) {
        for shared in self.0.drain(..).filter_map(|s| s.upgrade()) {
            StreamControl(shared).cancel();
        }
    }
    pub(super) fn deliver(&mut self, event: &mut reel_core::io::IoEvent) -> bool {
        for shared in self.0.iter().filter_map(Weak::upgrade) {
            let mut state = shared.state.lock().unwrap();
            if state.active != Some(event.id) {
                continue;
            }
            state.active = None;
            let result = std::mem::replace(&mut event.result, Terminal::Cancelled);
            state.response = Some(match result {
                Terminal::Completed(IoPayload::Described(info)) => {
                    if !info.range_read || !info.seekable || info.version.is_none() {
                        Err("media requires a seekable, versioned asset source".into())
                    } else {
                        state.source.version = info.version;
                        info.length
                            .map(Response::Info)
                            .ok_or_else(|| "media length unavailable".into())
                    }
                }
                Terminal::Completed(IoPayload::Read(read))
                    if state.source.version == Some(read.version) =>
                {
                    Ok(Response::Bytes(read.bytes))
                }
                Terminal::Completed(_) => Err("unexpected asset stream response or version".into()),
                Terminal::Failed(e) => Err(e.message),
                Terminal::Cancelled => Err("asset stream cancelled".into()),
            });
            shared.notify();
            return true;
        }
        false
    }
    pub(super) fn submit(
        &mut self,
        io: &mut dyn reel_core::io::AssetIo,
        input: &reel_core::budget::BudgetPool,
        completion: &reel_core::budget::BudgetPool,
    ) {
        self.0.retain(|s| s.strong_count() > 0);
        for shared in self.0.iter().filter_map(Weak::upgrade) {
            let mut state = shared.state.lock().unwrap();
            if state.closed {
                if let Some(id) = state.active.take() {
                    io.cancel(id);
                }
                state.request = None;
                state.response = None;
                continue;
            }
            let Some(request) = state.request.take() else {
                continue;
            };
            let ticket = match completion.reserve(512) {
                Ok(ticket) => ticket,
                Err(_) => {
                    state.request = Some(request);
                    continue;
                }
            };
            let result = match request {
                Request::Describe => io
                    .describe(DescribeRequest {
                        resource: state.source.clone(),
                        scope: Scope::Process,
                        priority: Priority::Realtime,
                        completion: ticket,
                    })
                    .map_err(|e| e.reason),
                Request::Read { offset, len } => {
                    let output = match input.reserve(u64::from(len)) {
                        Ok(ticket) => ticket,
                        Err(_) => {
                            state.request = Some(request);
                            continue;
                        }
                    };
                    io.read(ReadRequest {
                        resource: state.source.clone(),
                        offset,
                        len,
                        scope: Scope::Process,
                        priority: Priority::Realtime,
                        output,
                        completion: ticket,
                    })
                    .map_err(|e| e.reason)
                }
            };
            match result {
                Ok(id) => state.active = Some(id),
                Err(reason) if reason.retryable() => state.request = Some(request),
                Err(reason) => {
                    state.response = Some(Err(reason.message));
                    shared.notify();
                }
            }
        }
        if !self.0.is_empty() {
            // Tiny input allowances may fit one chunk; rotate admission so the
            // same stream cannot consume that sole reservation every host tick.
            self.0.rotate_left(1);
        }
    }
}
