use reel_core::{
    budget::{BudgetCategory, BudgetPool, QueueLimits},
    clock::MonoTime,
    identity::{AssetKey, ContentVersion, Epoch, OwnerId, ResourceVersion, Scope, SessionId},
    result::RejectCode,
    wake::WakeSignal,
};
use reel_media::*;

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
}
impl Rig {
    fn new(slots: u32, key_bytes: u64) -> Self {
        Self::with_mixer_capacity(slots, key_bytes, 64)
    }
    fn with_mixer_capacity(slots: u32, key_bytes: u64, max_sounds: usize) -> Self {
        let wake = WakeSignal::new(|| {});
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
        assert!(output.start(activities.take_fill().unwrap()).is_ok());
        assert!(activities.take_fill().is_none());
        Self {
            reclaim,
            output,
            activities: Some(activities),
            wake,
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
    fn request(&self, owner: u64, looping: bool) -> AudioActivityRequest {
        let frames = vec![[0.25, -0.25]; 64];
        let ticket = self
            .pool
            .reserve((frames.capacity() + frames.len()) as u64 * 8)
            .unwrap();
        let sound = DecodedSound::new(48_000, frames, ticket).unwrap();
        AudioActivityRequest {
            epoch: MediaEpoch(7),
            scope: scope(owner),
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
    let p = rig.service().drain(&mut no_capacity, 2, |_, _| true);
    assert!(p.needs_turn);
    assert_eq!(no_capacity.capacity(), 0);
    let mut out = Vec::with_capacity(1);
    rig.service().drain(&mut out, 2, |_, _| true);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].id, id);
    assert_eq!(out[0].scope, scope(3));
    out.clear();
    rig.service().maintain(2, 1);
    rig.service().drain(&mut out, 2, |_, _| true);
    assert!(out.is_empty());
}

#[test]
fn rejection_returns_original_request_and_terminal_reserves_slot_until_drain() {
    let mut rig = Rig::new(1, 4);
    let first = rig.play(1, true);
    assert!(rig.service().stop(first));
    let request = rig.request(2, false);
    let ptr = request.resource.key.0.as_ptr();
    let bytes = request.sound.resident_bytes();
    let rejected = rig.service().play(request).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Busy);
    assert_eq!(rejected.request.resource.key.0.as_ptr(), ptr);
    assert_eq!(rejected.request.sound.resident_bytes(), bytes);
    assert_eq!(rejected.request.scope, scope(2));
    let mut events = Vec::with_capacity(1);
    rig.service().drain(&mut events, 1, |_, _| true);
    let next = rig.service().play(rejected.request).unwrap();
    assert_ne!(first, next);
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
        rig.service().drain(&mut events, 1, |_, s| s == scope(2));
    }
    assert!(events.is_empty());
    assert_eq!(rig.service().occupied(), 1);
    assert!(rig.service().snapshot(a).is_none());
    assert!(rig.service().snapshot(c).is_none());
}

#[test]
fn epoch_fault_filters_old_terminal_and_fails_live_activity_once() {
    let mut rig = Rig::new(3, 100);
    let old = rig.play(1, true);
    rig.service().stop(old);
    let active = rig.play(1, true);
    rig.output.stop().unwrap();
    let epoch = rig
        .service()
        .fail_output(AudioErrorKind::ReconfigureRequired)
        .unwrap();
    assert_eq!(epoch, MediaEpoch(8));
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
        rig.service().drain(&mut events, 1, |_, _| true);
    }
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, active);
    assert_eq!(events[0].epoch, epoch);
    assert_eq!(
        events[0].terminal,
        AudioActivityTerminal::Failed(AudioErrorKind::ReconfigureRequired)
    );
    let request = rig.request(1, false);
    let rejected = rig.service().play(request).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Unavailable);
    drop(rejected);
    events.clear();
    rig.service().maintain(3, 3);
    rig.service().drain(&mut events, 3, |_, _| true);
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
        let p = rig.service().drain(&mut events, 1, |_, _| true);
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
    request.scope = Scope::Session {
        session_id: SessionId(1),
        epoch: Epoch(3),
        owner_id: OwnerId(1),
    };
    let newer_session = rig.service().play(request).unwrap();
    let mut request = rig.request(1, true);
    request.scope = Scope::Process;
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
    rig.service().drain(&mut out, 4, |_, _| false);
    assert!(rig.service().snapshot(cancelled).is_none());
    assert_eq!(rig.service().occupied(), 2);
    assert_eq!(out.capacity(), 0);
    assert!(rig.service().poll_after().is_some());
    rig.service().stop(newer_session);
    rig.service().stop(process);
    assert!(rig.service().poll_after().is_none());
    rig.service().drain(&mut out, 4, |_, _| false);
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
    let key_pointer = request.resource.key.0.as_ptr();
    let pcm_bytes = request.sound.resident_bytes();
    let rejected = rig.service().play(request).unwrap_err();
    assert_eq!(rejected.reason.code, RejectCode::Busy);
    assert_eq!(rejected.request.resource.key.0.as_ptr(), key_pointer);
    assert_eq!(rejected.request.sound.resident_bytes(), pcm_bytes);
    assert_eq!(rig.service().occupied(), 1);
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
    rig.service().drain(&mut events, 2, |_, _| true);
    assert_eq!(events[0].epoch, MediaEpoch(8));
    assert_eq!(
        events[0].terminal,
        AudioActivityTerminal::Failed(AudioErrorKind::DeviceFailure)
    );
}
