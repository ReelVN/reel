//! Transfer bounded VM samples to the diagnostic thread without main-thread IO.
use aside_vm::executor::{STEP_HOTSPOT_CAPACITY, STEP_MODES, STEP_NAMES, StepHotspot, Vm};
use std::{fmt::Write as _, sync::Mutex};

struct Batch {
    rows: [StepHotspot; STEP_HOTSPOT_CAPACITY],
    pending: bool,
    session: u32,
    program: u64,
    at_ms: u64,
    dropped: u64,
}
static BATCH: Mutex<Batch> = Mutex::new(Batch {
    rows: [StepHotspot {
        point: aside_vm::executor::StepPoint {
            function: 0,
            pc: 0,
            native: None,
            mode: 0,
        },
        phase: 0,
        samples: 0,
        sample_wall_ns: 0,
    }; STEP_HOTSPOT_CAPACITY],
    pending: false,
    session: 0,
    program: 0,
    at_ms: 0,
    dropped: 0,
});

pub(super) fn publish(vm: &Vm, session: u32, at_ms: u64) {
    let Ok(mut batch) = BATCH.try_lock() else {
        return;
    };
    // Leave samples in the VM when the worker has not consumed the prior batch.
    if batch.pending {
        return;
    }
    batch.dropped = vm.drain_step_hotspots(&mut batch.rows);
    batch.session = session;
    batch.program = vm.program().package().content_version;
    batch.at_ms = at_ms;
    batch.pending = true;
}

pub(super) fn print(rows: &mut Vec<StepHotspot>, line: &mut String) {
    let (session, program, at_ms, dropped) = {
        let Ok(mut batch) = BATCH.try_lock() else {
            return;
        };
        if !batch.pending {
            return;
        }
        rows.clear();
        rows.extend(batch.rows.iter().copied().filter(|row| row.samples != 0));
        batch.pending = false;
        (batch.session, batch.program, batch.at_ms, batch.dropped)
    };
    for row in rows.iter() {
        line.clear();
        let _ = write!(
            line,
            "[reel-vm-hotspot] at_ms={at_ms} session={session} program={program} phase={} mode={} fn={} pc={} native={} samples={} sample_wall_ns={}",
            STEP_NAMES[row.phase as usize],
            STEP_MODES[row.point.mode as usize],
            row.point.function,
            row.point.pc,
            row.point.native.map_or(-1, i64::from),
            row.samples,
            row.sample_wall_ns
        );
        eprintln!("{line}");
    }
    eprintln!(
        "[reel-vm-hotspot-batch] at_ms={at_ms} session={session} rows={} capacity={STEP_HOTSPOT_CAPACITY} dropped={dropped}",
        rows.len()
    );
}
