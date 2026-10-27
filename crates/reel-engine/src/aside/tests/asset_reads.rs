use super::*;
use crate::assets::{AssetData, AssetLoader};
use aside_vm::snapshot::SnapshotBudget;
use reel_core::{
    budget::{BudgetCategory, BudgetPool, OwnedBytes, QueueLimits},
    identity::ContentVersion,
};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc, sync::Arc};

#[derive(Default)]
struct State {
    reads: Vec<(String, Option<ContentVersion>)>,
    pending: BTreeMap<u64, String>,
    ready: bool,
    cancelled: Vec<u64>,
    maintains: u64,
}
#[derive(Clone)]
struct Loader {
    state: Rc<RefCell<State>>,
    bytes: Arc<OwnedBytes>,
    version: ContentVersion,
}
impl Loader {
    fn new(bytes: Vec<u8>, ready: bool) -> Self {
        let pool = BudgetPool::new(
            BudgetCategory::CpuInput,
            QueueLimits {
                max_requests: 8,
                max_bytes: 1 << 20,
            },
        );
        let ticket = pool.reserve(bytes.len() as u64).unwrap();
        Self {
            bytes: Arc::new(OwnedBytes::new(bytes, ticket).unwrap()),
            version: ContentVersion([19; 32]),
            state: Rc::new(RefCell::new(State {
                ready,
                ..Default::default()
            })),
        }
    }
}
impl AssetLoader for Loader {
    fn maintain(&mut self) {
        self.state.borrow_mut().maintains += 1;
    }
    fn read(&mut self, path: &str, version: Option<ContentVersion>) -> Result<u64, String> {
        let mut state = self.state.borrow_mut();
        state.reads.push((path.into(), version));
        let id = state.reads.len() as u64;
        state.pending.insert(id, path.into());
        Ok(id)
    }
    fn poll(&mut self, id: u64) -> Option<Result<AssetData, String>> {
        let mut state = self.state.borrow_mut();
        if !state.ready {
            return None;
        }
        if state
            .pending
            .get(&id)
            .is_some_and(|path| path == "delayed" || path == "scope")
        {
            return None;
        }
        let path = state.pending.remove(&id)?;
        Some(if path == "missing" {
            Err("asset missing: missing".into())
        } else {
            Ok(AssetData {
                version: self.version,
                bytes: self.bytes.clone(),
            })
        })
    }
    fn cancel(&mut self, id: u64) {
        let mut state = self.state.borrow_mut();
        state.pending.remove(&id);
        state.cancelled.push(id);
    }
}

#[test]
fn native_reentry_leaves_external_completion_polling_to_the_host_turn() {
    let code = r#"fn opening(){ let bytes=await assets.read("sample");
        if bytes.len()!=3 {throw "asset result";} narrator("completed"); }"#;
    let loader = Loader::new(vec![1, 2, 3], false);
    let (mut session, mut vm) = models::prepared(code, false);
    vm.set_asset_loader(Box::new(loader.clone())).unwrap();
    models::drive(&mut vm, &mut session, false);
    assert!(!vm.asset_reads_need_work());
    let maintains = loader.state.borrow().maintains;
    loader.state.borrow_mut().ready = true;
    for _ in 0..128 {
        assert!(matches!(
            vm.step(
                &mut session,
                SessionBudget::Exact(SliceBudget::default()),
                || 0
            )
            .unwrap(),
            SessionStep::Vm(RunOutcome::Waiting(_))
        ));
    }
    assert_eq!(loader.state.borrow().maintains, maintains);
    assert!(vm.maintain_asset_reads(64).unwrap());
    models::drive(&mut vm, &mut session, false);
    assert!(
        matches!(session.view().interaction,Some(crate::InteractionView::Dialogue(dialogue)) if dialogue.text()=="completed")
    );
}

