//! Unexecuted regression sources: VM/Aside execution is prohibited in this
//! integration. cargo check --all-targets only checks these Rust types.
use super::*;
use reel_engine::{Epoch, SessionId, SessionLimits};

fn session() -> Session {
    Session::new(SessionId(72), Epoch(1), SessionLimits::default()).unwrap()
}
fn driver(text: &str) -> AsideSessionUi {
    AsideSessionUi::from_source(
        session(),
        text,
        "story",
        1,
        "main",
        "en",
        VmLimits::default(),
    )
    .unwrap()
}
fn tick(driver: &mut AsideSessionUi) -> StoryProgress {
    driver.tick(SliceBudget::default(), 1, || 0).unwrap()
}
fn terminal(driver: &mut AsideSessionUi) {
    for _ in 0..10_000 {
        tick(driver);
        if driver.state() != StoryState::Running {
            return;
        }
    }
    panic!("terminal state not reached within source fixture bound")
}
fn cleanup(driver: &mut AsideSessionUi) {
    for _ in 0..10_000 {
        if !driver.cleanup_pending() {
            return;
        }
        driver
            .cleanup_step(1, SliceBudget::default(), || 0)
            .unwrap();
    }
    panic!("cleanup did not finish within source fixture bound")
}

#[test]
fn compiled_entry_uses_same_initializer_and_finish_contract() {
    let story = source::compile_story("fn main() {}", "story", 1, "main").unwrap();
    let mut d = AsideSessionUi::from_compiled(session(), story, "en", VmLimits::default()).unwrap();
    let mut initialized = false;
    for _ in 0..10_000 {
        match tick(&mut d) {
            StoryProgress::Initialized => initialized = true,
            StoryProgress::Finished => break,
            _ => {}
        }
    }
    assert!(initialized);
    assert_eq!(d.state(), StoryState::Finished);
    assert!(matches!(
        d.tick(SliceBudget::default(), 1, || panic!("terminal clock"))
            .unwrap(),
        StoryProgress::Finished
    ));
    d.cancel().unwrap();
    assert_eq!(d.state(), StoryState::Finished);
    assert!(d.fault().unwrap().is_none());
    cleanup(&mut d);
}

#[test]
fn compiled_metadata_cannot_substitute_entry_for_initializer() {
    let mut story = source::compile_story("fn main() {}", "story", 1, "main").unwrap();
    story.initializer = story.entry;
    assert!(AsideSessionUi::from_compiled(session(), story, "en", VmLimits::default()).is_err());
    let mut story = source::compile_story("fn main() {}", "story", 1, "main").unwrap();
    story.entry = source::StoryFunction(u32::MAX);
    assert!(AsideSessionUi::from_compiled(session(), story, "en", VmLimits::default()).is_err());
}

#[test]
fn fault_is_stable_inspectable_and_cancel_retires_delivery() {
    let mut d = driver("fn main() { throw \"boom\"; }");
    terminal(&mut d);
    let StoryState::Faulted(id) = d.state() else {
        panic!("expected fault")
    };
    let fault = d.fault().unwrap().unwrap();
    assert!(
        matches!(d.tick(SliceBudget::default(), 1, || panic!("terminal clock")).unwrap(), StoryProgress::Faulted(same) if same == id)
    );
    d.begin_collection().unwrap();
    // Enough bounded GC turns for this small fixture; delivery owns its graph.
    for _ in 0..10_000 {
        d.gc_step(GcBudget::default()).unwrap();
    }
    assert_eq!(d.fault().unwrap().unwrap(), fault);
    d.cancel().unwrap();
    d.cancel().unwrap();
    assert_eq!(d.state(), StoryState::Cancelled);
    assert!(d.fault().unwrap().is_none());
    assert!(matches!(
        d.tick(SliceBudget::default(), 1, || panic!("terminal clock"))
            .unwrap(),
        StoryProgress::Cancelled
    ));
    cleanup(&mut d);
    assert!(d.adapter().vm().delivery_fault(id).is_err());
}

#[test]
fn cancelling_published_wait_prevents_further_drive() {
    let mut d = driver("use reel::prelude::*; fn main() { narrator(\"wait\"); }");
    let mut published = false;
    for _ in 0..10_000 {
        if matches!(tick(&mut d), StoryProgress::Interaction(_)) {
            published = true;
            break;
        }
    }
    assert!(published);
    d.cancel().unwrap();
    assert!(matches!(tick(&mut d), StoryProgress::Cancelled));
    cleanup(&mut d);
    assert!(d.adapter().task().is_none());
}

#[test]
fn initializer_fault_does_not_start_story_entry() {
    let mut d = driver("throw \"init failed\"; fn main() {}");
    for _ in 0..10_000 {
        let progress = tick(&mut d);
        assert!(!matches!(progress, StoryProgress::Initialized));
        if matches!(progress, StoryProgress::Faulted(_)) {
            break;
        }
    }
    assert!(matches!(d.state(), StoryState::Faulted(_)));
    assert!(d.entry.is_some());
    d.cancel().unwrap();
    assert!(d.entry.is_none());
    cleanup(&mut d);
}
