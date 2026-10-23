//! Owned asset results reuse the shared result/wait and immutable byte storage.
use super::{AdapterError, HostResponse, HostTicket, SessionVm, results, text};
use crate::assets::{AssetData, AssetLoader};
use aside_vm::{HeapReservation, RootKind, RootToken, Value, ValueTag};
use reel_core::identity::ContentVersion;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub(super) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{named, nullable};
    api.refine(
        "reel::assets::read",
        named("reel::types::AssetRead"),
        &[
            ("path", TypeHint::String),
            (
                "owner",
                nullable(TypeHint::Union(vec![
                    named("reel::types::Node"),
                    named("reel::types::Scope"),
                ])),
            ),
        ],
    );
}

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Read a logical resource through the same loose or packaged asset service
        /// as images and media. Return an awaitable AssetRead; awaiting it yields
        /// immutable Bytes. Repeated awaits share the completed bytes or failure.
        /// `owner` defaults to the current scope or callback owner. Retiring it
        /// cancels pending IO and copying. Cancelling one waiter leaves the shared
        /// request running. Existing resource and VM budgets govern admission.
        /// Snapshots retain completed bytes and restart pending reads; known content
        /// versions remain pinned. Failures are raised at the author's await.
        "reel::assets::read";
    }
}

struct Pending {
    root: RootToken,
    load: Option<u64>,
    input: Option<AssetData>,
    offset: usize,
    version: Option<[u8; 32]>,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    root: RootToken,
    version: Option<[u8; 32]>,
}

#[derive(Default)]
pub(super) struct AssetReads {
    loader: Option<Box<dyn AssetLoader>>,
    pending: VecDeque<Pending>,
    charge: Option<HeapReservation>,
}

impl AssetReads {
    pub fn active(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn snapshot_records(&self) -> super::snapshot::RecordStream<'_> {
        Box::new(self.pending.iter().map(|pending| {
            super::snapshot::Record::new(
                super::snapshot::Part::AssetRead,
                &Saved {
                    root: pending.root,
                    version: pending.version,
                },
            )
        }))
    }

    pub fn restore_record(
        &mut self,
        record: &super::snapshot::Record,
        map: &aside_vm::snapshot::SnapshotMap,
        heap: &mut aside_vm::Heap,
    ) -> Result<(), AdapterError> {
        let saved: Saved = record.read()?;
        let root = map.root(saved.root)?;
        let result = heap.root_value(root)?;
        results::asset_path(heap, result)?;
        // A captured unpublished partial buffer belongs to the old IO attempt.
        // The result keeps its identity; rereading its pinned source starts fresh.
        results::attach(heap, result, Value::NULL)?;
        results::reserve_queue(heap, &mut self.pending, &mut self.charge)?;
        self.pending.push_back(Pending {
            root,
            load: None,
            input: None,
            offset: 0,
            version: saved.version,
        });
        Ok(())
    }

    pub fn close(&mut self) {
        if let Some(loader) = &mut self.loader {
            for pending in &self.pending {
                if let Some(load) = pending.load {
                    loader.cancel(load);
                }
            }
        }
        self.pending.clear();
        self.charge = None;
        self.loader = None;
    }
}

impl Drop for AssetReads {
    fn drop(&mut self) {
        self.close();
    }
}

impl SessionVm {
    /// Install the host's shared asset adapter before driving source or restored
    /// requests. Replacing an adapter with submitted IO would lose its ownership.
    pub fn set_asset_loader(&mut self, loader: Box<dyn AssetLoader>) -> Result<(), AdapterError> {
        if self
            .asset_reads
            .pending
            .iter()
            .any(|pending| pending.load.is_some())
        {
            return Err(AdapterError::Input("asset reads already submitted"));
        }
        self.asset_reads.loader = Some(loader);
        Ok(())
    }

    pub fn asset_reads_pending(&self) -> bool {
        !self.revoked && self.asset_reads.active()
    }
    /// In-flight IO sleeps on the asset worker's wake signal. Only submission
    /// and already-returned buffers need another bounded main-thread turn.
    pub fn asset_reads_need_work(&self) -> bool {
        !self.revoked
            && self
                .asset_reads
                .pending
                .iter()
                .any(|pending| pending.load.is_none())
    }

