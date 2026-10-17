//! Frozen, incremental VM checkpoints. Files and device resources belong to the host.
use crate::executor::{TaskId, WaitId};
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub(crate) const VERSION: u32 = 4;
const MAGIC: &[u8; 8] = b"ASIDSN01";

#[derive(Debug)]
pub enum SnapshotError {
    Heap(HeapError),
    Vm(executor::VmError),
    Encoding(postcard::Error),
    Invalid(String),
    Busy(&'static str),
    Version {
        format: &'static str,
        expected: u32,
        actual: u32,
    },
    MissingAdapter {
        object: u64,
    },
    AdapterVersion {
        name: String,
        version: u32,
    },
    CodeMismatch,
    Limit,
}
impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Version {
                format,
                expected,
                actual,
            } => write!(
                f,
                "unsupported {format} snapshot version {actual} (expected {expected})"
            ),
            _ => write!(f, "{self:?}"),
        }
    }
}
impl std::error::Error for SnapshotError {}
impl From<HeapError> for SnapshotError {
    fn from(e: HeapError) -> Self {
        Self::Heap(e)
    }
}
impl From<executor::VmError> for SnapshotError {
    fn from(e: executor::VmError) -> Self {
        Self::Vm(e)
    }
}
impl From<postcard::Error> for SnapshotError {
    fn from(e: postcard::Error) -> Self {
        Self::Encoding(e)
    }
}

/// Names and versions are saved; function addresses never enter a checkpoint.
#[derive(Default)]
pub struct SnapshotRegistry {
    validators: Vec<(&'static str, u32, HostPropertyValidator)>,
}
impl SnapshotRegistry {
    pub fn register_validator(
        &mut self,
        name: &'static str,
        version: u32,
        validator: HostPropertyValidator,
    ) -> Result<(), SnapshotError> {
        if self.validators.iter().any(|&(n, _, _)| n == name) {
            return Err(SnapshotError::Invalid(format!(
                "duplicate snapshot adapter {name}"
            )));
        }
        self.validators.push((name, version, validator));
        Ok(())
    }
    pub(crate) fn identify(
        &self,
        key: u64,
        validator: HostPropertyValidator,
    ) -> Result<(String, u32), SnapshotError> {
        self.validators
            .iter()
            .find(|&&(_, _, f)| std::ptr::fn_addr_eq(f, validator))
            .map(|&(name, version, _)| (name.into(), version))
            .ok_or(SnapshotError::MissingAdapter { object: key })
    }
    pub(crate) fn resolve(
        &self,
        name: &str,
        version: u32,
    ) -> Result<HostPropertyValidator, SnapshotError> {
        self.validators
            .iter()
            .find(|&&(n, v, _)| n == name && v == version)
            .map(|&(_, _, f)| f)
            .ok_or_else(|| SnapshotError::AdapterVersion {
                name: name.into(),
                version,
            })
    }
}

/// A unit is one metadata record, root slot, or payload block (64 values / 1024 bytes).
/// No script executes while a capture borrows its VM.
#[derive(Clone, Copy, Debug)]
pub struct SnapshotBudget {
    pub units: u32,
}
impl Default for SnapshotBudget {
    fn default() -> Self {
        Self { units: 64 }
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct SnapshotProgress {
    pub units: u32,
    pub bytes: u64,
    pub complete: bool,
}

/// Detached records contain no process handles and may be encoded/written on a worker.
#[derive(Serialize)]
pub struct VmSnapshot {
    pub(crate) version: u32,
    pub(crate) code: [u8; 32],
    pub(crate) source_heap: u32,
    pub(crate) source_epoch: u64,
    pub(crate) records: Vec<Record>,
    pub(crate) bytes: u64,
}
impl<'de> Deserialize<'de> for VmSnapshot {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Image {
            version: u32,
            code: [u8; 32],
            source_heap: u32,
            source_epoch: u64,
            records: Vec<Record>,
            bytes: u64,
        }
        let image = Image::deserialize(d)?;
        let actual = image.records.iter().try_fold(0u64, |total, record| {
            total.checked_add(record.data.len() as u64)
        });
        if image.version != VERSION {
            return Err(serde::de::Error::custom(SnapshotError::Version {
                format: "VM",
                expected: VERSION,
                actual: image.version,
            }));
        }
        if actual != Some(image.bytes) {
            return Err(serde::de::Error::custom("snapshot record length"));
        }
        Ok(Self {
            version: image.version,
            code: image.code,
            source_heap: image.source_heap,
            source_epoch: image.source_epoch,
            records: image.records,
            bytes: image.bytes,
        })
    }
}
impl VmSnapshot {
    /// Version of the VM records, also checked by embedding snapshot containers.
    pub const FORMAT_VERSION: u32 = VERSION;

