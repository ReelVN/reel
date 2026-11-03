//! Host-side distribution costs with optional ordinary rendering.
use aside_vm::{GcPhase, snapshot::SnapshotBudget};
use reel_engine::{
    Epoch, Session, SessionId, SessionIdentity,
    aside::{SessionVm, SliceBudget, VmLimits},
};
use reel_player::{aside_session::AsideSessionUi, story_runtime::StoryRuntime};
use std::time::Instant;
#[cfg(feature = "profiling")]
#[path = "runtime_cost/render.rs"]
mod render;
#[cfg(feature = "vm-profiling")]
#[path = "runtime_cost/vm_profile.rs"]
mod vm_profile;

fn measure<T>(work: impl FnOnce() -> T) -> (T, allocation_counter::AllocationInfo) {
    let mut result = None;
    let info = allocation_counter::measure(|| result = Some(work()));
    (result.unwrap(), info)
}

/// Profile a supplied release distribution without a window or game interaction.
/// Run with REEL_PROFILE_PACKAGE pointing at game.asb and --ignored --nocapture.
/// REEL_PROFILE_FRAMES optionally advances its clock at 60 Hz after startup.
/// REEL_PROFILE_RESOURCES optionally supplies an existing distribution's asset paths.
/// REEL_PROFILE_ENTRY optionally selects an existing module-level function.
/// REEL_PROFILE_HEAP_MIB overrides the 24 MiB heap allowance beyond retained code.
/// REEL_PROFILE_LAYOUT adds a CPU layout/presentation host, runs real elapsed time,
/// and observes one second without VM work (up to 15 seconds). It records no pixels.
/// REEL_PROFILE_SLICE_US optionally sets a wall deadline for each layout tick.
/// REEL_PROFILE_LAYOUT_FRAMES runs fixed 60 Hz clock frames through the CPU host.
/// A frame advances only after its script batch, resources, and publication settle.
/// REEL_PROFILE_RENDERER enables ordinary frame preparation and submission feedback.
/// Rendering uses a temporary WGSL effect overlay generated from the package's
/// retained effect programs; CPU-only measurements may select GXP asset paths
/// with REEL_PROFILE_EFFECT_FORMAT=gxp without creating that overlay.
#[test]
#[ignore = "requires an external release distribution"]
fn distribution_startup_cost() {
    let info = allocation_counter::measure(distribution_startup_cost_inner);
    eprintln!(
        "startup_thread_allocations={} allocated_bytes={} peak_live_bytes={}",
        info.count_total, info.bytes_total, info.bytes_max
    );
}

