//! Shared manual-completion host; each reader drains only its own terminals.
use reel_core::{
    identity::{ContentVersion, IoId},
    io::{AssetIo, DescribeRequest, IoCaps, IoEvent, IoPayload, ReadRequest, SaveWrite},
    result::{RejectCode, RejectReason, Rejected, Terminal},
    test_support::MemoryAssetIo,
    wake::WakeSignal,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

struct Clients {
    wakes: BTreeMap<u64, WakeSignal>,
    next: u64,
    closed: bool,
}
pub(crate) struct MemoryReader {
    io: Arc<Mutex<MemoryAssetIo>>,
    clients: Arc<Mutex<Clients>>,
    client: u64,
    owned: BTreeSet<IoId>,
    auto_complete: bool,
    description: Option<Option<ContentVersion>>,
}
impl MemoryReader {
    pub(crate) fn new(
        mut io: MemoryAssetIo,
        wake: WakeSignal,
        auto_complete: bool,
        description: Option<Option<ContentVersion>>,
    ) -> Self {
        let clients = Arc::new(Mutex::new(Clients {
            wakes: BTreeMap::from([(0, wake)]),
            next: 1,
            closed: false,
        }));
        let target = clients.clone();
        io.set_waker(WakeSignal::new(move || notify(&target)));
        Self {
            io: Arc::new(Mutex::new(io)),
            clients,
            client: 0,
            owned: BTreeSet::new(),
            auto_complete,
            description,
        }
    }
    pub(crate) fn control(&self) -> Arc<Mutex<MemoryAssetIo>> {
        self.io.clone()
    }
    fn accepted(&mut self, id: IoId) -> IoId {
        self.owned.insert(id);
        id
    }
    fn check<R>(&self, request: R) -> Result<R, Rejected<R>> {
        if self.clients.lock().unwrap().closed {
            Err(Rejected {
                request,
                reason: RejectReason::new(RejectCode::Unavailable, "memory IO closed"),
            })
        } else {
            Ok(request)
        }
    }
}
fn notify(clients: &Mutex<Clients>) {
    let wakes = clients
        .lock()
        .unwrap()
        .wakes
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for wake in wakes {
        wake.wake();
    }
}
impl AssetIo for MemoryReader {
    fn background_reader(&self, wake: WakeSignal) -> Result<Box<dyn AssetIo + Send>, RejectReason> {
        let max = self
            .io
            .lock()
            .unwrap()
            .capabilities()
            .read_queue
            .max_requests as usize;
        let mut clients = self.clients.lock().unwrap();
        if clients.closed {
            return Err(RejectReason::new(
                RejectCode::Unavailable,
                "memory IO closed",
            ));
        }
        if clients.wakes.len() > max {
            return Err(RejectReason::new(RejectCode::Busy, "memory reader limit"));
        }
        let client = clients.next;
        clients.next += 1;
        clients.wakes.insert(client, wake);
        Ok(Box::new(Self {
            io: self.io.clone(),
            clients: self.clients.clone(),
            client,
            owned: BTreeSet::new(),
            auto_complete: self.auto_complete,
            description: self.description,
        }))
    }
    fn capabilities(&self) -> IoCaps {
        self.io.lock().unwrap().capabilities()
    }
    fn describe(&mut self, request: DescribeRequest) -> Result<IoId, Rejected<DescribeRequest>> {
        let request = self.check(request)?;
        let mut io = self.io.lock().unwrap();
        let id = io.describe(request)?;
        if self.auto_complete {
            assert!(io.complete_next_read());
        }
        drop(io);
        Ok(self.accepted(id))
    }
    fn read(&mut self, request: ReadRequest) -> Result<IoId, Rejected<ReadRequest>> {
        let request = self.check(request)?;
        let mut io = self.io.lock().unwrap();
        let id = io.read(request)?;
        if self.auto_complete {
            assert!(io.complete_next_read());
        }
        drop(io);
        Ok(self.accepted(id))
    }
    fn replace_save(&mut self, request: SaveWrite) -> Result<IoId, Rejected<SaveWrite>> {
        let request = self.check(request)?;
        let id = self.io.lock().unwrap().replace_save(request)?;
        Ok(self.accepted(id))
    }
    fn promote_visible(&mut self, id: IoId) {
        if self.owned.contains(&id) {
            self.io.lock().unwrap().promote_visible(id);
        }
    }
    fn cancel(&mut self, id: IoId) {
        if self.owned.contains(&id) {
            self.io.lock().unwrap().cancel(id);
        }
    }
    fn poll(&mut self, out: &mut Vec<IoEvent>, limit: usize) {
        let first = out.len();
        self.io
            .lock()
            .unwrap()
            .poll_selected(out, limit, |id| self.owned.contains(&id));
        for event in &mut out[first..] {
            self.owned.remove(&event.id);
            if let Some(version) = self.description
                && let Terminal::Completed(IoPayload::Described(info)) = &mut event.result
            {
                info.version = version;
            }
        }
        if out.len() != first {
            let wakes = self
                .clients
                .lock()
                .unwrap()
                .wakes
                .iter()
                .filter(|(client, _)| **client != self.client)
                .map(|(_, wake)| wake.clone())
                .collect::<Vec<_>>();
            for wake in wakes {
                wake.wake();
            }
        }
    }
}
impl Drop for MemoryReader {
    fn drop(&mut self) {
        {
            let mut clients = self.clients.lock().unwrap();
            if self.client == 0 {
                clients.closed = true;
            }
            clients.wakes.remove(&self.client);
        }
        let mut io = self.io.lock().unwrap();
        for id in &self.owned {
            io.cancel(*id);
        }
        let mut events = Vec::new();
        io.poll_selected(&mut events, usize::MAX, |id| self.owned.contains(&id));
        drop(io);
        drop(events);
        notify(&self.clients);
    }
}
