//! Bounded control-side audio activities (design 1.1).
//!
//! The host revokes waits before cancellation, acknowledges its shared WakeSignal
//! before servicing ALL sources, and rechecks pending before sleeping. Use the
//! same signal for this ledger, output and AudioReclaimService: Kira retirement
//! wakes through the existing non-RT reclaimer. Never call these methods on PCM.
//!
//! IDs are unique within one service lifetime; the host supplies a fresh epoch
//! when replacing a service. No seek/reload or hardware-completion claim is made.
use std::time::Duration;

use crate::{
    AudioError, AudioErrorKind, DecodedSound, KiraMixer, PcmFill, Playback, ReclaimProgress,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioGroup {
    Music,
    Voice,
    Effects,
    Ui,
}
/// PCM and its existing ticket move unchanged on rejection. Loop configuration
/// belongs to DecodedSound; group is routing metadata, not a new Kira sub-track.
#[derive(Debug)]
pub struct AudioActivityRequest {
    pub epoch: MediaEpoch,
    pub scope: Scope,
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
    terminal: Option<AudioActivityTerminal>,
}
impl Entry {
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
/// max_bytes charges fixed slot storage plus owned asset-key capacities. PCM is
/// independently charged by the existing BudgetTicket/reclamation protocol.
/// Limits: 1..=256 slots; every sweep/drain inspects at most min(limit, slots).
pub struct AudioActivityService {
    mixer: KiraMixer,
    entries: Vec<Option<Entry>>,
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
            || !(1..=256).contains(&limits.max_requests)
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
        u64::from(slots) * std::mem::size_of::<Option<Entry>>() as u64
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
        } else if request.epoch != self.epoch {
            Some((RejectCode::InvalidRequest, "stale media epoch"))
        } else if self.next_id.is_none() {
            Some((RejectCode::ServiceFault, "media ID space exhausted"))
        } else if self.occupied == self.entries.len()
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
            scope,
            resource,
            group,
            sound,
        } = request;
        let playback = match self.mixer.play(sound) {
            Ok(playback) => playback,
            Err(Rejected {
                request: sound,
                reason,
            }) => {
                return Err(Rejected {
                    request: AudioActivityRequest {
                        epoch,
                        scope,
                        resource,
                        group,
                        sound,
                    },
                    reason,
                });
            }
        };
        let id = MediaId(self.next_id.expect("checked before acceptance"));
        self.next_id = id.0.checked_add(1);
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
            volume_db: 0.0,
            playback,
            terminal: None,
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
            position_seconds: match e.terminal {
                Some(
                    AudioActivityTerminal::Completed { position_seconds }
                    | AudioActivityTerminal::Stopped { position_seconds },
                ) => position_seconds,
                _ => e.playback.position_seconds(),
            },
            terminal: e.terminal,
        })
    }
    /// False means absent or already settled; an existing terminal is immutable.
    pub fn stop(&mut self, id: MediaId) -> bool {
        let Some(e) = self.entries.iter_mut().flatten().find(|e| e.id == id) else {
            return false;
        };
        let terminal = if let Some(reason) = self.failed {
            if e.terminal.is_none() {
                e.epoch = self.epoch;
            }
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
    pub fn set_volume_db(&mut self, id: MediaId, db: f32) -> Result<(), AudioError> {
        if self.failed.is_some() {
            return Err(AudioError::new(
                AudioErrorKind::Unavailable,
                "output recovery is required",
            ));
        }
        let e = self
            .entries
            .iter_mut()
            .flatten()
            .find(|e| e.id == id && e.terminal.is_none())
            .ok_or_else(|| {
                AudioError::new(AudioErrorKind::InvalidRequest, "no live audio activity")
            })?;
        e.playback.set_volume_db(db)?;
        e.volume_db = db;
        Ok(())
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
    /// Called after the output faults or is stopped permanently. Invalidates old
    /// queued events; live IDs survive to receive Failed in the new epoch during
    /// bounded maintenance. This is NOT recovery. Host must fence/drop old fill.
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
                    e.epoch = self.epoch;
                    Some(AudioActivityTerminal::Failed(reason))
                } else if e.playback.is_stopped() {
                    Some(AudioActivityTerminal::Completed {
                        position_seconds: e.playback.position_seconds(),
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
    /// Drain owns disposal even for stale results. The predicate must revalidate
    /// live Scope/wait identity; Cancelled never resumes business. At most limit
    /// slots are visited, and the caller's Vec NEVER grows beyond its capacity.
    /// Revalidate identity once more at actual business delivery if it is delayed.
    pub fn drain(
        &mut self,
        out: &mut Vec<AudioActivityEvent>,
        limit: usize,
        mut is_live: impl FnMut(MediaId, Scope) -> bool,
    ) -> AudioActivityProgress {
        let mut inspected = 0;
        while inspected < limit.min(self.drain_remaining) {
            let slot = &mut self.entries[self.drain_cursor];
            if let Some(e) = slot
                && let Some(terminal) = e.terminal
            {
                let deliver = e.epoch == self.epoch
                    && terminal != AudioActivityTerminal::Cancelled
                    && is_live(e.id, e.scope);
                if deliver && out.len() == out.capacity() {
                    break;
                }
                let e = slot.take().expect("terminal owns slot");
                self.occupied -= 1;
                self.descriptor_bytes -= e.resource.key.0.capacity() as u64;
                if deliver {
                    out.push(AudioActivityEvent {
                        id: e.id,
                        epoch: e.epoch,
                        scope: e.scope,
                        terminal,
                    });
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
        for e in self.entries.iter_mut().flatten() {
            e.playback.stop();
        }
        // Mixer Drop uses its reserved worker credits, never destroys PCM here.
    }
}