fn distribution_startup_cost_inner() {
    eprintln!(
        "host_options legacy_script={} legacy_body_clock={} renderer={}",
        std::env::var("ASIDE_VM_LEGACY_SCRIPT_EXECUTION").unwrap_or_else(|_| "0".into()),
        std::env::var("ASIDE_VM_LEGACY_BODY_CLOCK").unwrap_or_else(|_| "0".into()),
        std::env::var_os("REEL_PROFILE_RENDERER").is_some()
    );
    let path = std::env::var_os("REEL_PROFILE_PACKAGE").expect("REEL_PROFILE_PACKAGE");
    let started = Instant::now();
    #[cfg(feature = "profiling")]
    let profile_begin = reel_core::profile::Sample::snapshot();
    let mut loaded = reel_player::distribution::load(std::path::Path::new(&path), None).unwrap();
    if let Some(resources) = std::env::var_os("REEL_PROFILE_RESOURCES") {
        let path = std::path::Path::new(&resources);
        let manifest = reel_player::distribution::read_manifest(path).unwrap();
        let context =
            reel_player::distribution::resource_context(path.parent().unwrap(), manifest, None);
        loaded.packages = context.packages;
        loaded.loose = context.loose;
    }
    let load_time = started.elapsed();
    if let Ok(path) = std::env::var("ASIDE_DISASSEMBLY_OUT") {
        let mut text = String::new();
        aside_vm::bytecode::disassemble(loaded.package.program(), &mut text).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let code_bytes = loaded.package.program().execution_capacity_bytes();
    eprintln!(
        "decode_ms={} code_bytes={} code={:?}",
        load_time.as_millis(),
        code_bytes,
        loaded.package.program().code_cache_stats()
    );
    let started = Instant::now();
    let mut story = reel_player::distribution::story_from_package(loaded.package).unwrap();
    #[cfg(feature = "profiling")]
    let mut renderer = std::env::var_os("REEL_PROFILE_RENDERER")
        .map(|_| render::Probe::new(&story.effects, &mut loaded.loose));
    let assets =
        reel_player::assets::Assets::packaged(Default::default(), loaded.packages, loaded.loose)
            .unwrap();
    eprintln!("archives_ms={}", started.elapsed().as_millis(),);
    let media = reel_player::media::MediaPlayer::new(
        assets.clone(),
        Default::default(),
        reel_core::wake::WakeSignal::new(|| {}),
        |wake| {
            reel_media::OfflineOutput::new(
                reel_media::AudioDeviceConfig {
                    sample_rate_hz: 48_000,
                    sample_format: reel_media::SampleFormat::F32,
                    layout: reel_media::ChannelLayout::Stereo,
                },
                wake,
            )
            .map(|output| Box::new(output) as Box<dyn reel_media::AudioOutput>)
            .map_err(|error| error.to_string())
        },
    );
    if let Ok(entry) = std::env::var("REEL_PROFILE_ENTRY") {
        let (module, name) = entry.rsplit_once("::").unwrap();
        let package = story.program.package();
        let index = package
            .functions
            .iter()
            .position(|function| {
                package.symbols[function.name.index()] == name
                    && package.symbols[package.modules[function.module.index()].path.index()]
                        == module
            })
            .expect("profile entry exists");
        story.entry = aside_vm::bytecode::FunctionId(index.try_into().unwrap());
        story.validate().unwrap();
    }
    let mut limits = VmLimits::default();
    let heap_mib = std::env::var("REEL_PROFILE_HEAP_MIB")
        .map(|value| value.parse::<u64>().unwrap())
        .unwrap_or(24);
    limits.heap.max_bytes = code_bytes as u64 + (heap_mib << 20);
    limits.slots.initial_capacity = 4096;
    limits.slots.max_capacity = 65_536;
    limits.max_tasks = 256;
    let started = Instant::now();
    let mut session = AsideSessionUi::from_compiled(
        Session::new(SessionId(1), Epoch(1)).unwrap(),
        story,
        "zh-CN",
        limits,
    )
    .unwrap()
    .with_assets(assets)
    .with_media(Box::new(media))
    .unwrap();
    if let Ok(format) = std::env::var("REEL_PROFILE_EFFECT_FORMAT") {
        session.set_effect_shader_format(match format.as_str() {
            "gxp" => reel_ui::resources::EffectShaderFormat::Gxp,
            "wgsl" => reel_ui::resources::EffectShaderFormat::Wgsl,
            _ => panic!("REEL_PROFILE_EFFECT_FORMAT must be gxp or wgsl"),
        });
    }
    eprintln!(
        "vm_ms={} charged_bytes={}",
        started.elapsed().as_millis(),
        session.adapter().vm().heap().budget_usage().charged_bytes(),
    );
    let saves = reel_player::saves::SaveService::new(
        std::sync::Arc::new(reel_player::saves::DirectorySaves::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/reel-artifacts/vita-performance/profile-saves")
                .join(std::process::id().to_string()),
            64 << 20,
        )),
        reel_core::budget::QueueLimits {
            max_requests: 8,
            max_bytes: 64 << 20,
        },
        reel_core::wake::WakeSignal::new(|| {}),
    );
    let initializer_task = session.adapter().task();
    let mut canvas_id = None;
    let mut applied_canvas = None;
    let mut layout = std::env::var_os("REEL_PROFILE_LAYOUT").map(|_| {
        use reel_ui::{
            app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
            core::{DefaultProperties, Widget},
            dpi::PhysicalSize,
        };
        let widget = session.widget().prepare();
        let root = widget.id();
        let canvas = reel_ui::widgets::Canvas::viewport(widget).prepare();
        canvas_id = Some(canvas.id());
        let tree = RenderRoot::new(
            canvas,
            |_| {},
            RenderRootOptions {
                use_system_fonts: false,
                size: PhysicalSize::new(960, 544),
                default_properties: std::sync::Arc::new(DefaultProperties::new()),
                size_policy: WindowSizePolicy::User,
                scale_factor: 1.,
                default_font: None,
            },
        );
        let bridge = reel_ui::bridge::PresentationBridge::new(
            tree,
            reel_core::identity::SurfaceEpoch(1),
            reel_ui::bridge::BridgeLimits {
                geometry: reel_ui::app::GeometryLimits { max_bytes: 1 << 20 },
                max_candidates: 1,
                max_bridge_bytes: 1 << 20,
                max_snapshot_bytes: 8 << 20,
            },
        )
        .unwrap();
        (bridge, root, 0u64)
    });
    let frame_limit = std::env::var("REEL_PROFILE_LAYOUT_FRAMES")
        .ok()
        .map(|value| value.parse::<u64>().unwrap());
    assert!(frame_limit.is_none() || layout.is_some());
    let mut clock_frames = 0u64;
    let mut premature_projections = 0u64;
    let mut tick_ns = 0u128;
    let mut sync_ns = 0u128;
    let mut runtime = StoryRuntime::new(session)
        .with_saves(reel_player::saves::RuntimeSaves::new(saves, 64 << 20));
    let started = Instant::now();
    let mut max_tick_ns = 0;
    let mut last_report = 0;
    let mut initialized = false;
    let mut layout_last_work = (0, 0);
    let mut accepted_stamp = None;
    #[cfg(feature = "profiling")]
    let mut frame_profile = reel_core::profile::Sample::snapshot();
    #[cfg(feature = "profiling")]
    let mut frame_vm = [0u64; 4];
    #[cfg(feature = "vm-profiling")]
    let mut vm_profile = vm_profile::VmProfile::default();
    let slice_ns = std::env::var("REEL_PROFILE_SLICE_US")
        .ok()
        .filter(|_| layout.is_some())
        .map(|n| n.parse::<u64>().unwrap() * 1000);
    assert!(
        frame_limit.is_none() || slice_ns.is_none(),
        "fixed frames use instruction limits"
    );
    for steps in 1..1_000_000 {
        let tick = Instant::now();
        let budget = SliceBudget {
            deadline_ns: slice_ns.map(|n| started.elapsed().as_nanos() as u64 + n),
            ..Default::default()
        };
        let result = runtime.tick(budget, || {
            if frame_limit.is_some() {
                clock_frames * 16_666_667
            } else if layout.is_some() {
                started.elapsed().as_nanos() as u64
            } else {
                0
            }
        });
        tick_ns += tick.elapsed().as_nanos();
        if runtime.close_complete() {
            result.unwrap();
            #[cfg(feature = "vm-profiling")]
            vm_profile.finish(runtime.driver().adapter().vm());
            eprintln!(
                "host_cost clock_frames={clock_frames} tick_ns={tick_ns} sync_ns={sync_ns} premature_projections={premature_projections} game_closed=true"
            );
            return;
        }
        let sync_started = Instant::now();
        if let Some((bridge, root, frames)) = &mut layout {
            let driver = runtime.driver_mut().unwrap();
            let canvas = driver.adapter().game_settings().canvas_size;
            if applied_canvas != Some(canvas) {
                bridge.edit_widget(canvas_id.unwrap(), |mut widget| {
                    reel_ui::widgets::Canvas::set_size(
                        &mut widget.downcast::<reel_ui::widgets::Canvas>(),
                        canvas,
                    );
                });
                applied_canvas = Some(canvas);
            }
            let blocked =
                !driver.adapter().stage_ready() && !driver.adapter().virtual_feedback_boundary();
            let before = driver.adapter().stage().revision();
            driver.sync(bridge, *root).unwrap();
            premature_projections +=
                u64::from(blocked && driver.adapter().stage().revision() != before);
            let id = reel_core::identity::FrameId(*frames + 1);
            let stamp = driver.projection_stamp();
            if accepted_stamp != Some(stamp)
                && !driver.ui().images_pending()
                && let Some((source, receipt)) = driver.record(bridge, *root, id).unwrap()
            {
                let key = source.key();
                let epoch = key.surface_epoch();
                let notice = reel_ui::bridge::PresentationNotice::new(
                    key,
                    epoch,
                    reel_ui::bridge::PresentationOutcome::Accepted(id),
                );
                #[cfg(feature = "profiling")]
                if let Some(renderer) = &mut renderer {
                    renderer.stage(
                        source,
                        driver.ui().image_resources(),
                        receipt.frame_transitions(),
                    );
                }
                assert_eq!(
                    bridge.resolve(notice),
                    reel_ui::bridge::NoticeResult::Accepted
                );
                driver.presented(bridge, &receipt).unwrap();
                accepted_stamp = Some(stamp);
                *frames += 1;
                #[cfg(feature = "profiling")]
                {
                    use reel_core::profile::{Phase, Sample};
                    let now = Sample::snapshot();
                    let delta = now.delta(frame_profile);
                    frame_profile = now;
                    let stats = driver.adapter().vm().stats();
                    let now = [
                        stats.cumulative_instructions,
                        stats.cumulative_work,
                        stats.slices,
                        stats.native_entries,
                    ];
                    let work: [u64; 4] = std::array::from_fn(|i| now[i] - frame_vm[i]);
                    frame_vm = now;
                    let phase = |p: Phase| (delta.micros[p as usize], delta.calls[p as usize]);
                    eprintln!(
                        "host_presented frame={frames} title={:?} clock={clock_frames} vm_us_calls={:?} schedule_us_calls={:?} host_us_calls={:?} instructions={} work={} slices={} native_entries={}",
                        driver.adapter().game_settings().title,
                        phase(Phase::VmSlice),
                        phase(Phase::SessionSchedule),
                        phase(Phase::HostCall),
                        work[0],
                        work[1],
                        work[2],
                        work[3]
                    );
                    #[cfg(feature = "vm-profiling")]
                    vm_profile.observe(
                        driver.adapter().vm(),
                        &driver.adapter().game_settings().title,
                    );
                }
            }
            runtime.wake_input();
        }
        sync_ns += sync_started.elapsed().as_nanos();
        max_tick_ns = max_tick_ns.max(tick.elapsed().as_nanos());
        let elapsed_ms = started.elapsed().as_millis();
        if layout.is_some() {
            let slices = runtime.driver().adapter().vm().stats().slices;
            if slices != layout_last_work.0 {
                layout_last_work = (slices, elapsed_ms);
            }
        }
        if !initialized && runtime.driver().adapter().task() != initializer_task {
            initialized = true;
            eprintln!(
                "module_init_ms={elapsed_ms} heap={:?}",
                runtime.driver().adapter().vm().heap().stats()
            );
        }
        let settled = if let Some(limit) = frame_limit {
            if runtime.driver().adapter().stage_ready()
                && !runtime.driver().ui().images_pending()
                && !runtime.driver().has_runnable_tasks()
                && !runtime.driver().activities_pending()
                && !runtime.driver().transitions_pending()
                && !runtime.driver().cleanup_pending()
                && runtime.driver().adapter().vm().heap().phase() == GcPhase::Idle
                && accepted_stamp == Some(runtime.driver().projection_stamp())
            {
                clock_frames += 1;
            }
            clock_frames >= limit
        } else if let Some((_, _, count)) = &layout {
            !runtime.driver().adapter().activities_pending()
                && runtime.driver().adapter().stage_ready()
                && *count > 0
                && !runtime.driver().ui().images_pending()
                && elapsed_ms.saturating_sub(layout_last_work.1) >= 1000
        } else {
            !runtime.needs_tick() || runtime.tick_interval() > std::time::Duration::from_millis(1)
        };
        if elapsed_ms >= last_report + 1000 || settled || result.is_err() {
            last_report = elapsed_ms;
            eprintln!(
                "startup_ms={elapsed_ms} steps={steps} max_tick_ns={max_tick_ns} state={:?} stats={:?} result={result:?}",
                runtime.driver().state(),
                runtime.driver().adapter().vm().stats()
            );
            if let Some((_, _, frames)) = &layout {
                let vm = runtime.driver().adapter();
                let rows: Vec<_> = vm
                    .stage()
                    .nodes()
                    .filter_map(|n| {
                        if let reel_ui::stage::NodeKind::VirtualList(view) = &n.kind {
                            Some((view.revision, view.total_rows, view.rows.len()))
                        } else {
                            None
                        }
                    })
                    .collect();
                eprintln!(
                    "layout_frames={frames} clock_frames={clock_frames} interval={:?} images_pending={} accepted={} ready={} feedback={} nodes={} virtual_rows={rows:?} location={:?}",
                    runtime.tick_interval(),
                    runtime.driver().ui().images_pending(),
                    accepted_stamp == Some(runtime.driver().projection_stamp()),
                    vm.stage_ready(),
                    vm.virtual_feedback_ready(),
                    vm.stage().nodes().count(),
                    vm.task()
                        .and_then(|task| vm.vm().task_location(task).unwrap())
                );
            }
        }
        result.unwrap();
        assert!(
            layout.is_none()
                || elapsed_ms
                    < if std::env::var_os("REEL_PROFILE_RENDERER").is_some() {
                        180_000
                    } else {
                        15_000
                    },
            "layout probe did not settle"
        );
        if settled {
            #[cfg(feature = "profiling")]
            {
                let delta = reel_core::profile::Sample::snapshot().delta(profile_begin);
                for (i, name) in reel_core::profile::NAMES.iter().enumerate() {
                    if delta.calls[i] != 0 {
                        eprintln!(
                            "host_phase {name} us={} calls={}",
                            delta.micros[i], delta.calls[i]
                        );
                    }
                }
            }
            #[cfg(feature = "vm-profiling")]
            vm_profile.finish(runtime.driver().adapter().vm());
            eprintln!(
                "host_cost clock_frames={clock_frames} tick_ns={tick_ns} sync_ns={sync_ns} premature_projections={premature_projections}"
            );
            assert_eq!(
                runtime.driver().adapter().vm().heap().phase(),
                GcPhase::Idle
            );
            let frames = std::env::var("REEL_PROFILE_FRAMES")
                .map(|value| value.parse::<u64>().unwrap())
                .unwrap_or(0);
            let started = Instant::now();
            let mut max_clock_tick_ns = 0;
            for frame in 1..=frames {
                let (_, max_ns) = settle(&mut runtime, frame * 16_666_667);
                max_clock_tick_ns = max_clock_tick_ns.max(max_ns);
                if frame.is_multiple_of(60) || frame == frames {
                    let vm = runtime.driver().adapter().vm();
                    eprintln!(
                        "clock_frame={frame} elapsed_ms={} max_tick_ns={max_clock_tick_ns} heap={:?} location={:?}",
                        started.elapsed().as_millis(),
                        vm.heap().stats(),
                        runtime
                            .driver()
                            .adapter()
                            .task()
                            .and_then(|task| vm.task_location(task).unwrap())
                    );
                }
            }
            let started = Instant::now();
            runtime.driver_mut().unwrap().begin_collection().unwrap();
            while runtime.driver().adapter().vm().heap().phase() != GcPhase::Idle {
                runtime
                    .driver_mut()
                    .unwrap()
                    .gc_step(Default::default())
                    .unwrap();
            }
            eprintln!(
                "retained_gc_ms={} heap={:?}",
                started.elapsed().as_millis(),
                runtime.driver().adapter().vm().heap().stats()
            );
            return;
        }
    }
    panic!("distribution startup did not settle");
}