fn settle(vm: &mut SessionVm, session: &mut Session) {
    for _ in 0..1000 {
        models::drive(vm, session, false);
        vm.maintain_asset_reads(4).unwrap();
        if !vm.asset_reads_pending() {
            models::drive(vm, session, false);
            return;
        }
    }
    panic!("asset copy did not settle");
}

fn restore(vm: SessionVm, session: Session) -> (Session, SessionVm) {
    let program = vm.vm.program().clone();
    let mut capture = vm.snapshot_begin(&session).unwrap();
    while !capture.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    let bytes = capture.finish().unwrap().encode().unwrap();
    let mut image = snapshot::SessionSnapshot::decode(&bytes).unwrap();
    let mut restore = SessionVm::restore_begin(
        &mut image,
        program,
        VmLimits::default(),
        crate::SessionIdentity {
            session_id: SessionId(41),
            epoch: Epoch(10),
        },
        crate::ReadProgress::default(),
        None,
        None,
    )
    .unwrap();
    while !restore.step(SnapshotBudget { units: 1 }).unwrap().complete {}
    restore.finish().unwrap()
}

#[test]
fn asset_bytes_share_terminal_storage_and_validate_chunk_indices_source_bytecode() {
    let code = r#"use std::core;
    fn opening() {
        let request:AssetRead=assets.read("sample");
        let bytes:Bytes=await request;
        if bytes.len()!=8193 || bytes[0]!=0 || bytes[4095]!=127 || bytes[4096]!=255 || bytes[8192]!=255 {throw "byte storage";}
        let again:Bytes=await request;
        if again!=bytes || again[4096]!=255 {throw "terminal identity";}
        let mut errors=0;
        try {bytes[-1];} catch e {if core.error_info().code!="OutOfBounds" {throw "negative byte code";} errors+=1;}
        try {bytes[8193];} catch e {if core.error_info().code!="OutOfBounds" {throw "end byte code";} errors+=1;}
        try {bytes[0.0];} catch e {if core.error_info().code!="Type" {throw "index type code";} errors+=1;}
        try {bytes[0]=4;} catch e {if core.error_info().code!="ReadOnly" {throw "byte mutability code";} errors+=1;}
        if errors!=4 || bytes[0]!=0 {throw "byte boundary";}
        stage.replace(text("retained"));narrator("save");
        if (await request).len()!=8193 {throw "completed snapshot";}
        narrator("restored");
    }"#;
    for encoded in [false, true] {
        let mut data = vec![0; 8193];
        data[4095] = 127;
        data[4096] = 255;
        data[8192] = 255;
        let loader = Loader::new(data, true);
        let (mut session, mut vm) = models::prepared(code, encoded);
        vm.set_asset_loader(Box::new(loader.clone())).unwrap();
        settle(&mut vm, &mut session);
        assert_eq!(loader.state.borrow().reads.len(), 1);
        assert!(
            matches!(session.view().interaction, Some(crate::InteractionView::Dialogue(d)) if d.lines()[0].content().fallback.plain().as_ref()=="save")
        );
        (session, vm) = restore(vm, session);
        let id = session.view().interaction.unwrap().id();
        for _ in 0..2 {
            session
                .handle_local_input(session.root_owner(), InputAction::Advance(id))
                .unwrap();
        }
        models::drive(&mut vm, &mut session, false);
        assert!(!vm.asset_reads_pending());
        assert!(
            matches!(session.view().interaction, Some(crate::InteractionView::Dialogue(d)) if d.lines()[0].content().fallback.plain().as_ref()=="restored")
        );
    }
}

