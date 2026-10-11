use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    clock::MonoTime,
    identity::{AssetKey, ContentVersion, Epoch, OwnerId, ResourceVersion, Scope, SessionId},
    result::RejectCode,
    wake::WakeSignal,
};
use reel_media::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn scope(owner: u64) -> Scope {
    Scope::Session {
        session_id: SessionId(1),
        epoch: Epoch(2),
        owner_id: OwnerId(owner),
    }
}
struct Rig {
    reclaim: AudioReclaimService,
    output: OfflineOutput,
    activities: Option<AudioActivityService>,
    pool: BudgetPool,
    wake: WakeSignal,
    control_wakes: Arc<AtomicUsize>,
}
impl Rig {
    fn new(slots: u32, key_bytes: u64) -> Self {
        Self::with_mixer_capacity(slots, key_bytes, 64)
    }
    fn with_mixer_capacity(slots: u32, key_bytes: u64, max_sounds: usize) -> Self {
        Self::with_fill(slots, key_bytes, max_sounds, true)
    }
    fn with_fill(slots: u32, key_bytes: u64, max_sounds: usize, take_fill: bool) -> Self {
        let control_wakes = Arc::new(AtomicUsize::new(0));
        let count = control_wakes.clone();
        let control_thread = std::thread::current().id();
        let wake = WakeSignal::new(move || {
            if std::thread::current().id() == control_thread {
                count.fetch_add(1, Ordering::Relaxed);
            }
        });
        let reclaim = AudioReclaimService::new(ReclaimLimits::default(), wake.clone()).unwrap();
        let mut output = OfflineOutput::new(
            AudioDeviceConfig {
                sample_rate_hz: 48_000,
                sample_format: SampleFormat::F32,
                layout: ChannelLayout::Stereo,
            },
            wake.clone(),
        )
        .unwrap();
        let format = output.configure(AudioRequest::default()).unwrap();
        let mixer = KiraMixer::new(
            &format,
            MixerLimits {
                max_sounds,
                ..MixerLimits::default()
            },
            &reclaim,
        )
        .unwrap();
        let mut activities = AudioActivityService::new(
            mixer,
            QueueLimits {
                max_requests: slots,
                max_bytes: AudioActivityService::slot_bytes(slots) + key_bytes,
            },
            MediaEpoch(7),
            wake.clone(),
        )
        .ok()
        .unwrap();
        if take_fill {
            assert!(output.start(activities.take_fill().unwrap()).is_ok());
            assert!(activities.take_fill().is_none());
        }
        Self {
            reclaim,
            output,
            activities: Some(activities),
            wake,
            control_wakes,
            pool: BudgetPool::new(
                BudgetCategory::CpuOutput,
                QueueLimits {
                    max_requests: 32,
                    max_bytes: 1_000_000,
                },
            ),
        }
    }
    fn service(&mut self) -> &mut AudioActivityService {
        self.activities.as_mut().unwrap()
    }
    fn request(&mut self, owner: u64, looping: bool) -> AudioActivityRequest {
        let frames = vec![[0.25, -0.25]; 64];
        let ticket = self
            .pool
            .reserve((frames.capacity() + frames.len()) as u64 * 8)
            .unwrap();
        let sound = DecodedSound::new(48_000, frames, ticket).unwrap();
        AudioActivityRequest {
            epoch: MediaEpoch(7),
            owner: self.service().register_owner(scope(owner)).unwrap(),
            resource: ResourceVersion {
                key: AssetKey("tone".into()),
                version: ContentVersion([9; 32]),
            },
            group: AudioGroup::Voice,
            sound: if looping { sound.looping() } else { sound },
        }
    }
    fn play(&mut self, owner: u64, looping: bool) -> MediaId {
        let request = self.request(owner, looping);
        self.service().play(request).unwrap()
    }
    fn render(&mut self) {
        self.output.render(
            &mut [0.0f32; 1024],
            CallbackTiming {
                now: MonoTime(1),
                latency_frames: Some(500),
                uncertainty_frames: None,
            },
        );
    }
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.output.stop().unwrap();
        drop(self.activities.take());
        self.reclaim.drain_and_join().unwrap();
        assert_eq!(
            self.pool.usage().bytes,
            0,
            "accepted PCM releases through worker"
        );
    }
}

