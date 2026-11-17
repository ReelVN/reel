//! Real storage and accepted CPU projection; no window or device timing claims.
//! Run alone: `REEL_COST_ROWS=1000,10000 cargo test --locked --release -p
//! reel-player --lib save_flow_cost -- --ignored --nocapture --test-threads=1`.
//! Stage timings include the named tick's work; capture finish includes its final
//! step and queue submission. Host tick and storage worker allocations are
//! measured independently on their executing threads, including transient peaks.
//! The host drains work without frame pacing; no GPU or image/audio preparation.
use super::*;
use crate::saves::{DirectorySaves, RuntimeSaves, SaveService, SaveStorage, StorageError};
use crate::test_cost::measure;
use reel_core::{
    budget::QueueLimits,
    identity::{FrameId, SurfaceEpoch},
    wake::WakeSignal,
};
use reel_engine::{
    Epoch, Session, SessionId,
    aside::{VmLimits, checkpoint::CheckpointRecord, data::DataDocument, saves::SaveSlot},
};
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    bridge::{
        BridgeLimits, NoticeResult, PresentationBridge, PresentationNotice, PresentationOutcome,
    },
    core::{
        PointerButton, PointerButtonEvent, PointerEvent, PointerId, PointerInfo, PointerState,
        PointerType, PointerUpdate, Widget, WidgetId,
    },
    dpi::{PhysicalPosition, PhysicalSize},
    stage::NodeKind,
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
struct StorageSpan {
    operation: &'static str,
    start: Instant,
    end: Instant,
    live_delta: i64,
    peak_bytes: u64,
}

struct MeasuredStorage {
    directory: DirectorySaves,
    spans: Mutex<Vec<StorageSpan>>,
    data_writes: AtomicUsize,
}
impl MeasuredStorage {
    fn measure<T>(&self, operation: &'static str, f: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let (result, allocation) = measure(f);
        let end = Instant::now();
        self.spans.lock().unwrap().push(StorageSpan {
            operation,
            start,
            end,
            live_delta: allocation.bytes_current,
            peak_bytes: allocation.bytes_max,
        });
        result
    }
}
impl SaveStorage for MeasuredStorage {
    fn copy(&self, source: &str, destination: &str) -> Result<(), StorageError> {
        self.measure("copy_slot_publish", || {
            self.directory.copy(source, destination)
        })
    }
    fn write(
        &self,
        slot: SaveSlot,
        checkpoint: &CheckpointRecord,
        thumbnail: Option<&[u8]>,
    ) -> Result<(), StorageError> {
        self.measure("encode_compress_publish", || {
            self.directory.write(slot, checkpoint, thumbnail)
        })
    }
    fn read(&self, slot: &str) -> Result<CheckpointRecord, StorageError> {
        self.measure("read_decompress_decode", || self.directory.read(slot))
    }
    fn thumbnail(&self, slot: &str) -> Result<Option<Vec<u8>>, StorageError> {
        self.directory.thumbnail(slot)
    }
    fn delete(&self, slot: &str) -> Result<(), StorageError> {
        self.directory.delete(slot)
    }
    fn list(&self) -> Result<Vec<SaveSlot>, StorageError> {
        self.directory.list()
    }
    fn write_data(&self, key: &str, data: &DataDocument) -> Result<(), StorageError> {
        self.data_writes.fetch_add(1, Ordering::Relaxed);
        self.directory.write_data(key, data)
    }
    fn read_data(&self, key: &str) -> Result<Option<DataDocument>, StorageError> {
        self.directory.read_data(key)
    }
    fn delete_data(&self, key: &str) -> Result<(), StorageError> {
        self.directory.delete_data(key)
    }
}

#[derive(Default)]
struct Samples {
    count: usize,
    total: Duration,
    maximum: Duration,
}
impl Samples {
    fn add(&mut self, duration: Duration) {
        self.count += 1;
        self.total += duration;
        self.maximum = self.maximum.max(duration);
    }
    fn report(&self, rows: usize, phase: &str) {
        eprintln!(
            "save_flow,rows={rows},phase={phase},ticks={},tick_wall_ns={},max_tick_ns={}",
            self.count,
            self.total.as_nanos(),
            self.maximum.as_nanos()
        );
    }
}

