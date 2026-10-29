//! An accepted retained candidate can leave ready work before the next timer.
use super::*;

#[test]
fn retained_acceptance_leaves_a_ready_followup_before_the_next_story_deadline() {
    let mut f = Fixture::new();
    f.sync(0);
    assert!(f.render().accepted);
    f.sync(0); // Accepting the first stage starts the one-second tween.
    let first = f.view.bridge.accepted_key().unwrap().clone();

    f.sync(16_666_667);
    assert!(!f.render().accepted, "the one-slot backend is still busy");
    assert!(f.view.is_preparing());
    assert!(!f.view.needs_redraw(), "pressure must wait for completion");
    let retained = f.receipt.as_ref().unwrap().key();
    let builds = f.builds;

    // A later animation sample settles while the already recorded candidate is
    // retained. The host consumes this turn's redraw request before rendering.
    f.sync(33_333_334);
    assert_eq!(f.view.bridge.accepted_key(), Some(&first));
    assert!(!f.view.needs_redraw(), "new samples cannot bypass pressure");
    f.complete();
    assert!(f.view.needs_redraw());

    // Model five milliseconds of host/backend work after the last producer
    // sample. Acceptance must not rearm that producer's sampling interval.
    f.host.now = MonoTime(38_333_334);
    assert!(f.render().accepted);
    assert_eq!(f.view.bridge.accepted_key(), Some(&retained));
    assert_eq!(f.builds, builds, "completion continues the old candidate");
    assert!(
        f.view.needs_redraw(),
        "the newer settled animation is immediately eligible after acceptance"
    );
    assert!(!f.view.is_preparing());
    let next = f.story.wake_deadline(f.host.now).unwrap();
    assert_eq!(next, MonoTime(50_000_000));
    assert!(next > f.host.now);
    eprintln!(
        "retained accepted at {}ns: ready=true, story_deadline={}ns, avoidable_wait={}ns",
        f.host.now.0,
        next.0,
        next.0 - f.host.now.0
    );

    // The next host turn can build immediately. A still occupied backend may
    // then retain that new candidate, restoring the ordinary completion wait.
    assert!(!f.render().accepted);
    assert_eq!(f.builds, builds + 1);
    assert!(f.view.is_preparing());
    assert!(!f.view.needs_redraw());
}