#[test]
fn content_terminal_once_without_claiming_presentation_and_without_vector_growth() {
    let mut rig = Rig::new(2, 100);
    let id = rig.play(3, false);
    rig.render();
    // The controlled output has submitted a block but has presented none of it.
    assert_eq!(rig.output.clock_observation().presented_frames(), Some(0));
    rig.service().maintain(2, 1);
    assert!(matches!(
        rig.service().snapshot(id).unwrap().terminal,
        Some(AudioActivityTerminal::Completed { .. })
    ));
    assert!(!rig.service().stop(id));
    let mut no_capacity = Vec::new();
    let p = rig.service().drain(&mut no_capacity, 2, |_, _, _| true);
    assert_eq!(p.inspected, 2);
    assert!(!p.needs_turn);
    assert_eq!(no_capacity.capacity(), 0);
    let mut out = Vec::with_capacity(1);
    rig.service().drain(&mut out, 2, |_, _, _| true);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].id, id);
    assert_eq!(out[0].scope, scope(3));
    out.clear();
    rig.service().maintain(2, 1);
    rig.service().drain(&mut out, 2, |_, _, _| true);
    assert!(out.is_empty());
}

#[test]
fn rejection_returns_original_request_and_physical_release_reserves_slot_after_drain() {
    let mut rig = Rig::new(1, 4);
    let first = rig.play(1, true);
    assert!(rig.service().stop(first));
    let request = rig.request(1, false);
    let ptr = request.resource.key.0.as_ptr();
    let bytes = request.sound.resident_bytes();
    let rejected = rig.service().play(request).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Busy);
    assert_eq!(rejected.request.resource.key.0.as_ptr(), ptr);
    assert_eq!(rejected.request.sound.resident_bytes(), bytes);
    assert_eq!(rejected.request.owner.scope(), scope(1));
    let mut events = Vec::with_capacity(1);
    rig.service().drain(&mut events, 1, |_, _, _| true);
    let still_resident = rig.service().play(rejected.request).unwrap_err();
    assert_eq!(still_resident.reason.code, RejectCode::Busy);
    drop(still_resident);
    assert!(rig.service().retirement(first).is_some());
    assert_eq!(
        events[0].terminal,
        AudioActivityTerminal::Stopped {
            position_seconds: 0.0
        }
    );
}

#[test]
fn byte_and_epoch_rejections_do_not_consume_request_or_id() {
    let mut rig = Rig::new(2, 4);
    let mut too_large = rig.request(1, false);
    too_large.resource.key.0 = String::with_capacity(100);
    too_large.resource.key.0.push('x');
    let rejected = rig.service().play(too_large).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Busy);
    assert_eq!(rejected.request.resource.key.0.capacity(), 100);
    drop(rejected);
    let mut stale = rig.request(1, false);
    stale.epoch = MediaEpoch(6);
    let rejected = rig.service().play(stale).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::InvalidRequest);
    assert_eq!(rejected.request.epoch, MediaEpoch(6));
    drop(rejected);
    assert_eq!(rig.play(1, false), MediaId(1));
}

#[test]
fn owner_sweep_is_bounded_preserves_prior_terminal_and_filters_revoked_results() {
    let mut rig = Rig::new(4, 100);
    let a = rig.play(1, true);
    let b = rig.play(2, true);
    let c = rig.play(1, true);
    rig.service().stop(c);
    let mut cancel = AudioOwnerCancellation::new(scope(1));
    assert_eq!(rig.service().cancel_owner(&mut cancel, 0).inspected, 0);
    for i in 0..4 {
        let p = rig.service().cancel_owner(&mut cancel, 1);
        assert_eq!(p.inspected, 1);
        assert_eq!(p.needs_turn, i != 3);
    }
    assert_eq!(rig.service().cancel_owner(&mut cancel, 1).inspected, 0);
    assert_eq!(
        rig.service().snapshot(a).unwrap().terminal,
        Some(AudioActivityTerminal::Cancelled)
    );
    assert!(rig.service().snapshot(b).unwrap().terminal.is_none());
    assert!(matches!(
        rig.service().snapshot(c).unwrap().terminal,
        Some(AudioActivityTerminal::Stopped { .. })
    ));
    let mut events = Vec::with_capacity(4);
    for _ in 0..4 {
        rig.service().drain(&mut events, 1, |_, _, s| s == scope(2));
    }
    assert!(events.is_empty());
    assert_eq!(rig.service().occupied(), 1);
    assert!(rig.service().snapshot(a).is_none());
    assert!(rig.service().snapshot(c).is_none());
}

