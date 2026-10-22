//! CPU-only text-editor storage and stable-frame allocation measurement.
use reel_ui::{
    app::{RenderRoot, RenderRootOptions, WindowSizePolicy},
    core::{DefaultProperties, NewWidget},
    dpi::PhysicalSize,
    peniko::Blob,
    properties::Dimensions,
    stage::{NodeKind, NodeProps, StageNode},
    widgets::TextInput,
};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
use std::{alloc::System, sync::Arc, time::Instant};
#[global_allocator]
static ALLOCATOR: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
#[ignore = "isolated text editor allocation probe; no window or graphics device"]
fn text_input_storage_and_stable_layout_cost() {
    let font = Blob::new(Arc::new(
        include_bytes!("../../reel-player/tests/fixtures/font-weight/400.ttf").to_vec(),
    ));
    println!(
        "sizes,editor={},kind={},props={},node={}",
        size_of::<TextInput>(),
        size_of::<NodeKind>(),
        size_of::<NodeProps>(),
        size_of::<StageNode>()
    );
    let mut stable_baseline = None;
    for (value, hint) in [("", ""), ("AAA", "AAA"), ("", "AAA")] {
        let region = Region::new(ALLOCATOR);
        let widget = TextInput::new(value);
        let widget = if hint.is_empty() {
            widget
        } else {
            widget.with_placeholder(hint)
        };
        let widget = NewWidget::new(widget).with_props(Dimensions::MAX);
        let mut root = RenderRoot::new(
            widget,
            |_| {},
            RenderRootOptions {
                default_properties: Arc::new(DefaultProperties::new()),
                use_system_fonts: false,
                size_policy: WindowSizePolicy::User,
                size: PhysicalSize::new(280, 60),
                scale_factor: 1.,
                default_font: Some(font.clone()),
            },
        );
        for _ in 0..8 {
            drop(root.redraw());
        }
        let mounted = region.change();
        let stable = Region::new(ALLOCATOR);
        let started = Instant::now();
        for _ in 0..120 {
            drop(root.redraw());
        }
        let elapsed = started.elapsed();
        let stable = stable.change();
        drop(root);
        let retired = region.change();
        let allocation = (stable.allocations, stable.bytes_allocated);
        assert_eq!(
            *stable_baseline.get_or_insert(allocation),
            allocation,
            "a retained hint must not add stable-frame allocations"
        );
        assert_eq!(
            retired.bytes_allocated, retired.bytes_deallocated,
            "the retired editor must release its layout and input state"
        );
        println!(
            "editor,value={value:?},hint={hint:?},mounted_bytes={},stable_allocations={},stable_bytes={},stable_ns={},retired_bytes={}",
            mounted.bytes_allocated as i64 - mounted.bytes_deallocated as i64,
            stable.allocations,
            stable.bytes_allocated,
            elapsed.as_nanos(),
            retired.bytes_allocated as i64 - retired.bytes_deallocated as i64
        );
    }
}