    pub fn recorded_bytes(&self) -> u64 {
        self.bytes
    }
    pub fn record_count(&self) -> usize {
        self.records.len()
    }
    pub fn encode(&self) -> Result<Vec<u8>, SnapshotError> {
        Ok(postcard::to_extend(self, MAGIC.to_vec())?)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let bytes = bytes
            .strip_prefix(MAGIC)
            .ok_or_else(|| SnapshotError::Invalid("snapshot magic".into()))?;
        let (version, _): (u32, _) = postcard::take_from_bytes(bytes)?;
        if version != VERSION {
            return Err(SnapshotError::Version {
                format: "VM",
                expected: VERSION,
                actual: version,
            });
        }
        let (image, tail): (Self, _) = postcard::take_from_bytes(bytes)?;
        if !tail.is_empty() {
            return Err(SnapshotError::Invalid("snapshot format".into()));
        }
        Ok(image)
    }
    pub(crate) fn push(&mut self, record: Record) {
        self.bytes += record.data.len() as u64;
        self.records.push(record);
    }
}
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordKind {
    Vm,
    Task,
    Frame,
    Unwind,
    Caught,
    Equality,
    Root,
    Range,
    Slots,
    Object,
    Values,
    Bytes,
    ModelCopy,
    CopyObject,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct Record {
    pub kind: RecordKind,
    pub key: u64,
    pub offset: u32,
    pub data: Vec<u8>,
}
impl Record {
    pub fn new<T: Serialize>(
        kind: RecordKind,
        key: u64,
        offset: u32,
        data: &T,
    ) -> Result<Self, SnapshotError> {
        Ok(Self {
            kind,
            key,
            offset,
            data: postcard::to_allocvec(data)?,
        })
    }
    pub fn read<T: for<'a> Deserialize<'a>>(&self) -> Result<T, SnapshotError> {
        let (value, tail) = postcard::take_from_bytes(&self.data)?;
        if !tail.is_empty() {
            return Err(SnapshotError::Invalid("trailing record data".into()));
        }
        Ok(value)
    }
}

/// Explicit old-to-new identity map, also used by versioned host adapters.
/// It is valid only for the candidate restored VM, never an active session switch.
pub struct SnapshotMap {
    pub(crate) heap: HeapId,
    pub(crate) epoch: SessionEpoch,
    pub(crate) source_heap: u32,
    pub(crate) source_epoch: u64,
    pub(crate) objects: HashMap<u64, Value>,
    pub(crate) roots: HashMap<u64, RootToken>,
    pub(crate) ranges: HashMap<u64, RootRangeToken>,
}
impl SnapshotMap {
    pub(crate) fn new(heap: &Heap, image: &VmSnapshot) -> Self {
        Self {
            heap: heap.id(),
            epoch: heap.epoch(),
            source_heap: image.source_heap,
            source_epoch: image.source_epoch,
            objects: HashMap::new(),
            roots: HashMap::new(),
            ranges: HashMap::new(),
        }
    }
    pub(crate) fn check(
        &self,
        heap: HeapId,
        epoch: Option<SessionEpoch>,
    ) -> Result<(), SnapshotError> {
        if heap.get() != 0
            && (heap.get() != self.source_heap || epoch.is_some_and(|e| e.0 != self.source_epoch))
        {
            return Err(SnapshotError::Invalid(
                "identity from another snapshot".into(),
            ));
        }
        Ok(())
    }
    pub fn value(&self, value: Value) -> Result<Value, SnapshotError> {
        let Some((heap, key)) = value.object_parts() else {
            return Ok(value);
        };
        self.check(heap, None)?;
        self.objects
            .get(&key)
            .copied()
            .ok_or_else(|| SnapshotError::Invalid(format!("missing object {key}")))
    }
    pub fn handle(&self, handle: HeapHandle) -> Result<HeapHandle, SnapshotError> {
        self.check(handle.heap, Some(handle.epoch))?;
        let value = self.value(Value::object(handle.heap, handle.key))?;
        Ok(HeapHandle {
            heap: self.heap,
            epoch: self.epoch,
            key: value.object_parts().unwrap().1,
            _thread: std::marker::PhantomData,
        })
    }
    pub fn root(&self, root: RootToken) -> Result<RootToken, SnapshotError> {
        self.rebase_root(root)
    }
    pub fn range(&self, range: RootRangeToken) -> Result<RootRangeToken, SnapshotError> {
        self.rebase_range(range)
    }
    pub fn task(&self, task: TaskId) -> Result<TaskId, SnapshotError> {
        self.rebase_task(task)
    }
    pub fn wait(&self, wait: WaitId) -> Result<WaitId, SnapshotError> {
        self.rebase_wait(wait)
    }
}

pub use crate::executor::snapshot::{VmCapture, VmRestore};
