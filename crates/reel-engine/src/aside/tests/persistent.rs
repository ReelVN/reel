use super::*;
use crate::aside::data::{Capture, DataDocument};
use crate::aside::saves::{SaveOperation, SaveResult};

pub(super) fn pending_write(code: &str, encoded: bool) -> (Session, SessionVm, HostTicket, Value) {
    pending_write_version(code, encoded, 1)
}
pub(super) fn pending_write_version(
    code: &str,
    encoded: bool,
    version: u64,
) -> (Session, SessionVm, HostTicket, Value) {
    let (mut session, mut vm) = models::prepared_version(code, encoded, version);
    for _ in 0..20_000 {
        match next(&mut vm, &mut session) {
            RunOutcome::HostCall(ticket) => {
                let package = vm.vm.program().package();
                if package.symbols[package.natives[ticket.native.index()].symbol.index()]
                    == "reel::persistent::write"
                {
                    let value = vm.vm.host_arguments(ticket).unwrap()[1];
                    return (session, vm, ticket, value);
                }
                vm.dispatch(&mut session, ticket, "en", NativeBudget::RetryMinimum(1))
                    .unwrap();
            }
            other => panic!("expected persistent.write, got {other:?}"),
        }
    }
    panic!("persistent.write fixture exhausted its work budget")
}

#[test]
fn instance_records_preserve_inherited_data_aliases_cycles_and_model_values_source_asb() {
    let code = r#"
        class Base {
            let revision: int = 1;
            static let code = || null;
            fn label(self) { "code" }
        }
        class Settings: Base {
            let mut volume: float = 0.8;
            let mut rows: array = [];
            let mut alias: array = [];
            let mut parent = null;
            fn init(self, record: table | null = null) {
                if record != null { self.volume = record.volume; }
            }
        }
        fn opening() {
            let original = Settings();
            original.rows = [original];
            original.alias = original.rows;
            original.parent = original;
            let state = model(original);
            state.volume = 0.6;
            await persistent.write("settings", state);
            let record = await persistent.read("settings");
            if record.revision != 1 || record.volume != 0.6 { throw "field values"; }
            if record.rows != record.alias || record.rows[0] != record || record.parent != record {
                throw "data graph identity";
            }
            if record.has_own("code") || record.has_own("label") { throw "executable members"; }
            let loaded = model(Settings(record));
            if loaded.volume != 0.6 || loaded.label() != "code" { throw "typed reconstruction"; }
            true
        }
    "#;
    for encoded in [false, true] {
        let (mut session, mut vm) = models::prepared(code, encoded);
        let mut stored = None;
        let mut writes = 0;
        loop {
            match next(&mut vm, &mut session) {
                RunOutcome::HostCall(ticket) => {
                    vm.dispatch(&mut session, ticket, "en", NativeBudget::Exact(1))
                        .unwrap();
                    collect(&mut vm);
                }
                RunOutcome::Returned(id) => {
                    assert_eq!(vm.vm.delivery_value(id).unwrap().as_bool(), Some(true));
                    vm.discard_delivery(&mut session).unwrap();
                    break;
                }
                RunOutcome::Waiting(_) => {}
                other => panic!("record round trip: {other:?}"),
            }
            while let Some(request) = vm.take_save_request() {
                let result = match request.operation {
                    SaveOperation::WriteData { key, data } => {
                        assert_eq!(key, "settings");
                        let mut bytes = Vec::new();
                        data.write_to(&mut bytes).unwrap();
                        stored = Some(std::sync::Arc::new(DataDocument::decode(&bytes).unwrap()));
                        writes += 1;
                        SaveResult::Done
                    }
                    SaveOperation::ReadData { key } => {
                        assert_eq!(key, "settings");
                        SaveResult::Data(stored.clone())
                    }
                    other => panic!("unexpected storage request: {other:?}"),
                };
                assert!(vm.complete_save_request(request.id, result).unwrap());
            }
            vm.maintain_save_results(1).unwrap();
        }
        assert_eq!(writes, 1);
    }
}

#[test]
fn instance_capture_checks_field_cells_and_releases_cancelled_scratch() {
    for model in [false, true] {
        let value = if model {
            "model(Settings())"
        } else {
            "Settings()"
        };
        let code = format!(
            r#"
            class Settings {{ let mut volume: float = 0.8; let other = [1, 2, 3]; }}
            fn opening() {{ persistent.write("settings", {value}); }}
        "#
        );
        let (_, mut vm, _, value) = pending_write(&code, true);
        collect(&mut vm);
        let before = vm.vm.heap().stats();
        let mut capture = Capture::new(vm.vm.heap(), value).unwrap();
        assert!(!capture.step(vm.vm.heap(), 1, 1).unwrap().0);
        for _ in 0..5 {
            assert!(!capture.step(vm.vm.heap(), 1, 1).unwrap().0);
        }
        let cell = vm.vm.heap().instance_field(value, 0).unwrap().cell;
        let before_write = vm.vm.heap().stats().charged_bytes;
        vm.vm
            .heap_mut()
            .cell_write(cell, Value::float(0.3))
            .unwrap();
        let write_storage = vm.vm.heap().stats().charged_bytes - before_write;
        let error = loop {
            match capture.step(vm.vm.heap(), 1, 1) {
                Err(error) => break error,
                Ok((false, _)) => collect(&mut vm),
                Ok((true, _)) => panic!("modified captured field was accepted"),
            }
        };
        assert!(error.to_string().contains("changed during capture"));
        drop(capture);
        collect(&mut vm);
        assert_eq!(vm.vm.heap().stats().live_objects, before.live_objects);
        assert_eq!(
            vm.vm.heap().stats().charged_bytes,
            before.charged_bytes + write_storage
        );
    }
}