fn runtime(source: &str) -> StoryRuntime {
    StoryRuntime::new(
        AsideSessionUi::from_source(
            Session::new(SessionId(91), Epoch(1)).unwrap(),
            source,
            "runtime_cost",
            1,
            "main",
            "en",
            VmLimits::default(),
        )
        .unwrap(),
    )
}

fn settle(runtime: &mut StoryRuntime, now: u64) -> (usize, u128) {
    let mut max_ns = 0;
    for steps in 1..1_000_000 {
        let start = Instant::now();
        runtime.tick(SliceBudget::default(), || now).unwrap();
        max_ns = max_ns.max(start.elapsed().as_nanos());
        if !runtime.needs_tick() || runtime.tick_interval() > std::time::Duration::from_millis(1) {
            assert_eq!(
                runtime.driver().adapter().vm().heap().phase(),
                GcPhase::Idle
            );
            return (steps, max_ns);
        }
    }
    panic!("runtime did not settle");
}

#[test]
#[ignore = "measures host-side allocation and elapsed time"]
fn pending_asset_maintenance_cost() {
    use reel_core::{
        budget::QueueLimits,
        test_support::{CooperativeExecutor, FakeClock, MemoryAssetIo},
    };
    use reel_player::assets::{AssetKind, AssetOptions, Assets};

    let mut reused_storage = true;
    for count in [1, 16, 64] {
        let limits = QueueLimits {
            max_requests: 64,
            max_bytes: 1 << 20,
        };
        let assets = Assets::with_adapters(AssetOptions::default(), |wake| {
            (
                Box::new(MemoryAssetIo::new(limits, limits, wake.clone())),
                Box::new(CooperativeExecutor::new(FakeClock::default(), limits, wake)),
            )
        });
        let requests = (0..count)
            .map(|index| assets.request(format!("pending/{index}"), AssetKind::Image))
            .collect::<Vec<_>>();
        // The existing memory adapter holds admitted descriptions until an
        // explicit completion, keeping IO and decode work out of this measure.
        assets.maintain();
        assert!(assets.pending_io());
        let started = Instant::now();
        let stats = allocation_counter::measure(|| {
            for _ in 0..600 {
                assets.maintain();
            }
        });
        let elapsed = started.elapsed();
        eprintln!(
            "pending_assets={count} ticks=600 elapsed_ns={} allocations={} allocated_bytes={}",
            elapsed.as_nanos(),
            stats.count_total,
            stats.bytes_total
        );
        for request in requests {
            assert!(assets.poll(request).unwrap().is_none());
        }
        assets.close();
        assert!(!assets.pending_io());
        reused_storage &= stats.count_total == 0;
    }
    assert!(reused_storage, "waiting IO must reuse its request map");
}

