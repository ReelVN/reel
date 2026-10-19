//! Freshness only traverses the derived portion of a committed dependency graph.
use super::*;

impl Heap {
    fn dependency_payload(
        &self,
        key: HeapKey,
    ) -> Result<&crate::dependencies::Dependencies, HeapError> {
        let Payload::Dependencies(dependencies) = &self.object(key)?.payload else {
            unreachable!()
        };
        Ok(dependencies)
    }

    fn dependency_source(&self, dependencies: Value, index: u32) -> Result<PropertyId, HeapError> {
        let source = PropertyId {
            host: self.sequence_get(dependencies, index)?,
            field: self
                .sequence_get(dependencies, index + 1)?
                .as_int()
                .unwrap() as u32,
        };
        if source.field == u32::MAX {
            Ok(source)
        } else {
            Ok(self.property_source(source)?.0)
        }
    }

    fn push_derived_ancestor(&mut self, key: HeapKey, index: u32) -> Result<(), HeapError> {
        let inputs = &self.dependency_payload(key)?.ancestors;
        if inputs.len() == inputs.capacity() {
            let capacity = (inputs.capacity() * 2).max(8);
            // Allocate separately so both old and new storage are admitted and
            // counted while they overlap. Failure keeps the incomplete cache invalid.
            self.ensure_bytes((capacity * std::mem::size_of::<u32>()) as u64)?;
            let mut inputs = Vec::new();
            inputs
                .try_reserve_exact(capacity)
                .map_err(|_| HeapError::AllocationFailure)?;
            let bytes = (inputs.capacity() * std::mem::size_of::<u32>()) as u64;
            self.charge(bytes, 0);
            let object = self.objects.get_mut(key).unwrap();
            let Payload::Dependencies(dependencies) = &mut object.payload else {
                unreachable!()
            };
            inputs.extend_from_slice(&dependencies.ancestors);
            drop(std::mem::replace(&mut dependencies.ancestors, inputs));
            let old = std::mem::replace(&mut dependencies.ancestor_reserved, bytes);
            object.charged += bytes - old;
            self.freed(old);
        }
        let Payload::Dependencies(dependencies) = &mut self.objects.get_mut(key).unwrap().payload
        else {
            unreachable!()
        };
        dependencies.ancestors.push(index);
        Ok(())
    }

    fn prepare_derived_ancestors(&mut self, dependencies: Value) -> Result<(), HeapError> {
        let key = self.key(dependencies)?;
        if self.dependency_payload(key)?.ancestor_revision == self.binding_revision {
            return Ok(());
        }
        let Payload::Dependencies(payload) = &mut self.objects.get_mut(key).unwrap().payload else {
            unreachable!()
        };
        payload.ancestor_revision = 0;
        payload.ancestors.clear();
        for index in (0..self.sequence_len(dependencies)?).step_by(2) {
            let source = self.dependency_source(dependencies, index)?;
            if self.object(self.key(source.host)?)?.derived {
                self.push_derived_ancestor(key, index)?;
            }
        }
        let Payload::Dependencies(payload) = &mut self.objects.get_mut(key).unwrap().payload else {
            unreachable!()
        };
        payload.ancestor_revision = self.binding_revision;
        Ok(())
    }

    /// Check committed dependencies without running or dequeuing observers.
    /// `evaluating` covers a scheduler's claimed output before begin_derived.
    /// Ordinary endpoints use direct invalidation. Only derived ancestors need
    /// visiting; the private index rebuilds after binding changes or restoration.
    pub fn derived_is_current(
        &mut self,
        output: Value,
        evaluating: Option<Value>,
    ) -> Result<bool, HeapError> {
        self.property_work.clear();
        self.work_push(PropertyId {
            host: output,
            field: u32::MAX,
        })?;
        let mut cursor = 0;
        while cursor < self.property_work.len() {
            let mut current = self.property_work[cursor];
            cursor += 1;
            if current.field != u32::MAX {
                current = self.property_source(current)?.0;
            }
            let object = self.object(self.key(current.host)?)?;
            if !object.derived {
                continue;
            }
            if object.derived_dirty
                || evaluating.is_some_and(|v| v.same_bits(current.host))
                || self
                    .host_payload(current.host, DerivedSlot::Candidate as u32)?
                    .tag()
                    != crate::ValueTag::Null
            {
                return Ok(false);
            }
            // An untracked component factory also owns an unfinished capture.
            let mut active = self.reactive_capture;
            while let Some(value) = active {
                if value.same_bits(current.host) {
                    return Ok(false);
                }
                let parent = self.host_payload(value, DerivedSlot::Parent as u32)?;
                active = (parent.tag() != crate::ValueTag::Null).then_some(parent);
            }
            let dependencies = self.host_payload(current.host, DerivedSlot::Dependencies as u32)?;
            if dependencies.tag() == crate::ValueTag::Null {
                continue;
            }
            self.prepare_derived_ancestors(dependencies)?;
            let dependency_key = self.key(dependencies)?;
            let count = self.dependency_payload(dependency_key)?.ancestors.len();
            for index in 0..count {
                let i = self.dependency_payload(dependency_key)?.ancestors[index];
                let mut next = self.dependency_source(dependencies, i)?;
                next.field = u32::MAX;
                if !self
                    .property_work
                    .iter()
                    .any(|p| p.host.same_bits(next.host) && p.field == next.field)
                {
                    self.work_push(next)?;
                }
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
