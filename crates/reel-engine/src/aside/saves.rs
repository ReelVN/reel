//! Session requests; the host owns storage and commits restored candidates.
use super::{AdapterError, HostResponse, HostTicket, SessionVm, observable, results, text};
use aside_vm::{HeapReservation, RootKind, RootToken, Value, ValueTag};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveRequestId {
    pub session: crate::SessionIdentity,
    pub serial: u64,
}

#[derive(Clone, Debug)]
pub enum SaveOperation {
    Save { slot: String, title: String },
    Load { slot: String },
    Delete { slot: String },
    List,
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
}

#[derive(Serialize, Deserialize)]
struct SavedRequest {
    serial: u64,
    root: RootToken,
}
struct Pending {
    saved: SavedRequest,
    operation: Option<SaveOperation>,
}
#[derive(Default)]
pub(super) struct Saves {
    serial: u64,
    pending: VecDeque<Pending>,
    charge: Option<HeapReservation>,
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
) -> Result<(), AdapterError> {
    let heap = vm.heap_mut();
    let object = heap.root_value(root)?;
    let (status, error, result) = match result {
        SaveResult::Done => (
            "succeeded",
            Value::NULL,
            results::TaskResult::Returned(Value::NULL),
        ),
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
        self.saves.serial = serial;
        self.saves.pending.push_back(Pending {
            saved: SavedRequest { serial, root },
            operation: Some(operation),
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
        complete(
            &mut self.vm,
            self.saves.pending[index].saved.root,
            result,
            self.read_limit,
            false,
        )?;
        self.saves.pending.remove(index);
        self.results.changed();
        if self.saves.pending.is_empty() {
            self.saves.clear();
        }
        Ok(true)
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
            )?;
            self.results.changed();
        }
        Ok(())
    }
}