    pub(super) fn dispatch_asset_read(&mut self, ticket: HostTicket) -> Result<(), AdapterError> {
        let args = self.vm.host_arguments(ticket)?;
        let path = args[0];
        if self.vm.heap().info(path)?.kind != aside_vm::ObjectKind::String {
            return Err(AdapterError::Input("asset path requires string"));
        }
        let owner = if args[1].tag() == ValueTag::Null {
            self.default_activity_owner(ticket.task)?
        } else {
            args[1]
        };
        if owner.tag() != ValueTag::Null {
            super::scopes::validate_owner(self.vm.heap(), &self.stage, owner)?;
        }
        results::reserve_queue(
            self.vm.heap(),
            &mut self.asset_reads.pending,
            &mut self.asset_reads.charge,
        )?;
        let result = self
            .results
            .create_asset_read(&mut self.vm, owner, path, self.read_limit)?;
        let root = self
            .vm
            .heap_mut()
            .add_root(self.root_owner, RootKind::Host, result)?;
        self.asset_reads.pending.push_back(Pending {
            root,
            load: None,
            input: None,
            offset: 0,
            version: None,
        });
        self.vm.respond(ticket, HostResponse::Ready(result))?;
        self.issued = None;
        Ok(())
    }

    /// Poll bounded producer work and copy at most one configured VM mutation
    /// chunk per work unit. Return true after progress that may wake waiters.
    pub fn maintain_asset_reads(&mut self, work: u32) -> Result<bool, AdapterError> {
        let mut changed = false;
        if work != 0
            && !self.asset_reads.pending.is_empty()
            && let Some(loader) = &mut self.asset_reads.loader
        {
            loader.maintain();
        }
        for _ in 0..(work as usize).min(self.asset_reads.pending.len()) {
            let mut pending = self.asset_reads.pending.pop_front().unwrap();
            let result = self.vm.heap().root_value(pending.root)?;
            let mut keep = results::pending(self.vm.heap_mut(), result)?;
            if keep {
                let step = self.asset_read_step(&mut pending, result);
                match step {
                    Ok(done) => {
                        keep = !done;
                        changed |= done || pending.input.is_some();
                    }
                    Err(error) => {
                        let fallback = results::asset_error(self.vm.heap(), result)?;
                        let message = text::alloc_string(
                            self.vm.heap_mut(),
                            &error.to_string(),
                            self.read_limit,
                        )
                        .unwrap_or(fallback);
                        results::finish_task(
                            self.vm.heap_mut(),
                            result,
                            results::TaskResult::Rejected(message),
                        )?;
                        keep = false;
                        changed = true;
                    }
                }
            } else {
                changed = true;
            }
            if keep {
                self.asset_reads.pending.push_back(pending);
            } else {
                if let Some(load) = pending.load
                    && let Some(loader) = &mut self.asset_reads.loader
                {
                    loader.cancel(load);
                }
                results::attach(self.vm.heap_mut(), result, Value::NULL)?;
                self.vm.heap_mut().release_root(pending.root)?;
                self.results.changed();
            }
        }
        if self.asset_reads.pending.is_empty() {
            self.asset_reads.pending = VecDeque::new();
            self.asset_reads.charge = None;
        }
        Ok(changed)
    }

    fn asset_read_step(
        &mut self,
        pending: &mut Pending,
        result: Value,
    ) -> Result<bool, AdapterError> {
        if pending.input.is_none() {
            if pending.load.is_none() {
                let path = text::read_string(
                    self.vm.heap(),
                    results::asset_path(self.vm.heap(), result)?,
                    self.read_limit,
                )?;
                let loader = self
                    .asset_reads
                    .loader
                    .as_mut()
                    .ok_or(AdapterError::Input("asset loader is not installed"))?;
                pending.load = Some(
                    loader
                        .read(&path, pending.version.map(ContentVersion))
                        .map_err(AdapterError::Asset)?,
                );
            }
            let loader = self.asset_reads.loader.as_mut().unwrap();
            let Some(input) = loader.poll(pending.load.unwrap()) else {
                return Ok(false);
            };
            pending.load = None;
            let input = input.map_err(AdapterError::Asset)?;
            u32::try_from(input.bytes.as_slice().len())
                .map_err(|_| AdapterError::Input("asset exceeds immutable byte representation"))?;
            pending.version = Some(input.version.0);
            pending.input = Some(input);
            let builder = self.vm.heap_mut().alloc_byte_buffer(&[])?;
            results::attach(self.vm.heap_mut(), result, builder)?;
        }
        let bytes = pending.input.as_ref().unwrap().bytes.as_slice();
        let builder = results::attached(self.vm.heap(), result)?;
        let end = bytes
            .len()
            .min(pending.offset.saturating_add(self.read_limit));
        self.vm
            .heap_mut()
            .bytes_append(builder, &bytes[pending.offset..end])?;
        pending.offset = end;
        if end != bytes.len() {
            return Ok(false);
        }
        self.vm.heap_mut().seal_bytes(builder)?;
        results::finish_task(
            self.vm.heap_mut(),
            result,
            results::TaskResult::Returned(builder),
        )?;
        Ok(true)
    }
}
