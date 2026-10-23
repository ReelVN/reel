use super::*;
use reel_core::budget::{BudgetCategory, QueueLimits};

fn pool() -> BudgetPool {
    BudgetPool::new(
        BudgetCategory::Snapshot,
        QueueLimits {
            max_requests: 1024,
            max_bytes: 4 << 20,
        },
    )
}
fn command(op: FrameOp) -> FrameCommand {
    FrameCommand {
        origin: wire::origin(),
        op,
    }
}
fn saved(commands: Vec<FrameCommand>, masks: BTreeMap<u32, Arc<FrameMask>>) -> SavedRetainedScene {
    SavedRetainedScene(Arc::new(Archive {
        nodes: vec![SavedNode {
            drawing: Drawing::Stored(StoredDrawing {
                commands,
                masks,
                capture_exclusions: Vec::new(),
            }),
            resources: Vec::new(),
            transitions: Vec::new(),
        }],
        resources: Vec::new(),
        to_parent: Affine::IDENTITY,
        _ticket: None,
    }))
}
fn restore(value: &SavedRetainedScene) -> Result<RetainedScene, RetainError> {
    let mut work = value.restore_begin(BuildLimits { max_bytes: 2 << 20 }, &pool())?;
    let mut budget = StepBudget {
        max_commands: 2,
        max_units: 128,
        max_bytes: 1024,
    };
    for _ in 0..10000 {
        let progress = work.step(budget)?;
        if progress.build.complete {
            return work.finish();
        }
        budget.max_bytes = budget.max_bytes.max(progress.build.minimum_bytes);
        budget.max_units = budget.max_units.max(progress.build.minimum_units);
    }
    panic!("restore did not make bounded progress");
}
fn group() -> FrameGroup {
    FrameGroup {
        image_mask: None,
        clip: None,
        mask: None,
        filters: Vec::new(),
        composite: GroupComposite::default(),
        invert: 0.,
        owner: wire::origin().at.owner,
        effect_to_target: Affine::IDENTITY,
        ink_bounds: InkBounds::Empty,
    }
}

#[test]
fn saved_drawing_rejects_broken_scopes_mask_cycles_and_nonfinite_geometry() {
    for commands in [
        vec![command(FrameOp::PopClip)],
        vec![command(FrameOp::PushGroup(group()))],
        vec![
            command(FrameOp::PushGroup(group())),
            command(FrameOp::PopClip),
        ],
        vec![command(FrameOp::PopContext)],
        vec![
            command(FrameOp::PushContext(FrameContext {
                kind: FrameContextKind::Label,
                value: FrameContextValue::Str(OwnedText(vec![0xff])),
                source: None,
            })),
            command(FrameOp::PopContext),
        ],
    ] {
        assert_eq!(
            restore(&saved(commands, BTreeMap::new())).unwrap_err().kind,
            BuildErrorKind::InvalidRecord
        );
    }
    let mut invalid_group = group();
    invalid_group.mask = Some(MaskUse {
        mask: 1,
        transform: Affine::IDENTITY,
    });
    let commands = vec![
        command(FrameOp::PushGroup(invalid_group)),
        command(FrameOp::PopGroup),
    ];
    let masks = BTreeMap::from([(
        1,
        Arc::new(FrameMask {
            mode: MaskMode::Alpha,
            commands: commands.clone(),
        }),
    )]);
    assert_eq!(
        restore(&saved(commands, masks)).unwrap_err().kind,
        BuildErrorKind::InvalidRecord
    );

    let mut path = BezPath::new();
    path.move_to((f64::NAN, 0.));
    let mut invalid_group = group();
    invalid_group.clip = Some(Clip::Fill {
        transform: Affine::IDENTITY,
        shape: Geometry::Path(path),
        fill_rule: Default::default(),
    });
    assert_eq!(
        restore(&saved(
            vec![
                command(FrameOp::PushGroup(invalid_group)),
                command(FrameOp::PopGroup)
            ],
            BTreeMap::new()
        ))
        .unwrap_err()
        .kind,
        BuildErrorKind::InvalidRecord
    );
}