#[test]
fn asset_pending_and_partial_copy_snapshots_restart_pinned_source() {
    let code = r#"fn opening(){let request=assets.read("sample");let bytes=await request;
        if bytes.len()!=32769 || bytes[32768]!=231 {throw "restored bytes";} narrator("restored");}"#;
    for partial in [false, true] {
        let mut data = vec![0; 32769];
        data[32768] = 231;
        let loader = Loader::new(data, false);
        let (mut session, mut vm) = models::prepared(code, false);
        vm.set_asset_loader(Box::new(loader.clone())).unwrap();
        models::drive(&mut vm, &mut session, false);
        assert!(vm.asset_reads_pending());
        assert!(
            !vm.asset_reads_need_work(),
            "submitted IO sleeps until its worker wake"
        );
        if partial {
            loader.state.borrow_mut().ready = true;
            vm.maintain_asset_reads(1).unwrap();
            assert!(vm.asset_reads_need_work());
            assert!(vm.asset_reads_pending());
        }
        (session, vm) = restore(vm, session);
        let resumed = Loader::new(loader.bytes.as_slice().to_vec(), true);
        vm.set_asset_loader(Box::new(resumed.clone())).unwrap();
        settle(&mut vm, &mut session);
        assert_eq!(
            resumed.state.borrow().reads[0].1,
            if partial { Some(loader.version) } else { None }
        );
        assert!(
            matches!(session.view().interaction, Some(crate::InteractionView::Dialogue(d)) if d.lines()[0].content().fallback.plain().as_ref()=="restored")
        );
    }
}

#[test]
fn asset_empty_failure_and_owner_cancellation_are_stable() {
    let code = r#"use std::core;
    fn opening(){
        let empty=assets.read("empty"); if (await empty).len()!=0 || (await empty).len()!=0 {throw "empty";}
        let missing=assets.read("missing"); let mut rejected=0;
        for i in 0..2 {try {await missing;}catch e {if e!="asset missing: missing" {throw "IO message";}if core.error_info().origin.source==null {throw "IO source origin";}rejected+=1;}}
        if rejected!=2 {throw "repeated failure";}
        let node=stack();stage.replace(node);let request=assets.read("delayed",owner:node);node.remove();
        for i in 0..2 {try {await request;throw "cancel accepted";}catch e {if core.error_info().code!="Cancelled" {throw "cancel code";}}}
        let scoped=scope(body:|s| assets.read("scope",owner:s));
        try {await scoped;throw "scope survived";}catch e {if core.error_info().code!="Cancelled" {throw "scope cancel";}}
        narrator("done");
    }"#;
    let (mut session, mut vm) = models::prepared(code, false);
    let loader = Loader::new(Vec::new(), true);
    vm.set_asset_loader(Box::new(loader.clone())).unwrap();
    settle(&mut vm, &mut session);
    assert!(
        matches!(session.view().interaction, Some(crate::InteractionView::Dialogue(d)) if d.lines()[0].content().fallback.plain().as_ref()=="done")
    );
    assert!(!vm.asset_reads_pending());
}

#[test]
fn cancelling_one_asset_waiter_preserves_the_shared_producer() {
    let code = r#"fn opening(){
        let request=assets.read("sample");let waiter=spawn(|| {await request;});
        narrator("cancel waiter");waiter.cancel();try {await waiter;}catch e {}
        let bytes=await request;if bytes.len()!=3 || bytes[2]!=255 {throw "shared request";}
        narrator("kept");
    }"#;
    let (mut session, mut vm) = models::prepared(code, false);
    let loader = Loader::new(vec![0, 127, 255], false);
    vm.set_asset_loader(Box::new(loader.clone())).unwrap();
    models::drive(&mut vm, &mut session, false);
    let id = session.view().interaction.unwrap().id();
    for _ in 0..2 {
        session
            .handle_local_input(session.root_owner(), InputAction::Advance(id))
            .unwrap();
    }
    models::drive(&mut vm, &mut session, false);
    assert!(loader.state.borrow().cancelled.is_empty());
    assert!(vm.asset_reads_pending());
    loader.state.borrow_mut().ready = true;
    settle(&mut vm, &mut session);
    assert_eq!(loader.state.borrow().reads.len(), 1);
    assert!(
        matches!(session.view().interaction,Some(crate::InteractionView::Dialogue(d)) if d.lines()[0].content().fallback.plain().as_ref()=="kept")
    );
}
