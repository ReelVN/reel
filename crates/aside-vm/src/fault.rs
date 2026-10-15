//! Private same-Heap fault graph. Handles are borrowed identities; roots retain it.
use crate::bytecode::*;
use crate::executor::{CodeRef, ContinuationState, FaultKind, FrameId, Location, ModuleInstanceId};
use crate::payload::Payload;
use crate::root_ranges::{ChargedVec, Reservation};
use crate::*;
use std::rc::Rc;

/// Stable built-in registry. Future adapters must explicitly register other registries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FaultCode {
    pub registry_version: u64,
    pub kind: FaultKind,
}
impl FaultCode {
    pub const fn core(kind: FaultKind) -> Self {
        Self {
            registry_version: 1,
            kind,
        }
    }
}
/// Complete envelope identity, including heap, epoch and full slot generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct FaultId(pub(crate) HeapHandle);
impl FaultId {
    pub fn as_value(self) -> Value {
        Value::object(self.0.heap, self.0.key)
    }
}
#[derive(Clone, Copy, Debug)]
pub struct NewFault {
    pub value: Value,
    pub code: FaultCode,
    pub cause: Option<FaultId>,
}
impl NewFault {
    pub fn thrown(value: Value) -> Self {
        Self {
            value,
            code: FaultCode::core(FaultKind::Thrown),
            cause: None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum StackFrameKind {
    Script,
    Native {
        native: NativeId,
        abi_version: u32,
        continuation: ContinuationState,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StackEntry {
    pub frame: FrameId,
    pub location: Location,
    pub module: ModuleInstanceId,
    pub module_version: u64,
    pub source_version: u64,
    pub kind: StackFrameKind,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CloseKind {
    Generator,
    Thread,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ReplacedExit {
    Throw(FaultId),
    Return,
    Leave {
        code: CodeRef,
        target: Pc,
    },
    Close {
        kind: CloseKind,
        logical_id: u64,
        activation: u64,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CauseKind {
    CaughtFault,
    ExplicitCause,
    ReplacedExit(ReplacedExit),
    AwaitedFault { activation: u64 },
}
#[derive(Clone, Copy, Debug)]
pub struct CauseInfo {
    pub event: u64,
    pub kind: CauseKind,
    pub location: Location,
    pub fault: Option<FaultId>,
    pub value: Value,
}
/// An ephemeral read-only view. Store as_value() in a registered root/Value graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaultInfo {
    pub id: FaultId,
    pub kind: FaultKind,
    pub location: Location,
    pub code: FaultCode,
}
impl FaultInfo {
    pub fn as_value(self) -> Value {
        self.id.as_value()
    }
}
/// Owning host retention, independent of Task lifetime. Release explicitly.
#[must_use = "release the fault root explicitly through Heap::release_fault_root"]
pub struct FaultRoot {
    pub(crate) token: RootToken,
    pub(crate) id: FaultId,
}
impl FaultRoot {
    pub fn id(&self) -> FaultId {
        self.id
    }
    pub fn info(&self, heap: &Heap) -> Result<FaultInfo, HeapError> {
        heap.root_value(self.token)?;
        heap.fault_info(self.id)
    }
}

// The reservation follows immutable code through every envelope/stack/cause node.
pub(crate) struct CodeLease {
    pub program: VerifiedProgram,
    pub charge: Reservation,
    pub checks: Rc<crate::type_checks::TypeChecks>,
}
impl std::ops::Deref for CodeLease {
    type Target = VerifiedProgram;
    fn deref(&self) -> &VerifiedProgram {
        &self.program
    }
}
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(crate) enum FaultData {
    Envelope {
        code: FaultCode,
        origin: Location,
        complete: bool,
    },
    Stack(StackEntry),
    Cause {
        event: u64,
        kind: CauseKind,
        location: Location,
    },
    Error(FaultCode),
}
pub(crate) const FAULT_NODE_BYTES: u64 = std::mem::size_of::<FaultPayload>() as u64;
pub(crate) struct FaultPayload {
    pub data: FaultData,
    pub edges: [Value; 3],
    pub release_cursor: u32,
    pub code: Rc<CodeLease>,
}
impl Heap {
    fn fault_payload(&self, value: Value) -> Result<&FaultPayload, HeapError> {
        let Payload::Fault(p) = &self.object(self.key(value)?)?.payload else {
            return Err(HeapError::WrongKind);
        };
        Ok(p)
    }
    pub(crate) fn alloc_fault_node(
        &mut self,
        kind: ObjectKind,
        data: FaultData,
        edges: [Value; 3],
        code: Rc<CodeLease>,
    ) -> Result<Value, HeapError> {
        self.ensure_object_slot()?;
        for edge in edges {
            self.root_insertion_barrier(edge)?;
        }
        Ok(self.allocate(
            kind,
            Payload::Fault(FaultPayload {
                data,
                edges,
                release_cursor: 0,
                code,
            }),
            None,
            0,
        ))
    }
    pub(crate) fn fault_edge(
        &mut self,
        node: Value,
        index: usize,
        value: Value,
    ) -> Result<(), HeapError> {
        self.root_insertion_barrier(value)?;
        let key = self.key(node)?;
        let Payload::Fault(p) = &mut self
            .objects
            .get_mut(key)
            .ok_or(HeapError::InvalidHandle)?
            .payload
        else {
            return Err(HeapError::WrongKind);
        };
        p.edges[index] = value;
        Ok(())
    }
    pub(crate) fn finish_fault(&mut self, value: Value) -> Result<FaultId, HeapError> {
        let key = self.key(value)?;
        let Payload::Fault(p) = &mut self
            .objects
            .get_mut(key)
            .ok_or(HeapError::InvalidHandle)?
            .payload
        else {
            return Err(HeapError::WrongKind);
        };
        let FaultData::Envelope { complete, .. } = &mut p.data else {
            return Err(HeapError::WrongKind);
        };
        *complete = true;
        Ok(FaultId(self.handle(value)?))
    }
    pub fn fault_id(&self, value: Value) -> Result<FaultId, HeapError> {
        let p = self.fault_payload(value)?;
        if !matches!(p.data, FaultData::Envelope { complete: true, .. }) {
            return Err(HeapError::WrongKind);
        };
        Ok(FaultId(self.handle(value)?))
    }
    pub fn fault_info(&self, id: FaultId) -> Result<FaultInfo, HeapError> {
        let value = self.import_handle(id.0)?;
        let p = self.fault_payload(value)?;
        let FaultData::Envelope {
            code,
            origin,
            complete: true,
        } = p.data
        else {
            return Err(HeapError::WrongKind);
        };
        Ok(FaultInfo {
            id,
            kind: code.kind,
            location: origin,
            code,
        })
    }
    pub fn fault_value(&self, id: FaultId) -> Result<Value, HeapError> {
        self.fault_info(id)?;
        Ok(self.fault_payload(id.as_value())?.edges[0])
    }
    pub fn fault_stack(&self, id: FaultId) -> Result<FaultStackCursor, HeapError> {
        self.fault_info(id)?;
        Ok(FaultStackCursor {
            next: self.fault_payload(id.as_value())?.edges[1],
        })
    }
    pub fn fault_causes(&self, id: FaultId) -> Result<FaultCauseCursor, HeapError> {
        self.fault_info(id)?;
        Ok(FaultCauseCursor {
            next: self.fault_payload(id.as_value())?.edges[2],
        })
    }
    pub fn core_error_info(&self, value: Value) -> Result<CoreErrorInfo, HeapError> {
        let p = self.fault_payload(value)?;
        let FaultData::Error(code) = p.data else {
            return Err(HeapError::WrongKind);
        };
        Ok(CoreErrorInfo {
            code,
            message: code.kind.message(),
            details: p.edges[0],
        })
    }
    pub fn retain_fault(&mut self, owner: RootOwner, id: FaultId) -> Result<FaultRoot, HeapError> {
        self.fault_info(id)?;
        let token = self.add_root(owner, RootKind::Host, id.as_value())?;
        Ok(FaultRoot { token, id })
    }
    pub fn release_fault_root(&mut self, root: FaultRoot) -> Result<(), HeapError> {
        self.release_root(root.token)?;
        Ok(())
    }
    pub(crate) fn append_fault_cause(
        &mut self,
        id: FaultId,
        cause: CauseInfo,
        code: Rc<CodeLease>,
    ) -> Result<(), HeapError> {
        let CauseInfo {
            event,
            kind,
            location,
            fault,
            value,
        } = cause;
        let p = self.fault_payload(id.as_value())?;
        if !matches!(p.data, FaultData::Envelope { .. }) {
            return Err(HeapError::WrongKind);
        };
        if let Some(f) = fault {
            self.fault_info(f)?;
        }
        let next = self.fault_payload(id.as_value())?.edges[2];
        let node = self.alloc_fault_node(
            ObjectKind::FaultCause,
            FaultData::Cause {
                event,
                kind,
                location,
            },
            [next, fault.map_or(value, FaultId::as_value), Value::NULL],
            code,
        )?;
        self.fault_edge(id.as_value(), 2, node)
    }
    pub(crate) fn unfinished_stack(&self, value: Value) -> Result<Value, HeapError> {
        Ok(self.fault_payload(value)?.edges[1])
    }
    pub fn fault_program(&self, id: FaultId) -> Result<&VerifiedProgram, HeapError> {
        self.fault_info(id)?;
        Ok(&self.fault_payload(id.as_value())?.code.program)
    }
}
#[derive(Clone, Copy, Debug)]
pub struct CoreErrorInfo {
    pub code: FaultCode,
    pub message: &'static str,
    pub details: Value,
}
#[derive(Clone, Copy, Debug)]
pub struct FaultStackCursor {
    next: Value,
}
impl FaultStackCursor {
    pub fn next(&mut self, heap: &Heap) -> Result<Option<StackEntry>, HeapError> {
        if self.next.tag() == ValueTag::Null {
            return Ok(None);
        };
        let p = heap.fault_payload(self.next)?;
        let FaultData::Stack(entry) = p.data else {
            return Err(HeapError::WrongKind);
        };
        self.next = p.edges[0];
        Ok(Some(entry))
    }
}
#[derive(Clone, Copy, Debug)]
pub struct FaultCauseCursor {
    next: Value,
}
impl FaultCauseCursor {
    pub fn next(&mut self, heap: &Heap) -> Result<Option<CauseInfo>, HeapError> {
        if self.next.tag() == ValueTag::Null {
            return Ok(None);
        };
        let p = heap.fault_payload(self.next)?;
        let FaultData::Cause {
            event,
            kind,
            location,
        } = p.data
        else {
            return Err(HeapError::WrongKind);
        };
        let linked = matches!(
            kind,
            CauseKind::CaughtFault
                | CauseKind::ExplicitCause
                | CauseKind::AwaitedFault { .. }
                | CauseKind::ReplacedExit(ReplacedExit::Throw(_))
        );
        let fault = if linked {
            Some(heap.fault_id(p.edges[1])?)
        } else {
            None
        };
        let value = if let Some(f) = fault {
            heap.fault_value(f)?
        } else {
            p.edges[1]
        };
        self.next = p.edges[0];
        Ok(Some(CauseInfo {
            event,
            kind,
            location,
            fault,
            value,
        }))
    }
}
/// Incremental traversal results. Repeated identities are explicit reference edges.
#[derive(Clone, Copy, Debug)]
pub enum FaultGraphStep {
    Yield {
        work: u32,
    },
    Item {
        info: FaultInfo,
        repeated: bool,
        work: u32,
    },
    Complete {
        work: u32,
    },
}
#[derive(Clone, Copy)]
enum GraphPhase {
    Pick,
    Seen {
        id: FaultId,
        index: usize,
    },
    Causes {
        id: FaultId,
        cursor: FaultCauseCursor,
    },
    Emit {
        id: FaultId,
        repeated: bool,
    },
}
/// Roots the entry graph; both visited IDs and queued edges share the Heap budget.
#[must_use = "release the graph cursor's host root explicitly"]
pub struct FaultGraphCursor {
    root: FaultRoot,
    seen: ChargedVec<FaultId>,
    pending: ChargedVec<FaultId>,
    capacity: usize,
    phase: GraphPhase,
    _control_charge: Reservation,
}
impl FaultGraphCursor {
    pub fn new(
        heap: &mut Heap,
        owner: RootOwner,
        id: FaultId,
        capacity: usize,
    ) -> Result<Self, HeapError> {
        if capacity == 0 {
            return Err(HeapError::InvalidLimits);
        }
        let usage = heap.budget_usage();
        let control_charge = Reservation::new(&usage, std::mem::size_of::<Self>() as u64)?;
        let seen = ChargedVec::new(&usage, capacity)?;
        let mut pending = ChargedVec::new(&usage, capacity)?;
        let root = heap.retain_fault(owner, id)?;
        pending.data.push(id);
        Ok(Self {
            root,
            seen,
            pending,
            capacity,
            phase: GraphPhase::Pick,
            _control_charge: control_charge,
        })
    }
    pub fn step(&mut self, heap: &Heap, budget: u32) -> Result<FaultGraphStep, HeapError> {
        for work in 1..=budget {
            match self.phase {
                GraphPhase::Pick => {
                    let Some(id) = self.pending.data.pop() else {
                        return Ok(FaultGraphStep::Complete { work });
                    };
                    self.phase = GraphPhase::Seen { id, index: 0 };
                }
                GraphPhase::Seen { id, index } => {
                    if index == self.seen.data.len() {
                        if index == self.capacity {
                            return Err(HeapError::OperationLimit {
                                requested: index as u64 + 1,
                                limit: self.capacity as u64,
                            });
                        };
                        self.seen.data.push(id);
                        self.phase = GraphPhase::Causes {
                            id,
                            cursor: heap.fault_causes(id)?,
                        };
                    } else if self.seen.data[index] == id {
                        self.phase = GraphPhase::Emit { id, repeated: true };
                    } else {
                        self.phase = GraphPhase::Seen {
                            id,
                            index: index + 1,
                        };
                    }
                }
                GraphPhase::Causes { id, mut cursor } => {
                    let next = cursor.next(heap)?;
                    if let Some(cause) = next {
                        if let Some(fault) = cause.fault {
                            if self.pending.data.len() == self.capacity {
                                return Err(HeapError::OperationLimit {
                                    requested: self.capacity as u64 + 1,
                                    limit: self.capacity as u64,
                                });
                            };
                            self.pending.data.push(fault);
                        }
                        self.phase = GraphPhase::Causes { id, cursor };
                    } else {
                        self.phase = GraphPhase::Emit {
                            id,
                            repeated: false,
                        };
                    }
                }
                GraphPhase::Emit { id, repeated } => {
                    self.phase = GraphPhase::Pick;
                    return Ok(FaultGraphStep::Item {
                        info: heap.fault_info(id)?,
                        repeated,
                        work,
                    });
                }
            }
        }
        Ok(FaultGraphStep::Yield { work: budget })
    }
    pub fn release(self, heap: &mut Heap) -> Result<(), HeapError> {
        heap.release_fault_root(self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cyclic_cause_diagnostics_are_incremental_and_last_code_reference_is_released() {
        let program = verify(fixtures::exception_regions(), Limits::default()).unwrap();
        let mut heap = Heap::new(SessionEpoch(7), HeapLimits::default()).unwrap();
        let lease = Rc::new(CodeLease {
            checks: Rc::new(
                crate::type_checks::TypeChecks::new(program.package(), &heap.budget_usage())
                    .unwrap(),
            ),
            charge: Reservation::new(
                &heap.budget_usage(),
                program.stats().owned_capacity_bytes as u64,
            )
            .unwrap(),
            program,
        });
        let weak = Rc::downgrade(&lease);
        let location = Location {
            code: CodeRef {
                function: FunctionId(0),
                version: 1,
            },
            pc: Pc(1),
            source: None,
        };
        let value = heap
            .alloc_fault_node(
                ObjectKind::FaultEnvelope,
                FaultData::Envelope {
                    code: FaultCode::core(FaultKind::Thrown),
                    origin: location,
                    complete: false,
                },
                [Value::int(41), Value::NULL, Value::NULL],
                lease.clone(),
            )
            .unwrap();
        let id = heap.finish_fault(value).unwrap();
        let root = heap.retain_fault(RootOwner(1), id).unwrap();
        heap.append_fault_cause(
            id,
            CauseInfo {
                event: 91,
                kind: CauseKind::ReplacedExit(ReplacedExit::Throw(id)),
                location,
                fault: Some(id),
                value: Value::NULL,
            },
            lease.clone(),
        )
        .unwrap();
        drop(lease);
        assert!(weak.upgrade().is_some());
        let mut cursor = FaultGraphCursor::new(&mut heap, RootOwner(1), id, 4).unwrap();
        let mut items = Vec::new();
        loop {
            match cursor.step(&heap, 1).unwrap() {
                FaultGraphStep::Yield { work } => assert_eq!(work, 1),
                FaultGraphStep::Item {
                    info,
                    repeated,
                    work,
                } => {
                    assert_eq!(info.id, id);
                    assert_eq!(work, 1);
                    items.push(repeated)
                }
                FaultGraphStep::Complete { .. } => break,
            }
        }
        assert_eq!(items, [false, true]);
        cursor.release(&mut heap).unwrap();
        heap.release_fault_root(root).unwrap();
        heap.begin_collection().unwrap();
        while heap.phase() != GcPhase::Idle {
            heap.gc_step(GcBudget {
                units: 1,
                ..GcBudget::default()
            })
            .unwrap();
        }
        assert!(heap.fault_info(id).is_err());
        assert!(weak.upgrade().is_none());
    }
}
