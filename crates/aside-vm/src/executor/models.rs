//! Resumable data graph copying, owned by the calling native activation.
use super::*;
use std::collections::HashMap;

#[derive(Clone, Copy)]
struct CopyObject {
    source: Value,
    target: Value,
    kind: ObjectKind,
    version: u64,
    index: u32,
}

pub(super) struct ModelCopy {
    pub(super) frame: FrameId,
    roots: Value,
    result: Value,
    objects: Vec<CopyObject>,
    identities: HashMap<crate::HeapHandle, usize>,
    cursor: usize,
    validating: bool,
    charge: u64,
    usage: HeapBudgetUsage,
}

impl Drop for ModelCopy {
    fn drop(&mut self) {
        drop(std::mem::take(&mut self.objects));
        drop(std::mem::take(&mut self.identities));
        self.usage.refund(self.charge);
    }
}

impl ModelCopy {
    fn new(heap: &mut Heap, frame: FrameId, defaults: Value) -> Result<Self, HeapError> {
        let mut copy = Self {
            frame,
            roots: heap.alloc_array(&[])?,
            result: Value::NULL,
            objects: Vec::new(),
            identities: HashMap::new(),
            cursor: 0,
            validating: false,
            charge: 0,
            usage: heap.usage.clone(),
        };
        copy.result = copy.copy_value(heap, defaults)?;
        Ok(copy)
    }

    fn reserve(&mut self) -> Result<(), HeapError> {
        if self.objects.len() < self.objects.capacity()
            && self.identities.len() < self.identities.capacity()
        {
            return Ok(());
        }
        let count = (self.objects.len() * 2).max(8);
        // Include both the old and replacement hash allocation during growth.
        let stride = size_of::<CopyObject>() + 4 * size_of::<(crate::HeapHandle, usize)>() + 32;
        self.usage.ensure((count * stride + 64) as u64)?;
        self.objects
            .try_reserve_exact(count - self.objects.len())
            .map_err(|_| HeapError::AllocationFailure)?;
        let map_result = self.identities.try_reserve(count - self.identities.len());
        // std's hash table load factor and control bytes fit within two slots per entry.
        let charge = (self.objects.capacity() * size_of::<CopyObject>()
            + 2 * (self.identities.capacity() + 1) * (size_of::<(crate::HeapHandle, usize)>() + 1)
            + 64) as u64;
        self.usage.charge(charge - self.charge);
        self.charge = charge;
        map_result.map_err(|_| HeapError::AllocationFailure)
    }

    fn copy_value(&mut self, heap: &mut Heap, value: Value) -> Result<Value, HeapError> {
        if value.tag() != crate::ValueTag::Object {
            return Ok(value);
        }
        let info = heap.info(value)?;
        if matches!(info.kind, ObjectKind::String | ObjectKind::Bytes) {
            return Ok(value);
        }
        if info.kind == ObjectKind::Host && heap.is_immutable_host(value)? {
            return Ok(value);
        }
        let identity = heap.handle(value)?;
        if let Some(&index) = self.identities.get(&identity) {
            return Ok(self.objects[index].target);
        }
        self.reserve()?;
        let target = match info.kind {
            ObjectKind::Table => heap.alloc_table()?,
            ObjectKind::Array | ObjectKind::Tuple => heap.alloc_array(&[])?,
            ObjectKind::TextBuffer => heap.alloc_text_buffer("")?,
            ObjectKind::ByteBuffer => heap.alloc_byte_buffer(&[])?,
            _ => return Err(HeapError::WrongKind),
        };
        heap.array_push(self.roots, value)?;
        heap.array_push(self.roots, target)?;
        self.identities.insert(identity, self.objects.len());
        self.objects.push(CopyObject {
            source: value,
            target,
            kind: info.kind,
            version: info.content_version,
            index: 0,
        });
        Ok(target)
    }

    fn step(&mut self, heap: &mut Heap) -> Result<bool, HeapError> {
        if self.cursor == self.objects.len() {
            if self.validating {
                return Ok(true);
            }
            self.validating = true;
            self.cursor = 0;
            return Ok(false);
        }
        let mut item = self.objects[self.cursor];
        let info = heap.info(item.source)?;
        if info.content_version != item.version {
            return Err(HeapError::ConcurrentModification);
        }
        if self.validating {
            heap.observe_model_object(item.target)?;
            self.cursor += 1;
            return Ok(false);
        }
        if item.index == info.len {
            if item.kind == ObjectKind::Tuple {
                heap.seal_tuple(item.target)?;
            }
            self.cursor += 1;
            return Ok(false);
        }
        match item.kind {
            ObjectKind::Table => {
                let (key, value) = heap.table_entry(item.source, item.index)?;
                let key = self.copy_value(heap, key)?;
                let value = self.copy_value(heap, value)?;
                heap.table_store(item.target, item.index, key, value)?;
                item.index += 1;
            }
            ObjectKind::Array | ObjectKind::Tuple => {
                let value = heap.sequence_get(item.source, item.index)?;
                let value = self.copy_value(heap, value)?;
                heap.array_push(item.target, value)?;
                item.index += 1;
            }
            ObjectKind::TextBuffer | ObjectKind::ByteBuffer => {
                let mut bytes = [0; crate::BYTE_CHUNK_LEN];
                let segment = heap.byte_segment(item.source, item.index)?;
                let len = segment.len().min(bytes.len());
                bytes[..len].copy_from_slice(&segment[..len]);
                heap.append_buffer(item.target, &bytes[..len], item.kind)?;
                item.index += len as u32;
            }
            _ => unreachable!("copy_value admits data objects"),
        }
        self.objects[self.cursor] = item;
        Ok(false)
    }
}

impl Vm {
    /// Implement a native `model(defaults)` using its existing NativeFrame and
    /// continuation roots. `work` bounds copied edges/chunks and validation steps.
    /// The host keeps this synchronous task selected until completion (GC may run).
    /// Cancellation releases both roots and the charged identity map with the frame.
    pub fn respond_model(&mut self, ticket: HostTicket, work: u32) -> Result<u32, VmError> {
        self.select_task(ticket.task)?;
        let native = self.ticket(ticket)?;
        let result = (|| -> Result<(u32, bool), HeapError> {
            if matches!(ticket.input, NativeInput::Entry) {
                let defaults = self.slots.read(at(native.bound, 0))?;
                self.active.model_copy = Some(ModelCopy::new(&mut self.heap, native.id, defaults)?);
            }
            let copy = self
                .active
                .model_copy
                .as_mut()
                .ok_or(HeapError::WrongKind)?;
            let mut used = 0;
            while used < work {
                used += 1;
                if copy.step(&mut self.heap)? {
                    return Ok((used, true));
                }
            }
            Ok((used, false))
        })();
        match result {
            Ok((used, complete)) => {
                let copy = self.active.model_copy.as_ref().unwrap();
                let value = copy.result;
                let roots = copy.roots;
                if complete {
                    self.respond(ticket, HostResponse::Ready(value))?;
                    self.active.model_copy = None;
                } else {
                    self.respond(
                        ticket,
                        HostResponse::Continue(Continuation {
                            version: 1,
                            stage: 0,
                            values: &[roots],
                            faults: FaultRetention::Preserve,
                        }),
                    )?;
                }
                Ok(used)
            }
            Err(error) => {
                self.active.model_copy = None;
                self.respond_language_error(ticket, heap_fault(error))?;
                Ok(work)
            }
        }
    }
}
