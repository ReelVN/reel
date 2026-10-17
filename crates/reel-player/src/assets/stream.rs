//! Blocking Read/Seek views for decoder workers. Only the worker waits; the
//! shared resource service continues to own and poll its ordinary AssetIo lane.
use reel_core::{
    budget::OwnedBytes,
    identity::{AssetKey, AssetSource, IoId, Priority, Scope},
    io::{DescribeRequest, IoPayload, ReadRequest},
    result::Terminal,
    wake::WakeSignal,
};
use std::{
    io::{self, Read, Seek, SeekFrom},
    sync::{Arc, Condvar, Mutex, Weak},
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
    ready: Condvar,
    wake: WakeSignal,
}

/// Cancelling wakes a decoder even while its IO completion is still in flight.
#[derive(Clone)]
pub struct StreamControl(Arc<Shared>);
impl StreamControl {
    pub fn cancel(&self) {
        self.0.state.lock().unwrap().closed = true;
        self.0.ready.notify_all();
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
                io::ErrorKind::Interrupted,
                "asset stream closed",
            ));
        }
        state.request = Some(request);
        self.shared.wake.wake();
        loop {
            if state.closed {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "asset stream closed",
                ));
            }
            if let Some(result) = state.response.take() {
                return result.map_err(io::Error::other);
            }
            state = self.shared.ready.wait(state).unwrap();
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
    ) -> (StreamControl, AssetStream) {
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
            ready: Condvar::new(),
            wake,
        });
        self.0.push(Arc::downgrade(&shared));
        (
            StreamControl(shared.clone()),
            AssetStream {
                shared,
                position: 0,
                length: None,
                chunk,
            },
        )
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
            shared.ready.notify_one();
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
                        priority: Priority::Visible,
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
                        priority: Priority::Visible,
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
                    shared.ready.notify_one();
                }
            }
        }
    }
}