#[test]
fn saved_drawing_charges_large_clip_payload_before_copying_and_empty_masks_per_step() {
    let mut path = BezPath::new();
    path.move_to((0., 0.));
    for i in 0..4096 {
        path.line_to((i as f64, 1.));
    }
    let mut value = group();
    value.clip = Some(Clip::Fill {
        transform: Affine::IDENTITY,
        shape: Geometry::Path(path),
        fill_rule: Default::default(),
    });
    let source = saved(
        vec![
            command(FrameOp::PushGroup(value)),
            command(FrameOp::PopGroup),
        ],
        BTreeMap::new(),
    );
    let storage = pool();
    let mut work = source
        .restore_begin(BuildLimits { max_bytes: 2 << 20 }, &storage)
        .unwrap();
    let progress = work
        .step(StepBudget {
            max_commands: 8,
            max_units: 128,
            max_bytes: 4096,
        })
        .unwrap();
    assert!(progress.build.minimum_bytes > 4096 * size_of::<PathEl>() as u64);
    assert!(progress.build.minimum_units > 128);
    assert!(work.building.as_ref().unwrap().commands.is_empty());
    drop(work);
    assert_eq!(storage.usage().bytes, 0);
    assert_eq!(restore(&source).unwrap().drawing().commands().len(), 2);

    let masks = (0..100)
        .map(|id| {
            (
                id,
                Arc::new(FrameMask {
                    mode: MaskMode::Alpha,
                    commands: Vec::new(),
                }),
            )
        })
        .collect();
    let source = saved(Vec::new(), masks);
    let mut work = source
        .restore_begin(BuildLimits { max_bytes: 2 << 20 }, &storage)
        .unwrap();
    let progress = work
        .step(StepBudget {
            max_commands: 2,
            max_units: 1000,
            max_bytes: 4096,
        })
        .unwrap();
    assert!(!progress.build.complete);
    assert_eq!(work.building.as_ref().unwrap().masks.len(), 1);
    assert_eq!(progress.build.commands, 2);
}

#[test]
fn saved_drawing_rejects_a_transition_cut_through_an_open_group() {
    let mut source = saved(
        vec![
            command(FrameOp::PushGroup(group())),
            command(FrameOp::PopGroup),
        ],
        BTreeMap::new(),
    );
    let archive = Arc::get_mut(&mut source.0).unwrap();
    archive.nodes.insert(
        0,
        SavedNode {
            drawing: Drawing::Stored(StoredDrawing {
                capture_exclusions: Vec::new(),
                commands: Vec::new(),
                masks: BTreeMap::new(),
            }),
            resources: Vec::new(),
            transitions: Vec::new(),
        },
    );
    archive.nodes[1].transitions.push(SavedTransition {
        capture_included: true,
        commands: 1..2,
        previous: 0,
        to_parent: Affine::IDENTITY,
        progress: 0.5,
        mask: None,
        parent_to_window: Affine::IDENTITY,
        mask_to_window: Affine::IDENTITY,
    });
    assert_eq!(
        restore(&source).unwrap_err().kind,
        BuildErrorKind::InvalidRecord
    );
    Arc::get_mut(&mut source.0).unwrap().nodes[1].transitions[0].commands = 0..2;
    assert_eq!(restore(&source).unwrap().drawing().transitions().len(), 1);
}

#[test]
fn snapshot_workspace_admits_vec_growth_before_allocation_and_charges_overlap() {
    let pool = BudgetPool::new(
        BudgetCategory::Snapshot,
        QueueLimits {
            max_requests: 4,
            max_bytes: 40,
        },
    );
    let mut ticket = pool.reserve(0).unwrap();
    let mut values = Vec::<u64>::new();
    snapshot_reserve(&mut values, 2, &mut ticket).unwrap();
    values.extend([7, 9]);
    let capacity = values.capacity();
    assert_eq!(
        snapshot_reserve(&mut values, 1, &mut ticket)
            .unwrap_err()
            .kind,
        BuildErrorKind::Bytes
    );
    assert_eq!(
        values.capacity(),
        capacity,
        "failed admission allocated a replacement"
    );
    assert_eq!(values, [7, 9]);
    assert_eq!(pool.usage().bytes, 16);
    drop((values, ticket));
    assert_eq!(pool.usage().bytes, 0);
}

fn retained(
    pool: &BudgetPool,
    leases: Vec<ResourceLease>,
    transitions: Vec<SceneTransition>,
) -> RetainedScene {
    let bytes =
        (size_of::<FrameDrawing>() + transitions.capacity() * size_of::<SceneTransition>()) as u64;
    RetainedScene {
        drawing: Arc::new(FrameDrawing {
            capture_exclusions: Vec::new(),
            commands: Vec::new(),
            masks: BTreeMap::new(),
            resources: ResourceSnapshot::capture(leases.into_iter().map(Ok), pool).unwrap(),
            transitions,
            accounted_bytes: bytes,
            _ticket: pool.reserve(bytes).unwrap(),
        }),
        to_parent: Affine::IDENTITY,
        pool: pool.clone(),
    }
}

