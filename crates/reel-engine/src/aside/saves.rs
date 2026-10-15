//! Session requests; the host owns storage and commits restored candidates.
use super::{AdapterError, HostResponse, HostTicket, SessionVm, observable, results, text};
use super::{NativeDispatch, data};
use aside_vm::{HeapReservation, RootKind, RootToken, Value, ValueTag};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;

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
    },
    Load {
        slot: String,
    },
    Delete {
        slot: String,
    },
    List,
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
    ) -> Result<(), AdapterError> {
        let args = self.vm.host_arguments(ticket)?;
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
                    SaveOperation::Save { slot, title }
                }
                "reel::saves::load" => SaveOperation::Load { slot },
                "reel::saves::delete" => SaveOperation::Delete { slot },
                _ => unreachable!(),
            }
        };
        self.enqueue_save(ticket, operation, None, None)
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