#[test]
fn history_and_script_collection_clear_reclaim_before_sleeping() {
    for clear in ["game.reading().clear_history();", "history.clear();"] {
        let source = if clear.starts_with("game") {
            format!(
                r#"
                fn main() {{
                    let history = game.history();
                    for i in 0..1000 {{ game.record(f"line {{i}}"); }}
                    await run(delay(1));
                    {clear}
                    if history.len() != 0 {{ throw "history not cleared"; }}
                    await run(delay(1));
                }}
            "#
            )
        } else {
            r#"
                fn main() {
                    let history = [];
                    for i in 0..1000 { history.append([i]); }
                    await run(delay(1));
                    history.clear();
                    if history.len() != 0 { throw "array not cleared"; }
                    await run(delay(1));
                }
            "#
            .into()
        };
        let mut runtime = runtime(&source);
        settle(&mut runtime, 0);
        let before = runtime.driver().adapter().vm().heap().stats();
        settle(&mut runtime, 2_000_000_000);
        let after = runtime.driver().adapter().vm().heap().stats();
        assert!(after.collections > before.collections, "{clear}");
        assert!(
            after.live_objects < before.live_objects / 2,
            "{clear}: {before:?} -> {after:?}"
        );
        assert_eq!(after.pending_bytes, 0);
        assert!(!runtime.driver().adapter().vm().heap().collection_due());
    }
}

