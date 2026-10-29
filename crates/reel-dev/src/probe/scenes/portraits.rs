//! The authored probe's portrait pixels and layout through the render-only path.
use super::{Result, SIZE, error, r01_fixture};
use reel_core::{budget::BudgetPool, identity::FrameId};
use reel_player::assets::{AssetKind, Assets};
use reel_ui::{
    core::WindowEvent,
    dpi::PhysicalSize,
    frame::{FrameBuild, FrameInfo, RenderFrame},
    imaging::{
        Composite,
        record::{Draw, Geometry, Scene},
    },
    kurbo::{Affine, Rect},
    peniko::{Brush, Color},
    resources::{ResourceLease, ResourceSnapshot},
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

pub(super) struct Portraits {
    assets: Assets,
    image: ResourceLease,
}

impl Portraits {
    pub(super) fn load() -> Result<Self> {
        let assets = Assets::packaged_for_target(
            Default::default(),
            [PathBuf::from("app0:/authored/resources.rpk")],
            None,
            "vita".into(),
        )
        .map_err(error)?;
        let request = assets.request("baseline/left.png", AssetKind::Image);
        let deadline = Instant::now() + Duration::from_secs(30);
        let image = loop {
            match assets.poll(request) {
                Ok(Some(image)) => break image,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                result => {
                    assets.close();
                    return Err(format!("portrait fixture load: {result:?}").into());
                }
            }
        };
        eprintln!(
            "[reel-gpu-probe] portrait texture={:?}",
            image.texture_descriptor()
        );
        Ok(Self { assets, image })
    }

    pub(super) fn frame(&self, index: usize, pool: &BudgetPool) -> Result<RenderFrame> {
        let mut scene = Scene::new();
        scene.draw(Draw::Fill {
            transform: Affine::IDENTITY,
            fill_rule: Default::default(),
            brush: Brush::Solid(Color::from_rgb8(20, 34, 55)),
            brush_transform: None,
            shape: Geometry::Rect(Rect::new(0., 0., 960., 544.)),
            composite: Composite::default(),
        });
        let mut widget = r01_fixture::RecordWidget::new(scene);
        widget.clip = false;
        let phase = (index % 120) as f64 / 60.;
        let offset = 16. * if phase <= 1. { phase } else { 2. - phase };
        for i in 0..24 {
            widget.resource_draws.push((
                self.image.clone(),
                Rect::new(0., 0., 384., 512.),
                Affine::translate((
                    20. + (i % 8) as f64 * 112. + offset,
                    20. + (i / 8) as f64 * 160.,
                )) * Affine::scale_non_uniform(190. / 384., 254. / 512.),
                Default::default(),
            ));
        }
        let mut bridge = r01_fixture::widget_bridge(widget, Affine::IDENTITY);
        bridge.handle_window_event(WindowEvent::Resize(PhysicalSize::new(SIZE[0], SIZE[1])));
        let source = bridge.record(FrameId(index as u64 + 1)).map_err(error)?;
        let snapshot =
            ResourceSnapshot::capture([Ok(self.image.clone())].into_iter(), pool).map_err(error)?;
        let mut build = FrameBuild::new(
            source,
            snapshot,
            FrameInfo {
                logical_size: SIZE.map(f64::from),
                physical_size: SIZE,
            },
            r01_fixture::limits(),
            pool,
        )
        .map_err(error)?;
        let mut budget = r01_fixture::step_budget();
        loop {
            let progress = build.step(budget).map_err(error)?;
            if progress.complete {
                break;
            }
            budget.max_units = budget.max_units.max(progress.minimum_units);
            budget.max_bytes = budget.max_bytes.max(progress.minimum_bytes);
        }
        build.finish().map_err(error)
    }
}

impl Drop for Portraits {
    fn drop(&mut self) {
        self.assets.close();
    }
}