#[test]
fn snapshot_metadata_refunds_workspace_and_counts_shared_resident_pixels_once() {
    use crate::peniko::{ImageAlphaType, ImageFormat};
    use reel_core::{
        budget::OwnedBytes,
        identity::{AssetKey, ContentVersion},
    };
    let pool = pool();
    let producers = BudgetPool::new(
        BudgetCategory::CpuOutput,
        QueueLimits {
            max_requests: 8,
            max_bytes: 8 << 20,
        },
    );
    let size = 4 << 20;
    let mut registry = ResourceRegistry::new(1, 64 << 10).unwrap();
    let handle = registry
        .register_image(
            ResourceVersion {
                key: AssetKey("ordinary.png".into()),
                version: ContentVersion([7; 32]),
            },
            OwnedBytes::new(vec![128; size], producers.reserve(size as u64).unwrap()).unwrap(),
            [1024, 1024],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    let lease = registry.acquire(handle).unwrap();
    let child = retained(&pool, vec![lease.clone()], Vec::new());
    let scene = retained(
        &pool,
        vec![lease.clone()],
        (0..64)
            .map(|_| SceneTransition {
                capture_included: true,
                commands: 0..0,
                previous: child.clone(),
                progress: 0.5,
                mask: Some(FrameImageMask {
                    program: crate::stage::MaskProgram::image("ordinary.png".into()),
                    images: vec![handle],
                    bounds: Rect::new(0., 0., 1., 1.),
                    parameters: [0.5, 0.06, 0., 0., 0.],
                }),
                parent_to_window: Affine::IDENTITY,
                mask_to_window: Affine::IDENTITY,
            })
            .collect(),
    );
    let baseline = pool.usage().bytes;
    let saved = scene.snapshot(|_| None).unwrap();
    assert_eq!(
        saved.0.nodes.len(),
        2,
        "shared drawing was copied per transition"
    );
    assert_eq!(
        saved.0.resources.len(),
        1,
        "shared pixels were copied per drawing"
    );
    let SavedResourceKind::Inline(InlineResource::Image { image, .. }) = &saved.0.resources[0].kind
    else {
        panic!()
    };
    let ResourceData::Image {
        image: original, ..
    } = lease.data()
    else {
        panic!()
    };
    assert_eq!(
        image.data.as_ref().as_ptr(),
        original.data.as_ref().as_ptr()
    );
    let metadata = size_of::<Archive>()
        + 2 * size_of::<usize>()
        + saved.0.nodes.capacity() * size_of::<SavedNode>()
        + saved.0.resources.capacity() * size_of::<SavedResource>()
        + saved
            .0
            .nodes
            .iter()
            .map(|node| {
                node.resources.capacity() * size_of::<u32>()
                    + node.transitions.capacity() * size_of::<SavedTransition>()
                    + node
                        .transitions
                        .iter()
                        .map(|t| {
                            t.mask
                                .as_ref()
                                .map_or(0, |m| m.images.capacity() * size_of::<ResourceHandle>())
                        })
                        .sum::<usize>()
            })
            .sum::<usize>();
    assert_eq!(saved.0._ticket.as_ref().unwrap().bytes(), metadata as u64);
    assert_eq!(
        pool.usage().bytes - baseline,
        metadata as u64,
        "temporary ID maps or traversal remain charged"
    );
    assert!(
        pool.usage().peak_bytes > pool.usage().bytes,
        "workspace never entered the budget"
    );
    let retained_bytes: u64 = saved.retained_size_steps().sum();
    assert!(retained_bytes >= size as u64);
    assert!(
        retained_bytes < size as u64 + (128 << 10),
        "shared pixels counted repeatedly: {retained_bytes}"
    );
    drop(saved);
    assert_eq!(pool.usage().bytes, baseline);
    let saved = scene
        .snapshot(|lease| {
            Some(RetainedAsset {
                version: lease.version().clone(),
                kind: RetainedAssetKind::Image,
            })
        })
        .unwrap();
    assert_eq!(saved.assets().count(), 1);
    assert!(
        saved.retained_size_steps().sum::<u64>() >= size as u64,
        "live ordinary asset pixels escaped save-queue admission"
    );
    drop((saved, scene, child, lease, registry));
    assert_eq!(pool.usage().bytes, 0);
    assert_eq!(producers.usage().bytes, 0);
}

#[test]
#[expect(
    clippy::single_range_in_vec_init,
    reason = "fixtures contain command intervals"
)]
fn capture_exclusions_restore_and_reject_unbalanced_intervals() {
    let commands = vec![
        command(FrameOp::PushGroup(group())),
        command(FrameOp::PopGroup),
    ];
    let mut value = saved(commands, BTreeMap::new());
    let Drawing::Stored(drawing) = &mut Arc::get_mut(&mut value.0).unwrap().nodes[0].drawing else {
        unreachable!()
    };
    drawing.capture_exclusions = vec![0..2];
    let result = restore(&value).unwrap();
    assert_eq!(result.drawing().capture_exclusions(), &[0..2]);
    assert_eq!(result.drawing().capture_exclusion_end(0), Some(2));
    assert_eq!(result.drawing().capture_exclusion_end(2), None);
    for invalid in [
        vec![0..1],
        vec![1..2],
        vec![0..3],
        vec![1..1],
        vec![0..2, 1..2],
    ] {
        let Drawing::Stored(drawing) = &mut Arc::get_mut(&mut value.0).unwrap().nodes[0].drawing
        else {
            unreachable!()
        };
        drawing.capture_exclusions = invalid;
        assert_eq!(
            restore(&value).unwrap_err().kind,
            BuildErrorKind::InvalidRecord
        );
    }
}