#[test]
fn epoch_fault_preserves_settled_terminal_and_fails_live_activity_once() {
    let mut rig = Rig::new(3, 100);
    let old = rig.play(1, true);
    rig.service().stop(old);
    let active = rig.play(1, true);
    let receipt = rig.service().retirement(active).unwrap();
    assert_eq!(receipt.epoch(), MediaEpoch(7));
    assert_eq!(receipt.scope(), scope(1));
    rig.output.stop().unwrap();
    let epoch = rig
        .service()
        .fail_output(AudioErrorKind::ReconfigureRequired)
        .unwrap();
    assert_eq!(epoch, MediaEpoch(8));
    assert_eq!(receipt.epoch(), MediaEpoch(7));
    assert_eq!(receipt.scope(), scope(1));
    assert_eq!(
        rig.service()
            .fail_output(AudioErrorKind::DeviceFailure)
            .unwrap(),
        epoch
    );
    for _ in 0..3 {
        assert!(rig.service().maintain(1, 1).activities.inspected <= 1);
    }
    let mut events = Vec::with_capacity(3);
    for _ in 0..3 {
        rig.service().drain(&mut events, 1, |_, _, _| true);
    }
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].id, old);
    assert_eq!(events[0].epoch, MediaEpoch(7));
    assert_eq!(
        events[0].terminal,
        AudioActivityTerminal::Stopped {
            position_seconds: 0.0
        }
    );
    assert_eq!(events[1].id, active);
    assert_eq!(events[1].epoch, MediaEpoch(7));
    assert_eq!(
        events[1].terminal,
        AudioActivityTerminal::Failed(AudioErrorKind::ReconfigureRequired)
    );
    let request = rig.request(1, false);
    let rejected = rig.service().play(request).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Unavailable);
    drop(rejected);
    events.clear();
    rig.service().maintain(3, 3);
    rig.service().drain(&mut events, 3, |_, _, _| true);
    assert!(events.is_empty());
}

#[test]
fn small_quota_scans_finish_and_suspend_retains_logical_position() {
    let mut rig = Rig::new(4, 100);
    let mut request = rig.request(1, false);
    let frames = vec![[0.25, -0.25]; 4800];
    request.sound =
        DecodedSound::new(48_000, frames, rig.pool.reserve(4800 * 16).unwrap()).unwrap();
    let id = rig.service().play(request).unwrap();
    rig.render();
    rig.output.suspend().unwrap();
    let position = rig.service().snapshot(id).unwrap().position_seconds;
    rig.render();
    assert_eq!(
        rig.service().snapshot(id).unwrap().position_seconds,
        position
    );
    let mut events = Vec::with_capacity(1);
    for i in 0..4 {
        rig.wake.acknowledge();
        let p = rig.service().maintain(1, 1);
        assert_eq!(p.activities.needs_turn, i != 3);
        let p = rig.service().drain(&mut events, 1, |_, _, _| true);
        assert_eq!(p.needs_turn, i != 3);
    }
    assert!(events.is_empty());
    rig.service().set_volume_db(id, -6.0).unwrap();
    assert_eq!(rig.service().snapshot(id).unwrap().volume_db, -6.0);
    assert!(rig.service().set_volume_db(id, f32::NAN).is_err());
    rig.output.resume().unwrap();
    rig.render();
    assert_ne!(
        rig.service().snapshot(id).unwrap().position_seconds,
        position
    );
}

