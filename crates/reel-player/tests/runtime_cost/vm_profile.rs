//! Preserve the VM's existing bounded samples across complete runtime phases.
use aside_vm::executor::{
    DriverCost, FunctionEntries, STEP_HOTSPOT_CAPACITY, STEP_NAMES, StepCost, StepHotspot, Vm,
};
use std::collections::BTreeMap;

type Point = (u32, u32, Option<u32>, u8, u8);

#[derive(Default)]
pub(super) struct VmProfile {
    title: Option<String>,
    before: Option<DriverCost>,
    steps: [StepCost; 6],
    entries: Vec<FunctionEntries>,
    hotspots: BTreeMap<Point, (u64, u64)>,
    dropped: u64,
}

impl VmProfile {
    fn drain(&mut self, vm: &Vm) {
        let mut samples = [StepHotspot::default(); STEP_HOTSPOT_CAPACITY];
        self.dropped += vm.drain_step_hotspots(&mut samples);
        for sample in samples.into_iter().filter(|s| s.samples != 0) {
            let point = sample.point;
            let row = self
                .hotspots
                .entry((
                    point.function,
                    point.pc,
                    point.native,
                    point.mode,
                    sample.phase,
                ))
                .or_default();
            row.0 += sample.samples;
            row.1 += sample.sample_wall_ns;
        }
    }

    pub(super) fn observe(&mut self, vm: &Vm, title: &str) {
        self.drain(vm);
        if self.title.as_deref() == Some(title) {
            return;
        }
        self.finish(vm);
        self.title = Some(title.into());
        self.before = Some(vm.driver_costs());
        self.steps = vm.step_costs();
        self.entries = vm.function_entries().collect();
    }

    pub(super) fn finish(&mut self, vm: &Vm) {
        let Some(title) = self.title.take() else {
            self.hotspots.clear();
            self.dropped = 0;
            return;
        };
        self.drain(vm);
        let before = self.before.take().unwrap();
        let after = vm.driver_costs();
        let Some(delta) = after.checked_delta(&before) else {
            self.hotspots.clear();
            self.dropped = 0;
            return;
        };
        eprintln!("host_vm_driver title={title:?} cost={delta:?}");
        for (after, before) in vm.function_entries().zip(&self.entries) {
            let entries = after.script - before.script;
            let compact = after.compact - before.compact;
            if entries == 0 && compact == 0 {
                continue;
            }
            let program = vm.program().package();
            let definition = &program.functions[after.function.index()];
            let module = &program.symbols[program.modules[definition.module.index()].path.index()];
            let name = &program.symbols[definition.name.index()];
            eprintln!(
                "host_vm_function title={title:?} function={} module={module:?} name={name:?} entries={entries} compact={compact}",
                after.function.0
            );
        }
        for ((name, after), before) in STEP_NAMES.into_iter().zip(vm.step_costs()).zip(self.steps) {
            if let Some(delta) = after.checked_delta(&before) {
                eprintln!("host_vm_stage title={title:?} stage={name} cost={delta:?}");
            }
        }
        let mut rows = std::mem::take(&mut self.hotspots)
            .into_iter()
            .collect::<Vec<_>>();
        rows.sort_by_key(|(_, (_, ns))| std::cmp::Reverse(*ns));
        eprintln!(
            "host_vm_hotspots title={title:?} records={} dropped={}",
            rows.len(),
            self.dropped
        );
        for ((function, pc, native, mode, phase), (samples, ns)) in rows.into_iter().take(100) {
            let program = vm.program().package();
            let definition = &program.functions[function as usize];
            let module = &program.symbols[program.modules[definition.module.index()].path.index()];
            let name = &program.symbols[definition.name.index()];
            eprintln!(
                "host_vm_hotspot title={title:?} function={function} module={module:?} name={name:?} pc={pc} native={native:?} mode={mode} phase={phase} samples={samples} ns={ns}"
            );
        }
        self.dropped = 0;
    }
}
