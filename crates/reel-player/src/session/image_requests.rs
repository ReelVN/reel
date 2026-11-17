//! Visible image and tile resource requests.
use super::*;
use crate::stage_widget::OffscreenImage;
use reel_ui::stage::NodeId;
use std::time::{Duration, Instant};

const IMAGE_LOOKAHEAD: Duration = Duration::from_millis(120);

#[derive(Default)]
pub(super) struct ImageMotion {
    previous: Option<(Instant, Vec<OffscreenImage>)>,
}

impl ImageMotion {
    fn observe(
        &mut self,
        now: Instant,
        mut outside: Vec<OffscreenImage>,
    ) -> (Vec<NodeId>, Vec<NodeId>) {
        let mut approaching = Vec::new();
        if let Some((sample, previous)) = &self.previous {
            let elapsed = now.saturating_duration_since(*sample);
            if !elapsed.is_zero() && elapsed <= IMAGE_LOOKAHEAD {
                let lead = IMAGE_LOOKAHEAD.as_secs_f64() / elapsed.as_secs_f64();
                for image in &mut outside {
                    if let Ok(index) = previous.binary_search_by_key(&image.id, |old| old.id) {
                        let old = &previous[index];
                        image.moving_inward =
                            old.distance.is_finite() && image.distance < old.distance;
                        if old.moving_inward
                            && image.moving_inward
                            && image.distance <= (old.distance - image.distance) * lead
                        {
                            approaching.push(image.id);
                        }
                    }
                }
            }
        }
        let ids = outside.iter().map(|image| image.id).collect();
        self.previous = (!outside.is_empty()).then_some((now, outside));
        (ids, approaching)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_images_approaching_the_viewport_are_prefetched() {
        let mut motion = ImageMotion::default();
        let now = Instant::now();
        let sample = |distances: [f64; 5]| {
            distances
                .into_iter()
                .enumerate()
                .map(|(id, distance)| OffscreenImage {
                    id: NodeId(id as u64 + 1),
                    distance,
                    moving_inward: false,
                })
                .collect()
        };
        assert!(
            motion
                .observe(now, sample([100., 100., 100., f64::INFINITY, 1000.]))
                .1
                .is_empty()
        );
        let next = now + Duration::from_millis(20);
        assert!(
            motion
                .observe(next, sample([70., 100., 130., 2., 970.]))
                .1
                .is_empty()
        );
        let (_, approaching) = motion.observe(
            next + Duration::from_millis(20),
            sample([40., 100., 160., 1., 940.]),
        );
        assert_eq!(approaching, [NodeId(1)]);
    }
}

impl SessionUi {
    /// Fixed image dimensions let the existing candidate layout decide raster
    /// demand before IO. Repeated polls of that candidate reuse the same demand.
    pub(super) fn prepare_image_requests(
        &mut self,
        bridge: &mut PresentationBridge,
        root_id: WidgetId,
    ) -> Result<bool, Error> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::UiImageRequests);
        if !self.image_demand_dirty {
            return Ok(true);
        }
        let Some(candidate) = self.pending_stage.as_ref().or(self.stage.as_ref()) else {
            self.image_demand_dirty = false;
            return Ok(true);
        };
        if !self.image_layout_pending {
            let requests = candidate.layout_image_requests(self.images.shader_format)?;
            if !requests.deferred_images {
                self.pending_images_ready = self.images.request_stage_images(Ok(requests));
                self.image_demand_dirty = false;
                return Ok(true);
            }
            if !self.images.requests_ready(&requests) {
                self.images.request_stage_images(Ok(requests));
                self.image_layout_pending = true;
            }
        }
        if self.image_layout_pending {
            self.images.poll()?;
            if !self.images.metadata_ready() {
                return Ok(false);
            }
        }
        let mut layout = candidate.clone();
        layout.resolve_layout_images(&self.images)?;
        bridge.edit_widget(Self::stage_widget(bridge, root_id).unwrap(), |mut stage| {
            StageWidget::sync(&mut stage.downcast::<StageWidget>(), &layout);
        });
        self.sync_popup_view(bridge, root_id, Some(&layout));
        let outside = StageWidget::offscreen_images(bridge.root(), root_id);
        let (outside, approaching) = self.image_motion.observe(Instant::now(), outside);
        self.pending_images_ready =
            self.images
                .request_stage_images(candidate.viewport_image_requests(
                    outside,
                    approaching,
                    self.images.shader_format,
                ));
        if self.pending_stage.is_none() {
            self.pending_stage = Some(layout);
        }
        self.image_demand_dirty = false;
        self.image_layout_pending = false;
        Ok(true)
    }
    pub(super) fn prepare_tiles(
        &mut self,
        bridge: &mut PresentationBridge,
        root_id: WidgetId,
        content_changed: bool,
    ) -> Result<bool, Error> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::UiImageTiles);
        let mut candidate = self.pending_stage.take();
        if candidate.is_none() && content_changed {
            candidate = self.stage.clone();
        }
        if let Some(view) = &mut candidate {
            view.resolve_metadata(&self.images)?;
        }
        if let Some(view) = &candidate {
            bridge.edit_widget(Self::stage_widget(bridge, root_id).unwrap(), |mut stage| {
                StageWidget::sync(&mut stage.downcast::<StageWidget>(), view);
            });
        }
        self.sync_popup_view(bridge, root_id, candidate.as_ref().or(self.stage.as_ref()));
        let mut regions = std::collections::BTreeMap::new();
        let mut demand: std::collections::BTreeMap<String, crate::images::TileDemand> =
            std::collections::BTreeMap::new();
        if !self.images.manifests.is_empty() {
            let mut widgets = bridge
                .edit_widget(Self::stage_widget(bridge, root_id).unwrap(), |mut stage| {
                    StageWidget::image_widgets(&mut stage.downcast::<StageWidget>())
                });
            let count = bridge
                .root()
                .get_widget(root_id)
                .unwrap()
                .downcast::<SessionWidget>()
                .unwrap()
                .popup_layers
                .len();
            for index in 0..count {
                let stage = bridge
                    .root()
                    .get_widget(root_id)
                    .unwrap()
                    .downcast::<SessionWidget>()
                    .unwrap()
                    .popup_layers[index]
                    .stage;
                widgets.extend(bridge.edit_widget(stage, |mut widget| {
                    StageWidget::image_widgets(&mut widget.downcast::<StageWidget>())
                }));
            }
            let geometry = bridge
                .layout_geometry()
                .map_err(|error| format!("image layout geometry: {error:?}"))?;
            for (id, image) in widgets {
                if let Some(manifest) = self.images.manifests.get(&image.source) {
                    let (region, tiles) =
                        crate::images::tiles::visible_tiles(*manifest, &image, &geometry);
                    regions.insert(id, region);
                    for (key, tile) in tiles {
                        demand
                            .entry(key)
                            .and_modify(|previous| previous.required |= tile.required)
                            .or_insert(tile);
                    }
                }
            }
        }
        let changed = self.images.request_tiles(demand);
        if candidate.is_none() && changed {
            candidate = self.stage.clone();
        }
        if !self.images.poll()? {
            self.pending_stage = candidate;
            self.images_waiting_presentation = true;
            return Ok(false);
        }
        if let Some(mut view) = candidate {
            // Rebuild leases from this demand; previous accepted frames retain their own.
            self.images_waiting_presentation = true;
            view.resolve_metadata(&self.images)?;
            view.resolve_tiles(&self.images, &regions);
            self.stage = Some(view);
            self.stage_dirty = true;
            self.images.commit();
        }
        Ok(true)
    }
}