#[test]
fn cancellation_matches_complete_scope_and_stale_drain_needs_no_destination_space() {
    let mut rig = Rig::new(4, 100);
    let cancelled = rig.play(1, true);
    let mut request = rig.request(1, true);
    request.owner = rig
        .service()
        .register_owner(Scope::Session {
            session_id: SessionId(1),
            epoch: Epoch(3),
            owner_id: OwnerId(1),
        })
        .unwrap();
    let newer_session = rig.service().play(request).unwrap();
    let mut request = rig.request(1, true);
    request.owner = rig.service().register_owner(Scope::Process).unwrap();
    let process = rig.service().play(request).unwrap();
    rig.wake.acknowledge();
    let mut cancel = AudioOwnerCancellation::new(scope(1));
    rig.service().cancel_owner(&mut cancel, 4);
    assert!(rig.wake.is_pending());
    assert!(
        rig.service()
            .snapshot(newer_session)
            .unwrap()
            .terminal
            .is_none()
    );
    assert!(rig.service().snapshot(process).unwrap().terminal.is_none());
    let mut out = Vec::new();
    rig.service().drain(&mut out, 4, |_, _, _| false);
    assert!(rig.service().snapshot(cancelled).is_none());
    assert_eq!(rig.service().occupied(), 2);
    assert_eq!(out.capacity(), 0);
    assert!(rig.service().poll_after().is_some());
    rig.service().stop(newer_session);
    rig.service().stop(process);
    assert!(rig.service().poll_after().is_none());
    rig.service().drain(&mut out, 4, |_, _, _| false);
    assert_eq!(rig.service().occupied(), 0);
}

#[test]
fn completion_after_its_scan_is_revisited_by_explicit_poll_fallback() {
    let mut rig = Rig::new(4, 100);
    let id = rig.play(1, false);
    rig.service().maintain(1, 1); // Slot zero was still playing when inspected.
    rig.render(); // Completion/retirement wake can be acknowledged mid-sweep.
    for _ in 0..3 {
        rig.wake.acknowledge();
        rig.service().maintain(1, 1);
    }
    assert!(rig.service().snapshot(id).unwrap().terminal.is_none());
    assert!(rig.service().poll_after().is_some());
    rig.service().maintain(1, 1); // The scheduled fallback, no further PCM needed.
    assert!(matches!(
        rig.service().snapshot(id).unwrap().terminal,
        Some(AudioActivityTerminal::Completed { .. })
    ));
    assert!(rig.service().poll_after().is_none());
}

#[test]
fn underlying_kira_rejection_preserves_owned_request_without_ledger_acceptance() {
    let mut rig = Rig::with_mixer_capacity(3, 100, 1);
    rig.play(1, true);
    let request = rig.request(2, false);
    let owner = request.owner.clone();
    let key_pointer = request.resource.key.0.as_ptr();
    let pcm_bytes = request.sound.resident_bytes();
    let rejected = rig.service().play(request).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Busy);
    assert_eq!(rejected.request.resource.key.0.as_ptr(), key_pointer);
    assert_eq!(rejected.request.sound.resident_bytes(), pcm_bytes);
    assert_eq!(rig.service().occupied(), 1);
    rig.service().retire_owner(&owner, 3).unwrap();
    assert!(
        owner.is_retired(),
        "rejected request is not an admitted obligation"
    );
    assert_eq!(
        rig.pool.usage().tickets,
        2,
        "rejection still owns its original PCM"
    );
    drop(rejected);
}

#[test]
fn observed_output_failure_precedes_later_stop_and_volume_commands() {
    let mut rig = Rig::new(2, 100);
    let id = rig.play(1, true);
    rig.output.stop().unwrap();
    rig.service()
        .fail_output(AudioErrorKind::DeviceFailure)
        .unwrap();
    assert!(rig.service().set_volume_db(id, -3.0).is_err());
    assert!(rig.service().stop(id));
    assert!(!rig.service().stop(id));
    let mut events = Vec::with_capacity(1);
    rig.service().drain(&mut events, 2, |_, _, _| true);
    assert_eq!(events[0].epoch, MediaEpoch(7));
    assert_eq!(
        events[0].terminal,
        AudioActivityTerminal::Failed(AudioErrorKind::DeviceFailure)
    );
}

