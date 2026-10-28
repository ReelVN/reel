//! Freshness only traverses the derived portion of a committed dependency graph.
use super::*;

impl Heap {
    /// Semantic reads in an observer refresh their actual bound source. Ordinary
    /// script segments retain the last published value until their boundary.
    pub(crate) fn reactive_read_source(
        &mut self,
        property: PropertyId,
    ) -> Result<(Option<Value>, u64), HeapError> {
        if self.reactive_capture.is_none() {
            return Ok((None, 0));
        }
        let object = self.object(self.key(property.host)?)?;
        if !object.model && property.field >= object.observable_fields {
            return Ok((None, 0));
        }
        let output = self.property_source(property)?.0.host;
        if !self.object(self.key(output)?)?.derived {
            return Ok((None, 0));
        }
        let mut active = self.reactive_capture;
        let mut work = 1;
        while let Some(capture) = active {
            work += 1;
            if capture.same_bits(output) {
                return Err(HeapError::BindingCycle);
            }
            let parent = self.host_payload(capture, DerivedSlot::Parent as u32)?;
            active = (parent.tag() != crate::ValueTag::Null).then_some(parent);
        }
        let current = self.derived_is_current_work(output, None, &mut work)?;
        Ok(((!current).then_some(output), work))
    }
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

    fn push_derived_ancestor(
        &mut self,
        key: HeapKey,
        index: u32,
        work: &mut u64,
    ) -> Result<(), HeapError> {
        let inputs = &self.dependency_payload(key)?.ancestors;
        if inputs.len() == inputs.capacity() {
            *work += inputs.len() as u64;
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

    fn prepare_derived_ancestors(
        &mut self,
        dependencies: Value,
        work: &mut u64,
    ) -> Result<(), HeapError> {
        let key = self.key(dependencies)?;
        if self.dependency_payload(key)?.ancestor_revision == self.binding_revision {
            return Ok(());
        }
        let Payload::Dependencies(payload) = &mut self.objects.get_mut(key).unwrap().payload else {
            unreachable!()
        };
        payload.ancestor_revision = 0;
        payload.ancestors.clear();
        let mut inputs = DependencyCursor::new(dependencies);
        while let Some((host, selector)) = inputs.next(self)? {
            *work += 1;
            let mut source = PropertyId {
                host,
                field: selector.as_int().unwrap() as u32,
            };
            if source.field != u32::MAX {
                source = self.property_source(source)?.0;
            }
            if self.object(self.key(source.host)?)?.derived {
                self.push_derived_ancestor(key, inputs.offset - 2, work)?;
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
        self.derived_is_current_work(output, evaluating, &mut 0)
    }

    fn queue_freshness_input(&mut self, property: PropertyId) -> Result<(), HeapError> {
        if !self.object(self.key(property.host)?)?.derived {
            return self.work_push(property);
        }
        let dependencies = self.host_payload(property.host, DerivedSlot::Dependencies as u32)?;
        let key = if dependencies.same_bits(Value::NULL) {
            None
        } else {
            Some(self.key(dependencies)?)
        };
        if let Some(key) = key
            && self.dependency_payload(key)?.freshness_visit == self.freshness_walk
        {
            return Ok(());
        }
        // Admission precedes marking, so a failed queue growth publishes no mark.
        // Each committed dependency payload belongs to exactly one derived output.
        self.work_push(property)?;
        if let Some(key) = key {
            let Payload::Dependencies(dependencies) =
                &mut self.objects.get_mut(key).unwrap().payload
            else {
                unreachable!()
            };
            dependencies.freshness_visit = self.freshness_walk;
        }
        Ok(())
    }

    fn derived_is_current_work(
        &mut self,
        output: Value,
        evaluating: Option<Value>,
        work: &mut u64,
    ) -> Result<bool, HeapError> {
        self.freshness_walk = self
            .freshness_walk
            .checked_add(1)
            .ok_or(HeapError::IdentityExhausted)?;
        self.property_work.clear();
        self.queue_freshness_input(PropertyId {
            host: output,
            field: u32::MAX,
        })?;
        let mut cursor = 0;
        while cursor < self.property_work.len() {
            *work += 1;
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
                || (self
                    .host_payload(current.host, DerivedSlot::Candidate as u32)?
                    .tag()
                    != crate::ValueTag::Null
                    && self
                        .host_payload(current.host, DerivedSlot::Evaluation as u32)?
                        .as_int()
                        != Some(0))
            {
                return Ok(false);
            }
            // An untracked component factory also owns an unfinished capture.
            let mut active = self.reactive_capture;
            while let Some(value) = active {
                *work += 1;
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
            self.prepare_derived_ancestors(dependencies, work)?;
            let dependency_key = self.key(dependencies)?;
            let count = self.dependency_payload(dependency_key)?.ancestors.len();
            for index in 0..count {
                *work += 1;
                let i = self.dependency_payload(dependency_key)?.ancestors[index];
                let mut next = self.dependency_source(dependencies, i)?;
                next.field = u32::MAX;
                *work += 1;
                self.queue_freshness_input(next)?;
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
