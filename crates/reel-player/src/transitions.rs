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
    resources::ResourceLease,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

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
    prefetch_masks: BTreeSet<String>,
    mask_programs: Vec<(Arc<reel_ui::stage::MaskProgram>, bool)>,
    gate_stamp: Option<(u64, crate::session::ProjectionStamp, WidgetId)>,
    #[cfg(test)]
    mask_rebuilds: usize,
    #[cfg(test)]
    gate_refreshes: usize,
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
        self.restoring.is_empty() && (self.wanted_masks.is_empty() || self.masks_ready)
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
                    RetainedAssetKind::EffectShader => AssetKind::EffectShader,
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
        changed |= self.request_masks(
            || {
                vm.transition_views().filter_map(|view| {
                    let mask = view.mask?;
                    let required = view.previous.is_some();
                    (required || view.phase == TransitionPhase::OldPending)
                        .then_some((mask, required))
                })
            },
            assets,
        );
        changed |= self.poll_masks()?;
        Ok(changed)
    }
    fn poll_masks(&mut self) -> Result<bool, Error> {
        if let Some(images) = &mut self.masks
            && (!self.masks_ready || !self.prefetch_masks.is_empty())
            && images.poll()?
            && !self.masks_ready
        {
            images.commit();
            // Only masks used in the current candidate belong to its drawing.
            self.resources = self
                .wanted_masks
                .keys()
                .map(|source| {
                    images.loaded.get(source).cloned().ok_or_else(|| {
                        format!("stage.transition: mask {source:?} requires a single image texture")
                    })
                })
                .collect::<Result<_, _>>()?;
            self.masks_ready = true;
            return Ok(true);
        }
        Ok(false)
    }
    fn request_masks<I: Iterator<Item = (Arc<reel_ui::stage::MaskProgram>, bool)>>(
        &mut self,
        masks: impl Fn() -> I,
        assets: &Assets,
    ) -> bool {
        let mut previous = self.mask_programs.iter();
        if masks().all(|(mask, required)| {
            previous.next().is_some_and(|(old, was_required)| {
                required == *was_required && Arc::ptr_eq(old, &mask)
            })
        }) && previous.next().is_none()
        {
            return false;
        }
        #[cfg(test)]
        {
            self.mask_rebuilds += 1;
        }
        self.mask_programs.clear();
        self.mask_programs.extend(masks());
        let mut wanted = BTreeMap::new();
        let mut prefetch = BTreeSet::new();
        for (mask, required) in &self.mask_programs {
            for source in mask.sources() {
                if *required {
                    wanted.insert(source.to_string(), Some("stage.transition".into()));
                } else {
                    prefetch.insert(source.to_string());
                }
            }
        }
        prefetch.retain(|source| !wanted.contains_key(source));
        let required_changed = wanted != self.wanted_masks;
        if !required_changed && prefetch == self.prefetch_masks {
            return false;
        }
        self.masks
            .get_or_insert_with(|| crate::images::Images::new(assets.clone()))
            .request_paths(wanted.clone(), prefetch.clone());
        self.wanted_masks = wanted;
        self.prefetch_masks = prefetch;
        if required_changed {
            self.masks_ready = false;
        }
        true
    }
    pub fn gate(
        &mut self,
        vm: &SessionVm,
        bridge: &mut PresentationBridge,
        root: WidgetId,
        projection: crate::session::ProjectionStamp,
    ) -> Result<(), Error> {
        let stamp = (vm.transition_revision(), projection, root);
        if self.release_gates.is_none() && self.gate_stamp == Some(stamp) {
            return Ok(());
        }
        #[cfg(test)]
        {
            self.gate_refreshes += 1;
        }
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
        self.gate_stamp = Some(stamp);
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
                        lease.clone()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_transition_masks_reuse_resource_demand() {
        let assets = Assets::directory(Default::default());
        let mut transitions = Transitions::default();
        let first = reel_ui::stage::MaskProgram::image("first.png".into());
        assert!(transitions.request_masks(|| [(first.clone(), true)].into_iter(), &assets));
        for _ in 0..4096 {
            assert!(!transitions.request_masks(|| [(first.clone(), true)].into_iter(), &assets));
        }
        assert_eq!(transitions.mask_rebuilds, 1);
        println!(
            "transition_masks refreshes=4097 demand_rebuilds={}",
            transitions.mask_rebuilds
        );
        assert!(transitions.wanted_masks.contains_key("first.png"));
        let second = reel_ui::stage::MaskProgram::image("second.png".into());
        assert!(transitions.request_masks(|| [(second.clone(), true)].into_iter(), &assets));
        assert_eq!(transitions.wanted_masks.len(), 1);
        assert!(transitions.wanted_masks.contains_key("second.png"));
        assert!(transitions.request_masks(std::iter::empty, &assets));
        assert!(transitions.wanted_masks.is_empty());
    }

    #[test]
    fn announced_mask_keeps_old_drawing_ready_and_cancels_with_transition() {
        use crate::{assets::ImageExecutor, images::tests::ControlledIo};
        use reel_core::{budget::QueueLimits, identity::FrameId, test_support::MemoryAssetIo};
        use std::{cell::RefCell, rc::Rc};

        let mut controlled_io = None;
        let assets = Assets::with_adapters(Default::default(), |wake| {
            let limits = QueueLimits {
                max_requests: 8,
                max_bytes: 1 << 20,
            };
            let io = Rc::new(RefCell::new(MemoryAssetIo::new(
                limits,
                limits,
                wake.clone(),
            )));
            controlled_io = Some(io.clone());
            (
                Box::new(ControlledIo(io)),
                Box::new(ImageExecutor::new(limits, wake)),
            )
        });
        let io = controlled_io.unwrap();
        let mut driver = crate::aside_session::AsideSessionUi::from_source(
            crate::session::tests::session(),
            r##"fn main() {
                let view = stack(width: 64, height: 64, background: "#ff0000");
                stage.replace(view);
                stage.transition(target: view, duration: 1, mask: "held.png", change: || {
                    view.background = "#0000ff";
                });
            }"##,
            "mask_preparation",
            1,
            "main",
            "en",
            Default::default(),
        )
        .unwrap()
        .with_assets(assets.clone());
        for _ in 0..1000 {
            driver.tick(Default::default(), 256, || 0).unwrap();
            if driver.adapter().stage_ready() && !driver.has_runnable_tasks() {
                break;
            }
        }
        let view = driver.adapter().transition_views().next().unwrap();
        assert_eq!(view.phase, TransitionPhase::OldPending);
        assert!(view.previous.is_none());
        let (mut bridge, root) = crate::session::tests::mount(driver.ui());
        driver.sync(&mut bridge, root).unwrap();
        assert_eq!(io.borrow().read_usage().requests, 1);
        assert!(driver.transition_resources().is_empty());
        let (_, receipt) = driver
            .record(&mut bridge, root, FrameId(1))
            .unwrap()
            .expect("old drawing can be recorded while the mask is still loading");
        assert!(receipt.needs_transition_drawing());

        driver.cancel().unwrap();
        assets.maintain();
        assert_eq!(io.borrow().read_usage().requests, 0);
        assert!(!io.borrow_mut().complete_next_read());
    }

    #[test]
    fn transition_takes_over_the_in_flight_mask() {
        use std::{sync::mpsc, time::Duration};

        let assets = crate::assets::tests::memory_assets();
        let (send, receive) = mpsc::channel();
        assets.set_waker(Arc::new(move || {
            let _ = send.send(());
        }));
        let mut transitions = Transitions::default();
        let mask = reel_ui::stage::MaskProgram::image("assets/panels.png".into());
        assert!(transitions.request_masks(|| [(mask.clone(), false)].into_iter(), &assets));
        assert!(transitions.ready());
        transitions.poll_masks().unwrap();
        assert!(transitions.resources().is_empty());
        assert_eq!(transitions.masks.as_ref().unwrap().loads, 1);

        assert!(transitions.request_masks(|| [(mask.clone(), true)].into_iter(), &assets));
        assert!(!transitions.ready());
        while !transitions.poll_masks().unwrap() {
            receive.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        assert!(transitions.ready());
        assert_eq!(transitions.resources().len(), 1);
        assert_eq!(transitions.masks.as_ref().unwrap().loads, 1);
    }

    #[test]
    fn transition_gates_reuse_stable_projections_and_apply_accepted_releases() {
        let mut driver = crate::aside_session::AsideSessionUi::from_source(
            reel_engine::Session::new(reel_engine::SessionId(881), reel_engine::Epoch(1)).unwrap(),
            "fn main(){stage.replace(stack()); await completion();}",
            "gate",
            1,
            "main",
            "en",
            reel_engine::aside::VmLimits::default(),
        )
        .unwrap();
        for _ in 0..1000 {
            driver
                .tick(reel_engine::aside::SliceBudget::default(), 256, || 0)
                .unwrap();
            if driver.adapter().stage_ready() && !driver.has_runnable_tasks() {
                break;
            }
        }
        let (mut bridge, root) = crate::session::tests::mount(driver.ui());
        driver.sync(&mut bridge, root).unwrap();
        let mut transitions = Transitions::default();
        let stamp = driver.ui().projection_stamp();
        for _ in 0..4096 {
            transitions
                .gate(driver.adapter(), &mut bridge, root, stamp)
                .unwrap();
        }
        assert_eq!(transitions.gate_refreshes, 1);
        println!(
            "transition_gates refreshes=4096 widget_search_rounds={}",
            transitions.gate_refreshes
        );
        transitions
            .accepted_projection(driver.adapter_mut(), &TransitionReceipt::default(), 0.)
            .unwrap();
        transitions
            .gate(driver.adapter(), &mut bridge, root, stamp)
            .unwrap();
        assert_eq!(transitions.gate_refreshes, 2);
        transitions
            .gate(driver.adapter(), &mut bridge, root, stamp)
            .unwrap();
        assert_eq!(transitions.gate_refreshes, 2);
    }
}