#[test]
fn completed_before_fault_survives_backpressure_and_exact_wait_validation_once() {
    let mut rig = Rig::new(2, 100);
    let id = rig.play(1, false);
    rig.render();
    rig.service().maintain(2, 1);
    let terminal = rig.service().snapshot(id).unwrap().terminal.unwrap();
    assert!(matches!(terminal, AudioActivityTerminal::Completed { .. }));
    rig.output.stop().unwrap();
    rig.service()
        .fail_output(AudioErrorKind::DeviceFailure)
        .unwrap();
    let mut full = Vec::new();
    let exact =
        |actual_id, epoch, owner| actual_id == id && epoch == MediaEpoch(7) && owner == scope(1);
    assert!(!rig.service().drain(&mut full, 2, exact).needs_turn);
    assert_eq!(rig.service().snapshot(id).unwrap().terminal, Some(terminal));
    let mut events = Vec::with_capacity(1);
    rig.service().drain(&mut events, 2, exact);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].terminal, terminal);
    assert_eq!(events[0].epoch, MediaEpoch(7));
    events.clear();
    rig.service().maintain(2, 1);
    rig.service().drain(&mut events, 2, exact);
    assert!(events.is_empty());
}

#[test]
fn stale_exact_wait_is_disposed_without_discarding_another_live_old_epoch_wait() {
    let mut rig = Rig::new(2, 100);
    let stale = rig.play(1, true);
    let live = rig.play(1, true);
    rig.service().stop(stale);
    rig.service().stop(live);
    rig.output.stop().unwrap();
    rig.service()
        .fail_output(AudioErrorKind::DeviceFailure)
        .unwrap();
    let mut events = Vec::with_capacity(1);
    rig.service().drain(&mut events, 2, |id, epoch, owner| {
        id == live && epoch == MediaEpoch(7) && owner == scope(1)
    });
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, live);
    assert_eq!(rig.service().occupied(), 0);
}

#[test]
fn full_destination_cannot_starve_cancelled_slots_or_self_wake_after_a_scan() {
    let mut rig = Rig::new(2, 100);
    let blocked = rig.play(1, true);
    let cancelled = rig.play(2, true);
    rig.service().stop(blocked);
    rig.service()
        .cancel_owner(&mut AudioOwnerCancellation::new(scope(2)), 2);
    let mut out = Vec::new();
    for round in 0..8 {
        rig.wake.acknowledge();
        let notifications = rig.control_wakes.load(Ordering::Relaxed);
        let progress = rig.service().drain(&mut out, 1, |_, _, _| true);
        if round % 2 == 1 {
            assert_eq!(rig.control_wakes.load(Ordering::Relaxed), notifications);
        }
        assert_eq!(progress.inspected, 1);
        assert_eq!(progress.needs_turn, round % 2 == 0);
        if round >= 1 {
            assert!(rig.service().snapshot(cancelled).is_none());
        }
        assert!(rig.service().snapshot(blocked).is_some());
    }
    assert!(rig.service().poll_after().is_none());
    out.reserve_exact(1); // Consumer makes space and explicitly schedules drain.
    for _ in 0..2 {
        rig.service().drain(&mut out, 1, |_, _, _| true);
    }
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].id, blocked);
    assert_eq!(rig.service().occupied(), 0);
}

