//! CPU-only stage projection, shaping reuse, geometry and portable-frame cost.
use super::*;
use reel_ui::{
    frame::{BuildLimits, FrameBuild, FrameDraw, FrameInfo, FrameOp, StepBudget},
    resources::{ResourceRegistry, ResourceSnapshot},
};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region};
use std::time::Instant;

#[test]
#[ignore = "isolated CPU allocation probe; requires REEL_SESSION_FONT; run with --test-threads=1"]
fn animated_scene_projection_and_frame_cost() {
    let font = reel_ui::peniko::Blob::from(
        std::fs::read(std::env::var_os("REEL_SESSION_FONT").expect("real font")).unwrap(),
    );
    let font_pool = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 4,
            max_bytes: 16 << 20,
        },
    );
    let mut registry = ResourceRegistry::new(4, 64 << 10).unwrap();
    let font_handle = registry
        .register(
            reel_core::identity::ResourceVersion {
                key: reel_core::identity::AssetKey("probe-font".into()),
                version: reel_core::identity::ContentVersion([1; 32]),
            },
            ResourceData::Font(font.clone()),
            font_pool.reserve(font.len() as u64).unwrap(),
        )
        .unwrap();
    let font_lease = registry.acquire(font_handle).unwrap();
    for button_count in [6, 24] {
        let mut f = Fixture::new(Some(font.clone()));
        let mut stage = Stage::default();
        let mut children = Vec::new();
        for index in 0..4 {
            children.push(
                stage
                    .create(
                        NodeKind::Stack,
                        NodeProps {
                            x: index as f64 * 120.,
                            y: 20.,
                            width: Some(100.),
                            height: Some(220.),
                            background: Some([64, 96, 128, 255]),
                            ..Default::default()
                        },
                        vec![],
                    )
                    .unwrap(),
            );
        }
        let moving = children[0];
        let text = "A conversation keeps the same shaped text while its scenery moves. The next line continues in the same dialogue panel.";
        let dialogue = stage
            .create(
                NodeKind::Text(text.into()),
                NodeProps {
                    x: 30.,
                    y: 270.,
                    width: Some(580.),
                    height: Some(80.),
                    font_size: 18.,
                    ..Default::default()
                },
                vec![],
            )
            .unwrap();
        children.push(dialogue);
        let mut buttons = Vec::new();
        for index in 0..button_count {
            buttons.push(
                stage
                    .create(
                        NodeKind::Button {
                            label: Some(format!("Menu {index}").into()),
                            listening: true,
                            focus_on_pointer: true,
                        },
                        NodeProps {
                            x: (index % 6) as f64 * 100.,
                            y: (index / 6) as f64 * 28.,
                            width: Some(96.),
                            height: Some(26.),
                            font_size: 16.,
                            ..Default::default()
                        },
                        vec![],
                    )
                    .unwrap(),
            );
        }
        children.push(
            stage
                .create(
                    NodeKind::Stack,
                    NodeProps {
                        y: 350.,
                        ..Default::default()
                    },
                    buttons,
                )
                .unwrap(),
        );
        let root = stage
            .create(
                NodeKind::Stack,
                NodeProps {
                    width: Some(640.),
                    height: Some(420.),
                    ..Default::default()
                },
                children,
            )
            .unwrap();
        stage.replace(root).unwrap();
        f.stage = stage;
        f.root = root;
        let first = f.record();
        f.accept(&first);
        drop(first);
        let initial_layouts = f.layouts();

        let region = Region::new(&INSTRUMENTED_SYSTEM);
        let started = Instant::now();
        for index in 0..1_200 {
            f.edit(moving, |props| props.x = (index % 120) as f64);
            f.ui.set_stage(&f.stage);
        }
        let projection_time = started.elapsed();
        let projection = region.change();
        let settle = f.record();
        f.accept(&settle);
        drop(settle);

        let pool = BudgetPool::new(
            BudgetCategory::CpuOutput,
            QueueLimits {
                max_requests: 64,
                max_bytes: 32 << 20,
            },
        );
        let region = Region::new(&INSTRUMENTED_SYSTEM);
        let started = Instant::now();
        let mut command_count = 0;
        let mut glyph_count = 0;
        for index in 0..240 {
            f.edit(moving, |props| props.x = (index % 120) as f64);
            let recorded = f.record();
            f.accept(&recorded);
            let mut build = FrameBuild::new(
                recorded.source,
                ResourceSnapshot::capture([Ok(font_lease.clone())].into_iter(), &pool).unwrap(),
                FrameInfo {
                    logical_size: [640., 420.],
                    physical_size: [960, 544],
                },
                BuildLimits { max_bytes: 8 << 20 },
                &pool,
            )
            .unwrap();
            while !build
                .step(StepBudget {
                    max_commands: 512,
                    max_units: 65536,
                    max_bytes: 1 << 20,
                })
                .unwrap()
                .complete
            {}
            let frame = build.finish().unwrap();
            command_count = frame.commands().len();
            glyph_count = frame
                .commands()
                .iter()
                .map(|command| match &command.op {
                    FrameOp::Draw(FrameDraw::GlyphRun(run)) => run.glyphs.len(),
                    _ => 0,
                })
                .sum::<usize>();
        }
        let frame_time = started.elapsed();
        let frame = region.change();
        assert_eq!(
            f.layouts(),
            initial_layouts,
            "moving scenery must reuse text layout and widget identities"
        );
        assert!(
            glyph_count >= text.len(),
            "the measurement must compile actual shaped glyphs"
        );
        assert_eq!(pool.usage().bytes, 0, "retired frames return their storage");
        println!(
            "animation_cost buttons={button_count} nodes={} commands={command_count} glyphs={glyph_count} projection_frames=1200 projection_us={} projection_allocations={} projection_bytes={} complete_frames=240 complete_us={} complete_allocations={} complete_bytes={}",
            initial_layouts.len(),
            projection_time.as_micros(),
            projection.allocations,
            projection.bytes_allocated,
            frame_time.as_micros(),
            frame.allocations,
            frame.bytes_allocated
        );
    }
}
