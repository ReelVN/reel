//! Unpublished model observation plans, rooted by an ordinary native continuation.
use super::*;

pub(in crate::heap) enum ModelAdoptionProgress {
    Preparing { work: u64, required_work: u64 },
    Complete { work: u64, commit_work: u64 },
}

#[cfg(test)]
mod tests;

#[derive(Clone, Copy)]
#[repr(u32)]
enum Slot {
    Visited,
    Versions,
    Pending,
    Queue,
    Edge,
    Phase,
    Work,
    Count,
}
const ITEM_WORK: u64 = 128;
const PUBLISH_WORK: u64 = 16;

impl Heap {
    fn adoption_get(&self, plan: Value, slot: Slot) -> Result<Value, HeapError> {
        self.sequence_get(plan, slot as u32)
    }
    fn adoption_number(&self, plan: Value, slot: Slot) -> Result<u64, HeapError> {
        Ok(self
            .adoption_get(plan, slot)?
            .as_int()
            .ok_or(HeapError::TypeMismatch)? as u64)
    }
    fn adoption_set(&mut self, plan: Value, slot: Slot, value: u64) -> Result<(), HeapError> {
        self.set_field(plan, slot as u32, Value::int(value as i64))
    }
    pub(in crate::heap) fn begin_model_adoption(
        &mut self,
        value: Value,
    ) -> Result<Value, HeapError> {
        let plan = self.alloc_array(&[Value::NULL; Slot::Count as usize])?;
        let visited = self.alloc_dependencies()?;
        self.set_field(plan, Slot::Visited as u32, visited)?;
        let versions = self.alloc_array(&[])?;
        self.set_field(plan, Slot::Versions as u32, versions)?;
        self.set_field(plan, Slot::Pending as u32, value)?;
        for slot in [Slot::Queue, Slot::Edge, Slot::Phase, Slot::Work] {
            self.adoption_set(plan, slot, 0)?;
        }
        Ok(plan)
    }
    pub(in crate::heap) fn model_adoption_required_work(
        &self,
        plan: Value,
    ) -> Result<u64, HeapError> {
        Ok(if self.adoption_number(plan, Slot::Phase)? == 2 {
            0
        } else {
            ITEM_WORK
        })
    }
    pub(in crate::heap) fn model_adoption_work(&self, plan: Value) -> Result<u64, HeapError> {
        self.adoption_number(plan, Slot::Work)
    }
    pub(in crate::heap) fn model_adoption_versions_len(
        &self,
        plan: Value,
    ) -> Result<u32, HeapError> {
        self.sequence_len(self.adoption_get(plan, Slot::Versions)?)
    }
    pub(in crate::heap) fn model_adoption_version_valid(
        &self,
        plan: Value,
        index: u32,
    ) -> Result<bool, HeapError> {
        let current = self.sequence_get(
            self.adoption_get(plan, Slot::Visited)?,
            index.checked_mul(2).ok_or(HeapError::OutOfBounds)?,
        )?;
        let expected = self
            .sequence_get(self.adoption_get(plan, Slot::Versions)?, index)?
            .as_int()
            .ok_or(HeapError::TypeMismatch)? as u64;
        let object = self.object(self.key(current)?)?;
        // Another complete adoption already observes all current data edges.
        // Subsequent mutations use the ordinary model assignment hooks.
        Ok(object.model || object.content_version == expected)
    }
    /// Each step admits one candidate lookup or one data edge. The dependency
    /// payload provides the existing strong, snapshot-remapped identity index.
    /// No model flags change until the caller validates every proposal proof.
    pub(in crate::heap) fn step_model_adoption(
        &mut self,
        plan: Value,
        budget: u64,
    ) -> Result<ModelAdoptionProgress, HeapError> {
        let mut work = 0;
        loop {
            let phase = self.adoption_number(plan, Slot::Phase)?;
            if phase == 2 {
                return Ok(ModelAdoptionProgress::Complete {
                    work,
                    commit_work: PUBLISH_WORK
                        * (u64::from(self.model_adoption_versions_len(plan)?) + 1),
                });
            }
            if budget - work < ITEM_WORK {
                return Ok(ModelAdoptionProgress::Preparing {
                    work,
                    required_work: ITEM_WORK,
                });
            }
            work += ITEM_WORK;
            self.adoption_set(
                plan,
                Slot::Work,
                self.adoption_number(plan, Slot::Work)?
                    .checked_add(ITEM_WORK)
                    .ok_or(HeapError::OutOfBounds)?,
            )?;
            match phase {
                0 => {
                    let pending = self.adoption_get(plan, Slot::Pending)?;
                    let visited = self.adoption_get(plan, Slot::Visited)?;
                    if self.model_candidate(pending)?
                        && self.capture_dependency(visited, pending, None)?
                    {
                        let version = self.info(pending)?.content_version;
                        let versions = self.adoption_get(plan, Slot::Versions)?;
                        self.array_push(versions, Value::int(version as i64))?;
                    }
                    self.set_field(plan, Slot::Pending as u32, Value::NULL)?;
                    self.adoption_set(plan, Slot::Phase, 1)?;
                }
                1 => {
                    let visited = self.adoption_get(plan, Slot::Visited)?;
                    let queue = self.adoption_number(plan, Slot::Queue)? as u32;
                    if queue == self.sequence_len(visited)? {
                        self.adoption_set(plan, Slot::Phase, 2)?;
                        continue;
                    }
                    let current = self.sequence_get(visited, queue)?;
                    let edge = self.adoption_number(plan, Slot::Edge)? as u32;
                    let child = if self.object(self.key(current)?)?.model {
                        None
                    } else {
                        self.model_data_edge(current, edge)?
                    };
                    if let Some(child) = child {
                        self.set_field(plan, Slot::Pending as u32, child)?;
                        self.adoption_set(plan, Slot::Edge, u64::from(edge) + 1)?;
                        self.adoption_set(plan, Slot::Phase, 0)?;
                    } else {
                        self.adoption_set(plan, Slot::Queue, u64::from(queue) + 2)?;
                        self.adoption_set(plan, Slot::Edge, 0)?;
                    }
                }
                _ => return Err(HeapError::WrongKind),
            }
        }
    }
    /// Publish only after the outer property transaction has checked every
    /// version and reserved its complete atomic commit budget. No allocation or
    /// traversal of user data occurs here; the prepared queue keeps all nodes live.
    pub(in crate::heap) fn publish_model_adoption(&mut self, plan: Value) -> Result<(), HeapError> {
        if self.adoption_number(plan, Slot::Phase)? != 2 {
            return Err(HeapError::WrongKind);
        }
        let visited = self.adoption_get(plan, Slot::Visited)?;
        let len = self.sequence_len(visited)?;
        self.property_traversal_work
            .set(self.property_traversal_work.get() + PUBLISH_WORK);
        for index in (0..len).step_by(2) {
            self.property_traversal_work
                .set(self.property_traversal_work.get() + PUBLISH_WORK);
            self.observe_model_object(self.sequence_get(visited, index)?)?;
        }
        Ok(())
    }
}