#[test]
fn suspended_stop_and_cancel_do_not_publish_physical_retirement() {
    let mut rig = Rig::new(2, 100);
    let owner = rig.service().register_owner(scope(1)).unwrap();
    let alias = owner.clone();
    let id = rig.play(1, true);
    let receipt = rig.service().retirement(id).unwrap();
    let repeated = rig.service().retirement(id).unwrap();
    assert_eq!(receipt.id(), id);
    assert_eq!(receipt.epoch(), MediaEpoch(7));
    assert_eq!(receipt.scope(), scope(1));
    rig.output.suspend().unwrap();
    assert!(rig.service().stop(id));
    assert_eq!(rig.service().retire_owner(&owner, 0).unwrap().inspected, 0);
    assert!(alias.is_closed());
    assert!(!alias.is_retired());
    for _ in 0..2 {
        assert_eq!(rig.service().retire_owner(&owner, 1).unwrap().inspected, 1);
    }
    assert_eq!(rig.service().retire_owner(&owner, 1).unwrap().inspected, 0);
    let mut events = Vec::with_capacity(1);
    rig.service().drain(&mut events, 2, |_, _, _| true);
    rig.service().maintain(2, 2);
    assert!(receipt.descriptor_released());
    assert!(!receipt.resources_released());
    assert!(!repeated.is_retired());
    assert!(!owner.is_retired());
    let request = rig.request(1, false);
    assert!(request.owner.is_closed());
    let rejected = rig.service().play(request).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::InvalidRequest);
    drop(rejected);
    rig.output.stop().unwrap();
    drop(rig.activities.take());
    rig.reclaim.drain_and_join().unwrap();
    assert!(receipt.resources_released());
    assert!(repeated.is_retired());
    assert!(alias.is_retired());
}

#[test]
fn owner_identity_includes_all_scope_fields_and_process() {
    let mut rig = Rig::new(5, 100);
    let target = rig.service().register_owner(scope(1)).unwrap();
    let scopes = [
        Scope::Session {
            session_id: SessionId(2),
            epoch: Epoch(2),
            owner_id: OwnerId(1),
        },
        Scope::Session {
            session_id: SessionId(1),
            epoch: Epoch(3),
            owner_id: OwnerId(1),
        },
        scope(2),
        Scope::Process,
    ];
    let id = rig.play(1, true);
    let others: Vec<_> = scopes
        .into_iter()
        .map(|scope| {
            let mut request = rig.request(1, true);
            request.owner = rig.service().register_owner(scope).unwrap();
            let owner = request.owner.clone();
            (rig.service().play(request).unwrap(), owner)
        })
        .collect();
    rig.service().retire_owner(&target, usize::MAX).unwrap();
    assert_eq!(
        rig.service().snapshot(id).unwrap().terminal,
        Some(AudioActivityTerminal::Cancelled)
    );
    for (id, owner) in others {
        assert!(!owner.is_closed());
        assert!(rig.service().snapshot(id).unwrap().terminal.is_none());
    }
}

#[test]
fn foreign_owner_is_rejected_without_consuming_pcm() {
    let mut left = Rig::new(1, 100);
    let mut right = Rig::new(1, 100);
    let foreign = left.service().register_owner(scope(1)).unwrap();
    let mut request = right.request(1, false);
    request.owner = foreign.clone();
    let bytes = request.sound.resident_bytes();
    let rejected = right.service().play(request).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::InvalidRequest);
    assert_eq!(rejected.request.sound.resident_bytes(), bytes);
    drop(rejected);
    assert!(right.service().retire_owner(&foreign, 1).is_err());
    assert!(!foreign.is_closed());
}

#[test]
fn held_owner_and_activity_receipts_bound_capacity_until_dropped() {
    let mut rig = Rig::new(1, 100);
    let owner = rig.service().register_owner(scope(1)).unwrap();
    assert_eq!(
        rig.service().register_owner(scope(2)).unwrap_err().code,
        RejectCode::Busy
    );
    let id = rig.play(1, false);
    let receipt = rig.service().retirement(id).unwrap();
    rig.render();
    rig.service().maintain(1, 1);
    let mut events = Vec::with_capacity(1);
    rig.service().drain(&mut events, 1, |_, _, _| true);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !receipt.resources_released() {
        rig.render();
        rig.service().maintain(1, 1);
        assert!(
            std::time::Instant::now() < deadline,
            "worker did not release offline sound"
        );
        std::thread::yield_now();
    }
    assert!(receipt.is_retired());
    assert!(!owner.is_retired(), "open admission keeps owner live");
    let request = rig.request(1, false);
    let rejected = rig.service().play(request).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Busy);
    drop(rejected);
    rig.service().retire_owner(&owner, 1).unwrap();
    assert!(owner.is_retired());
    drop(receipt);
    assert!(rig.service().register_owner(scope(2)).is_err());
    drop(owner);
    let replacement = rig.service().register_owner(scope(2)).unwrap();
    assert!(!replacement.is_closed());
    assert!(!replacement.is_retired());
}

