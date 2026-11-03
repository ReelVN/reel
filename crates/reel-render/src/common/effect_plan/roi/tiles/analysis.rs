//! Offline scalar inputs extracted from one retained frame and its native trace.
use super::*;
use std::io::Write;

fn numbers<T: std::str::FromStr, const N: usize>(value: &str) -> [T; N]
where
    T::Err: std::fmt::Debug,
{
    value
        .split(',')
        .map(|v| v.parse().unwrap())
        .collect::<Vec<_>>()
        .try_into()
        .ok()
        .unwrap()
}
fn rectangle(value: &str) -> Scissor {
    let [x, y, width, height] = numbers(value);
    Scissor {
        x,
        y,
        width,
        height,
    }
}

#[test]
#[ignore = "requires REEL_ROI_PLAN extracted from a retained frame and matching trace"]
fn actual_chain_tile_demand() {
    let input = std::path::PathBuf::from(std::env::var_os("REEL_ROI_PLAN").unwrap());
    let data = std::fs::read_to_string(&input).unwrap();
    let mut rows = data
        .lines()
        .map(|line| line.split('\t').collect::<Vec<_>>());
    let first = rows.next().unwrap();
    assert_eq!(first[0], "consumer");
    let extent = numbers::<u32, 2>(first[1]);
    let mut old = rectangle(first[2]);
    let mut cover = Cover::rectangle(old, extent).unwrap();
    let mut outer = Affine::IDENTITY;
    let output = input.with_extension("result.tsv");
    let mut report = std::io::BufWriter::new(std::fs::File::create(&output).unwrap());
    for fields in rows {
        if fields[0] == "view" {
            use reel_ui::{
                core::Widget,
                frame::{FrameGroup, GroupComposite, InkBounds},
                widgets::Label,
            };
            let parent = numbers::<u32, 2>(fields[1]);
            let scissor = rectangle(fields[2]);
            let [x0, y0, x1, y1] = numbers(fields[3]);
            let ink = InkBounds::Rect(Rect::new(x0, y0, x1, y1));
            let group = FrameGroup {
                owner: Label::new("").prepare().id(),
                effect_to_target: Affine::IDENTITY,
                effect: None,
                mask_effect: None,
                mask: None,
                image_mask: None,
                clip: None,
                filters: Vec::new(),
                invert: 0.,
                composite: GroupComposite::default(),
                ink_bounds: ink,
            };
            let view = crate::common::source_view::SourceView::group(
                &group,
                ink,
                Affine::IDENTITY,
                parent,
                scissor,
                false,
                4096,
            )
            .unwrap();
            assert_eq!(view.storage_size, extent);
            assert_eq!(view.source_rect, old);
            outer = view.frame_to_source(Affine::IDENTITY);
            writeln!(report, "source_view\t{view:?}").unwrap();
            continue;
        }
        assert_eq!(fields[0], "step");
        let [x0, y0, x1, y1] = numbers(fields[4]);
        let effect = FrameEffect {
            definition: [0; 32],
            shader: ResourceHandle {
                registry: 1,
                slot: 1,
                generation: 1,
            },
            values: Vec::new(),
            images: Vec::new(),
            bounds: Rect::new(x0, y0, x1, y1),
            coverage_invert: false,
            resolution_scale: 1.,
        };
        let world = Affine::new(numbers(fields[5]));
        let viewport = numbers::<u32, 2>(fields[6]);
        let source_logical = numbers::<u32, 2>(fields[7]);
        let source_extent = numbers::<u32, 2>(fields[8]);
        let source_bounds = rectangle(fields[9]);
        let footprint = EffectSourceFootprint {
            logical_halo: numbers(fields[10]),
        };
        let to_output = match fields[3] {
            "frame" => outer * world,
            "local" => crate::common::effect_plan::local_transform(&effect, world, viewport),
            _ => panic!("unknown coordinate map"),
        };
        let mut context = context(&effect, to_output, viewport, source_logical).unwrap();
        // Origin-zero physical windows preserve the original map by an exact
        // power-of-two rescale; direct storage ratios give the identical f32 rows.
        map_storage(&mut context, source_logical, source_extent);
        let predicted = super::super::super::required(&context, footprint, old, source_extent)
            .unwrap()
            .intersect(Scissor::full(source_logical));
        writeln!(
            report,
            "old_roi\tconsumer={}\tpredicted={predicted:?}\tactual={source_bounds:?}",
            fields[1]
        )
        .unwrap();
        report.flush().unwrap();
        assert_eq!(
            predicted, source_bounds,
            "must reproduce the actual old draw scissor before estimating a new plan"
        );
        let baseline = Cover::rectangle(source_bounds, source_extent).unwrap();
        let plan = backward(&context, footprint, &cover, source_extent, source_bounds).unwrap();
        writeln!(report, "plan\tproducer={}\tconsumer={}\told_tiles={}\tnew_tiles={}\tdraws={}\tqueries={}\tcontext_bits={:?}", fields[2], fields[1], baseline.tile_count(), plan.cover.tile_count(), plan.cover.draw_count(), plan.queries, context.map(|r| r.map(f32::to_bits))).unwrap();
        for rectangle in plan.cover.rectangles() {
            writeln!(report, "rectangle\tproducer={}\t{rectangle:?}", fields[2]).unwrap();
        }
        cover = plan.cover;
        old = source_bounds;
    }
    report.flush().unwrap();
    eprintln!("Actual tile demand: {}", output.display());
}
