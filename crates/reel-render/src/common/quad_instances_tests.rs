use super::*;
use crate::common::{GpuLimits, renderer::Budgets};
use reel_ui::{
    core::Widget,
    frame::{FrameImage, SpriteInstance},
    imaging::Composite,
    peniko::{ImageBrush, ImageSampler},
    resources::ResourceHandle,
};
fn sprite_batch() -> FrameSprites {
    FrameSprites {
        transform: Affine::new([1.2, 0.1, 0.2, 0.8, 30., 40.]),
        bounds: Rect::new(2., 1., 14., 7.),
        brush: ImageBrush {
            image: FrameImage {
                resource: ResourceHandle {
                    registry: 1,
                    slot: 0,
                    generation: 1,
                },
                color: [0.4, 0.3, 0.2, 0.5],
                appearance: None,
                source_rect: Some(Rect::new(2., 1., 14., 7.)),
            },
            sampler: ImageSampler {
                alpha: 0.75,
                ..Default::default()
            },
        },
        composite: Composite {
            alpha: 0.5,
            ..Default::default()
        },
        instances: [
            SpriteInstance {
                position: [10., 20.],
                size: 24.,
                rotation: 0.4,
                alpha: 0.2,
            },
            SpriteInstance {
                position: [22., 21.],
                size: 32.,
                rotation: -0.7,
                alpha: 0.8,
            },
            SpriteInstance {
                position: [10000.; 2],
                size: 16.,
                rotation: 0.,
                alpha: 1.,
            },
            SpriteInstance {
                position: [0.; 2],
                size: 16.,
                rotation: 0.,
                alpha: 0.,
            },
        ]
        .into_iter()
        .map(|s| s.compiled(Rect::new(2., 1., 14., 7.)))
        .collect(),
    }
}
fn at() -> RecordLocation {
    RecordLocation {
        fragment: 0,
        command: 0,
        mask_depth: 0,
        owner: reel_ui::widgets::Label::new("").prepare().id(),
    }
}
fn layout() -> TextureLayout {
    TextureLayout {
        logical_size: [16, 8],
        content_size: [16, 8],
        storage_size: [32, 16],
    }
}
#[test]
fn instances_and_expanded_fallback_preserve_every_old_vertex_bit() {
    let limits = GpuLimits::default();
    let shared = Budgets::new(limits);
    let local = Budgets::new(limits);
    let allocation = || Allocation::new(&shared, &local, limits.transient_bytes, 1 << 24);
    let first = sprite_batch();
    let mut second = first.clone();
    second.transform = Affine::translate((7., 9.)) * second.transform;
    let outer = Affine::new([-1., 0., 0., 1., 150., 7.]);
    let uv = Affine::scale_non_uniform(1. / 32., 1. / 16.);
    let input = |batch| {
        Input::sprites(batch, outer, layout(), uv, None, [200, 200], at())
            .unwrap()
            .unwrap()
    };
    let mut actual = Batch::new(&input(&first), allocation()).unwrap();
    assert_eq!(size_of::<Instance>(), 48);
    assert_eq!(size_of::<Template>(), 112);
    assert_eq!(actual.instances.len(), 2);
    assert!(!actual.worthwhile());
    actual
        .merge(Batch::new(&input(&second), allocation()).unwrap())
        .unwrap();
    assert_eq!(actual.instances.len(), 4);
    assert!(actual.worthwhile());
    assert!(actual.instanced(false, false));
    assert!(!actual.instanced(true, false));
    assert!(!actual.instanced(false, true));
    assert_eq!(actual.bytes(), 112 + 12 + 4 * 48);
    let mut expected = Mesh::new(allocation());
    for batch in [&first, &second] {
        expected
            .sprites(batch, outer, layout(), uv, None, [200, 200], at())
            .unwrap();
    }
    for format in [
        None,
        Some(reel_core::texture::TextureFormat::Rgba8),
        Some(reel_core::texture::TextureFormat::Bc1Rgb),
        Some(reel_core::texture::TextureFormat::Bc3Rgba),
    ] {
        for repeat in [[false; 2], [true, false]] {
            let direct = expected.direct_paint(mesh::PaintSource::Texture, repeat, None, format);
            assert_eq!(
                actual.direct_paint(mesh::PaintSource::Texture, repeat, None, format),
                direct
            );
            assert_eq!(actual.opaque_paint(direct), expected.opaque_paint(direct));
            assert_eq!(
                actual.bounded_color.paint(direct, format),
                expected.bounded_color.paint(direct, format)
            );
        }
    }
    for scissor in [
        super::super::scissor::Scissor::full([200, 200]),
        super::super::scissor::Scissor {
            x: 50,
            y: 40,
            width: 120,
            height: 110,
        },
    ] {
        assert_eq!(
            actual.scissor(scissor, [200, 200]),
            scissor.for_vertices(&expected.vertices, [200, 200])
        );
    }
    let actual = actual.into_mesh().unwrap();
    assert_eq!(
        bytemuck::cast_slice::<_, u8>(&actual.vertices),
        bytemuck::cast_slice::<_, u8>(&expected.vertices)
    );
    assert_eq!(actual.indices, expected.indices);
    drop((actual, expected));
    assert_eq!(shared.transient.usage().bytes, 0);
    assert_eq!(local.transient.usage().bytes, 0);
}