struct Flow {
    ticks: [Samples; 7],
    live: i64,
    peak_live: i64,
    frozen_at: Option<Instant>,
    frozen: Duration,
    switched_at: Option<Instant>,
}
impl Flow {
    fn new() -> Self {
        Self {
            ticks: Default::default(),
            live: 0,
            peak_live: 0,
            frozen_at: None,
            frozen: Duration::ZERO,
            switched_at: None,
        }
    }
    fn tick(&mut self, runtime: &mut StoryRuntime) {
        let capturing = runtime.capturing();
        let restoring = runtime.saves.as_ref().is_some_and(|s| s.restore.is_some());
        let candidate = runtime
            .saves
            .as_ref()
            .is_some_and(|s| s.candidate.is_some());
        let identity = runtime.driver().ui().session().identity();
        let start = Instant::now();
        let (_, allocation) = measure(|| runtime.tick(SliceBudget::default(), || 0).unwrap());
        self.peak_live = self.peak_live.max(self.live + allocation.bytes_max as i64);
        self.live += allocation.bytes_current;
        let elapsed = start.elapsed();
        let switched = runtime.driver().ui().session().identity() != identity;
        let phase = if switched {
            self.switched_at = Some(Instant::now());
            5
        } else if restoring || candidate {
            4
        } else if runtime.saves.as_ref().is_some_and(|s| s.restore.is_some()) {
            3
        } else if !capturing && runtime.capturing() {
            self.frozen_at = Some(start);
            0
        } else if capturing {
            if runtime.capturing() { 1 } else { 2 }
        } else {
            6
        };
        if capturing && !runtime.capturing() {
            self.frozen += self.frozen_at.take().unwrap().elapsed();
        }
        self.ticks[phase].add(elapsed);
    }
    fn report(&self, rows: usize, operation: &str, elapsed: Duration) {
        for (name, sample) in [
            "capture_begin",
            "capture_step",
            "capture_last_step_finish_submit",
            "restore_begin_and_first_step",
            "restore_steps_and_prepare",
            "restore_finish_prepare_switch",
            "execution_gc_or_io_poll",
        ]
        .into_iter()
        .zip(&self.ticks)
        {
            sample.report(rows, name);
        }
        eprintln!(
            "save_flow,rows={rows},operation={operation},wall_ns={},frozen_ns={},host_tick_live_delta_bytes={},host_tick_peak_delta_bytes={}",
            elapsed.as_nanos(),
            self.frozen.as_nanos(),
            self.live,
            self.peak_live
        );
    }
}

pub(super) struct Screen {
    bridge: PresentationBridge,
    root: WidgetId,
    frame: u64,
}
impl Screen {
    pub(super) fn new(runtime: &StoryRuntime) -> Self {
        let widget = runtime.driver().widget().prepare();
        let root = widget.id();
        let tree = RenderRoot::new(
            widget,
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(Default::default()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(800, 600),
                scale_factor: 1.,
                default_font: Some(reel_ui::peniko::Blob::new(Arc::new(
                    include_bytes!("../../tests/fixtures/font-weight/400.ttf").to_vec(),
                ))),
            },
        );
        Self {
            bridge: PresentationBridge::new(
                tree,
                SurfaceEpoch(1),
                BridgeLimits {
                    geometry: reel_ui::app::GeometryLimits { max_bytes: 1 << 20 },
                    max_candidates: 4,
                    max_bridge_bytes: 64 << 10,
                    max_snapshot_bytes: 8 << 20,
                },
            )
            .unwrap(),
            root,
            frame: 0,
        }
    }
    pub(super) fn present(&mut self, runtime: &mut StoryRuntime) -> bool {
        runtime
            .driver_mut()
            .unwrap()
            .sync(&mut self.bridge, self.root)
            .unwrap();
        self.frame += 1;
        let id = FrameId(self.frame);
        let Some((recording, receipt)) = runtime
            .driver()
            .record(&mut self.bridge, self.root, id)
            .unwrap()
        else {
            return false;
        };
        assert_eq!(
            self.bridge.resolve(PresentationNotice::new(
                recording.key(),
                SurfaceEpoch(1),
                PresentationOutcome::Accepted(id)
            )),
            NoticeResult::Accepted
        );
        runtime
            .driver_mut()
            .unwrap()
            .presented(&self.bridge, &receipt)
            .unwrap();
        runtime.remember_dialogue(&receipt);
        runtime.wake_input();
        true
    }
    pub(super) fn click(&mut self, runtime: &mut StoryRuntime, x: f64) {
        let pointer = PointerInfo {
            pointer_id: Some(PointerId::PRIMARY),
            persistent_device_id: None,
            pointer_type: PointerType::Mouse,
        };
        let state = PointerState {
            position: PhysicalPosition::new(x, 20.),
            ..Default::default()
        };
        for event in [
            PointerEvent::Move(PointerUpdate {
                pointer,
                current: state.clone(),
                coalesced: vec![],
                predicted: vec![],
            }),
            PointerEvent::Down(PointerButtonEvent {
                pointer,
                state: state.clone(),
                button: Some(PointerButton::Primary),
            }),
            PointerEvent::Up(PointerButtonEvent {
                pointer,
                state,
                button: Some(PointerButton::Primary),
            }),
        ] {
            runtime
                .driver_mut()
                .unwrap()
                .handle_pointer(&mut self.bridge, event)
                .unwrap();
        }
        runtime.wake_input();
    }
}

