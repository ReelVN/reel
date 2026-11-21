use super::*;
use crate::assets::AssetOptions;
use reel_ui::resources::ColorEncoding;
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    time::Duration,
};

fn preload_stage(source: &str) -> (reel_ui::stage::Stage, reel_ui::stage::NodeId) {
    use reel_ui::stage::{NodeKind, NodeProps, Stage};
    let mut stage = Stage::default();
    let id = stage
        .create(
            NodeKind::Image {
                source: source.into(),
                origin: None,
                sampling: Default::default(),
                source_rect: None,
                fit: Default::default(),
                slice: None,
                slice_scale: 1.,
                slice_center: true,
                repeat: [false; 2],
                tile_scale: 1.,
                corner_colors: None,
                corners: None,
                alpha_layout: Default::default(),
                adjustment: Default::default(),
            },
            NodeProps {
                visible: false,
                modal: true,
                width: Some(160.),
                height: Some(90.),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    stage.replace(id).unwrap();
    (stage, id)
}

#[test]
fn hidden_prefetch_keeps_presentation_ready_and_reuses_decoded_pixels_on_show() {
    let (mut images, wake) = loader();
    let (mut stage, id) = preload_stage("assets/panels.png");
    let hidden = crate::stage_widget::StageView::capture(&stage);
    images.request_stage(&hidden);
    assert!(images.poll().unwrap());
    let pending = images.pending[0].id;
    let scans = images.metadata_scans;
    let revision = images.content_revision;
    images.request_stage(&hidden);
    assert_eq!(images.metadata_scans, scans);
    assert_eq!(
        images.pending[0].id, pending,
        "unchanged foreground keeps its in-flight prefetch"
    );
    while !images.loaded.contains_key("assets/panels.png") {
        wake.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(
            images.poll().unwrap(),
            "hidden preparation must retain foreground readiness"
        );
    }
    assert_eq!(
        images.content_revision, revision,
        "hidden pixels do not change the visible layout"
    );
    images.commit();
    let handle = images.loaded["assets/panels.png"].handle();
    let loads = images.loads;
    let mut node = stage.node(id).unwrap().clone();
    node.props.visible = true;
    stage.update(id, node.kind, node.props).unwrap();
    images.request_stage(&crate::stage_widget::StageView::capture(&stage));
    assert!(images.poll().unwrap());
    assert_eq!(images.loaded["assets/panels.png"].handle(), handle);
    assert_eq!(images.loads, loads);
    images.close();
    assert_eq!(images.pool.usage().bytes, 0);
}

#[test]
fn visible_skin_prepares_the_next_state_once_and_drops_offscreen_demand() {
    use crate::stage_widget::StageView;
    use reel_ui::stage::NodeKind;
    // The 96x96 hover fits in the prefetch quarter, the 160x90 base does not.
    let (mut images, wake) = configured(160 * 90 * 4 * 3, 256 << 20);
    let base = "assets/panels.png";
    let hover = "assets/sun.jpg";
    let paths = Arc::from([base.to_owned(), hover.to_owned()]);
    let (mut stage, id) = preload_stage(base);
    let mut art = stage.node(id).unwrap().clone();
    let NodeKind::Image { source, .. } = &mut art.kind else {
        unreachable!()
    };
    source.skin_paths = Some(Arc::clone(&paths));
    art.props.visible = true;
    art.props.modal = false;
    stage.update(id, art.kind, art.props).unwrap();
    let view = StageView::capture(&stage);
    let format = images.shader_format;
    images.request_stage_images(view.viewport_image_requests(vec![id], vec![], format));
    assert!(images.prefetch.stage.is_empty());
    images.request_stage_images(view.viewport_image_requests(vec![], vec![], format));
    assert!(images.is_required(base) && !images.is_required(hover));
    assert!(images.prefetch.contains(hover));
    while !images.loaded.contains_key(hover) {
        images.poll().unwrap();
        if !images.loaded.contains_key(hover) {
            wake.recv_timeout(Duration::from_secs(10)).unwrap();
        }
    }
    images.commit();
    let handle = images.loaded[hover].handle();
    let loads = images.loads;
    let mut art = stage.node(id).unwrap().clone();
    let NodeKind::Image { source, .. } = &mut art.kind else {
        unreachable!()
    };
    *source = hover.into();
    source.skin_paths = Some(paths);
    stage.update(id, art.kind, art.props).unwrap();
    let view = StageView::capture(&stage);
    images.request_stage_images(view.viewport_image_requests(vec![], vec![], format));
    assert!(images.poll().unwrap());
    assert_eq!(images.loaded[hover].handle(), handle);
    assert_eq!(images.loads, loads);
    images.commit();
    assert!(!images.loaded.contains_key(base));
    assert_eq!(images.hidden_prefetch_bytes(), 0);
    images.request_stage_images(view.viewport_image_requests(vec![id], vec![], format));
    images.commit();
    assert!(images.prefetch.stage.is_empty());
    assert!(!images.loaded.contains_key(base));
}

#[test]
fn visible_image_takes_over_in_flight_prefetch() {
    let (mut images, wake) = configured(160 * 90 * 4, 256 << 20);
    let source = "assets/panels.png";
    let (mut stage, id) = preload_stage(source);
    images.request_stage(&crate::stage_widget::StageView::capture(&stage));
    assert!(images.poll().unwrap());
    let pending = images.pending[0].id;
    let loads = images.loads;
    let mut node = stage.node(id).unwrap().clone();
    node.props.visible = true;
    stage.update(id, node.kind, node.props).unwrap();
    images.request_stage(&crate::stage_widget::StageView::capture(&stage));
    assert_eq!(images.pending[0].id, pending);
    assert!(!images.pending[0].optional);
    ready(&mut images, &wake).unwrap();
    assert!(images.loaded.contains_key(source));
    assert_eq!(images.loads, loads);
}

#[test]
fn dialogue_prediction_yields_to_a_visible_request_in_the_same_update() {
    let (mut images, wake) = loader();
    let source = "assets/panels.png";
    images.set_image_prediction(&[source.into()]);
    assert!(images.poll().unwrap());
    let id = images.pending[0].id;

    images.set_image_prediction(&[]);
    let (mut stage, node) = preload_stage(source);
    let mut image = stage.node(node).unwrap().clone();
    image.props.visible = true;
    image.props.modal = false;
    stage.update(node, image.kind, image.props).unwrap();
    images.request_stage(&crate::stage_widget::StageView::capture(&stage));
    assert_eq!(images.pending[0].id, id);
    assert!(!images.pending[0].optional);
    ready(&mut images, &wake).unwrap();
    assert_eq!(images.loads, 1);
}

#[test]
fn predicted_fixed_image_survives_intrinsic_layout_before_viewport_demand() {
    use crate::stage_widget::StageView;
    use reel_ui::stage::{NodeKind, NodeProps, Stage};

    let (mut images, wake) = loader();
    let source = "assets/panels.png";
    images.set_image_prediction(&[source.into()]);
    assert!(images.poll().unwrap());
    let prefetch = images.pending[0].id;
    images.set_image_prediction(&[]);

    let (sample, id) = preload_stage(source);
    let mut kind = sample.node(id).unwrap().kind.clone();
    let mut stage = Stage::default();
    let fixed = stage
        .create(
            kind.clone(),
            NodeProps {
                width: Some(160.),
                height: Some(90.),
                ..Default::default()
            },
            vec![],
        )
        .unwrap();
    let NodeKind::Image { source, .. } = &mut kind else {
        unreachable!()
    };
    *source = "assets/sun.jpg".into();
    let intrinsic = stage.create(kind, NodeProps::default(), vec![]).unwrap();
    let root = stage
        .create(
            NodeKind::Stack,
            NodeProps::default(),
            vec![fixed, intrinsic],
        )
        .unwrap();
    stage.replace(root).unwrap();
    let view = StageView::capture(&stage);
    let format = images.shader_format;

    images.request_stage_images(view.layout_image_requests(format));
    assert!(!images.metadata_ready());
    assert_eq!(images.pending[0].id, prefetch);
    assert!(images.pending[0].prefetch);
    ready(&mut images, &wake).unwrap();

    images.request_stage_images(view.viewport_image_requests(vec![], vec![], format));
    ready(&mut images, &wake).unwrap();
    assert!(images.loaded.contains_key("assets/panels.png"));
    assert!(images.loaded.contains_key("assets/sun.jpg"));
    assert_eq!(images.loads, 2);
}

#[test]
fn dialogue_prefetch_reuses_matching_work_and_replaces_unrelated_stage_work() {
    use reel_core::{budget::QueueLimits, test_support::MemoryAssetIo};
    let assets = Assets::with_adapters(Default::default(), |wake| {
        let limits = QueueLimits {
            max_requests: 8,
            max_bytes: 1 << 20,
        };
        (
            Box::new(MemoryAssetIo::new(limits, limits, wake.clone())),
            Box::new(crate::assets::ImageExecutor::new(limits, wake)),
        )
    });
    let mut images = Images::new(assets);
    let skin = "skin.png";
    let story = "story.png";
    images.request_paths(BTreeMap::new(), BTreeSet::from([skin.into()]));
    assert!(images.poll().unwrap());
    let skin_id = images.pending[0].id;

    images.set_image_prediction(&[skin.into()]);
    assert!(images.poll().unwrap());
    assert_eq!(images.pending.len(), 1);
    assert_eq!(images.pending[0].id, skin_id);
    assert!(images.pending[0].prefetch);

    images.set_image_prediction(&[story.into()]);
    assert!(images.poll().unwrap());
    assert_eq!(images.pending.len(), 1);
    assert_eq!(images.pending[0].source, story);
    assert_ne!(images.pending[0].id, skin_id);

    images.set_image_prediction(&[]);
    assert!(images.poll().unwrap());
    assert_eq!(images.pending.len(), 1);
    assert_eq!(images.pending[0].source, skin);
    assert_ne!(images.pending[0].id, skin_id);
}

#[test]
fn superseded_candidate_cannot_lend_its_foreground_allocation_to_prediction() {
    let (mut images, wake) = configured(3 * 160 * 90 * 4, 256 << 20);
    let source = "assets/panels.png";
    images.request(wanted(&[source]));
    ready(&mut images, &wake).unwrap();
    images.set_image_prediction(&[source.into()]);
    images.request_stage(&crate::stage_widget::StageView::capture(
        &reel_ui::stage::Stage::default(),
    ));
    assert!(images.poll().unwrap());
    assert!(!images.loaded.contains_key(source));
    assert_eq!(images.hidden_prefetch_bytes(), 0);
}

#[test]
fn removing_a_preloaded_page_cancels_its_work_without_retaining_pixels() {
    let (mut images, _) = loader();
    let (mut stage, _) = preload_stage("assets/panels.png");
    images.request_stage(&crate::stage_widget::StageView::capture(&stage));
    assert!(images.poll().unwrap());
    assert_eq!(images.pending.len(), 1);
    let root = stage
        .create(reel_ui::stage::NodeKind::Stack, Default::default(), vec![])
        .unwrap();
    stage.replace(root).unwrap();
    images.request_stage(&crate::stage_widget::StageView::capture(&stage));
    assert!(images.pending.is_empty());
    assert!(images.prefetch.stage.is_empty());
    assert!(images.loaded.is_empty());
    assert!(images.poll().unwrap());
}

#[test]
fn returning_gpu_image_does_not_cancel_unrelated_prefetch() {
    let (mut images, wake) = loader();
    let visible = "assets/panels.png";
    images.request(wanted(&[visible]));
    ready(&mut images, &wake).unwrap();
    assert!(images.loaded[visible].discard_uploaded_pixels());
    let _gpu_source = images.loaded[visible].retain_uploaded_image().unwrap();
    images.commit();
    images.request(wanted(&[]));
    images.commit();

    images.prefetch.stage.insert("assets/sun.jpg".into());
    images.settled = false;
    assert!(images.poll().unwrap());
    let queued = images.pending[0].id;
    images.wanted = wanted(&[visible]);
    images.mounted.insert(visible.into());
    images.reconcile_requests();
    assert!(images.loaded.contains_key(visible));
    assert_eq!(images.pending[0].id, queued);
}

#[test]
fn encoded_hidden_images_use_the_same_bounded_prefetch_path() {
    use reel_ui::stage::{ImageSource, NodeKind};
    let (mut images, wake) = loader();
    let (mut stage, id) = preload_stage("unused.png");
    let mut node = stage.node(id).unwrap().clone();
    let NodeKind::Image { source, .. } = &mut node.kind else {
        unreachable!()
    };
    *source = ImageSource::encoded(Arc::<[u8]>::from(
        &include_bytes!("../../../../examples/assets/panels.png")[..],
    ));
    let key = source.key().to_owned();
    stage.update(id, node.kind, node.props).unwrap();
    images.request_stage(&crate::stage_widget::StageView::capture(&stage));
    assert!(images.poll().unwrap());
    while !images.loaded.contains_key(&key) {
        wake.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(images.poll().unwrap());
    }
    assert_eq!(images.loaded[&key].image_size(), Some([160, 90]));
}

#[test]
fn prefetch_quota_rejection_is_settled_then_visible_demand_loads_normally() {
    let (mut images, wake) = configured(160 * 90 * 4, 256 << 20);
    let (stage, _) = preload_stage("assets/panels.png");
    images.request_stage(&crate::stage_widget::StageView::capture(&stage));
    assert!(images.poll().unwrap());
    while !images.skipped.contains("assets/panels.png") {
        wake.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(images.poll().unwrap());
    }
    assert!(images.loaded.is_empty());
    let loads = images.loads;
    for _ in 0..8 {
        assert!(images.poll().unwrap());
    }
    assert_eq!(
        images.loads, loads,
        "settled prefetch must not retry each frame"
    );
    images.request(wanted(&["assets/panels.png"]));
    ready(&mut images, &wake).unwrap();
    assert!(images.loaded.contains_key("assets/panels.png"));
}

#[test]
fn foreground_residency_does_not_consume_the_hidden_prefetch_share() {
    const PANEL: u64 = 160 * 90 * 4;
    let (mut images, wake) = configured(PANEL * 4, 256 << 20);
    let foreground = images.pool.reserve(PANEL * 2).unwrap();
    let (stage, _) = preload_stage("assets/panels.png");
    images.request_stage(&crate::stage_widget::StageView::capture(&stage));
    images.prefetch.stage.insert("assets/sun.jpg".into());
    assert!(images.poll().unwrap());
    while !images.skipped.contains("assets/sun.jpg") {
        wake.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(images.poll().unwrap());
    }
    assert!(images.loaded.contains_key("assets/panels.png"));
    assert!(!images.loaded.contains_key("assets/sun.jpg"));
    assert_eq!(images.hidden_prefetch_bytes(), PANEL);
    assert_eq!(images.pool.usage().bytes, PANEL * 3);
    images.close();
    assert_eq!(images.pool.usage().bytes, PANEL * 2);
    drop(foreground);
    assert_eq!(images.pool.usage().bytes, 0);
}

fn loader() -> (Images, Receiver<()>) {
    configured(256 << 20, 256 << 20)
}
fn configured(resident_bytes: u64, decode_bytes: u64) -> (Images, Receiver<()>) {
    let mut images = Images::new(Assets::directory(AssetOptions {
        root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples"),
        resident_bytes,
        decode_bytes,
        ..Default::default()
    }));
    let (send, receive) = mpsc::channel();
    images.set_waker(Arc::new(move || {
        let _ = send.send(());
    }));
    (images, receive)
}
fn wanted(sources: &[&str]) -> BTreeMap<String, Option<String>> {
    sources
        .iter()
        .map(|source| (source.to_string(), Some("main:17:9".into())))
        .collect()
}
fn ready(images: &mut Images, wake: &Receiver<()>) -> Result<(), String> {
    loop {
        if images.poll()? {
            return Ok(());
        }
        wake.recv_timeout(Duration::from_secs(10))
            .expect("decoder must wake its host");
    }
}
fn pixels(lease: &ResourceLease) -> reel_ui::peniko::ImageData {
    assert_eq!(
        lease.image_descriptor().expect("expected image").encoding,
        ColorEncoding::Srgb
    );
    crate::test_image::data(lease)
}

#[test]
fn real_images_share_storage_replace_atomically_and_release_after_frame_leases() {
    let (mut images, wake) = loader();
    images.request(wanted(&["assets/panels.png", "assets/panels.png"]));
    ready(&mut images, &wake).unwrap();
    images.commit();
    let submitted_frame = images.loaded["assets/panels.png"].clone();
    let panel = pixels(&submitted_frame);
    assert_eq!([panel.width, panel.height], [160, 90]);
    assert_eq!(&panel.data.as_ref()[0..4], &[224, 64, 48, 255]);
    assert_eq!(&panel.data.as_ref()[80 * 4..81 * 4], &[32, 112, 224, 128]);
    images.request(wanted(&["assets/panels.png"]));
    ready(&mut images, &wake).unwrap();
    assert_eq!(images.loads, 1);
    assert_eq!(
        pixels(&images.loaded["assets/panels.png"]).data.id(),
        panel.data.id()
    );
    assert_eq!(images.pool.usage().bytes, 160 * 90 * 4);
    drop(panel);

    images.request(wanted(&["assets/sun.jpg"]));
    assert!(!images.poll().unwrap());
    assert!(
        !submitted_frame.is_retired(),
        "old complete scene remains publishable while loading"
    );
    assert!(images.loaded.contains_key("assets/panels.png"));
    ready(&mut images, &wake).unwrap();
    assert!(
        !submitted_frame.is_retired(),
        "readiness alone must not retire the visible scene"
    );
    let sun = pixels(&images.loaded["assets/sun.jpg"]);
    assert_eq!([sun.width, sun.height], [96, 96]);
    for (offset, expected) in [
        (0, [20_i16, 48, 88, 255]),
        ((48 * 96 + 48) * 4, [255, 200, 48, 255]),
    ] {
        let actual = &sun.data.as_ref()[offset..offset + 4];
        assert!(
            actual
                .iter()
                .zip(expected)
                .all(|(actual, expected)| (i16::from(*actual) - expected).abs() <= 8),
            "JPEG pixel={actual:?}, expected={expected:?}"
        );
    }
    drop(sun);
    images.commit();
    assert_eq!(images.loads, 2);
    assert!(!submitted_frame.is_retired());
    assert!(!images.loaded.contains_key("assets/panels.png"));
    assert_eq!(
        &pixels(&submitted_frame).data.as_ref()[0..4],
        &[224, 64, 48, 255]
    );
    let mut other = Images::new(images.assets.clone());
    other.request(wanted(&["assets/panels.png"]));
    ready(&mut other, &wake).unwrap();
    other.commit();
    assert_eq!(
        other.loaded["assets/panels.png"].handle(),
        submitted_frame.handle()
    );
    images.close();
    assert!(!other.loaded["assets/panels.png"].is_retired());
    assert_eq!(images.pool.usage().bytes, 160 * 90 * 4);
    other.close();
    assert_eq!(images.pool.usage().bytes, 160 * 90 * 4);
    drop(submitted_frame);
    assert_eq!(images.pool.usage().bytes, 0);
    assert_eq!(images.work.usage().bytes, 0);
    eprintln!(
        "decodes={}, resident={:?}, decoding={:?}",
        images.loads,
        images.pool.usage(),
        images.work.usage()
    );
}

#[test]
fn returning_uploaded_images_reuses_identity_without_reading_pixels_again() {
    let (mut images, wake) = loader();
    images.request(wanted(&["assets/panels.png"]));
    ready(&mut images, &wake).unwrap();
    let panel = &images.loaded["assets/panels.png"];
    let first = panel.handle();
    let weak = panel.downgrade();
    assert!(panel.discard_uploaded_pixels());
    let gpu_source = panel.retain_uploaded_image().unwrap();
    images.commit();

    images.request(wanted(&["assets/sun.jpg"]));
    ready(&mut images, &wake).unwrap();
    images.commit();
    assert!(!images.loaded.contains_key("assets/panels.png"));
    assert!(!weak.is_released());
    assert_eq!(images.loads, 2);

    images.prefetch.stage.insert("assets/panels.png".to_owned());
    images.settled = false;
    while !images.loaded.contains_key("assets/panels.png") {
        assert!(images.poll().unwrap());
        if !images.loaded.contains_key("assets/panels.png") {
            wake.recv_timeout(Duration::from_secs(10)).unwrap();
        }
    }
    assert_eq!(images.loaded["assets/panels.png"].handle(), first);
    assert!(images.pending.is_empty());
    let loads = images.loads;

    images.request(wanted(&["assets/panels.png"]));
    assert!(images.poll().unwrap());
    assert_eq!(images.loaded["assets/panels.png"].handle(), first);
    assert!(!images.loaded["assets/panels.png"].pixels_resident());
    assert_eq!(
        images.loads, loads,
        "a prefetched GPU identity needs no additional lookup"
    );
    images.close();
    assert!(!weak.is_released());
    let mut replacement = Images::new(images.assets.clone());
    replacement.request(wanted(&["assets/panels.png"]));
    ready(&mut replacement, &wake).unwrap();
    assert_eq!(replacement.loaded["assets/panels.png"].handle(), first);
    replacement.close();
    drop(gpu_source);
    images.assets.close();
    assert!(weak.is_released());
    assert_eq!(images.pool.usage().bytes, 0);
}

#[test]
fn decoded_stage_images_are_reused_until_visible_loads_need_the_budget() {
    let (mut images, wake) = configured(2 << 20, 256 << 20);
    let source = "assets/panels.png";
    images.request(wanted(&[source]));
    ready(&mut images, &wake).unwrap();
    let handle = images.loaded[source].handle();
    let weak = images.loaded[source].downgrade();
    images.commit();

    images.request(wanted(&[]));
    images.commit();
    assert!(!weak.is_released());
    images.request(wanted(&[source]));
    assert!(images.poll().unwrap());
    assert_eq!(images.loaded[source].handle(), handle);
    assert_eq!(images.loads, 1);
    images.commit();

    images.request(wanted(&[]));
    images.commit();
    let competing = images
        .pool
        .reserve(images.pool.available_bytes() - 1)
        .unwrap();
    images.request(wanted(&["assets/sun.jpg"]));
    ready(&mut images, &wake).unwrap();
    assert!(weak.is_released());
    assert_eq!(images.loads, 2);
    drop(competing);
}

#[test]
fn worker_errors_keep_constructor_location_and_cancelled_completions_cannot_resurrect() {
    for (source, resident, decode, expected) in [
        ("assets/missing.png", 1 << 20, 1 << 20, "image"),
        (
            "author_images/src/main.aside",
            1 << 20,
            1 << 20,
            "expected PNG, JPEG, static WebP or QOI artwork",
        ),
        ("assets/panels.png", 1, 1 << 20, "resident budget"),
        ("assets/panels.png", 1 << 20, 1, "limit"),
    ] {
        let (mut images, wake) = configured(resident, decode);
        images.request(wanted(&[source]));
        let error = ready(&mut images, &wake).unwrap_err();
        assert!(
            error.contains("main:17:9") && error.contains(source) && error.contains(expected),
            "{error}"
        );
        assert_eq!(images.pool.usage().bytes, 0);
        assert_eq!(images.work.usage().bytes, 0);
    }
    let (mut images, wake) = loader();
    images.request(wanted(&["assets/panels.png"]));
    assert!(!images.poll().unwrap());
    // Leave a successful decoded lease queued, then withdraw its request before
    // polling. This tests an actual late result, rather than only early IO cancellation.
    loop {
        wake.recv_timeout(Duration::from_secs(10)).unwrap();
        if images.pool.usage().bytes == 160 * 90 * 4 && images.work.usage().bytes == 0 {
            break;
        }
        assert!(!images.poll().unwrap());
    }
    assert_eq!(images.pool.usage().bytes, 160 * 90 * 4);
    assert_eq!(images.work.usage().bytes, 0);
    images.request(BTreeMap::new());
    assert!(images.poll().unwrap());
    assert!(images.loaded.is_empty());
    assert_eq!(images.pool.usage().bytes, 0);
    images.request(wanted(&["assets/sun.jpg"]));
    assert!(!images.poll().unwrap());
    let resident = images.pool.clone();
    let working = images.work.clone();
    images.close();
    // Exit notification follows receiver destruction and dropping any late result.
    loop {
        wake.recv_timeout(Duration::from_secs(10)).unwrap();
        if resident.usage().bytes == 0 && working.usage().bytes == 0 {
            break;
        }
    }
    assert!(!images.poll().unwrap());
    assert!(images.loaded.is_empty());
    assert_eq!(resident.usage().bytes, 0);
    assert_eq!(working.usage().bytes, 0);
}

pub(crate) struct ControlledIo(
    pub(crate) std::rc::Rc<std::cell::RefCell<reel_core::test_support::MemoryAssetIo>>,
);
impl reel_core::io::AssetIo for ControlledIo {
    fn capabilities(&self) -> reel_core::io::IoCaps {
        self.0.borrow().capabilities()
    }
    fn describe(
        &mut self,
        request: reel_core::io::DescribeRequest,
    ) -> Result<
        reel_core::identity::IoId,
        reel_core::result::Rejected<reel_core::io::DescribeRequest>,
    > {
        self.0.borrow_mut().describe(request)
    }
    fn read(
        &mut self,
        request: reel_core::io::ReadRequest,
    ) -> Result<reel_core::identity::IoId, reel_core::result::Rejected<reel_core::io::ReadRequest>>
    {
        self.0.borrow_mut().read(request)
    }
    fn replace_save(
        &mut self,
        request: reel_core::io::SaveWrite,
    ) -> Result<reel_core::identity::IoId, reel_core::result::Rejected<reel_core::io::SaveWrite>>
    {
        self.0.borrow_mut().replace_save(request)
    }
    fn promote_visible(&mut self, id: reel_core::identity::IoId) {
        self.0.borrow_mut().promote_visible(id);
    }
    fn cancel(&mut self, id: reel_core::identity::IoId) {
        self.0.borrow_mut().cancel(id);
    }
    fn poll(&mut self, out: &mut Vec<reel_core::io::IoEvent>, limit: usize) {
        self.0.borrow_mut().poll(out, limit);
    }
}

#[test]
fn batch_image_reads_fill_existing_queue_and_superseded_results_retire() {
    use reel_core::{
        budget::{BudgetCategory, OwnedBytes, QueueLimits},
        identity::{AssetKey, ContentVersion, ResourceVersion},
        test_support::{MemoryAsset, MemoryAssetIo},
    };
    use std::{cell::RefCell, rc::Rc, time::Instant};
    const PNG: &[u8] = include_bytes!("../../../../examples/assets/panels.png");
    for supersede in [false, true] {
        let mut host = None;
        let assets = Assets::with_adapters(
            AssetOptions {
                input_bytes: PNG.len() as u64 * 2,
                ..Default::default()
            },
            |wake| {
                let limits = QueueLimits {
                    max_requests: 32,
                    max_bytes: 768 << 20,
                };
                let storage = BudgetPool::new(
                    BudgetCategory::CpuInput,
                    QueueLimits {
                        max_requests: 40,
                        ..limits
                    },
                );
                let mut io = MemoryAssetIo::new(limits, limits, wake.clone());
                for index in 0..40 {
                    io.insert_asset(
                        ResourceVersion {
                            key: AssetKey(format!("{index:02}.png")),
                            version: ContentVersion([index; 32]),
                        },
                        MemoryAsset {
                            bytes: OwnedBytes::new(
                                PNG.to_vec(),
                                storage.reserve(PNG.len() as u64).unwrap(),
                            )
                            .unwrap(),
                            media_type: None,
                        },
                    )
                    .unwrap();
                }
                let io = Rc::new(RefCell::new(io));
                host = Some(io.clone());
                (
                    Box::new(ControlledIo(io)),
                    Box::new(crate::assets::ImageExecutor::new(limits, wake)),
                )
            },
        );
        let host = host.unwrap();
        let mut images = Images::new(assets);
        images.request(
            (0..40)
                .map(|n| (format!("{n:02}.png"), Some("batch:1:1".into())))
                .collect(),
        );
        assert!(!images.poll().unwrap());
        assert_eq!(
            host.borrow().read_usage().requests,
            32,
            "first batch must occupy the existing queue before any completion"
        );
        assert!(!images.poll().unwrap());
        assert_eq!(
            host.borrow().read_usage().requests,
            32,
            "repeated polling must preserve bounded admission"
        );
        while host.borrow_mut().complete_next_read() {}
        if supersede {
            // Descriptions have completed, but their original consumer is already gone.
            images.request(wanted(&["00.png", "39.png"]));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            while host.borrow_mut().complete_next_read() {}
            if images.poll().unwrap() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "bounded batch stalled under input contention"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(images.loaded.len(), if supersede { 2 } else { 40 });
        if supersede {
            assert!(images.loaded.contains_key("00.png"));
            assert!(images.loaded.contains_key("39.png"));
        } else {
            assert_eq!(
                images.loads, 40,
                "each source is decoded once while filling later batches"
            );
        }
        assert_eq!(
            images.pool.usage().bytes,
            images.loaded.len() as u64 * 160 * 90 * 4
        );
        images.close();
        images.assets.maintain();
        assert_eq!(host.borrow().read_usage().requests, 0);
        assert_eq!(images.pool.usage().bytes, 0);
        assert_eq!(images.work.usage().bytes, 0);
    }
}

#[test]
fn encoded_image_batch_waits_for_input_capacity_without_failing() {
    const PNG: &[u8] = include_bytes!("../../../../examples/assets/panels.png");
    let mut images = Images::new(Assets::directory(AssetOptions {
        // Reserve the streaming quarter while admitting exactly one ordinary image.
        input_bytes: (PNG.len() as u64 * 4).div_ceil(3),
        ..Default::default()
    }));
    let (send, wake) = mpsc::channel();
    images.set_waker(Arc::new(move || {
        let _ = send.send(());
    }));
    images.encoded = ["first", "second", "third"]
        .into_iter()
        .map(|key| (key.into(), Arc::from(PNG)))
        .collect();
    images.request(wanted(&["first", "second", "third"]));
    ready(&mut images, &wake).unwrap();
    assert_eq!(images.loaded.len(), 3);
    assert_eq!(images.loads, 3);
    assert_eq!(images.pool.usage().bytes, 3 * 160 * 90 * 4);
    images.close();
    assert_eq!(images.pool.usage().bytes, 0);
}

#[test]
fn settled_image_demand_skips_rescans_and_reopens_for_new_sources() {
    let (mut images, wake) = loader();
    images.request(wanted(&["assets/panels.png"]));
    ready(&mut images, &wake).unwrap();
    images.commit();
    let scans = images.poll_scans;
    for _ in 0..4096 {
        assert!(images.poll().unwrap());
    }
    assert_eq!(images.poll_scans, scans);
    println!(
        "settled_images polls=4096 demand_scans={}",
        images.poll_scans - scans
    );
    images.request(wanted(&["assets/sun.jpg"]));
    assert!(!images.poll().unwrap());
    assert!(images.poll_scans > scans);
    ready(&mut images, &wake).unwrap();
    images.commit();
    let scans = images.poll_scans;
    for _ in 0..4096 {
        assert!(images.poll().unwrap());
    }
    assert_eq!(images.poll_scans, scans);
}

#[test]
fn tiled_metadata_stays_ready_while_optional_pixels_wait_for_old_frame_leases() {
    const PANEL_BYTES: u64 = 160 * 90 * 4;
    let (mut images, wake) = configured(PANEL_BYTES, 1 << 20);
    images.request(wanted(&["assets/panels.png"]));
    assert!(!images.metadata_ready());
    ready(&mut images, &wake).unwrap();
    images.commit();
    let old_frame = images.loaded["assets/panels.png"].clone();
    let manifest =
        PreparedImage::new(1024, 512, 128, reel_core::prepared_image::TileEncoding::Png).unwrap();
    images
        .encoded
        .insert("background.reel-image".into(), Arc::from(manifest.encode()));
    images.request(wanted(&["background.reel-image"]));
    assert!(!images.metadata_ready());
    assert!(!images.poll().unwrap());
    // An encoded manifest completes synchronously in Assets::maintain; no
    // decoder thread wake is needed before taking its metadata next poll.
    assert!(images.poll().unwrap());
    assert!(images.metadata_ready());
    images.commit();
    assert!(images.loaded.is_empty());
    assert_eq!(images.pool.usage().bytes, PANEL_BYTES);
    let optional = TileDemand {
        owner: "background.reel-image".into(),
        required: false,
        dimensions: [96, 96],
        transform: Affine::IDENTITY,
        clip: Rect::new(0., 0., 96., 96.),
    };
    assert!(images.request_tiles(BTreeMap::from([("assets/sun.jpg".into(), optional)])));
    let scans = images.metadata_scans;
    for _ in 0..4096 {
        assert!(images.metadata_ready());
        assert!(images.poll().unwrap());
        assert!(!images.settled, "optional pixels still need a retry");
    }
    assert!(images.pending.is_empty());
    assert_eq!(
        images.metadata_scans, scans,
        "tile retries must not rescan wanted metadata"
    );
    assert!(!old_frame.is_retired());
    drop(old_frame);
    assert!(images.poll().unwrap());
    assert_eq!(
        images.pending.len(),
        1,
        "released resident capacity reopens optional IO"
    );
    while !images.loaded.contains_key("assets/sun.jpg") {
        wake.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(images.poll().unwrap());
    }
    assert!(images.settled);
    assert_eq!(images.metadata_scans, scans);
    assert_eq!(
        images.loads, 3,
        "whole image, manifest and optional tile load once"
    );
    assert!(images.request_tiles(BTreeMap::new()));
    assert!(images.metadata_ready());
    assert!(images.poll().unwrap());
    assert_eq!(images.metadata_scans, scans);
    images.close();
    assert!(images.metadata_ready(), "closed demand is empty");
    assert!(!images.poll().unwrap());
    assert_eq!(images.pool.usage().bytes, 0);
}

#[test]
fn metadata_readiness_reopens_after_failure_and_cancelled_demand() {
    let (mut images, wake) = loader();
    images.request(wanted(&["assets/missing.png"]));
    assert!(!images.metadata_ready());
    assert!(
        ready(&mut images, &wake)
            .unwrap_err()
            .contains("assets/missing.png")
    );
    assert!(!images.metadata_ready());
    images.request(wanted(&["assets/panels.png"]));
    ready(&mut images, &wake).unwrap();
    assert!(images.metadata_ready());
    let revision = images.content_revision;
    images.request(wanted(&["assets/panels.png", "assets/sun.jpg"]));
    assert!(!images.metadata_ready());
    assert!(!images.poll().unwrap());
    images.request(wanted(&["assets/panels.png"]));
    assert!(
        images.metadata_ready(),
        "withdrawing missing demand restores readiness"
    );
    assert!(images.poll().unwrap());
    assert_eq!(
        images.content_revision, revision,
        "cancelled source never became stage content"
    );
    images.request(BTreeMap::new());
    assert!(images.metadata_ready());
    assert!(images.poll().unwrap());
}

#[test]
fn pending_tile_failure_follows_current_viewport_requirement() {
    let (mut images, wake) = loader();
    let owner = "background.reel-image";
    let source = "assets/missing-tile.png";
    let manifest =
        PreparedImage::new(128, 128, 128, reel_core::prepared_image::TileEncoding::Png).unwrap();
    images
        .encoded
        .insert(owner.into(), Arc::from(manifest.encode()));
    images.request(wanted(&[owner]));
    assert!(!images.poll().unwrap());
    assert!(images.poll().unwrap());

    let tiles = |required| {
        BTreeMap::from([(
            source.into(),
            TileDemand {
                owner: owner.into(),
                required,
                dimensions: [128, 128],
                transform: Affine::IDENTITY,
                clip: Rect::new(0., 0., 128., 128.),
            },
        )])
    };
    assert!(images.request_tiles(tiles(true)));
    assert!(!images.poll().unwrap());
    let id = images.pending[0].id;
    assert!(images.request_tiles(tiles(false)));
    assert_eq!(images.pending[0].id, id);
    while !images.settled {
        assert!(
            images.poll().unwrap(),
            "optional tiles cannot block the frame"
        );
        if !images.settled {
            wake.recv_timeout(Duration::from_secs(10)).unwrap();
        }
    }
    assert!(images.skipped.contains(source));

    assert!(images.request_tiles(tiles(true)));
    let error = ready(&mut images, &wake).unwrap_err();
    assert!(error.contains("main:17:9"));
    assert!(error.contains(source));
}
