//! Explicit measurements for choosing logical-checkpoint capture boundaries.
use super::*;
use aside_vm::snapshot::SnapshotBudget;
use std::time::Instant;

/// Compare whole-session capture with the existing detached data graph using
/// a fixed business update and independently increasing UI/data working sets.
/// Encoding is reported separately; production encoding runs on its IO worker.
#[test]
#[ignore = "release-mode save capture measurement; run with --ignored --nocapture"]
fn save_capture_cost() {
    println!(
        "save_capture,fields,nodes,full_units,full_steps,full_bytes,full_capture_ns,full_max_step_ns,full_encode_ns,data_work,data_steps,data_bytes,data_capture_ns,data_max_step_ns,data_encode_ns"
    );
    for nodes in [0, 256] {
        for fields in [0, 1024, 16384] {
            let code = format!(
                r#"fn opening() {{
                let root=column();
                for i in 0..{nodes} {{
                    root.add(button("item",on_click:|event| {{event.target.opacity=0.5;}}));
                }}
                stage.replace(root);
                narrator("checkpoint",history:false);
            }}"#
            );
            let (mut session, mut runtime) = models::prepared(&code, true);
            models::drive(&mut runtime, &mut session, false);
            collect(&mut runtime);
            let state = {
                let heap = runtime.vm.heap_mut();
                let values = heap.alloc_array(&[]).unwrap();
                for i in 0..fields {
                    heap.array_push(values, Value::int(i)).unwrap();
                }
                let state = heap.alloc_table().unwrap();
                for (index, (name, value)) in [
                    ("values", values),
                    ("score", Value::int(0)),
                    ("same_values", values),
                    ("self", state),
                ]
                .into_iter()
                .enumerate()
                {
                    let name = heap.alloc_string(name).unwrap();
                    heap.table_store(state, index as u32, name, value).unwrap();
                }
                state
            };
            let root = runtime
                .vm
                .heap_mut()
                .add_root(runtime.root_owner, RootKind::Host, state)
                .unwrap();
            // Change one business field without changing the retained data/UI size.
            let score_key = runtime.vm.heap().table_entry(state, 1).unwrap().0;
            runtime
                .vm
                .heap_mut()
                .table_store(state, 1, score_key, Value::int(1))
                .unwrap();
            let started = Instant::now();
            let mut capture = runtime.snapshot_begin(&session).unwrap();
            let (mut full_units, mut full_steps, mut full_max) = (0, 0, 0);
            loop {
                let step = Instant::now();
                let progress = capture.step(SnapshotBudget { units: 64 }).unwrap();
                full_max = full_max.max(step.elapsed().as_nanos());
                full_units += progress.units;
                full_steps += 1;
                if progress.complete {
                    break;
                }
            }
            let image = capture.finish().unwrap();
            let full_capture = started.elapsed().as_nanos();
            let started = Instant::now();
            let encoded = image.encode().unwrap();
            let full_encode = started.elapsed().as_nanos();
            // Exercise the actual storage representation, including the VM image.
            snapshot::SessionSnapshot::decode(&encoded).unwrap();
            let full_bytes = encoded.len();
            drop((image, encoded));

            let started = Instant::now();
            let mut capture = data::Capture::new(runtime.vm.heap(), state).unwrap();
            let (mut data_work, mut data_steps, mut data_max) = (0, 0, 0);
            loop {
                let step = Instant::now();
                let (done, work) = capture.step(runtime.vm.heap(), 64, 4096).unwrap();
                data_max = data_max.max(step.elapsed().as_nanos());
                data_work += u64::from(work);
                data_steps += 1;
                if done {
                    break;
                }
            }
            let (document, charge) = capture.finish().unwrap();
            let data_capture = started.elapsed().as_nanos();
            let started = Instant::now();
            let mut encoded = Vec::new();
            document.write_to(&mut encoded).unwrap();
            let data_encode = started.elapsed().as_nanos();
            data::DataDocument::decode(&encoded).unwrap();
            println!(
                "save_capture,{fields},{nodes},{full_units},{full_steps},{full_bytes},{full_capture},{full_max},{full_encode},{data_work},{data_steps},{},{data_capture},{data_max},{data_encode}",
                encoded.len()
            );
            drop((encoded, document, charge));
            runtime.vm.heap_mut().release_root(root).unwrap();
        }
    }
}
