use super::*;
use reel_ui::stage::MaskProgram;

#[test]
fn hidden_preload_is_inherited_without_becoming_visible_demand() {
    let (mut stage, root, _) = fixture(3);
    let mut props = stage.node(root).unwrap().props.clone();
    props.visible = false;
    props.preload = true;
    stage.update(root, NodeKind::Stack, props.clone()).unwrap();
    let view = StageView::capture(&stage);
    let requests = view.image_requests(EffectShaderFormat::Wgsl);
    assert_eq!(requests.required().count(), 0);
    assert_eq!(
        requests.preloaded().collect::<Vec<_>>(),
        ["picture-0.png", "picture-1.png", "picture-2.png"]
    );
    props.preload = false;
    stage.update(root, NodeKind::Stack, props).unwrap();
    let changed = StageView::update(&stage, Some(&view));
    assert!(!same_inputs(view.root.as_ref(), changed.root.as_ref()));
    let requests = changed.image_requests(EffectShaderFormat::Wgsl);
    assert_eq!(requests.preloaded().count(), 0);
    assert_eq!(requests.required().count(), 0);
}

fn image(source: &str) -> NodeKind {
    NodeKind::Image {
        source: source.into(),
        origin: Some(format!("{source}:1")),
        sampling: Default::default(),
        source_rect: None,
        fit: Default::default(),
        slice: None,
        slice_scale: 1.,
        slice_center: true,
        repeat: [false; 2],
        tile_scale: 1.,
        corner_colors: None,
        alpha_layout: Default::default(),
        adjustment: Default::default(),
    }
}