#[test]
fn instance_capture_rejects_unfinished_failed_and_executable_data() {
    for body in [
        "class State { fn init(self) { persistent.write(\"bad\", self); } } fn opening() { State(); }",
        "let escaped={value:null}; class State { fn init(self) { escaped.value=self; throw \"failed\"; } } fn opening() { try { State(); } catch error {} persistent.write(\"bad\", escaped.value); }",
        "class State { let callback=|| null; } fn opening() { persistent.write(\"bad\", State()); }",
    ] {
        let (_, vm, _, value) = pending_write(body, false);
        let failed = match Capture::new(vm.vm.heap(), value) {
            Err(_) => true,
            Ok(mut capture) => loop {
                match capture.step(vm.vm.heap(), 1, 1) {
                    Err(_) => break true,
                    Ok((true, _)) => break false,
                    Ok((false, _)) => {}
                }
            },
        };
        assert!(failed, "unsupported data was captured: {body}");
    }
}

#[test]
fn instance_capture_includes_unread_field_edits_at_its_consistent_capture_point() {
    let code = r#"
        class Settings { let first: int = 1; let mut second: int = 2; }
        fn opening() { persistent.write("settings", model(Settings())); }
    "#;
    let (_, mut vm, _, value) = pending_write(code, true);
    let mut capture = Capture::new(vm.vm.heap(), value).unwrap();
    assert!(!capture.step(vm.vm.heap(), 1, 4096).unwrap().0);
    let field = vm.vm.heap().instance_field(value, 1).unwrap().cell;
    vm.vm.heap_mut().cell_write(field, Value::int(3)).unwrap();
    while !capture.step(vm.vm.heap(), 1, 1).unwrap().0 {}
    let (document, charge) = capture.finish().unwrap();
    let owner = vm.root_owner;
    let mut restore = crate::aside::data::Restore::new(vm.vm.heap_mut(), owner, document).unwrap();
    while !restore.step(vm.vm.heap_mut(), 1, 1).unwrap() {}
    let root = restore.value_root(vm.vm.heap_mut(), owner).unwrap();
    restore.release(vm.vm.heap_mut()).unwrap();
    let record = vm.vm.heap().root_value(root).unwrap();
    assert_eq!(
        vm.vm.heap().table_entry(record, 0).unwrap().1.as_int(),
        Some(1)
    );
    assert_eq!(
        vm.vm.heap().table_entry(record, 1).unwrap().1.as_int(),
        Some(3)
    );
    vm.vm.heap_mut().release_root(root).unwrap();
    drop(charge);
}

#[test]
fn instance_capture_reuses_field_names_and_creates_no_vm_projection() {
    let code = r#"
        class Settings { let volume: float = 0.8; let language: string = "en"; }
        fn opening() {
            let rows = [];
            for index in 0..128 { rows.append(Settings()); }
            persistent.write("settings", rows);
        }
    "#;
    let (_, mut vm, _, value) = pending_write(code, false);
    collect(&mut vm);
    let before = vm.vm.heap().stats();
    let started = std::time::Instant::now();
    let mut capture = Capture::new(vm.vm.heap(), value).unwrap();
    let mut steps = 0;
    while !capture.step(vm.vm.heap(), 1, 1).unwrap().0 {
        steps += 1;
    }
    let (document, charge) = capture.finish().unwrap();
    let after = vm.vm.heap().stats();
    assert_eq!(before.live_objects, after.live_objects);
    assert_eq!(before.chunk_allocations, after.chunk_allocations);
    let mut bytes = Vec::new();
    document.write_to(&mut bytes).unwrap();
    for name in [b"volume".as_slice(), b"language".as_slice()] {
        assert_eq!(
            bytes
                .windows(name.len())
                .filter(|window| *window == name)
                .count(),
            1
        );
    }
    eprintln!(
        "instance_capture rows=128 steps={steps} elapsed_us={} record_bytes={} retained_bytes={} charged_bytes={}",
        started.elapsed().as_micros(),
        bytes.len(),
        document.retained_bytes(),
        charge.bytes()
    );
    drop((document, charge));
    assert_eq!(vm.vm.heap().stats().charged_bytes, before.charged_bytes);
}