#[test]
fn image_color_appearance_restores_with_one_shared_resource() {
    use crate::peniko::{ImageAlphaType, ImageFormat};
    use reel_core::{
        budget::OwnedBytes,
        identity::{AssetKey, ContentVersion},
    };
    let pool = pool();
    let mut registry = ResourceRegistry::new(1, 64 << 10).unwrap();
    let handle = registry
        .register_image(
            ResourceVersion {
                key: AssetKey("shared.png".into()),
                version: ContentVersion([7; 32]),
            },
            OwnedBytes::new(vec![255; 4], pool.reserve(4).unwrap()).unwrap(),
            [1, 1],
            ImageFormat::Rgba8,
            ImageAlphaType::Alpha,
            ColorEncoding::Srgb,
        )
        .unwrap();
    let adjustment = ImageColorAdjustment {
        saturation: 0.25,
        gain: [1.5, 0.5, 1.],
        offset: [0., 0.1, -0.1],
    };
    let appearance = ImageAppearance::new(
        Some(ImageColorGradient {
            colors: [[0.5; 4]; 4],
            transform: Affine::scale(40.),
        }),
        adjustment,
        Default::default(),
    );
    let commands = [None, appearance]
        .into_iter()
        .map(|appearance| {
            command(FrameOp::Draw(FrameDraw::Fill {
                transform: Affine::IDENTITY,
                fill_rule: Default::default(),
                brush: Brush::Image(ImageBrush {
                    image: FrameImage {
                        source_rect: Some(Rect::new(16., 8., 32., 24.)),
                        resource: handle,
                        color: [1.; 4],
                        appearance,
                    },
                    sampler: Default::default(),
                }),
                brush_transform: None,
                shape: Geometry::Rect(Rect::new(0., 0., 40., 40.)),
                composite: Default::default(),
            }))
        })
        .collect::<Vec<_>>();
    let bytes =
        (size_of::<FrameDrawing>() + commands.capacity() * size_of::<FrameCommand>()) as u64;
    let scene = RetainedScene {
        drawing: Arc::new(FrameDrawing {
            commands,
            capture_exclusions: Vec::new(),
            masks: BTreeMap::new(),
            transitions: Vec::new(),
            resources: ResourceSnapshot::capture(
                [Ok(registry.acquire(handle).unwrap())].into_iter(),
                &pool,
            )
            .unwrap(),
            accounted_bytes: bytes,
            _ticket: pool.reserve(bytes).unwrap(),
        }),
        to_parent: Affine::IDENTITY,
        pool: pool.clone(),
    };
    let saved = scene.snapshot(|_| None).unwrap();
    assert_eq!(saved.0.resources.len(), 1);
    let restored = restore(&saved).unwrap();
    assert_eq!(restored.drawing().resources().leases().len(), 1);
    for (command, expected) in restored.drawing().commands().iter().zip([None, appearance]) {
        let FrameOp::Draw(FrameDraw::Fill {
            brush: Brush::Image(image),
            ..
        }) = &command.op
        else {
            panic!()
        };
        assert_eq!(image.image.appearance, expected);
        assert_eq!(image.image.source_rect, Some(Rect::new(16., 8., 32., 24.)));
    }
}