fn fixture(count: usize) -> (Stage, NodeId, Vec<NodeId>) {
    let mut stage = Stage::default();
    let children = (0..count)
        .map(|i| {
            stage
                .create(
                    image(&format!("picture-{i}.png")),
                    NodeProps {
                        width: Some(64.),
                        height: Some(64.),
                        ..Default::default()
                    },
                    vec![],
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    let root = stage
        .create(NodeKind::Stack, NodeProps::default(), children.clone())
        .unwrap();
    stage.replace(root).unwrap();
    (stage, root, children)
}

#[test]
fn viewport_and_animation_share_identities_but_visibility_rebuilds() {
    let (mut stage, root, nodes) = fixture(3);
    let mut props = stage.node(nodes[0]).unwrap().props.clone();
    props.mask = Some(MaskProgram::image("mask.png".into()));
    stage
        .update(nodes[0], image("picture-0.png"), props)
        .unwrap();
    let first = StageView::capture(&stage);
    let layout = first.layout_image_requests(EffectShaderFormat::Wgsl);
    assert_eq!(layout.required().collect::<Vec<_>>(), [("mask.png", None)]);
    assert!(layout.index.has_fixed_images);
    let outside = vec![nodes[1]];
    let visible = first.viewport_image_requests(outside, vec![nodes[1]], EffectShaderFormat::Wgsl);
    assert!(Arc::ptr_eq(&layout.index, &visible.index));
    assert_eq!(
        visible.required().map(|(s, _)| s).collect::<Vec<_>>(),
        ["mask.png", "picture-0.png", "picture-2.png"]
    );
    assert!(visible.index.mounted().any(|s| s == "picture-1.png"));
    assert_eq!(visible.preloaded().collect::<Vec<_>>(), ["picture-1.png"]);
    let mut props = stage.node(root).unwrap().props.clone();
    props.x = 60.;
    props.opacity = 0.4;
    stage.update(root, NodeKind::Stack, props.clone()).unwrap();
    let moved = StageView::update(&stage, Some(&first));
    assert!(Arc::ptr_eq(
        &layout.index,
        &moved.resource_index(EffectShaderFormat::Wgsl)
    ));
    props.visible = false;
    stage.update(root, NodeKind::Stack, props).unwrap();
    let hidden = StageView::update(&stage, Some(&moved));
    let hidden_requests = hidden.image_requests(EffectShaderFormat::Wgsl);
    assert!(!Arc::ptr_eq(&layout.index, &hidden_requests.index));
    assert!(hidden_requests.required().next().is_none());
    assert_eq!(hidden_requests.index.mounted().count(), 4);
    assert_eq!(
        visible.required().count(),
        3,
        "accepted view remains independent"
    );
}

#[test]
fn shared_source_uses_visible_origin_and_fixed_size_requirement() {
    let (mut stage, _, nodes) = fixture(2);
    let props = stage.node(nodes[0]).unwrap().props.clone();
    stage.update(nodes[0], image("shared.png"), props).unwrap();
    let mut props = stage.node(nodes[1]).unwrap().props.clone();
    props.width = None;
    let mut kind = image("shared.png");
    if let NodeKind::Image { origin, .. } = &mut kind {
        *origin = Some("auto-size:7".into());
    }
    stage.update(nodes[1], kind, props).unwrap();
    let view = StageView::capture(&stage);
    let layout = view.layout_image_requests(EffectShaderFormat::Wgsl);
    assert_eq!(
        layout.required().collect::<Vec<_>>(),
        [("shared.png", Some("auto-size:7"))]
    );
    let request =
        view.viewport_image_requests(vec![nodes[1]], Vec::new(), EffectShaderFormat::Wgsl);
    assert_eq!(
        request.required().collect::<Vec<_>>(),
        [("shared.png", Some("shared.png:1"))]
    );
    assert_eq!(request.index.mounted().count(), 1);
}

#[test]
#[ignore = "CPU allocation measurement; run with --release --ignored --exact --nocapture"]
fn viewport_resource_collection_cost() {
    let (stage, _, _) = fixture(512);
    let view = StageView::capture(&stage);
    let index = view.resource_index(EffectShaderFormat::Wgsl);
    let retained = std::mem::size_of::<ResourceIndex>()
        + index.entries.capacity() * std::mem::size_of::<(String, Entry)>()
        + index
            .entries
            .iter()
            .map(|(source, e)| {
                source.capacity()
                    + e.uses.capacity() * std::mem::size_of::<Usage>()
                    + e.uses
                        .iter()
                        .filter_map(|u| u.origin.as_ref())
                        .map(String::capacity)
                        .sum::<usize>()
            })
            .sum::<usize>();
    let mut counts = [0; 2];
    for indexed in [false, true] {
        let start = std::time::Instant::now();
        let allocations = allocation_counter::measure(|| {
            for _ in 0..2000 {
                if indexed {
                    let requests = view.viewport_image_requests(
                        Vec::new(),
                        Vec::new(),
                        EffectShaderFormat::Wgsl,
                    );
                    counts[1] += std::hint::black_box(
                        requests
                            .required()
                            .map(|(source, origin)| source.len() + origin.map_or(0, str::len))
                            .sum::<usize>(),
                    );
                } else {
                    let mut required = BTreeMap::new();
                    let mut mounted = std::collections::BTreeSet::new();
                    let mut pending = vec![view.root.as_ref().unwrap()];
                    while let Some(node) = pending.pop() {
                        pending.extend(node.children.iter());
                        if let Some((source, origin)) = image_source(&node.node.kind) {
                            mounted.insert(source.to_owned());
                            required
                                .entry(source.to_owned())
                                .or_insert_with(|| origin.clone());
                        }
                    }
                    counts[0] += std::hint::black_box(
                        required
                            .iter()
                            .map(|(source, origin)| {
                                source.len() + origin.as_deref().map_or(0, str::len)
                            })
                            .sum::<usize>(),
                    );
                    std::hint::black_box(mounted);
                }
            }
        });
        eprintln!(
            "resource index={indexed}, 512 images x 2000 queries: {:?}, allocations={}, bytes={}, retained={retained}",
            start.elapsed(),
            allocations.count_total,
            allocations.bytes_total
        );
        if indexed {
            assert_eq!(allocations.count_total, 0);
        }
    }
    assert_eq!(counts[0], counts[1]);
    assert!(counts[0] > 1_024_000);
}