fn drain(runtime: &mut StoryRuntime, flow: &mut Flow) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while runtime.needs_tick() {
        assert!(Instant::now() < deadline, "save/load did not settle");
        flow.tick(runtime);
        std::thread::yield_now();
    }
}
pub(super) fn has_text(runtime: &StoryRuntime, expected: &str) {
    assert!(
        runtime
            .driver()
            .adapter()
            .stage()
            .nodes()
            .any(|n| matches!(&n.kind, NodeKind::Text(s) if s.as_ref() == expected)),
        "expected {expected}"
    );
}

fn save_flow(rows: usize, report: bool) {
    let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/reel-artifacts/save-flow")
        .join(format!(
            "{}-{rows}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    let storage = Arc::new(MeasuredStorage {
        directory: DirectorySaves::new(&folder, 256 << 20),
        spans: Mutex::new(Vec::with_capacity(2)),
        data_writes: AtomicUsize::new(0),
    });
    let source = format!(
        r#"
        use std::resume::{{state,root,flow,point}};
        class World {{
            let mut items:array=[];
            let mut resumes:int=0;
            let mut clicks:int=0;
            let mut restored:bool=false;
            let mut done:bool=false;
            let mut bootstraps:int=0;
        }}
        state(World,"cost-world");root(main,"save-cost");
        fn story(data:World) {{
            flow("cost-story");
            point("complete");choose([{{key:"complete",label:"Complete"}}]);
            data.clicks+=1;data.done=true;
            if data.items.len() != {rows} || data.items[{last}].amount != {rows} || game.history().len() != {rows} {{ throw "restored data changed"; }}
            await stage.presented();
            if data.resumes != 1 || data.clicks != 1 {{ throw "continuation repeated"; }}
        }}
        fn main(data:World=model(World())) {{
            game.configure(width: 800, height: 600);
            if data.bootstraps==0 {{
                for i in 0..{rows} {{
                    data.items.append({{id: i, name: f"Inventory {{i}}: the observatory map and a brass key.", amount: i + 1}});
                    game.record(f"History {{i}}: We crossed the courtyard and watched the last light leave the windows.");
                }}
            }}
            data.restored=data.bootstraps!=0;data.bootstraps+=1;
            let choice=game.choice();
            stage.replace(stack(width: 800, height: 600) {{
                button("Save", width: 90, height: 40, on_click: || {{
                    if !saves.can_capture() {{ throw "save point unavailable"; }}
                    await persistent.write("proof", {{rows: {rows}}});
                    data.resumes += 1;
                    await saves.save("slot", title: "Cost fixture");
                }});
                button("Load", x: 100, width: 90, height: 40, on_click: || {{ await saves.load("slot"); throw "old load callback resumed"; }});
                button("Complete", x: 200, width: 90, height: 40, on_click: || {{ choice.select(choice.options[0].id); }});
                text(value: derive(|| f"resumes={{data.resumes}} restored={{data.restored}} clicks={{data.clicks}} done={{data.done}}"), y: 50);
                column(y: 100, gap: 4) {{ for i in 0..12 {{ text(value: derive(|| f"{{data.items[i].name}} x{{data.items[i].amount}}")); }} }};
            }});
            run_flow(story,data);
        }}
    "#,
        last = rows - 1
    );
    let driver = AsideSessionUi::from_source(
        Session::new(SessionId(91), Epoch(1)).unwrap(),
        &source,
        "save_cost",
        1,
        "main",
        "en",
        VmLimits::default(),
    )
    .unwrap();
    let service = SaveService::new(
        storage.clone(),
        QueueLimits {
            max_requests: 1,
            max_bytes: 256 << 20,
        },
        WakeSignal::new(|| {}),
    );
    let mut runtime = StoryRuntime::new(driver).with_saves(RuntimeSaves::new(service, 256 << 20));
    drain(&mut runtime, &mut Flow::new());
    let mut screen = Screen::new(&runtime);
    assert!(screen.present(&mut runtime));
    drain(&mut runtime, &mut Flow::new());

    let mut save = Flow::new();
    let save_start = Instant::now();
    screen.click(&mut runtime, 40.);
    drain(&mut runtime, &mut save);
    let save_elapsed = save_start.elapsed();
    has_text(&runtime, "resumes=1 restored=false clicks=0 done=false");
    assert_eq!(storage.data_writes.load(Ordering::Relaxed), 1);
    let slot_bytes = std::fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap())
        .find(|e| e.path().extension().is_some_and(|s| s == "save"))
        .unwrap()
        .metadata()
        .unwrap()
        .len();
    if report {
        save.report(rows, "save", save_elapsed);
    }
    assert!(screen.present(&mut runtime));

    let mut load = Flow::new();
    let load_start = Instant::now();
    screen.click(&mut runtime, 140.);
    let deadline = Instant::now() + Duration::from_secs(120);
    while load.switched_at.is_none() {
        assert!(Instant::now() < deadline, "load did not switch sessions");
        load.tick(&mut runtime);
        std::thread::yield_now();
    }
    has_text(&runtime, "resumes=1 restored=true clicks=0 done=false");
    let first_frame = Instant::now();
    assert!(screen.present(&mut runtime));
    let first_frame_elapsed = first_frame.elapsed();
    let accepted_elapsed = load_start.elapsed();
    drain(&mut runtime, &mut load);
    has_text(&runtime, "resumes=1 restored=true clicks=0 done=false");
    assert_eq!(
        storage.data_writes.load(Ordering::Relaxed),
        1,
        "external write replayed"
    );
    if report {
        load.report(rows, "load_until_resumed", load_start.elapsed());
        eprintln!(
            "save_flow,rows={rows},slot_bytes={slot_bytes},load_to_first_accepted_ns={},first_sync_record_accept_ns={}",
            accepted_elapsed.as_nanos(),
            first_frame_elapsed.as_nanos()
        );
        for span in storage.spans.lock().unwrap().iter() {
            eprintln!(
                "save_flow,rows={rows},worker={},wall_ns={},thread_live_delta_bytes={},thread_peak_delta_bytes={}",
                span.operation,
                span.end.duration_since(span.start).as_nanos(),
                span.live_delta,
                span.peak_bytes
            );
        }
    }
    assert!(screen.present(&mut runtime));
    screen.click(&mut runtime, 240.);
    drain(&mut runtime, &mut Flow::new());
    has_text(&runtime, "resumes=1 restored=true clicks=1 done=true");
    assert!(screen.present(&mut runtime));
    drain(&mut runtime, &mut Flow::new());
    runtime.close().unwrap();
    drain(&mut runtime, &mut Flow::new());
    assert!(runtime.close_complete());
    assert_eq!(storage.data_writes.load(Ordering::Relaxed), 1);
    storage.delete("slot").unwrap();
    storage.delete_data("proof").unwrap();
    std::fs::remove_dir(folder).unwrap();
}

#[test]
fn save_flow_restores_input_wait_and_external_write_once() {
    save_flow(32, false);
}

#[test]
#[ignore = "real save/load stage costs; run alone with --test-threads=1"]
fn save_flow_cost() {
    for rows in std::env::var("REEL_COST_ROWS")
        .unwrap_or_else(|_| "1000,10000".into())
        .split(',')
        .map(|s| s.parse::<usize>().unwrap())
    {
        assert!(rows >= 12);
        save_flow(rows, true);
    }
}
