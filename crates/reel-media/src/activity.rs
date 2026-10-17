//! Bounded control-side audio activities (design 1.1).
//!
//! The host revokes waits before cancellation, acknowledges its shared WakeSignal
//! before servicing ALL sources, and rechecks pending before sleeping. Use the
//! same signal for this ledger, output and AudioReclaimService: Kira retirement
//! wakes through the existing non-RT reclaimer. Never call these methods on PCM.
//!
//! IDs are unique within one service lifetime; the host supplies a fresh epoch
//! when replacing a service. Positions describe content, not hardware presentation.
use crate::retirement::{ActivityState, DescriptorRelease, OwnerState, PhysicalRelease};
use std::sync::{Arc, Weak, atomic::Ordering};
use std::time::Duration;

use crate::{
    AudioActivityRetirement, AudioError, AudioErrorKind, AudioOwner, DecodedSound, KiraMixer,
    PcmFill, Playback, ReclaimProgress,
};
use reel_core::{
    budget::{BudgetPool, BudgetTicket, QueueLimits},
    identity::{ResourceVersion, Scope},
    result::{RejectCode, RejectReason, Rejected},
    wake::WakeSignal,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MediaId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MediaEpoch(pub u64);
pub use reel_core::audio::AudioGroup;
/// PCM and its existing ticket move unchanged on rejection. The group selects
/// a persistent mixer bus; loop configuration belongs to DecodedSound.
#[derive(Debug)]
pub struct AudioActivityRequest {
    pub epoch: MediaEpoch,
    pub owner: AudioOwner,
    pub resource: ResourceVersion,
    pub group: AudioGroup,
    pub sound: DecodedSound,
}
/// Content consumption only, NEVER device submission/presentation or PCM release.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AudioActivityTerminal {
    Completed { position_seconds: f64 },
    Stopped { position_seconds: f64 },
    Cancelled,
    Failed(AudioErrorKind),
}
#[derive(Debug, PartialEq)]
pub struct AudioActivityEvent {
    pub id: MediaId,
    pub epoch: MediaEpoch,
    pub scope: Scope,
    pub terminal: AudioActivityTerminal,
    pub position_seconds: f64,
}
/// Borrowed logical state; no native handle or PCM alias escapes the ledger.
#[derive(Debug)]
pub struct AudioActivitySnapshot<'a> {
    pub id: MediaId,
    pub epoch: MediaEpoch,
    pub scope: Scope,
    pub resource: &'a ResourceVersion,
    pub group: AudioGroup,
    pub volume_db: f32,
    pub position_seconds: f64,
    pub terminal: Option<AudioActivityTerminal>,
}
struct Entry {
    id: MediaId,
    epoch: MediaEpoch,
    scope: Scope,
    resource: ResourceVersion,
    group: AudioGroup,
    volume_db: f32,
    playback: Playback,
    stopping: bool,
    terminal: Option<AudioActivityTerminal>,
    // Last: native control handle/resource key must be gone before publication.
    _descriptor: DescriptorRelease,
}
struct OwnerSlot {
    state: Weak<OwnerState>,
    cancellation_next: usize,
}
impl Entry {
    fn position_seconds(&self) -> f64 {
        match self.terminal {
            Some(
                AudioActivityTerminal::Completed { position_seconds }
                | AudioActivityTerminal::Stopped { position_seconds },
            ) => position_seconds,
            _ => self.playback.position_seconds(),
        }
    }
    fn settle(&mut self, terminal: AudioActivityTerminal) -> bool {
        if self.terminal.is_some() {
            return false;
        }
        self.terminal = Some(terminal);
        if !matches!(terminal, AudioActivityTerminal::Completed { .. }) {
            self.playback.stop();
        }
        true
    }
}
/// A one-pass cancellation cursor. Revoke business waits and prevent new plays
/// for this Scope BEFORE starting; finish the sweep before releasing the owner.
/// Matching uses the full SessionId/Epoch/OwnerId, or exactly Scope::Process.
#[derive(Debug)]
pub struct AudioOwnerCancellation {
    scope: Scope,
    next: usize,
}
impl AudioOwnerCancellation {
    pub fn new(scope: Scope) -> Self {
        Self { scope, next: 0 }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioActivityProgress {
    /// Includes empty slots; no hidden unbounded scan on sparse ledgers.
    pub inspected: usize,
    pub needs_turn: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioActivityMaintenance {
    pub activities: AudioActivityProgress,
    pub reclaim: ReclaimProgress,
}
/// One preallocated slot reserves both playback ownership and its terminal.
/// A terminal keeps that slot until drained (including filtered/discarded events).
/// Additionally, max_requests bounds live owner registrations and physical
/// activity evidence, including caller-held completed receipts. max_bytes charges
/// their fixed storage plus owned asset-key capacities. PCM remains independently
/// charged by the existing BudgetTicket/reclamation protocol.
/// Every sweep/drain inspects at most min(limit, configured slots).
pub struct AudioActivityService {
    mixer: KiraMixer,
    entries: Vec<Option<Entry>>,
    owners: Vec<OwnerSlot>,
    evidence: Vec<Weak<ActivityState>>,
    identity: Arc<()>,
    limits: QueueLimits,
    descriptor_bytes: u64,
    occupied: usize,
    active: usize,
    next_id: Option<u64>,
    epoch: MediaEpoch,
    failed: Option<AudioErrorKind>,
    maintenance_cursor: usize,
    drain_cursor: usize,
    maintenance_remaining: usize,
    drain_remaining: usize,
    wake: WakeSignal,
}
impl AudioActivityService {
    /// Failure returns the original mixer, including its untaken fill.
    pub fn new(
        mixer: KiraMixer,
        limits: QueueLimits,
        epoch: MediaEpoch,
        wake: WakeSignal,
    ) -> Result<Self, Rejected<KiraMixer>> {
        if epoch.0 == u64::MAX
            || limits.max_requests == 0
            || Self::slot_bytes(limits.max_requests) > limits.max_bytes
        {
            return Err(Rejected {
                request: mixer,
                reason: RejectReason::new(RejectCode::InvalidRequest, "invalid activity limits"),
            });
        }
        Ok(Self {
            mixer,
            entries: (0..limits.max_requests).map(|_| None).collect(),
            owners: (0..limits.max_requests)
                .map(|_| OwnerSlot {
                    state: Weak::new(),
                    cancellation_next: 0,
                })
                .collect(),
            evidence: (0..limits.max_requests).map(|_| Weak::new()).collect(),
            identity: Arc::new(()),
            limits,
            descriptor_bytes: 0,
            occupied: 0,
            active: 0,
            next_id: Some(1),
            epoch,
            failed: None,
            maintenance_cursor: 0,
            drain_cursor: 0,
            maintenance_remaining: limits.max_requests as usize,
            drain_remaining: limits.max_requests as usize,
            wake,
        })
    }
    pub fn slot_bytes(slots: u32) -> u64 {
        // Fixed vectors and maximum live Arc payloads/control blocks. Allocator
        // overhead is implementation-defined, as for the existing slot budget.
        let per_slot = std::mem::size_of::<Option<Entry>>()
            + std::mem::size_of::<OwnerSlot>()
            + std::mem::size_of::<Weak<ActivityState>>()
            + std::mem::size_of::<OwnerState>()
            + std::mem::size_of::<ActivityState>()
            + std::mem::size_of::<Option<PhysicalRelease>>()
            + 4 * std::mem::size_of::<usize>();
        u64::from(slots) * per_slot as u64 + 2 * std::mem::size_of::<usize>() as u64
    }
    /// At most max_requests live owner registrations. Existing complete Scope
    /// matches return the same capability, including when closed. After ALL
    /// capabilities/evidence/resources disappear, a later registration is new;
    /// Session hosts must not recycle a retired Scope as the same logical owner.
    pub fn register_owner(&mut self, scope: Scope) -> Result<AudioOwner, RejectReason> {
        for slot in &self.owners {
            if let Some(state) = slot.state.upgrade()
                && state.scope == scope
            {
                return Ok(AudioOwner(state));
            }
        }
        let slot = self
            .owners
            .iter_mut()
            .find(|slot| slot.state.upgrade().is_none())
            .ok_or_else(|| {
                RejectReason::new(RejectCode::Busy, "owner receipt capacity exhausted")
            })?;
        // Weak keeps its allocation even after the payload is destroyed. Free
        // the old block BEFORE allocating the replacement to respect the bound.
        slot.state = Weak::new();
        let owner = AudioOwner::new(scope, self.identity.clone());
        slot.state = Arc::downgrade(&owner.0);
        slot.cancellation_next = 0;
        Ok(owner)
    }
    /// First close admission, then cancel with a persistent bounded cursor.
    /// Revoke business waits before calling. Progress is only cancellation work;
    /// owner.is_retired() additionally requires actual resource/descriptor drops.
    /// Already settled terminals retain their original admitted identity/result.
    pub fn retire_owner(
        &mut self,
        owner: &AudioOwner,
        limit: usize,
    ) -> Result<AudioActivityProgress, RejectReason> {
        if !Arc::ptr_eq(&owner.0.service, &self.identity) {
            return Err(RejectReason::new(
                RejectCode::InvalidRequest,
                "foreign audio owner",
            ));
        }
        owner.0.closed.store(true, Ordering::Release);
        let index = self
            .owners
            .iter()
            .position(|slot| slot.state.ptr_eq(&Arc::downgrade(&owner.0)))
            .expect("live capability retains registration");
        let mut cursor = AudioOwnerCancellation {
            scope: owner.scope(),
            next: self.owners[index].cancellation_next,
        };
        let progress = self.cancel_owner(&mut cursor, limit);
        self.owners[index].cancellation_next = cursor.next;
        Ok(progress)
    }
    /// Obtain before draining the terminal. The receipt remains queryable even
    /// after entry/service destruction and occupies one of max_requests slots.
    pub fn retirement(&self, id: MediaId) -> Option<AudioActivityRetirement> {
        self.evidence
            .iter()
            .filter_map(Weak::upgrade)
            .map(AudioActivityRetirement)
            .find(|receipt| receipt.id() == id)
    }
    pub fn epoch(&self) -> MediaEpoch {
        self.epoch
    }
    pub fn occupied(&self) -> usize {
        self.occupied
    }
    /// Schedule a control-only timer while live audio exists, including suspended
    /// output. Playback exposes no per-handle completion subscription: retirement
    /// wakes can race a partially completed scan. This fallback prevents a missed
    /// wake from stranding a terminal; it does not request layout/presentation.
    /// Partial scans still use needs_turn. This is a polling interval, not a
    /// hardware clock or a terminal latency guarantee under scheduler overload.
    pub fn poll_after(&self) -> Option<Duration> {
        (self.active != 0).then_some(Duration::from_millis(10))
    }
    pub fn take_fill(&mut self) -> Option<PcmFill> {
        self.mixer.take_fill()
    }
    pub fn reserve_decode_budget(
        &mut self,
        pool: &BudgetPool,
        bytes: u64,
        reclaim_limit: usize,
    ) -> (ReclaimProgress, Result<BudgetTicket, RejectReason>) {
        self.mixer.reserve_decode_budget(pool, bytes, reclaim_limit)
    }
    pub fn play(
        &mut self,
        request: AudioActivityRequest,
    ) -> Result<MediaId, Rejected<AudioActivityRequest>> {
        let bytes = request.resource.key.0.capacity() as u64;
        let reason = if self.failed.is_some() {
            Some((RejectCode::Unavailable, "output recovery is required"))
        } else if !Arc::ptr_eq(&request.owner.0.service, &self.identity)
            || request.owner.is_closed()
        {
            Some((RejectCode::InvalidRequest, "foreign or closed audio owner"))
        } else if request.epoch != self.epoch {
            Some((RejectCode::InvalidRequest, "stale media epoch"))
        } else if self.next_id.is_none() {
            Some((RejectCode::ServiceFault, "media ID space exhausted"))
        } else if self.occupied == self.entries.len()
            || self.evidence.iter().all(|state| state.upgrade().is_some())
            || bytes
                > self.limits.max_bytes
                    - Self::slot_bytes(self.limits.max_requests)
                    - self.descriptor_bytes
        {
            Some((RejectCode::Busy, "activity/result capacity exhausted"))
        } else {
            None
        };
        if let Some((code, message)) = reason {
            return Err(Rejected {
                request,
                reason: RejectReason::new(code, message),
            });
        }
        let AudioActivityRequest {
            epoch,
            owner,
            resource,
            group,
            mut sound,
        } = request;
        let scope = owner.scope();
        let id = MediaId(self.next_id.expect("checked before acceptance"));
        let evidence_slot = self
            .evidence
            .iter()
            .position(|slot| slot.upgrade().is_none())
            .expect("reserved physical evidence slot");
        self.evidence[evidence_slot] = Weak::new();
        let state = ActivityState::new(id, epoch, owner.clone());
        let descriptor = DescriptorRelease(state.clone());
        let volume_db = sound.volume_db();
        sound.track_retirement(PhysicalRelease(state.clone()));
        let playback = match self.mixer.play_in(group, sound) {
            Ok(playback) => playback,
            Err(Rejected {
                request: mut sound,
                reason,
            }) => {
                sound.untrack_retirement();
                return Err(Rejected {
                    request: AudioActivityRequest {
                        epoch,
                        owner,
                        resource,
                        group,
                        sound,
                    },
                    reason,
                });
            }
        };
        self.next_id = id.0.checked_add(1);
        self.evidence[evidence_slot] = Arc::downgrade(&state);
        *self
            .entries
            .iter_mut()
            .find(|e| e.is_none())
            .expect("reserved terminal slot") = Some(Entry {
            id,
            epoch,
            scope,
            resource,
            group,
            volume_db,
            playback,
            stopping: false,
            terminal: None,
            _descriptor: descriptor,
        });
        self.maintenance_remaining = self.entries.len();
        self.occupied += 1;
        self.active += 1;
        self.descriptor_bytes += bytes;
        self.wake.wake();
        Ok(id)
    }
    pub fn snapshot(&self, id: MediaId) -> Option<AudioActivitySnapshot<'_>> {
        let e = self.entries.iter().flatten().find(|e| e.id == id)?;
        Some(AudioActivitySnapshot {
            id: e.id,
            epoch: e.epoch,
            scope: e.scope,
            resource: &e.resource,
            group: e.group,
            volume_db: e.volume_db,
            position_seconds: e.position_seconds(),
            terminal: e.terminal,
        })
    }
    /// False means absent or already settled; an existing terminal is immutable.
    pub fn stop(&mut self, id: MediaId) -> bool {
        let Some(e) = self.entries.iter_mut().flatten().find(|e| e.id == id) else {
            return false;
        };
        let terminal = if let Some(reason) = self.failed {
            AudioActivityTerminal::Failed(reason)
        } else {
            AudioActivityTerminal::Stopped {
                position_seconds: e.playback.position_seconds(),
            }
        };
        let changed = e.settle(terminal);
        if changed {
            self.active -= 1;
            self.drain_remaining = self.entries.len();
            self.wake.wake();
        }
        changed
    }
    pub fn set_volume_db(
        &mut self,
        id: MediaId,
        db: f32,
        duration: Duration,
    ) -> Result<(), AudioError> {
        let e = self.live_mut(id)?;
        e.playback.set_volume_db(db, duration)?;
        e.volume_db = db;
        Ok(())
    }
    pub fn set_group_volume_db(
        &mut self,
        group: Option<AudioGroup>,
        db: f32,
        duration: Duration,
    ) -> Result<(), AudioError> {
        if self.failed.is_some() {
            return Err(AudioError::new(
                AudioErrorKind::Unavailable,
                "output recovery is required",
            ));
        }
        self.mixer.set_group_volume_db(group, db, duration)
    }
    pub fn set_pan(&mut self, id: MediaId, pan: f32, duration: Duration) -> Result<(), AudioError> {
        self.live_mut(id)?.playback.set_pan(pan, duration)
    }
    /// Keep the activity identity and terminal wait while changing content position.
    pub fn seek(&mut self, id: MediaId, position: Duration) -> Result<Duration, AudioError> {
        Ok(self.live_mut(id)?.playback.seek(position))
    }
    fn live_mut(&mut self, id: MediaId) -> Result<&mut Entry, AudioError> {
        if self.failed.is_some() {
            return Err(AudioError::new(
                AudioErrorKind::Unavailable,
                "output recovery is required",
            ));
        }
        self.entries
            .iter_mut()
            .flatten()
            .find(|e| e.id == id && e.terminal.is_none())
            .ok_or_else(|| {
                AudioError::new(AudioErrorKind::InvalidRequest, "no live audio activity")
            })
    }
    pub fn fade_out(&mut self, id: MediaId, duration: Duration) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .flatten()
            .find(|e| e.id == id && e.terminal.is_none() && !e.stopping)
        {
            entry.stopping = true;
            entry.playback.fade_out(duration);
        }
    }
    pub fn pause(&mut self, id: MediaId) {
        if self
            .entries
            .iter()
            .flatten()
            .any(|e| e.id == id && e.stopping && e.terminal.is_none())
        {
            self.stop(id);
            return;
        }
        if let Some(e) = self
            .entries
            .iter_mut()
            .flatten()
            .find(|e| e.id == id && e.terminal.is_none())
        {
            e.playback.pause();
        }
    }
    pub fn resume(&mut self, id: MediaId) {
        if let Some(e) = self
            .entries
            .iter_mut()
            .flatten()
            .find(|e| e.id == id && e.terminal.is_none() && !e.stopping)
        {
            e.playback.resume();
        }
    }
    pub fn cancel_owner(
        &mut self,
        cursor: &mut AudioOwnerCancellation,
        limit: usize,
    ) -> AudioActivityProgress {
        let capacity = self.entries.len();
        let start = cursor.next.min(capacity);
        let end = start.saturating_add(limit).min(self.entries.len());
        for slot in &mut self.entries[start..end] {
            if let Some(e) = slot
                && e.scope == cursor.scope
                && e.settle(AudioActivityTerminal::Cancelled)
            {
                self.active -= 1;
                self.drain_remaining = capacity;
                self.wake.wake();
            }
        }
        cursor.next = end;
        let needs_turn = end < self.entries.len();
        if needs_turn {
            self.wake.wake();
        }
        AudioActivityProgress {
            inspected: end - start,
            needs_turn,
        }
    }
    /// Called after the output faults or is stopped permanently. Advances the
    /// service generation, but never rewrites accepted request identities or
    /// settled terminals. Live activities receive Failed with their ORIGINAL
    /// epoch during bounded maintenance, so existing exact waits can settle.
    /// This is NOT recovery. Host must fence/drop old fill.
    /// Repeated faults are idempotent; exhaustion refuses without wrapping.
    pub fn fail_output(&mut self, reason: AudioErrorKind) -> Result<MediaEpoch, AudioError> {
        if self.failed.is_some() {
            return Ok(self.epoch);
        }
        self.epoch = MediaEpoch(self.epoch.0.checked_add(1).ok_or_else(|| {
            AudioError::new(AudioErrorKind::Unavailable, "media epoch exhausted")
        })?);
        self.failed = Some(reason);
        self.maintenance_remaining = self.entries.len();
        self.drain_remaining = self.entries.len();
        self.wake.wake();
        Ok(self.epoch)
    }
    pub fn maintain(
        &mut self,
        activity_limit: usize,
        reclaim_limit: usize,
    ) -> AudioActivityMaintenance {
        let inspected = activity_limit.min(self.maintenance_remaining);
        for _ in 0..inspected {
            if let Some(e) = &mut self.entries[self.maintenance_cursor]
                && e.terminal.is_none()
            {
                let terminal = if let Some(reason) = self.failed {
                    Some(AudioActivityTerminal::Failed(reason))
                } else if e.playback.is_stopped() {
                    Some(if e.stopping {
                        AudioActivityTerminal::Stopped {
                            position_seconds: e.playback.position_seconds(),
                        }
                    } else {
                        AudioActivityTerminal::Completed {
                            position_seconds: e.playback.position_seconds(),
                        }
                    })
                } else {
                    None
                };
                if let Some(terminal) = terminal {
                    e.settle(terminal);
                    self.active -= 1;
                    self.drain_remaining = self.entries.len();
                    self.wake.wake();
                }
            }
            self.maintenance_cursor = (self.maintenance_cursor + 1) % self.entries.len();
        }
        // A partial scan conservatively requests another turn. Complete scans
        // rely on retirement/output/worker wakes plus the poll_after fallback.
        self.maintenance_remaining -= inspected;
        let needs_turn = self.maintenance_remaining != 0 && self.occupied != 0;
        if self.maintenance_remaining == 0 {
            self.maintenance_remaining = self.entries.len();
        }
        if needs_turn {
            self.wake.wake();
        }
        AudioActivityMaintenance {
            activities: AudioActivityProgress {
                inspected,
                needs_turn,
            },
            reclaim: self.mixer.reclaim_unused(reclaim_limit),
        }
    }
    /// Drain owns disposal even for stale results. The predicate revalidates the
    /// complete accepted (ID, epoch, Scope) against the live wait, NOT against
    /// the service's current output generation. Settled terminals survive output
    /// faults; Cancelled never resumes business. At most limit
    /// slots are visited, and the caller's Vec NEVER grows beyond its capacity.
    /// Revalidate identity once more at actual business delivery if it is delayed.
    /// A full destination retains deliverable records but does not stop scanning.
    /// After one complete scan, consumer backpressure alone requests no wake:
    /// call drain again when destination capacity is freed (or explicitly scheduled).
    pub fn drain(
        &mut self,
        out: &mut Vec<AudioActivityEvent>,
        limit: usize,
        mut is_live: impl FnMut(MediaId, MediaEpoch, Scope) -> bool,
    ) -> AudioActivityProgress {
        let mut inspected = 0;
        while inspected < limit.min(self.drain_remaining) {
            let slot = &mut self.entries[self.drain_cursor];
            if let Some(e) = slot
                && let Some(terminal) = e.terminal
            {
                let deliver =
                    terminal != AudioActivityTerminal::Cancelled && is_live(e.id, e.epoch, e.scope);
                // Backpressure retains this terminal but cannot block disposal
                // of cancelled/stale records in later slots. Charge and advance
                // even when the caller has no room for a deliverable event.
                if !deliver || out.len() < out.capacity() {
                    let e = slot.take().expect("terminal owns slot");
                    self.occupied -= 1;
                    self.descriptor_bytes -= e.resource.key.0.capacity() as u64;
                    if deliver {
                        out.push(AudioActivityEvent {
                            id: e.id,
                            epoch: e.epoch,
                            scope: e.scope,
                            terminal,
                            position_seconds: e.position_seconds(),
                        });
                    }
                }
            }
            self.drain_cursor = (self.drain_cursor + 1) % self.entries.len();
            inspected += 1;
        }
        self.drain_remaining -= inspected;
        let needs_turn = self.drain_remaining != 0 && self.occupied != 0;
        if self.drain_remaining == 0 {
            self.drain_remaining = self.entries.len();
        }
        if needs_turn {
            self.wake.wake();
        }
        AudioActivityProgress {
            inspected,
            needs_turn,
        }
    }
}
impl Drop for AudioActivityService {
    fn drop(&mut self) {
        for owner in self.owners.iter().filter_map(|slot| slot.state.upgrade()) {
            owner.closed.store(true, Ordering::Release);
        }
        for e in self.entries.iter_mut().flatten() {
            e.playback.stop();
        }
        // Mixer Drop uses its reserved worker credits, never destroys PCM here.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AudioFormat, AudioReclaimService, ChannelLayout, MixerLimits, ReclaimLimits, SampleFormat,
    };
    use reel_core::{
        budget::BudgetCategory,
        identity::{AssetKey, ContentVersion},
    };

