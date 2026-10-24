//! Accepted drawing retention and asset rebinding shared by every story host.
use crate::assets::{AssetKind, Assets, LoadId};
use reel_engine::aside::{
    SessionVm,
    transitions::{TransitionId, TransitionPhase, TransitionRequest},
};
use reel_ui::{
    bridge::{PresentationBridge, RecordedSource},
    core::{PaintImageMask, WidgetId},
    frame::{
        BuildLimits, FrameTransition, RenderFrame, RetainBuild, RetainedAssetKind, RetainedRestore,
        SavedRetainedScene, StepBudget,
    },
    resources::{ResourceData, ResourceLease},
};
use std::{collections::BTreeMap, sync::Arc};

type Error = Box<dyn std::error::Error>;
const LIMITS: BuildLimits = BuildLimits { max_bytes: 8 << 20 };
const STEP: StepBudget = StepBudget {
    max_commands: 64,
    max_units: 8192,
    max_bytes: 64 << 10,
};

struct Retaining {
    id: TransitionId,
    build: RetainBuild,
    budget: StepBudget,
}
struct Restoring {
    id: TransitionId,
    saved: Arc<SavedRetainedScene>,
    build: RetainedRestore,
    asset: Option<(u32, RestoreAsset)>,
    budget: StepBudget,
}
struct RestoreAsset {
    assets: Assets,
    id: LoadId,
}
impl Drop for RestoreAsset {
    fn drop(&mut self) {
        self.assets.cancel(self.id);
    }
}

#[derive(Default, Debug)]
pub(crate) struct TransitionReceipt {
    retains: Vec<(TransitionId, WidgetId)>,
    accepted: Vec<(TransitionId, TransitionPhase, f64)>,
    gates: Vec<(TransitionId, WidgetId)>,
}
impl TransitionReceipt {
    pub fn needs_drawing(&self) -> bool {
        !self.retains.is_empty() || !self.accepted.is_empty()
    }
}

