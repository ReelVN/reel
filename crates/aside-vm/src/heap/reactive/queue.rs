//! Pending derived output scheduling.
use super::*;

impl Heap {
    // Links are weak object identities. Object reclamation unlinks before slot reuse.
    // No queue growth or traversal is needed at a lazy-read claim boundary.
    pub(in crate::heap) fn enqueue_derived(&mut self, key: HeapKey) {
        let object = self.objects.get_mut(key).unwrap();
        if object.derived_dirty {
            return;
        }
        object.derived_dirty = true;
        object.derived_previous = self.reactive_tail;
        object.derived_next = None;
        if let Some(tail) = self.reactive_tail {
            self.objects.get_mut(tail).unwrap().derived_next = Some(key);
        } else {
            self.reactive_head = Some(key);
        }
        self.reactive_tail = Some(key);
    }
    pub(in crate::heap) fn unlink_derived(&mut self, key: HeapKey) {
        let object = self.objects.get_mut(key).unwrap();
        if !object.derived_dirty {
            return;
        }
        object.derived_dirty = false;
        let previous = object.derived_previous.take();
        let next = object.derived_next.take();
        if let Some(previous) = previous {
            self.objects.get_mut(previous).unwrap().derived_next = next;
        } else {
            self.reactive_head = next;
        }
        if let Some(next) = next {
            self.objects.get_mut(next).unwrap().derived_previous = previous;
        } else {
            self.reactive_tail = previous;
        }
    }
    pub(crate) fn derived_claim_work(&self) -> u64 {
        6
    }
    pub(crate) fn claim_derived(&mut self, output: Value) -> Result<(), HeapError> {
        let key = self.key(output)?;
        self.object(key)?;
        self.unlink_derived(key);
        Ok(())
    }
    /// Whether queued outputs or an active dependency capture need processing.
    pub fn derived_pending(&self) -> bool {
        self.reactive_head.is_some() || self.reactive_capture.is_some()
    }
    /// Queue a host-owned observer at an explicit cooperative boundary.
    pub fn schedule_derived(&mut self, output: Value) -> Result<(), HeapError> {
        let key = self.key(output)?;
        self.object(key)?;
        self.unlink_derived(key);
        self.enqueue_derived(key);
        Ok(())
    }
    /// Whether an output has a pending derived evaluation.
    pub fn derived_is_queued(&self, output: Value) -> Result<bool, HeapError> {
        Ok(self.object(self.key(output)?)?.derived_dirty)
    }
    /// Read existing queued identities without copying or rooting another graph.
    pub fn pending_derived_outputs(&self) -> impl Iterator<Item = Value> + '_ {
        let mut next = self.reactive_head;
        std::iter::from_fn(move || {
            while let Some(key) = next {
                let object = self.objects.get(key).unwrap();
                next = object.derived_next;
                if !object.dying && !(self.gc.cutoff() && object.mark != self.gc.cycle) {
                    return Some(Value::object(self.id, key.data().as_ffi()));
                }
            }
            None
        })
    }
    /// Claim the oldest live pending output and publish its root barrier.
    pub fn next_derived(&mut self) -> Result<Option<Value>, HeapError> {
        while let Some(key) = self.reactive_head {
            self.unlink_derived(key);
            let object = self.objects.get(key).unwrap();
            if object.dying || (self.gc.cutoff() && object.mark != self.gc.cycle) {
                continue;
            }
            let value = Value::object(self.id, key.data().as_ffi());
            self.root_insertion_barrier(value)?;
            return Ok(Some(value));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pending(heap: &Heap) -> Vec<Value> {
        heap.pending_derived_outputs().collect()
    }
    #[test]
    fn direct_claim_rescheduling_disconnect_and_collection_preserve_queue_links() {
        let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
        let a = heap.alloc_derived(Value::NULL).unwrap();
        let b = heap.alloc_derived(Value::NULL).unwrap();
        let c = heap.alloc_derived(Value::NULL).unwrap();
        let d = heap.alloc_derived(Value::NULL).unwrap();
        for output in [a, b, c, d] {
            heap.schedule_derived(output).unwrap();
        }
        heap.schedule_derived(b).unwrap();
        assert_eq!(
            pending(&heap)
                .iter()
                .map(|v| v.object_parts())
                .collect::<Vec<_>>(),
            [a, c, d, b]
                .iter()
                .map(|v| v.object_parts())
                .collect::<Vec<_>>()
        );
        heap.claim_derived(c).unwrap();
        heap.claim_derived(c).unwrap();
        heap.disconnect_derived(d).unwrap();
        assert_eq!(
            pending(&heap)
                .iter()
                .map(|v| v.object_parts())
                .collect::<Vec<_>>(),
            [a, b].iter().map(|v| v.object_parts()).collect::<Vec<_>>()
        );
        heap.schedule_derived(c).unwrap();
        heap.add_root(RootOwner(1), RootKind::Host, a).unwrap();
        heap.add_root(RootOwner(1), RootKind::Host, c).unwrap();
        heap.begin_collection().unwrap();
        while heap.phase() != GcPhase::Idle {
            heap.gc_step(crate::GcBudget {
                units: 1,
                ..Default::default()
            })
            .unwrap();
        }
        assert!(heap.info(b).is_err());
        assert!(heap.info(d).is_err());
        assert_eq!(
            pending(&heap)
                .iter()
                .map(|v| v.object_parts())
                .collect::<Vec<_>>(),
            [a, c].iter().map(|v| v.object_parts()).collect::<Vec<_>>()
        );
        let replacement = heap.alloc_derived(Value::NULL).unwrap();
        heap.schedule_derived(replacement).unwrap();
        for output in [a, c, replacement] {
            assert!(heap.next_derived().unwrap().unwrap().same_bits(output));
        }
        assert!(heap.next_derived().unwrap().is_none());
        assert!(!heap.derived_pending());
        assert!(heap.reactive_head.is_none() && heap.reactive_tail.is_none());
    }

    #[test]
    #[ignore = "release lazy derived claiming measurement"]
    fn derived_claim_queue_cost() {
        for count in [128, 1024, 4096] {
            let mut heap = Heap::new(SessionEpoch(1), HeapLimits::default()).unwrap();
            let outputs: Vec<_> = (0..count)
                .map(|_| heap.alloc_derived(Value::NULL).unwrap())
                .collect();
            let mut elapsed = std::time::Duration::ZERO;
            for _ in 0..32 {
                for &output in &outputs {
                    heap.schedule_derived(output).unwrap();
                }
                let start = std::time::Instant::now();
                for &output in outputs.iter().rev() {
                    heap.claim_derived(output).unwrap();
                }
                elapsed += start.elapsed();
                assert!(heap.next_derived().unwrap().is_none());
            }
            eprintln!(
                "derived_claim count={count} rounds=32 elapsed_us={}",
                elapsed.as_micros()
            );
        }
    }
}