    #[test]
    fn identity_exhaustion_never_wraps_and_receipt_keeps_admitted_epoch() {
        let wake = WakeSignal::new(|| {});
        let mut reclaim = AudioReclaimService::new(ReclaimLimits::default(), wake.clone()).unwrap();
        let format =
            AudioFormat::new(48_000, SampleFormat::F32, ChannelLayout::Stereo, 512).unwrap();
        let mixer = KiraMixer::new(&format, MixerLimits::default(), &reclaim).unwrap();
        let mut service = AudioActivityService::new(
            mixer,
            QueueLimits {
                max_requests: 2,
                max_bytes: AudioActivityService::slot_bytes(2) + 100,
            },
            MediaEpoch(u64::MAX - 1),
            wake,
        )
        .ok()
        .unwrap();
        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 2,
                max_bytes: 2048,
            },
        );
        let owner = service.register_owner(Scope::Process).unwrap();
        let request = || AudioActivityRequest {
            epoch: MediaEpoch(u64::MAX - 1),
            owner: owner.clone(),
            resource: ResourceVersion {
                key: AssetKey("tone".into()),
                version: ContentVersion([0; 32]),
            },
            group: AudioGroup::Music,
            sound: DecodedSound::new(48_000, vec![[0.0; 2]; 64], pool.reserve(1024).unwrap())
                .unwrap(),
        };
        service.next_id = Some(u64::MAX);
        let id = service.play(request()).unwrap();
        assert_eq!(id, MediaId(u64::MAX));
        let receipt = service.retirement(id).unwrap();
        let rejected = service.play(request()).unwrap_err();
        assert_eq!(rejected.reason.code, RejectCode::ServiceFault);
        assert_eq!(service.occupied(), 1);
        drop(rejected);
        assert_eq!(
            service.fail_output(AudioErrorKind::DeviceFailure).unwrap(),
            MediaEpoch(u64::MAX)
        );
        assert_eq!(
            service.fail_output(AudioErrorKind::DeviceFailure).unwrap(),
            MediaEpoch(u64::MAX)
        );
        service.maintain(2, 2);
        service.retire_owner(&owner, 2).unwrap();
        let mut events = Vec::with_capacity(1);
        service.drain(&mut events, 2, |_, _, _| true);
        assert_eq!(events[0].epoch, MediaEpoch(u64::MAX - 1));
        assert_eq!(receipt.epoch(), MediaEpoch(u64::MAX - 1));
        assert_eq!(receipt.id(), id);
        assert_eq!(receipt.scope(), Scope::Process);
        drop(service);
        reclaim.drain_and_join().unwrap();
        assert!(receipt.is_retired());
        assert!(owner.is_retired());
    }
}