#[derive(Default)]
pub(crate) struct Transitions {
    waiting: Vec<TransitionRequest>,
    retaining: Vec<Retaining>,
    restoring: Vec<Restoring>,
    masks: Option<crate::images::Images>,
    wanted_masks: BTreeMap<String, Option<String>>,
    resources: Vec<ResourceLease>,
    masks_ready: bool,
    gates: Vec<WidgetId>,
    release_gates: Option<Vec<WidgetId>>,
    revision: u64,
}
impl Transitions {
    pub fn pending(&self) -> bool {
        !self.waiting.is_empty() || !self.retaining.is_empty() || !self.ready()
    }
    pub fn ready(&self) -> bool {
        self.restoring.is_empty() && (self.masks.is_none() || self.masks_ready)
    }
    pub fn resources(&self) -> &[ResourceLease] {
        &self.resources
    }
    pub fn revision_changed(&mut self, vm: &SessionVm) -> bool {
        let revision = vm.transition_revision();
        let changed = self.revision != revision;
        self.revision = revision;
        changed
    }
    pub fn maintain(&mut self, vm: &mut SessionVm, assets: &Assets) -> Result<bool, Error> {
        let mut changed = false;
        let mut waiting = Vec::new();
        for request in self.waiting.drain(..) {
            if vm.transition_request_pending(request.id)? {
                waiting.push(request);
            }
        }
        self.waiting = waiting;
        while let Some(request) = vm.take_transition_request()? {
            self.waiting.push(request);
            changed = true;
        }
        let mut index = 0;
        while index < self.retaining.len() {
            if !vm.transition_request_pending(self.retaining[index].id)? {
                self.retaining.remove(index);
                changed = true;
                continue;
            }
            let work = &mut self.retaining[index];
            let step = work.build.step(work.budget);
            match step {
                Ok(progress) if !progress.complete => {
                    work.budget.max_units = work.budget.max_units.max(progress.minimum_units);
                    work.budget.max_bytes = work.budget.max_bytes.max(progress.minimum_bytes);
                    index += 1;
                }
                result => {
                    let work = self.retaining.remove(index);
                    let retained = result
                        .and_then(|_| work.build.finish())
                        .and_then(|scene| {
                            let saved = scene.snapshot(|lease| assets.retained_asset(lease))?;
                            Ok((Arc::new(scene), Arc::new(saved)))
                        })
                        .map_err(|error| format!("retaining transition drawing: {error:?}"));
                    changed |= vm.complete_transition_request(work.id, retained)?;
                }
            }
        }
        for (id, saved) in vm.pending_transition_pictures() {
            if self.restoring.iter().any(|work| work.id == id) {
                continue;
            }
            let build = saved
                .restore_begin(LIMITS, &assets.output_pool())
                .map_err(|error| format!("restoring transition drawing: {error:?}"))?;
            self.restoring.push(Restoring {
                id,
                saved,
                build,
                asset: None,
                budget: STEP,
            });
        }
        let mut index = 0;
        while index < self.restoring.len() {
            if !vm
                .transition_views()
                .any(|view| view.id == self.restoring[index].id)
            {
                self.restoring.remove(index);
                continue;
            }
            let work = &mut self.restoring[index];
            if let Some((slot, request)) = &work.asset {
                let Some(lease) = assets.poll(request.id)? else {
                    index += 1;
                    continue;
                };
                work.build
                    .bind_asset(*slot, lease)
                    .map_err(|error| format!("restoring transition asset: {error:?}"))?;
                work.asset = None;
            }
            let progress = work
                .build
                .step(work.budget)
                .map_err(|error| format!("restoring transition drawing: {error:?}"))?;
            if let Some(slot) = progress.missing_asset {
                let (_, asset) = work
                    .saved
                    .assets()
                    .find(|(index, _)| *index == slot)
                    .expect("restore requests an indexed saved asset");
                let kind = match asset.kind {
                    RetainedAssetKind::Image => AssetKind::Image,
                    RetainedAssetKind::Font => AssetKind::Font,
                };
                work.asset = Some((
                    slot,
                    RestoreAsset {
                        assets: assets.clone(),
                        id: assets.request_version(asset.version.clone(), kind),
                    },
                ));
                index += 1;
            } else if progress.build.complete {
                let work = self.restoring.remove(index);
                let scene = work
                    .build
                    .finish()
                    .map_err(|error| format!("restoring transition drawing: {error:?}"))?;
                changed |= vm.install_transition_picture(work.id, Arc::new(scene))?;
            } else {
                work.budget.max_units = work.budget.max_units.max(progress.build.minimum_units);
                work.budget.max_bytes = work.budget.max_bytes.max(progress.build.minimum_bytes);
                index += 1;
            }
        }
        let wanted = vm
            .transition_views()
            .filter(|view| view.previous.is_some())
            .flat_map(|view| {
                view.mask
                    .into_iter()
                    .flat_map(|mask| mask.sources().to_vec())
            })
            .map(|source| (source.to_string(), Some("stage.transition".into())))
            .collect::<BTreeMap<_, _>>();
        if wanted != self.wanted_masks {
            let images = self
                .masks
                .get_or_insert_with(|| crate::images::Images::new(assets.clone()));
            images.request(wanted.clone());
            self.wanted_masks = wanted;
            self.masks_ready = false;
            changed = true;
        }
        if let Some(images) = &mut self.masks
            && !self.masks_ready
            && images.poll()?
        {
            images.commit();
            self.resources = images.loaded.values().cloned().collect();
            self.masks_ready = true;
            changed = true;
        }
        Ok(changed)
    }
    pub fn gate(
        &mut self,
        vm: &SessionVm,
        bridge: &mut PresentationBridge,
        root: WidgetId,
    ) -> Result<(), Error> {
        if let Some(gates) = self.release_gates.take() {
            self.gates = gates;
        }
        for view in vm.transition_views() {
            if let Some(widget) =
                crate::session::SessionUi::transition_target(bridge, root, view.target)
                && !self.gates.contains(&widget)
            {
                self.gates.push(widget);
            }
        }
        bridge.set_input_gates(&self.gates)?;
        Ok(())
    }
    pub fn record(
        &self,
        vm: &SessionVm,
        bridge: &PresentationBridge,
        root: WidgetId,
        source: &RecordedSource,
        presentation_request: u64,
    ) -> Result<(TransitionReceipt, Vec<FrameTransition>), Error> {
        let mut receipt = TransitionReceipt::default();
        for request in &self.waiting {
            if presentation_request >= request.presentation_request
                && let Some(widget) =
                    crate::session::SessionUi::transition_target(bridge, root, request.target)
            {
                receipt.retains.push((request.id, widget));
            }
        }
        let mut frames = Vec::new();
        for view in vm.transition_views() {
            let Some(widget) =
                crate::session::SessionUi::transition_target(bridge, root, view.target)
            else {
                continue;
            };
            receipt.gates.push((view.id, widget));
            receipt.accepted.push((view.id, view.phase, view.progress));
            let Some(previous) = view.previous else {
                continue;
            };
            let mask = view.mask.map(|program| {
                let images = program
                    .sources()
                    .iter()
                    .map(|source| {
                        let lease = &self.masks.as_ref().unwrap().loaded[source.as_ref()];
                        let ResourceData::Image { image, .. } = lease.data() else {
                            unreachable!()
                        };
                        image.clone()
                    })
                    .collect();
                PaintImageMask {
                    program,
                    images,
                    bounds: source.geometry().get(widget).unwrap().size.to_rect(),
                    parameters: [
                        view.progress as f32,
                        view.mask_softness.min(f32::MAX as f64) as f32,
                        view.mask_x as f32,
                        view.mask_y as f32,
                        f32::from(view.mask_invert),
                    ],
                }
            });
            frames.push(FrameTransition {
                root: Some(widget),
                previous: (*previous).clone(),
                progress: view.progress as f32,
                mask,
            });
        }
        Ok((receipt, frames))
    }
    pub fn accepted(
        &mut self,
        vm: &mut SessionVm,
        receipt: &TransitionReceipt,
        frame: &RenderFrame,
        assets: &Assets,
    ) -> Result<(), Error> {
        for &(id, widget) in &receipt.retains {
            if !vm.transition_request_pending(id)?
                || !self.waiting.iter().any(|request| request.id == id)
            {
                continue;
            }
            self.waiting.retain(|request| request.id != id);
            match RetainBuild::new(frame, Some(widget), LIMITS, &assets.output_pool()) {
                Ok(build) => self.retaining.push(Retaining {
                    id,
                    build,
                    budget: STEP,
                }),
                Err(error) => {
                    vm.complete_transition_request(
                        id,
                        Err(format!("retaining transition drawing: {error:?}")),
                    )?;
                }
            }
        }
        Ok(())
    }
    pub fn accepted_projection(
        &mut self,
        vm: &mut SessionVm,
        receipt: &TransitionReceipt,
        accepted_time: f64,
    ) -> Result<(), Error> {
        let mut gates = receipt.gates.clone();
        for &(id, phase, progress) in &receipt.accepted {
            if vm.accept_transition(id, phase, progress, accepted_time)?
                && phase == TransitionPhase::FinalPending
            {
                gates.retain(|(candidate, _)| *candidate != id);
            }
        }
        self.release_gates = Some(gates.into_iter().map(|(_, widget)| widget).collect());
        Ok(())
    }
    pub fn close(&mut self) {
        *self = Self::default();
    }
}
