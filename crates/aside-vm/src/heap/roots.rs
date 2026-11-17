//! Individual host roots and their collector scan positions.
use super::{Heap, HeapError, Root, RootKey, RootKind, RootOwner, RootToken, Value, index};
use std::marker::PhantomData;

impl Heap {
    pub fn add_root(
        &mut self,
        owner: RootOwner,
        kind: RootKind,
        value: Value,
    ) -> Result<RootToken, HeapError> {
        self.ensure_root_slot()?;
        self.barrier(value)?;
        let key = self.roots.insert(Root { value, owner, kind });
        self.root_index[index(key)] = Some(key);
        self.root_scan_len = self.root_scan_len.max(index(key) + 1);
        Ok(RootToken {
            heap: self.id,
            epoch: self.epoch,
            key,
            _thread: PhantomData,
        })
    }
    fn scoped_root_key(&self, token: RootToken) -> Result<RootKey, HeapError> {
        if token.heap != self.id {
            return Err(HeapError::WrongHeap);
        }
        if token.epoch != self.epoch {
            return Err(HeapError::WrongEpoch);
        }
        Ok(token.key)
    }
    fn root(&self, token: RootToken) -> Result<&Root, HeapError> {
        self.roots
            .get(self.scoped_root_key(token)?)
            .ok_or(HeapError::InvalidRoot)
    }
    pub fn root_value(&self, token: RootToken) -> Result<Value, HeapError> {
        Ok(self.root(token)?.value)
    }
    pub fn root_owner(&self, token: RootToken) -> Result<(RootOwner, RootKind), HeapError> {
        let root = self.root(token)?;
        Ok((root.owner, root.kind))
    }
    pub fn set_root(&mut self, token: RootToken, value: Value) -> Result<(), HeapError> {
        let key = self.scoped_root_key(token)?;
        self.roots.get(key).ok_or(HeapError::InvalidRoot)?;
        self.barrier(value)?;
        self.roots.get_mut(key).unwrap().value = value;
        Ok(())
    }
    pub fn release_root(&mut self, token: RootToken) -> Result<Value, HeapError> {
        let key = self.scoped_root_key(token)?;
        let root = self.roots.remove(key).ok_or(HeapError::InvalidRoot)?;
        self.root_index[index(key)] = None;
        Ok(root.value)
    }
    /// Bounded owner cancellation; cursor must start at zero and persist until done.
    /// New roots for a cancelled owner must be prevented by the owning host.
    pub fn release_owner_step(&mut self, owner: RootOwner, cursor: &mut u32, slots: u32) -> u32 {
        let mut released = 0;
        let end = (*cursor as usize)
            .saturating_add(slots as usize)
            .min(self.root_scan_len);
        while (*cursor as usize) < end {
            let position = *cursor as usize;
            if let Some(key) = self.root_index[position]
                && self.roots.get(key).is_some_and(|r| r.owner == owner)
            {
                self.roots.remove(key);
                self.root_index[position] = None;
                released += 1;
            }
            *cursor += 1;
        }
        released
    }
    /// Descriptor high-water bound, including sentinel and released holes.
    /// Reserved capacity that has never been used is not a scan position.
    pub fn root_slots(&self) -> u32 {
        self.root_scan_len as u32
    }
    pub(crate) fn scan_root(&mut self, position: usize) {
        if let Some(value) = self.root_index[position]
            .and_then(|k| self.roots.get(k))
            .map(|r| r.value)
        {
            self.shade(value);
        }
    }
}