#[test]
fn one_table_removal_uses_allocation_pressure_while_clear_requests_reclamation() {
    let mut runtime = runtime(
        r#"
        fn main() {
            let retained = [];
            for i in 0..1000 { retained.append([i]); }
            let records = model({gone: [1], kept: [2]});
            let alias = records.kept;
            stage.replace(text(value: derive(|| f"left={records.len()}")));
            await run(delay(1));
            let removed = records.remove("gone");
            if removed[0] != 1 || records.len() != 1 || alias[0] != 2 || retained.len() != 1000 {
                throw "remove changed retained aliases";
            }
            await run(delay(1));
            records.clear();
            retained.clear();
            if records.len() != 0 || alias[0] != 2 || removed[0] != 1 { throw "clear changed retained aliases"; }
            await run(delay(1));
        }
    "#,
    );
    settle(&mut runtime, 0);
    runtime.driver_mut().unwrap().begin_collection().unwrap();
    while runtime.driver().adapter().vm().heap().phase() != GcPhase::Idle {
        runtime
            .driver_mut()
            .unwrap()
            .gc_step(Default::default())
            .unwrap();
    }
    let before = runtime.driver().adapter().vm().heap().stats();
    settle(&mut runtime, 2_000_000_000);
    let removed = runtime.driver().adapter().vm().heap().stats();
    assert_eq!(
        removed.collections, before.collections,
        "one removal must not rescan the retained heap"
    );
    assert!(!runtime.driver().adapter().vm().heap().collection_due());
    assert!(runtime.driver().adapter().stage().nodes().any(|node| matches!(&node.kind, reel_ui::stage::NodeKind::Text(text) if text.as_ref() == "left=1")));
    settle(&mut runtime, 4_000_000_000);
    let cleared = runtime.driver().adapter().vm().heap().stats();
    assert!(cleared.collections > removed.collections);
    assert!(cleared.live_objects < before.live_objects / 2);
    assert_eq!(cleared.pending_bytes, 0);
    assert!(!runtime.driver().adapter().vm().heap().collection_due());
    assert!(runtime.driver().adapter().stage().nodes().any(|node| matches!(&node.kind, reel_ui::stage::NodeKind::Text(text) if text.as_ref() == "left=0")));
}

