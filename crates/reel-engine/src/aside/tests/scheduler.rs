//! Scheduler maintenance costs use real scoped tasks and clock subscriptions.
use super::{
    models::{drive, prepared},
    *,
};

fn waiting_tasks(count: usize) -> (Session, SessionVm) {
    let code = format!(
        r#"fn opening() {{
        let timer = clock(interval: 1);
        for i in 0..{count} {{
            spawn(|| scope(body: |s| {{ timer.next(); }}));
        }}
        await completion();
    }}"#
    );
    let (mut session, mut vm) = prepared(&code, false);
    drive(&mut vm, &mut session, false);
    assert_eq!(vm.tasks.len(), count);
    assert_eq!(vm.scopes.live.len(), count);
    (session, vm)
}

fn settle_once(vm: &mut SessionVm, session: &mut Session) {
    assert!(matches!(
        vm.step(session, SessionBudget::Exact(SliceBudget::default()), || 0,)
            .unwrap(),
        SessionStep::Vm(RunOutcome::Waiting(_))
    ));
}

#[test]
fn unchanged_waits_reuse_checks_and_clock_completion_releases_every_scope() {
    let (mut session, mut vm) = waiting_tasks(16);
    settle_once(&mut vm, &mut session);
    let checks = (
        vm.callback_lifetime_checks,
        vm.scopes.lifetime_checks,
        vm.activities.wait_checks,
    );
    for _ in 0..128 {
        assert!(!vm.has_runnable_tasks());
        settle_once(&mut vm, &mut session);
    }
    assert_eq!(
        checks,
        (
            vm.callback_lifetime_checks,
            vm.scopes.lifetime_checks,
            vm.activities.wait_checks
        )
    );
    activities::advance_clock(&mut vm, &session, 1., 1);
    drive(&mut vm, &mut session, false);
    assert!(vm.tasks.is_empty());
    assert!(vm.scopes.live.is_empty());
    assert!(vm.activities.wait_checks > checks.2);
}

#[test]
fn owner_revocation_cancels_a_cached_blocked_task_without_advancing_time() {
    let (mut session, mut vm) = waiting_tasks(2);
    settle_once(&mut vm, &mut session);
    let before = vm.activities.time();
    let owner = vm.tasks[0].owner;
    session
        .revoke_owner_with_retirement(
            owner,
            OwnerRetirementRequirements {
                tasks_pending: true,
                ..Default::default()
            },
        )
        .unwrap();
    drive(&mut vm, &mut session, false);
    assert_eq!(vm.tasks.len(), 1);
    assert_eq!(vm.scopes.live.len(), 1);
    assert_eq!(
        session.owner_lifecycle(owner),
        Some(OwnerLifecycle::Retired)
    );
    assert_eq!(vm.activities.time(), before);
}

#[test]
fn restoring_cached_waits_rebuilds_the_task_index_and_subscription_checks() {
    let (mut session, mut vm) = waiting_tasks(16);
    settle_once(&mut vm, &mut session);
    let old_wait = vm.tasks[0].wait.unwrap();
    (session, vm) = scopes::restored(&mut vm, &session);
    assert!(!vm.vm.wait_is_pending(old_wait));
    assert!(
        vm.tasks
            .iter()
            .all(|task| vm.vm.wait_is_pending(task.wait.unwrap()))
    );
    collect(&mut vm);
    activities::advance_clock(&mut vm, &session, 1., 1);
    drive(&mut vm, &mut session, false);
    assert!(vm.tasks.is_empty());
    assert!(vm.scopes.live.is_empty());
}

#[test]
fn scope_host_retirement_invalidates_cached_callbacks_at_the_same_time() {
    let code = r#"fn opening() {
        scope(body: |parent| {
            parent.spawn(|| { await completion(); throw "retired task resumed"; });
            narrator("inside");
        });
        narrator("after");
    }"#;
    for encoded in [false, true] {
        let (mut session, mut vm) = prepared(code, encoded);
        drive(&mut vm, &mut session, false);
        settle_once(&mut vm, &mut session);
        let time = vm.activities.time();
        let stage_retirement = vm.stage.retirement_revision;
        let host_retirement = vm.vm.heap().host_retirement_revision();
        let id = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap();
        }
        drive(&mut vm, &mut session, false);
        assert!(vm.tasks.is_empty());
        assert!(vm.scopes.live.is_empty());
        assert_eq!(vm.activities.time(), time);
        assert_eq!(vm.stage.retirement_revision, stage_retirement);
        assert!(vm.vm.heap().host_retirement_revision() > host_retirement);
        assert!(
            matches!(session.view().interaction, Some(crate::InteractionView::Dialogue(dialogue)) if dialogue.text() == "after")
        );
    }
}

#[test]
#[ignore = "isolated scheduler maintenance cost measurement"]
fn unchanged_waiting_scheduler_cost() {
    for count in [16, 64, 128] {
        let (mut session, mut vm) = waiting_tasks(count);
        let started = std::time::Instant::now();
        for _ in 0..2_000 {
            let outcome = vm
                .step(
                    &mut session,
                    SessionBudget::Exact(SliceBudget::default()),
                    || 0,
                )
                .unwrap();
            assert!(matches!(outcome, SessionStep::Vm(RunOutcome::Waiting(_))));
        }
        println!(
            "scheduler,count={count},steps=2000,ns_per_step={}",
            started.elapsed().as_nanos() / 2_000
        );
    }
}