#[test]
fn compatible_instance_runs_preserve_the_original_mesh_upload_boundary() {
    let limits = GpuLimits::default();
    let shared = Budgets::new(limits);
    let local = Budgets::new(limits);
    let make = |count| {
        let mut batch = sprite_batch();
        let sprite = batch.instances[0];
        batch.instances = vec![sprite; count];
        let input = Input::sprites(
            &batch,
            Affine::IDENTITY,
            layout(),
            Affine::IDENTITY,
            None,
            [200, 200],
            at(),
        )
        .unwrap()
        .unwrap();
        Batch::new(
            &input,
            Allocation::new(&shared, &local, limits.transient_bytes, 1 << 24),
        )
        .unwrap()
    };
    let first = make(1000);
    assert!(first.can_merge(&make(213)));
    assert!(!first.can_merge(&make(214)));
}
#[test]
fn sparse_support_and_special_composites_keep_the_existing_expansion() {
    let batch = sprite_batch();
    let mut bytes = vec![0; 16 * 8 * 4];
    bytes[3] = 255;
    let coverage = ImageCoverage::rgba8([16, 8], &bytes).unwrap();
    assert!(
        Input::sprites(
            &batch,
            Affine::IDENTITY,
            layout(),
            Affine::IDENTITY,
            Some(&coverage),
            [200, 200],
            at()
        )
        .unwrap()
        .is_none()
    );
    let mut special = batch;
    special.composite.blend = reel_ui::peniko::Mix::Multiply.into();
    assert!(
        Input::sprites(
            &special,
            Affine::IDENTITY,
            layout(),
            Affine::IDENTITY,
            None,
            [200, 200],
            at()
        )
        .unwrap()
        .is_none()
    );
}
#[test]
fn instance_admission_keeps_both_budget_ledgers_and_refuses_before_allocation() {
    let limits = GpuLimits {
        transient_bytes: 0,
        ..Default::default()
    };
    let shared = Budgets::new(limits);
    let local = Budgets::new(limits);
    let batch = sprite_batch();
    let input = Input::sprites(
        &batch,
        Affine::IDENTITY,
        layout(),
        Affine::IDENTITY,
        None,
        [200, 200],
        at(),
    )
    .unwrap()
    .unwrap();
    assert!(Batch::new(&input, Allocation::new(&shared, &local, 0, 1 << 20)).is_err());
    assert_eq!(shared.transient.usage().bytes, 0);
    assert_eq!(local.transient.usage().bytes, 0);
}

#[test]
fn a_one_visible_quad_fallback_keeps_the_original_exact_byte_quota() {
    let limits = GpuLimits {
        transient_bytes: 216,
        ..Default::default()
    };
    let shared = Budgets::new(limits);
    let local = Budgets::new(limits);
    let mut batch = sprite_batch();
    batch.instances.truncate(1);
    let input = Input::sprites(
        &batch,
        Affine::IDENTITY,
        layout(),
        Affine::IDENTITY,
        None,
        [200, 200],
        at(),
    )
    .unwrap()
    .unwrap();
    let packed = Batch::new(&input, Allocation::new(&shared, &local, 216, 1 << 20)).unwrap();
    let mesh = packed.into_mesh().unwrap();
    assert_eq!(mesh.indices.len(), 6);
    assert_eq!(shared.transient.usage().bytes, 216);
    drop(mesh);
    assert_eq!(shared.transient.usage().bytes, 0);
}