#[test]
fn finite_object_quota_can_reclaim_short_lived_values() {
    let driver = AsideSessionUi::from_source(
        Session::new(SessionId(91), Epoch(1)).unwrap(),
        "fn main() { for i in 0..1000 { let garbage = [i]; } narrator(\"done\", history: false); }",
        "finite_cost",
        1,
        "main",
        "en",
        VmLimits {
            heap: aside_vm::HeapLimits {
                max_objects: 256,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let mut runtime = StoryRuntime::new(driver);
    for _ in 0..100_000 {
        runtime
            .tick(
                SliceBudget {
                    instructions: 1,
                    ..Default::default()
                },
                || 0,
            )
            .unwrap();
        if !runtime.needs_tick() {
            assert!(matches!(runtime.driver().ui().session().view().interaction,
                Some(reel_engine::InteractionView::Dialogue(dialogue)) if dialogue.text() == "done"));
            return;
        }
    }
    panic!("finite quota failed to settle");
}

#[test]
fn constructing_animated_layers_makes_progress_with_short_slices() {
    let source = r#"
        fn main() {
            stage.replace(stack(width: 960, height: 544));
            let world = stack(width: 1200, height: 720);
            for i in 0..24 {
                let x = 40 + (i % 8) * 140;
                let layer = image("layer.png", x: x, width: 190, height: 254, opacity: 0.65);
                world.add(layer);
                run(repeat(sequence(
                    tween(layer, x: x + 80, rotation: 12, opacity: 0.9,
                        duration: 1.5, easing: "sine_in_out"),
                    tween(layer, x: x, rotation: -8, opacity: 0.65,
                        duration: 1.5, easing: "sine_in_out")
                )), owner: world);
            }
            let sparks = particles("spark.png", x: 600, y: 300, seed: 17, blend: "add");
            world.add(sparks);
            let view = camera(width: 960, height: 544, camera_x: 600, camera_y: 360, clip: true) {
                world;
            };
            game.configure(title: "constructed");
            stage.replace(view);
            await run(delay(0.01));
            game.configure(title: "finished");
        }
    "#;
    let mut runtime = runtime(source);
    for step in 0..20_000 {
        // Sampling time and entering the VM consume the whole first slice.
        // A resumed turn must reach the VM before sampling another clock batch.
        let mut now = step * 1_000_000;
        runtime
            .tick(
                SliceBudget {
                    deadline_ns: Some(now + 4),
                    ..Default::default()
                },
                || {
                    now += 1;
                    now
                },
            )
            .unwrap();
        if runtime.driver().adapter().game_settings().title == "finished" {
            return;
        }
    }
    let adapter = runtime.driver().adapter();
    panic!(
        "construction stalled: nodes={} location={:?} stats={:?}",
        adapter.stage().nodes().count(),
        adapter
            .task()
            .and_then(|task| adapter.vm().task_location(task).unwrap()),
        adapter.vm().stats()
    );
}

#[test]
#[ignore = "reproducible runtime allocation and latency measurement"]
fn runtime_cost() {
    let rows = std::env::var("REEL_COST_ROWS")
        .ok()
        .map(|s| s.parse::<usize>().unwrap())
        .unwrap_or(10_000);
    let source = format!(
        r#"
        let items = [];
        fn main() {{
            for i in 0..{rows} {{ items.append([i, i]); }}
            let label = text("stable");
            stage.replace(label);
            await run(delay(1));
            for i in 0..10000 {{ label.x = i; }}
            if items.len() != {rows} || items[{last}][1] != {last} || label.x != 9999 {{ throw "lost retained data"; }}
            for i in 0..100 {{ await run(delay(1)); label.y = i; }}
            if label.y != 99 {{ throw "lost interaction update"; }}
            await run(delay(1));
            narrator("done", history: false);
        }}
    "#,
        last = rows - 1
    );
    let mut runtime = runtime(&source);
    settle(&mut runtime, 0);
    let before = runtime.driver().adapter().vm().heap().stats();
    let start = Instant::now();
    let ((steps, max_ns), allocation) = measure(|| settle(&mut runtime, 2_000_000_000));
    let elapsed = start.elapsed();
    let after = runtime.driver().adapter().vm().heap().stats();
    eprintln!(
        "stable_updates,rows={rows},updates=10000,elapsed_ns={},steps={steps},max_tick_ns={max_ns},gc_cycles={},gc_units={},allocations={},allocated_bytes={},live_delta_bytes={},live_objects={},payload_bytes={}",
        elapsed.as_nanos(),
        after.collections - before.collections,
        after.gc_units - before.gc_units,
        allocation.count_total,
        allocation.bytes_total,
        allocation.bytes_current,
        after.live_objects,
        after.payload_bytes
    );
    let before = after;
    let start = Instant::now();
    let ((steps, max_ns), allocation) = measure(|| {
        let mut max_ns = 0;
        let mut steps = 0;
        for i in 1..=100 {
            let (count, maximum) = settle(&mut runtime, (i + 1) * 2_000_000_000);
            steps += count;
            max_ns = max_ns.max(maximum);
        }
        (steps, max_ns)
    });
    let after = runtime.driver().adapter().vm().heap().stats();
    eprintln!(
        "stable_wakeups,rows={rows},wakeups=100,elapsed_ns={},steps={steps},max_tick_ns={max_ns},gc_cycles={},gc_units={},allocations={},allocated_bytes={},live_delta_bytes={},live_objects={},payload_bytes={}",
        start.elapsed().as_nanos(),
        after.collections - before.collections,
        after.gc_units - before.gc_units,
        allocation.count_total,
        allocation.bytes_total,
        allocation.bytes_current,
        after.live_objects,
        after.payload_bytes
    );
    assert!(
        after.collections - before.collections < 20,
        "low-allocation wakeups repeatedly scanned the retained graph"
    );

    let driver = runtime.driver();
    let program = driver.adapter().vm().program().clone();
    let start = Instant::now();
    let ((mut image, begin_ns, finish_ns, steps, max_ns), allocation) = measure(|| {
        let begin = Instant::now();
        let mut capture = driver
            .adapter()
            .snapshot_begin(driver.ui().session())
            .unwrap();
        let begin_ns = begin.elapsed().as_nanos();
        let mut max_ns = 0;
        let mut steps = 0;
        loop {
            let slice = Instant::now();
            let progress = capture.step(SnapshotBudget { units: 256 }).unwrap();
            max_ns = max_ns.max(slice.elapsed().as_nanos());
            steps += 1;
            if progress.complete {
                break;
            }
        }
        let finish = Instant::now();
        let image = capture.finish().unwrap();
        (image, begin_ns, finish.elapsed().as_nanos(), steps, max_ns)
    });
    eprintln!(
        "capture,rows={rows},elapsed_ns={},begin_ns={begin_ns},finish_ns={finish_ns},steps={steps},max_slice_ns={max_ns},allocated_bytes={},live_delta_bytes={},peak_delta_bytes={}",
        start.elapsed().as_nanos(),
        allocation.bytes_total,
        allocation.bytes_current,
        allocation.bytes_max
    );
    let start = Instant::now();
    let ((restored, begin_ns, finish_ns, steps, max_ns), allocation) = measure(|| {
        let begin = Instant::now();
        let mut restore = SessionVm::restore_begin(
            &mut image,
            program,
            VmLimits::default(),
            SessionIdentity {
                session_id: SessionId(92),
                epoch: Epoch(2),
            },
            Default::default(),
            None,
            None,
        )
        .unwrap();
        let begin_ns = begin.elapsed().as_nanos();
        let mut max_ns = 0;
        let mut steps = 0;
        loop {
            let slice = Instant::now();
            let progress = restore.step(SnapshotBudget { units: 256 }).unwrap();
            max_ns = max_ns.max(slice.elapsed().as_nanos());
            steps += 1;
            if progress.complete {
                break;
            }
        }
        let finish = Instant::now();
        let (_session, restored) = restore.finish().unwrap();
        (
            restored,
            begin_ns,
            finish.elapsed().as_nanos(),
            steps,
            max_ns,
        )
    });
    eprintln!(
        "restore,rows={rows},elapsed_ns={},begin_ns={begin_ns},finish_ns={finish_ns},steps={steps},max_slice_ns={max_ns},allocated_bytes={},live_delta_bytes={},peak_delta_bytes={},candidate_charged_bytes={}",
        start.elapsed().as_nanos(),
        allocation.bytes_total,
        allocation.bytes_current,
        allocation.bytes_max,
        restored.vm().heap().stats().charged_bytes
    );
}
