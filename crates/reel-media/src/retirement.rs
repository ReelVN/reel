//! Physical lifetime evidence; no destruction queue or PCM callback work.
use crate::{MediaEpoch, MediaId};
use reel_core::identity::Scope;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

/// A service-local admission capability and owner retirement receipt.
/// Clones share one bounded registration. A closed registration never reopens.
/// Holding a receipt (even a completed one) retains its registration slot.
#[derive(Clone, Debug)]
pub struct AudioOwner(pub(crate) Arc<OwnerState>);

#[derive(Debug)]
pub(crate) struct OwnerState {
    pub scope: Scope,
    pub service: Arc<()>,
    pub closed: AtomicBool,
    resources: AtomicUsize,
    descriptors: AtomicUsize,
}
impl AudioOwner {
    pub(crate) fn new(scope: Scope, service: Arc<()>) -> Self {
        Self(Arc::new(OwnerState {
            scope,
            service,
            closed: AtomicBool::new(false),
            resources: AtomicUsize::new(0),
            descriptors: AtomicUsize::new(0),
        }))
    }
    pub fn scope(&self) -> Scope {
        self.0.scope
    }
    pub fn is_closed(&self) -> bool {
        self.0.closed.load(Ordering::Acquire)
    }
    /// True only after admission closes and every accepted resource/ticket and
    /// ledger descriptor has physically dropped. Does not claim presentation.
    /// Read-only and repeatable, including after the activity service is dropped.
    pub fn is_retired(&self) -> bool {
        self.is_closed()
            && self.0.resources.load(Ordering::Acquire) == 0
            && self.0.descriptors.load(Ordering::Acquire) == 0
    }
}

/// Evidence for exactly one admitted (ID, media epoch, owner registration).
/// Unlike a logical terminal, this survives ledger drain and service teardown.
/// Obtain it before draining the entry. Clones retain one bounded evidence slot;
/// drop unneeded receipts to permit subsequent admissions.
#[derive(Clone, Debug)]
pub struct AudioActivityRetirement(pub(crate) Arc<ActivityState>);

#[derive(Debug)]
pub(crate) struct ActivityState {
    id: MediaId,
    epoch: MediaEpoch,
    owner: AudioOwner,
    resources_released: AtomicBool,
    descriptor_released: AtomicBool,
}
impl ActivityState {
    pub(crate) fn new(id: MediaId, epoch: MediaEpoch, owner: AudioOwner) -> Arc<Self> {
        // The service reserves a bounded evidence slot BEFORE creating this
        // state. Counts cannot exceed 256, including on 32-bit targets.
        owner.0.resources.fetch_add(1, Ordering::AcqRel);
        owner.0.descriptors.fetch_add(1, Ordering::AcqRel);
        Arc::new(Self {
            id,
            epoch,
            owner,
            resources_released: AtomicBool::new(false),
            descriptor_released: AtomicBool::new(false),
        })
    }
}
impl AudioActivityRetirement {
    pub fn id(&self) -> MediaId {
        self.0.id
    }
    /// The admitted epoch, unaffected by subsequent output faults.
    pub fn epoch(&self) -> MediaEpoch {
        self.0.epoch
    }
    pub fn scope(&self) -> Scope {
        self.0.owner.scope()
    }
    pub fn resources_released(&self) -> bool {
        self.0.resources_released.load(Ordering::Acquire)
    }
    pub fn descriptor_released(&self) -> bool {
        self.0.descriptor_released.load(Ordering::Acquire)
    }
    pub fn is_retired(&self) -> bool {
        self.resources_released() && self.descriptor_released()
    }
}

/// Must follow the actual BudgetTicket in the shared residency allocation.
/// All PCM aliases drop their frames BEFORE their residency Arc. Consequently
/// this runs after the last PCM alias AND after BudgetTicket::drop completes.
/// Accepted resources reach this drop only through the existing worker jobs.
#[derive(Debug)]
pub(crate) struct PhysicalRelease(pub Arc<ActivityState>);
impl Drop for PhysicalRelease {
    fn drop(&mut self) {
        self.0.resources_released.store(true, Ordering::Release);
        self.0.owner.0.resources.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Last field of Entry: native control handle and owned key drop first.
pub(crate) struct DescriptorRelease(pub Arc<ActivityState>);
impl Drop for DescriptorRelease {
    fn drop(&mut self) {
        self.0.descriptor_released.store(true, Ordering::Release);
        self.0.owner.0.descriptors.fetch_sub(1, Ordering::AcqRel);
    }
}
