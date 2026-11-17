//! Session owner creation, revocation and retirement.
use super::{
    CancellationReason, OwnerEntry, OwnerHandle, OwnerId, OwnerLifecycle,
    OwnerRetirementRequirements, OwnerRetirementView, Session, SessionError, SessionStatus,
};

impl Session {
    pub fn root_owner(&self) -> OwnerHandle {
        self.owners[0].handle
    }

    pub fn is_owner_live(&self, owner: OwnerHandle) -> bool {
        self.status == SessionStatus::Running
            && owner.identity == self.identity
            && self
                .owners
                .iter()
                .any(|entry| entry.handle == owner && entry.lifecycle == OwnerLifecycle::Live)
    }

    /// Retired IDs remain observable until their storage slot is reused.
    pub fn owner_lifecycle(&self, owner: OwnerHandle) -> Option<OwnerLifecycle> {
        self.owners
            .iter()
            .find(|entry| entry.handle == owner)
            .map(|entry| entry.lifecycle)
    }

    /// A live owner has no retirement record; revoked and retired records are
    /// kept until actual retirement permits a later storage-slot reuse.
    pub fn owner_retirement(&self, owner: OwnerHandle) -> Option<OwnerRetirementView> {
        self.owners
            .iter()
            .find(|entry| entry.handle == owner)
            .and_then(|entry| entry.retirement)
    }

    /// Register a UI node owner. Retired storage slots may be reused, IDs never are.
    pub fn create_owner(&mut self) -> Result<OwnerHandle, SessionError> {
        self.ensure_running()?;
        let retired_index = self
            .owners
            .iter()
            .position(|entry| entry.lifecycle == OwnerLifecycle::Retired);
        let id = self.next_owner.ok_or(SessionError::IdentityExhausted)?;
        if retired_index.is_none() {
            self.owners
                .try_reserve(1)
                .map_err(|_| SessionError::AllocationFailed)?;
        }
        let handle = OwnerHandle {
            identity: self.identity,
            id: OwnerId(id),
        };
        let entry = OwnerEntry {
            handle,
            lifecycle: OwnerLifecycle::Live,
            retirement: None,
        };
        if let Some(index) = retired_index {
            self.owners[index] = entry;
        } else {
            self.owners.push(entry);
        }
        self.next_owner = id.checked_add(1);
        Ok(handle)
    }

    /// Pure-Rust zero-obligation shortcut: the caller guarantees that the owner
    /// has no executor tasks, host roots or external resources. A root shortcut
    /// makes the same assertion for every still-live peer. It never relaxes or
    /// overwrites an already-revoked owner's declared cleanup obligations.
    /// A dead main owner produces cleanup, never a wakeup.
    pub fn revoke_owner(&mut self, owner: OwnerHandle) -> Result<bool, SessionError> {
        self.revoke_owner_internal(owner, OwnerRetirementRequirements::default(), false)
    }

    /// Reserve the in-slot retirement record, revoke permissions, then wait for
    /// all declared cleanup categories. Repeated revoke cannot replace a record.
    /// Before revoking the root, a real coordinator must declare and revoke all
    /// live peers; otherwise this returns a conflict before any mutation.
    pub fn revoke_owner_with_retirement(
        &mut self,
        owner: OwnerHandle,
        requirements: OwnerRetirementRequirements,
    ) -> Result<bool, SessionError> {
        self.revoke_owner_internal(owner, requirements, true)
    }

