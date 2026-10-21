//! Session requests; the host owns storage and commits restored candidates.
use super::{AdapterError, HostResponse, HostTicket, SessionVm, observable, results, text};
use super::{NativeDispatch, data};
use aside_vm::{HeapReservation, RootKind, RootToken, Value, ValueTag};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;

pub(super) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{Member, named, nullable};
    let operation = named("reel::types::Operation");
    api.refine(
        "reel::saves::save",
        operation.clone(),
        &[
            ("slot", TypeHint::String),
            ("title", nullable(TypeHint::String)),
            ("thumbnail", nullable(named("Bytes"))),
        ],
    );
    for method in ["load", "delete", "thumbnail"] {
        api.refine(
            &format!("reel::saves::{method}"),
            operation.clone(),
            &[("slot", TypeHint::String)],
        );
    }
    api.refine("reel::saves::list", operation.clone(), &[]);
    for method in ["read", "write", "delete"] {
        api.refine(
            &format!("reel::persistent::{method}"),
            operation.clone(),
            &[("key", TypeHint::String)],
        );
    }
    let descriptor = api
        .types
        .iter_mut()
        .find(|ty| ty.name == "reel::types::Operation")
        .expect("registered save operation type");
    for (name, ty, documentation) in [
        (
            "status",
            TypeHint::String,
            "The storage request's current status: pending, succeeded or failed.",
        ),
        (
            "error",
            nullable(TypeHint::String),
            "The storage failure message, or null before failure.",
        ),
        (
            "restored",
            TypeHint::Bool,
            "Whether this save operation resumed from its committed story snapshot.",
        ),
    ] {
        descriptor.members.push(Member::field(
            &descriptor.name,
            name,
            ty,
            None,
            documentation,
            None,
        ));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveRequestId {
    pub session: crate::SessionIdentity,
    pub serial: u64,
}

#[derive(Clone, Debug)]
pub enum SaveOperation {
    Save {
        slot: String,
        title: String,
        thumbnail: Option<Arc<data::DataDocument>>,
    },
    Load {
        slot: String,
    },
    Delete {
        slot: String,
    },
    List,
    Thumbnail {
        slot: String,
    },
    ReadData {
        key: String,
    },
    WriteData {
        key: String,
        data: Arc<data::DataDocument>,
    },
    DeleteData {
        key: String,
    },
}
#[derive(Clone, Debug)]
pub struct SaveRequest {
    pub id: SaveRequestId,
    pub operation: SaveOperation,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveSlot {
    pub id: String,
    pub title: String,
    /// UTC Unix seconds, assigned by storage when committing the slot.
    pub saved_at: u64,
    pub has_thumbnail: bool,
}
pub enum SaveResult {
    Done,
    Slots(Vec<SaveSlot>),
    Failed(String),
    Data(Option<Arc<data::DataDocument>>),
    Value(Value),
}

#[derive(Serialize, Deserialize)]
struct SavedRequest {
    serial: u64,
    root: RootToken,
    fallback: Option<RootToken>,
}
struct Pending {
    saved: SavedRequest,
    operation: Option<SaveOperation>,
    _charge: Option<HeapReservation>,
}
#[derive(Default)]
pub(super) struct Saves {
    serial: u64,
    pending: VecDeque<Pending>,
    charge: Option<HeapReservation>,
    pub(super) capture: Option<(HostTicket, data::Capture)>,
    restores: VecDeque<(SaveRequestId, data::Restore)>,
    restore_charge: Option<HeapReservation>,
}
impl Saves {
    pub fn active(&self) -> bool {
        !self.pending.is_empty()
    }
    pub fn clear(&mut self) {
        // Session owner retirement releases producer roots.
        self.pending.clear();
        self.pending.shrink_to_fit();
        self.charge = None;
        self.capture = None;
        self.restores.clear();
        self.restores.shrink_to_fit();
        self.restore_charge = None;
    }
    pub fn snapshot_records(&self) -> super::snapshot::RecordStream<'_> {
        use super::snapshot::{Part, Record};
        Box::new(
            std::iter::once_with(|| Record::new(Part::Saves, &self.serial)).chain(
                self.pending
                    .iter()
                    .map(|p| Record::new(Part::SaveRequest, &p.saved)),
            ),
        )
    }
}

fn complete(
    vm: &mut aside_vm::executor::Vm,
    root: RootToken,
    result: SaveResult,
    read_limit: usize,
    restored: bool,
    fallback: Option<RootToken>,
) -> Result<(), AdapterError> {
    let heap = vm.heap_mut();
    let object = heap.root_value(root)?;
    let (status, error, result) = match result {
        SaveResult::Done => (
            "succeeded",
            Value::NULL,
            results::TaskResult::Returned(Value::NULL),
        ),
        SaveResult::Value(value) => (
            "succeeded",
            Value::NULL,
            results::TaskResult::Returned(value),
        ),
        SaveResult::Data(None) => (
            "succeeded",
            Value::NULL,
            results::TaskResult::Returned(
                fallback
                    .map(|root| heap.root_value(root))
                    .transpose()?
                    .unwrap_or(Value::NULL),
            ),
        ),
        SaveResult::Data(Some(_)) => unreachable!("data graph is restored before settlement"),
        SaveResult::Failed(error) => {
            let error = text::alloc_string(heap, &error, read_limit)?;
            ("failed", error, results::TaskResult::Rejected(error))
        }
        SaveResult::Slots(slots) => {
            let array = observable::array(heap)?;
            for slot in slots {
                let id = text::alloc_string(heap, &slot.id, read_limit)?;
                let title = text::alloc_string(heap, &slot.title, read_limit)?;
                let record = observable::record(
                    heap,
                    &[
                        ("id", id),
                        ("title", title),
                        ("saved_at", Value::int(slot.saved_at as i64)),
                        ("has_thumbnail", Value::bool(slot.has_thumbnail)),
                    ],
                    &[],
                )?;
                heap.array_push(array, record)?;
            }
            (
                "succeeded",
                Value::NULL,
                results::TaskResult::Returned(array),
            )
        }
    };
    let status = heap.alloc_string(status)?;
    observable::publish(heap, object, 0, status)?;
    observable::publish(heap, object, 1, error)?;
    observable::publish(heap, object, 2, Value::bool(restored))?;
    results::finish_task(heap, object, result)?;
    heap.release_root(root)?;
    if let Some(root) = fallback {
        heap.release_root(root)?;
    }
    Ok(())
}

impl SessionVm {
    pub fn has_save_requests(&self) -> bool {
        !self.revoked && self.saves.pending.iter().any(|p| p.operation.is_some())
    }
    pub(super) fn dispatch_save(
        &mut self,
        ticket: HostTicket,
        symbol: &str,
        work: u32,
    ) -> Result<NativeDispatch, AdapterError> {
        let args = self.vm.host_arguments(ticket)?;
        let mut charge = None;
        let mut used = 1;
        let operation = if symbol == "reel::saves::list" {
            SaveOperation::List
        } else {
            let slot = text::read_string(self.vm.heap(), args[0], self.read_limit)?;
            if slot.is_empty() {
                return Err(AdapterError::Input("save slot must not be empty"));
            }
            match symbol {
                "reel::saves::save" => {
                    let title = if args[1].tag() == ValueTag::Null {
                        String::new()
                    } else {
                        text::read_string(self.vm.heap(), args[1], self.read_limit)?
                    };
                    let thumbnail = if args[2].tag() == ValueTag::Null {
                        None
                    } else {
                        if self.vm.heap().info(args[2])?.kind != aside_vm::ObjectKind::Bytes {
                            return Err(AdapterError::Input("save thumbnail requires PNG Bytes"));
                        }
                        let mut capture = match self.saves.capture.take() {
                            Some((pending, capture)) => {
                                debug_assert_eq!(pending, ticket);
                                capture
                            }
                            None => data::Capture::new(self.vm.heap(), args[2])?,
                        };
                        let (complete, spent) =
                            capture.step(self.vm.heap(), work, self.read_limit)?;
                        used = spent;
                        if !complete {
                            self.saves.capture = Some((ticket, capture));
                            return Ok(NativeDispatch::Decoding { work: used });
                        }
                        let (data, reservation) = capture.finish();
                        charge = Some(reservation);
                        Some(data)
                    };
                    SaveOperation::Save {
                        slot,
                        title,
                        thumbnail,
                    }
                }
                "reel::saves::load" => SaveOperation::Load { slot },
                "reel::saves::delete" => SaveOperation::Delete { slot },
                "reel::saves::thumbnail" => SaveOperation::Thumbnail { slot },
                _ => unreachable!(),
            }
        };
        self.enqueue_save(ticket, operation, None, charge)?;
        Ok(NativeDispatch::Ready { work: used })
    }

    pub(super) fn dispatch_persistent(
        &mut self,
        ticket: HostTicket,
        symbol: &str,
        work: u32,
    ) -> Result<NativeDispatch, AdapterError> {
        let args = self.vm.host_arguments(ticket)?;
        let key = text::read_string(self.vm.heap(), args[0], self.read_limit)?;
        if key.is_empty() {
            return Err(AdapterError::Input("persistent key must not be empty"));
        }
        let mut used = 1;
        let (operation, fallback, charge) = match symbol {
            "reel::persistent::read" => (SaveOperation::ReadData { key }, Some(args[1]), None),
            "reel::persistent::delete" => (SaveOperation::DeleteData { key }, None, None),
            "reel::persistent::write" => {
                let mut capture = match self.saves.capture.take() {
                    Some((pending, capture)) => {
                        debug_assert_eq!(pending, ticket);
                        capture
                    }
                    None => data::Capture::new(self.vm.heap(), args[1])?,
                };
                let (complete, spent) = capture.step(self.vm.heap(), work, self.read_limit)?;
                used = spent;
                if !complete {
                    self.saves.capture = Some((ticket, capture));
                    return Ok(NativeDispatch::Decoding { work: used });
                }
                let (data, charge) = capture.finish();
                (SaveOperation::WriteData { key, data }, None, Some(charge))
            }
            _ => unreachable!(),
        };
        self.enqueue_save(ticket, operation, fallback, charge)?;
        Ok(NativeDispatch::Ready { work: used })
    }

    fn enqueue_save(
        &mut self,
        ticket: HostTicket,
        operation: SaveOperation,
        fallback: Option<Value>,
        charge: Option<HeapReservation>,
    ) -> Result<(), AdapterError> {
        let serial = self
            .saves
            .serial
            .checked_add(1)
            .ok_or(AdapterError::Input("save request identity exhausted"))?;
        results::reserve_queue(
            self.vm.heap(),
            &mut self.saves.pending,
            &mut self.saves.charge,
        )?;
        let object = self.results.create_operation(&mut self.vm)?;
        let heap = self.vm.heap_mut();
        let fields = heap.host_fields(object)?;
        let status = heap.alloc_string("pending")?;
        for (i, (key, value)) in [
            ("status", status),
            ("error", Value::NULL),
            ("restored", Value::bool(false)),
        ]
        .into_iter()
        .enumerate()
        {
            let key = heap.alloc_string(key)?;
            heap.table_store(fields, i as u32, key, value)?;
        }
        heap.observe_host_properties(object, 0, 3)?;
        let root = heap.add_root(self.root_owner, RootKind::Host, object)?;
        let fallback = fallback
            .map(|value| heap.add_root(self.root_owner, RootKind::Host, value))
            .transpose()?;
        self.saves.serial = serial;
        self.saves.pending.push_back(Pending {
            saved: SavedRequest {
                serial,
                root,
                fallback,
            },
            operation: Some(operation),
            _charge: charge,
        });
        self.vm.respond(ticket, HostResponse::Ready(object))?;
        self.issued = None;
        Ok(())
    }

    /// Claim once, in submission order. Claimed writes belong to the host service.
    pub fn take_save_request(&mut self) -> Option<SaveRequest> {
        if self.revoked {
            return None;
        }
        let pending = self
            .saves
            .pending
            .iter_mut()
            .find(|p| p.operation.is_some())?;
        Some(SaveRequest {
            id: SaveRequestId {
                session: self.identity,
                serial: pending.saved.serial,
            },
            operation: pending.operation.take().unwrap(),
        })
    }

    /// Returns false for an old session or a result already delivered.
    pub fn complete_save_request(
        &mut self,
        id: SaveRequestId,
        result: SaveResult,
    ) -> Result<bool, AdapterError> {
        if self.revoked || id.session != self.identity {
            return Ok(false);
        }
        let Some(index) = self
            .saves
            .pending
            .iter()
            .position(|p| p.saved.serial == id.serial)
        else {
            return Ok(false);
        };
        let result = if let SaveResult::Data(Some(data)) = result {
            let prepared = results::reserve_queue(
                self.vm.heap(),
                &mut self.saves.restores,
                &mut self.saves.restore_charge,
            )
            .and_then(|()| data::Restore::new(self.vm.heap_mut(), self.root_owner, data));
            match prepared {
                Ok(restore) => {
                    self.saves.restores.push_back((id, restore));
                    return Ok(true);
                }
                Err(error) => SaveResult::Failed(error.to_string()),
            }
        } else {
            result
        };
        complete(
            &mut self.vm,
            self.saves.pending[index].saved.root,
            result,
            self.read_limit,
            false,
            self.saves.pending[index].saved.fallback,
        )?;
        self.saves.pending.remove(index);
        self.results.changed();
        if self.saves.pending.is_empty() {
            self.saves.pending.shrink_to_fit();
            self.saves.charge = None;
        }
        Ok(true)
    }

    pub fn save_results_pending(&self) -> bool {
        !self.saves.restores.is_empty()
    }

    /// Rebuild a read result under the same work budget as other VM adapters.
    pub fn maintain_save_results(&mut self, work: u32) -> Result<(), AdapterError> {
        let Some((id, mut restore)) = self.saves.restores.pop_front() else {
            return Ok(());
        };
        match restore.step(self.vm.heap_mut(), work, self.read_limit) {
            Ok(false) => {
                self.saves.restores.push_front((id, restore));
            }
            result => {
                let value = match result {
                    Ok(true) => restore.value_root(self.vm.heap_mut(), self.root_owner),
                    Err(error) => Err(error),
                    Ok(false) => unreachable!(),
                };
                restore.release(self.vm.heap_mut())?;
                match value {
                    Ok(root) => {
                        let value = self.vm.heap().root_value(root)?;
                        let result = self.complete_save_request(id, SaveResult::Value(value));
                        self.vm.heap_mut().release_root(root)?;
                        result?;
                    }
                    Err(error) => {
                        self.complete_save_request(id, SaveResult::Failed(error.to_string()))?;
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn restore_save_record(
        &mut self,
        record: &super::snapshot::Record,
        map: &aside_vm::snapshot::SnapshotMap,
        committed: Option<u64>,
    ) -> Result<(), AdapterError> {
        if matches!(record.part, super::snapshot::Part::Saves) {
            self.saves.serial = record.read()?;
        } else {
            let saved: SavedRequest = record.read()?;
            let result = if committed == Some(saved.serial) {
                SaveResult::Done
            } else {
                SaveResult::Failed("OperationInterrupted".into())
            };
            complete(
                &mut self.vm,
                map.root(saved.root)?,
                result,
                self.read_limit,
                committed == Some(saved.serial),
                saved.fallback.map(|root| map.root(root)).transpose()?,
            )?;
            self.results.changed();
        }
        Ok(())
    }
}

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Save the current session to a nonempty slot identifier.
        /// `title` defaults to an empty string; `thumbnail` is null or PNG Bytes, commonly
        /// obtained from `stage.capture`. Returns an awaitable operation with reactive
        /// `status`, `error` and `restored` fields. Awaiting yields null on success and
        /// throws on storage failure. After loading this saved continuation, `restored`
        /// identifies the resumed save operation. Host-claimed writes are session operations
        /// and continue independently of the requesting UI's node lifetime.
        "reel::saves::save";
        /// Request restoration of a nonempty save slot and return an awaitable operation.
        /// A successful load replaces the running session with the saved continuation;
        /// code after this request does not continue in the old session. Failure rejects
        /// the operation and remains catchable in the requesting session.
        "reel::saves::load";
        /// Delete a nonempty save slot identifier and return a session-owned operation.
        /// Awaiting yields null after deletion or throws on storage failure.
        /// This affects story save slots rather than persistent key/value records.
        "reel::saves::delete";
        /// Return a session-owned awaitable operation listing available save slots.
        /// Awaiting yields records with `id`, `title`, `saved_at` (UTC Unix seconds)
        /// and `has_thumbnail`. Storage failures reject the operation.
        "reel::saves::list";
        /// Read a nonempty save slot's thumbnail through a session-owned operation.
        /// Awaiting yields the PNG Bytes or null when no thumbnail is available.
        /// The bytes can be used as an image source; storage failures throw.
        "reel::saves::thumbnail";
    }
}