#[test]
fn owner_retirement_is_independent_of_other_owners_sharing_budget_pool() {
    let mut rig = Rig::new(2, 100);
    let first = rig.service().register_owner(scope(1)).unwrap();
    let second = rig.service().register_owner(scope(2)).unwrap();
    let a = rig.play(1, true);
    let b = rig.play(2, true);
    let a_receipt = rig.service().retirement(a).unwrap();
    let b_receipt = rig.service().retirement(b).unwrap();
    rig.service().retire_owner(&first, 2).unwrap();
    let mut ignored = Vec::new();
    rig.service().drain(&mut ignored, 2, |_, _, _| false);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !first.is_retired() {
        rig.render();
        rig.service().maintain(2, 2);
        assert!(
            std::time::Instant::now() < deadline,
            "owner failed to retire"
        );
        std::thread::yield_now();
    }
    assert!(a_receipt.is_retired());
    assert!(!b_receipt.resources_released());
    assert!(!second.is_closed());
    assert!(!second.is_retired());
    assert!(rig.pool.usage().bytes > 0);
    assert!(rig.service().snapshot(b).unwrap().terminal.is_none());
}

#[test]
fn service_drop_seals_owner_but_retained_output_fill_defers_resources() {
    let mut rig = Rig::new(1, 100);
    let owner = rig.service().register_owner(scope(1)).unwrap();
    let id = rig.play(1, true);
    let receipt = rig.service().retirement(id).unwrap();
    rig.render(); // Move the accepted Sound into the retained renderer arena.
    drop(rig.activities.take());
    assert!(owner.is_closed());
    assert!(receipt.descriptor_released());
    assert!(!receipt.resources_released());
    assert!(!owner.is_retired());
    rig.output.stop().unwrap();
    rig.reclaim.drain_and_join().unwrap();
    assert!(owner.is_retired());
    assert!(receipt.is_retired());
}

#[test]
fn service_drop_with_untaken_fill_releases_pcm_and_descriptor_on_worker() {
    let mut rig = Rig::with_fill(1, 100, 1, false);
    let owner = rig.service().register_owner(scope(1)).unwrap();
    let id = rig.play(1, true);
    let receipt = rig.service().retirement(id).unwrap();
    assert!(!receipt.resources_released());
    drop(rig.activities.take());
    assert!(owner.is_closed());
    rig.reclaim.drain_and_join().unwrap();
    assert!(receipt.descriptor_released());
    assert!(receipt.resources_released());
    assert!(receipt.is_retired());
    assert!(owner.is_retired());
    assert_eq!(rig.pool.usage().bytes, 0);
}

#[test]
fn physical_release_alone_cannot_retire_owner_with_undrained_descriptor() {
    let mut rig = Rig::new(1, 100);
    let owner = rig.service().register_owner(scope(1)).unwrap();
    let id = rig.play(1, false);
    let receipt = rig.service().retirement(id).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !receipt.resources_released() {
        rig.render();
        rig.service().maintain(1, 1);
        assert!(
            std::time::Instant::now() < deadline,
            "worker did not release PCM"
        );
        std::thread::yield_now();
    }
    rig.service().retire_owner(&owner, 1).unwrap();
    assert!(matches!(
        rig.service().snapshot(id).unwrap().terminal,
        Some(AudioActivityTerminal::Completed { .. })
    ));
    assert_eq!(rig.pool.usage().bytes, 0);
    assert!(!receipt.descriptor_released());
    assert!(!receipt.is_retired());
    assert!(!owner.is_retired());
    let mut full = Vec::new();
    rig.service().drain(&mut full, 1, |_, _, _| true);
    assert!(!owner.is_retired());
    rig.service().drain(&mut full, 1, |_, _, _| false);
    assert!(receipt.descriptor_released());
    assert!(receipt.is_retired());
    assert!(owner.is_retired());
}