    fn revoke_owner_internal(
        &mut self,
        owner: OwnerHandle,
        requirements: OwnerRetirementRequirements,
        require_peer_declarations: bool,
    ) -> Result<bool, SessionError> {
        if owner.identity != self.identity {
            return Err(SessionError::ForeignOwner);
        }
        let Some(index) = self.owners.iter().position(|entry| entry.handle == owner) else {
            return Ok(false);
        };
        if self.owners[index].lifecycle != OwnerLifecycle::Live {
            return Ok(false);
        }
        let root = owner == self.root_owner();
        if root
            && require_peer_declarations
            && self
                .owners
                .iter()
                .any(|entry| entry.handle != owner && entry.lifecycle == OwnerLifecycle::Live)
        {
            return Err(SessionError::PeerOwnerRequirementsMissing);
        }
        self.owners[index].revoke(requirements);
        self.owner_revocation_revision = self.owner_revocation_revision.wrapping_add(1);
        if root {
            self.status = SessionStatus::Cancelled;
            if !require_peer_declarations {
                self.retire_live_owners_without_obligations();
            }
            self.cancel_interaction(CancellationReason::OwnerRevoked);
        }
        Ok(true)
    }

    /// Report an actual adapter's cursor, including every traversed empty slot.
    /// Its forward delta must fit this step's root work budget and the declared
    /// single-root snapshot bound. The adapter limits its real scan to remaining
    /// work and `root_slots_end - root_cursor`; this is not an extra heap API
    /// parameter. This bookkeeping performs no root, range-release or GC operation.
    pub fn report_owner_root_progress(
        &mut self,
        owner: OwnerHandle,
        new_cursor: usize,
        work_budget: usize,
    ) -> Result<OwnerLifecycle, SessionError> {
        let entry = self.retirement_entry_mut(owner)?;
        let record = entry
            .retirement
            .as_mut()
            .expect("revoked owner has reserved record");
        if new_cursor < record.root_cursor {
            return Err(SessionError::RetirementCursorRegression);
        }
        if new_cursor > record.root_slots_end {
            return Err(SessionError::RetirementCursorOutOfRange);
        }
        if new_cursor - record.root_cursor > work_budget {
            return Err(SessionError::RetirementBudgetExceeded);
        }
        record.root_cursor = new_cursor;
        entry.retire_if_complete();
        Ok(entry.lifecycle)
    }

    /// Confirm actual task cleanup, not just a cancel request or stop return.
    /// A coordinator may call this only after all task slices and their owned
    /// cleanup obligations are released. This never recreates a live owner.
    pub fn confirm_owner_task_cleanup(
        &mut self,
        owner: OwnerHandle,
    ) -> Result<OwnerLifecycle, SessionError> {
        let entry = self.retirement_entry_mut(owner)?;
        entry
            .retirement
            .as_mut()
            .expect("revoked owner has reserved record")
            .tasks_pending = false;
        entry.retire_if_complete();
        Ok(entry.lifecycle)
    }

    /// Confirm all actual external cleanup, including independently tracked
    /// host-owned non-task root ranges. This only clears the declared category;
    /// it performs no IO, range release, device retirement or heap reclamation.
    pub fn confirm_owner_resources_released(
        &mut self,
        owner: OwnerHandle,
    ) -> Result<OwnerLifecycle, SessionError> {
        let entry = self.retirement_entry_mut(owner)?;
        entry
            .retirement
            .as_mut()
            .expect("revoked owner has reserved record")
            .external_resources_pending = false;
        entry.retire_if_complete();
        Ok(entry.lifecycle)
    }

    fn retirement_entry_mut(
        &mut self,
        owner: OwnerHandle,
    ) -> Result<&mut OwnerEntry, SessionError> {
        if owner.identity != self.identity {
            return Err(SessionError::ForeignOwner);
        }
        let entry = self
            .owners
            .iter_mut()
            .find(|entry| entry.handle == owner)
            .ok_or(SessionError::OwnerUnknown)?;
        if entry.lifecycle == OwnerLifecycle::Live {
            return Err(SessionError::OwnerNotRevoked);
        }
        Ok(entry)
    }

    pub(super) fn retire_live_owners_without_obligations(&mut self) {
        let mut changed = false;
        for entry in &mut self.owners {
            if entry.lifecycle == OwnerLifecycle::Live {
                entry.revoke(OwnerRetirementRequirements::default());
                changed = true;
            }
        }
        if changed {
            self.owner_revocation_revision = self.owner_revocation_revision.wrapping_add(1);
        }
    }
}
