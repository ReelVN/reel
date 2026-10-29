use super::*;

#[test]
fn vm_snapshot_reports_selected_task_execution_and_collection_progress() {
    use reel_engine::{
        Epoch, Session, SessionId,
        aside::{SliceBudget, VmLimits},
    };
    let driver = crate::aside_session::AsideSessionUi::from_source(
        Session::new(SessionId(1), Epoch(1)).unwrap(),
        "fn main() { await run(delay(1)); }",
        "probe_wait",
        1,
        "main",
        "en",
        VmLimits::default(),
    )
    .unwrap();
    let mut story = StoryRuntime::new(driver);
    for _ in 0..1000 {
        story.tick(SliceBudget::default(), || 0).unwrap();
        if story.tick_interval() == std::time::Duration::from_secs(1) {
            break;
        }
    }
    assert_eq!(story.tick_interval(), std::time::Duration::from_secs(1));
    let sample = vm_sample(Some(&story));
    let field = |sample: [u64; VM_NAMES.len()], name| {
        sample[VM_NAMES.iter().position(|value| *value == name).unwrap()]
    };
    let vm = story.driver().adapter().vm();
    let location = vm.task_location(vm.task_id().unwrap()).unwrap().unwrap();
    assert_eq!(field(sample, "fn"), u64::from(location.code.function.0));
    assert_eq!(field(sample, "pc"), u64::from(location.pc.0));
    assert_eq!(field(sample, "native_entries"), vm.stats().native_entries);
    assert!(field(sample, "native_entries") > 0);
    assert_eq!(
        field(sample, "instructions"),
        vm.stats().cumulative_instructions
    );
    assert_eq!(field(sample, "work"), vm.stats().cumulative_work);
    assert!(field(sample, "instructions") > 0);
    assert_eq!(
        field(sample, "compact_derived_starts"),
        vm.stats().compact_derived_starts
    );
    assert_eq!(
        field(sample, "last_instructions"),
        u64::from(vm.stats().usage.instructions)
    );
    assert_eq!(field(sample, "stage_ready"), 1);
    assert_eq!(field(sample, "derived_task"), 0);
    let collections = field(sample, "collections");
    let units = field(sample, "gc_units");
    story.driver_mut().unwrap().begin_collection().unwrap();
    assert_ne!(field(vm_sample(Some(&story)), "gc_phase"), 0);
    while story.driver().adapter().vm().heap().phase() != aside_vm::GcPhase::Idle {
        story
            .driver_mut()
            .unwrap()
            .gc_step(Default::default())
            .unwrap();
    }
    let collected = vm_sample(Some(&story));
    assert!(field(collected, "collections") > collections);
    assert!(field(collected, "gc_units") > units);
    assert_eq!(field(collected, "slices"), field(sample, "slices"));
    assert_eq!(field(collected, "gc_phase"), 0);
    assert_eq!(vm_sample(None), [ABSENT; VM_NAMES.len()]);
}

#[test]
fn recording_keeps_bounded_history_and_writes_complete_totals() {
    let live = Live::start().unwrap();
    let mut recording = Recording::new();
    for index in 1..=600 {
        recording.started = Instant::now() - std::time::Duration::from_millis(index * 300);
        drop(reel_core::profile::Scope::new(
            reel_core::profile::Phase::Draw,
        ));
        recording.record(
            Instant::now() - std::time::Duration::from_millis(60),
            None,
            1024,
            if index <= 300 { "First" } else { "Second" },
        );
    }
    assert_eq!(recording.windows.len(), WINDOWS);
    assert_eq!(recording.slow.len(), SLOW);
    assert_eq!(recording.total.loops, 600);
    assert_eq!(recording.segments[0].1.loops, 300);
    assert_eq!(recording.segment.1.loops, 300);
    recording.record_device([77; device::NAMES.len()]);
    assert_eq!(
        recording.total.phases.calls[reel_core::profile::Phase::Draw as usize],
        600
    );
    let mut output = Vec::new();
    recording
        .report(&mut output, "Window\tlight", "Device failure")
        .unwrap();
    let text = String::from_utf8(output).unwrap();
    let rows: Vec<_> = text.lines().skip(5).collect();
    let width = rows[0].split('\t').count();
    assert!(
        rows[1..]
            .iter()
            .all(|line| line.split('\t').count() == width)
    );
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("window\t"))
            .count(),
        WINDOWS
    );
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("slow\t"))
            .count(),
        SLOW
    );
    let mut output = Vec::new();
    recording.device.write(&mut output).unwrap();
    let device_text = String::from_utf8(output).unwrap();
    let rows: Vec<_> = device_text.lines().skip(1).collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].split('\t').count(), device::NAMES.len() + 1);
    assert_eq!(rows[1].split('\t').count(), device::NAMES.len() + 1);
    assert!(rows[1].ends_with("\t77"));
    drop(live);
}
