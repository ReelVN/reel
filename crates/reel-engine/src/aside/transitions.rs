//! Same-task transitions retain accepted drawing, never script nodes or input.
use super::*;

pub(super) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{named, nullable, number};
    api.refine(
        "reel::stage::transition",
        TypeHint::Any,
        &[
            ("change", TypeHint::Closure),
            ("target", nullable(named("reel::types::Node"))),
            ("duration", number()),
            ("easing", TypeHint::String),
            (
                "mask",
                nullable(TypeHint::Union(vec![
                    TypeHint::String,
                    number(),
                    named("reel::types::Mask"),
                ])),
            ),
            ("mask_softness", number()),
            ("mask_invert", TypeHint::Bool),
            ("mask_x", number()),
            ("mask_y", number()),
        ],
    );
}

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Retain the accepted drawing of target, call change in the current task, then
        /// blend into the newly presented drawing. A null target captures the full stage
        /// including popups. A mounted body node scopes the transition to its presentation
        /// layer; target a popup directly to transition it independently. Cursor nodes are
        /// excluded as targets.
        /// Duration, easing and masks control the blend. The call waits for final frame
        /// acceptance and returns the original value returned by `change`.
        /// A string mask reveals the new drawing through the image's grayscale ranks as
        /// progress advances. A Mask expression supplies coverage directly; compose it
        /// with mask.param("progress") when its coverage should change during the blend.
        /// Retained drawing owns resources independently of retired script nodes.
        "reel::stage::transition";
    }
}
use aside_vm::executor::{Continuation, FaultRetention, FrameId};
use aside_vm::{HeapReservation, ValueTag};
use reel_ui::{
    frame::{RetainedScene, SavedRetainedScene},
    stage::{MaskProgram, NodeId, NodeKind},
};
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, sync::Arc};

const OWNER: u32 = 0;
const TARGET: u32 = 1;
const CHANGE: u32 = 2;
const RETURNED: u32 = 3;
const MASK: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransitionId {
    pub session: SessionIdentity,
    pub serial: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionPhase {
    OldPending,
    Changing,
    NewPending,
    Playing,
    FinalPending,
}

#[derive(Clone, Copy, Debug)]
pub struct TransitionRequest {
    pub id: TransitionId,
    pub target: Option<NodeId>,
    /// Only a newly accepted frame carrying this fence may supply the old picture.
    pub presentation_request: u64,
}

#[derive(Clone)]
pub struct TransitionView {
    pub id: TransitionId,
    pub target: Option<NodeId>,
    pub previous: Option<Arc<RetainedScene>>,
    pub phase: TransitionPhase,
    pub progress: f64,
    pub mask: Option<Arc<MaskProgram>>,
    pub mask_softness: f64,
    pub mask_invert: bool,
    pub mask_x: f64,
    pub mask_y: f64,
}

#[derive(Serialize, Deserialize)]
struct SavedTransition {
    serial: u64,
    task: TaskId,
    frame: FrameId,
    root: RootToken,
    wait: Option<WaitId>,
    phase: TransitionPhase,
    duration: f64,
    easing: i64,
    elapsed: f64,
    last_time: f64,
    presentation_request: u64,
    mask_softness: f64,
    mask_invert: bool,
    mask_x: f64,
    mask_y: f64,
    finished: bool,
}

struct Transition {
    state: SavedTransition,
    target: Option<NodeId>,
    previous: Option<Arc<RetainedScene>>,
    saved: Option<Arc<SavedRetainedScene>>,
    mask: Option<Arc<MaskProgram>>,
    claimed: bool,
}

#[derive(Default)]
pub(super) struct Transitions {
    serial: u64,
    revision: u64,
    live: VecDeque<Transition>,
    charge: Option<HeapReservation>,
    time: f64,
    round: Option<(f64, usize)>,
    checked_lifetimes: Option<(u64, u64)>,
    #[cfg(test)]
    pub(super) lifetime_checks: std::cell::Cell<usize>,
}

impl Transitions {
    fn changed(&mut self) {
        self.revision += 1;
    }
    pub fn needs_time(&self) -> bool {
        self.live
            .iter()
            .any(|t| t.state.phase == TransitionPhase::Playing && !t.state.finished)
    }
    pub fn active(&self) -> bool {
        !self.live.is_empty()
    }
    pub fn pending(&self, time: f64) -> bool {
        self.round.is_some() || (self.needs_time() && time > self.time)
    }
    pub fn snapshot_ready(&self) -> bool {
        self.live.iter().all(|t| {
            t.state.finished
                || (t.state.phase != TransitionPhase::OldPending
                    && (t.saved.is_none() || t.previous.is_some()))
        })
    }
    pub fn advance(&mut self, time: f64, work: u32) -> ActivityProgress {
        if !self.pending(time) {
            return ActivityProgress::default();
        }
        let (time, mut cursor) = self.round.unwrap_or((time, 0));
        let mut changed = false;
        let mut used = 0;
        while cursor < self.live.len() && used < work {
            let transition = &mut self.live[cursor];
            cursor += 1;
            used += 1;
            let state = &mut transition.state;
            if state.phase != TransitionPhase::Playing || state.finished {
                continue;
            }
            // Acceptance may be newer than the latest activity sample. Earlier
            // maintenance must neither consume preparation time nor rewind it.
            let time = time.max(state.last_time);
            let elapsed = (state.elapsed + (time - state.last_time)).min(state.duration);
            state.last_time = time;
            if state.elapsed != elapsed {
                state.elapsed = elapsed;
                changed = true;
            }
            if state.elapsed == state.duration {
                state.phase = TransitionPhase::FinalPending;
                changed = true;
            }
        }
        if changed {
            self.changed();
        }
        self.round = (cursor < self.live.len()).then_some((time, cursor));
        if self.round.is_none() {
            self.time = time;
        }
        ActivityProgress {
            work: used,
            pending: self.round.is_some(),
            woke: changed,
        }
    }
}

impl SessionVm {
    fn transition_value(&self, index: usize, slot: u32) -> Result<Value, AdapterError> {
        let heap = self.vm.heap();
        Ok(heap.sequence_get(
            heap.root_value(self.transitions.live[index].state.root)?,
            slot,
        )?)
    }

    fn transition_index(&self, id: TransitionId) -> Option<usize> {
        (!self.revoked && id.session == self.identity)
            .then(|| {
                self.transitions
                    .live
                    .iter()
                    .position(|t| t.state.serial == id.serial)
            })
            .flatten()
    }

    fn transition_owner_live(&self, index: usize) -> Result<bool, AdapterError> {
        #[cfg(test)]
        self.transitions
            .lifetime_checks
            .set(self.transitions.lifetime_checks.get() + 1);
        let entry = &self.transitions.live[index];
        if entry.state.finished {
            return Ok(true);
        }
        Ok(
            scopes::owner_live(self.vm.heap(), self.transition_value(index, OWNER)?)
                && entry
                    .target
                    .is_none_or(|target| self.stage.scene.node(target).is_some()),
        )
    }

    fn transition_lifetimes(&self) -> (u64, u64) {
        (
            self.vm.heap().host_retirement_revision(),
            self.stage.retirement_revision,
        )
    }

    pub(super) fn transitions_pending(&self) -> bool {
        if self.transitions.checked_lifetimes == Some(self.transition_lifetimes()) {
            return false;
        }
        (0..self.transitions.live.len()).any(|index| {
            !self
                .transition_owner_live(index)
                .expect("live transition root")
        })
    }

    fn close_transition(&mut self, index: usize) -> Result<(), AdapterError> {
        let transition = self.transitions.live.remove(index).unwrap();
        self.vm.heap_mut().release_root(transition.state.root)?;
        self.transitions.changed();
        if let Some((_, cursor)) = &mut self.transitions.round
            && index < *cursor
        {
            *cursor -= 1;
        }
        if self.transitions.live.is_empty() {
            self.transitions.live = VecDeque::new();
            self.transitions.charge = None;
            self.transitions.round = None;
        }
        Ok(())
    }

    pub(super) fn cancel_task_transitions(
        &mut self,
        task: TaskId,
        after: Option<FrameId>,
    ) -> Result<(), AdapterError> {
        for index in (0..self.transitions.live.len()).rev() {
            let state = &self.transitions.live[index].state;
            if state.task == task && after.is_none_or(|frame| state.frame.0 > frame.0) {
                self.close_transition(index)?;
            }
        }
        Ok(())
    }

    pub(super) fn maintain_transitions(
        &mut self,
        session: &mut Session,
    ) -> Result<(), AdapterError> {
        let lifetimes = self.transition_lifetimes();
        if self.transitions.checked_lifetimes == Some(lifetimes) {
            return Ok(());
        }
        let mut index = 0;
        while index < self.transitions.live.len() {
            if self.transition_owner_live(index)? {
                index += 1;
                continue;
            }
            let state = &self.transitions.live[index].state;
            let (task, frame) = (state.task, state.frame);
            self.cancel_native_region(session, task, frame)?;
            self.cancel_inner_scopes(session, task, frame)?;
            self.close_transition(index)?;
        }
        // Cancellation can retire another owner; observe that on the next turn.
        self.transitions.checked_lifetimes = Some(lifetimes);
        Ok(())
    }

    pub fn transition_revision(&self) -> u64 {
        self.transitions.revision
    }

    pub fn transition_views(&self) -> impl Iterator<Item = TransitionView> + '_ {
        self.transitions.live.iter().filter_map(|entry| {
            let state = &entry.state;
            if state.finished {
                return None;
            }
            let progress = match state.phase {
                TransitionPhase::FinalPending => 1.,
                TransitionPhase::Playing => activity::easing::Easing::from_code(state.easing)
                    .sample(state.elapsed / state.duration)
                    .clamp(0., 1.),
                _ => 0.,
            };
            Some(TransitionView {
                id: TransitionId {
                    session: self.identity,
                    serial: state.serial,
                },
                target: entry.target,
                previous: entry.previous.clone(),
                phase: state.phase,
                progress,
                mask: entry.mask.clone(),
                mask_softness: state.mask_softness,
                mask_invert: state.mask_invert,
                mask_x: state.mask_x,
                mask_y: state.mask_y,
            })
        })
    }

    pub fn transition_request_pending(&self, id: TransitionId) -> Result<bool, AdapterError> {
        let Some(index) = self.transition_index(id) else {
            return Ok(false);
        };
        let transition = &self.transitions.live[index];
        Ok(transition.state.phase == TransitionPhase::OldPending
            && transition.previous.is_none()
            && transition
                .state
                .wait
                .is_some_and(|wait| self.vm.wait_is_pending(wait))
            && self.transition_owner_live(index)?)
    }

    pub fn take_transition_request(&mut self) -> Result<Option<TransitionRequest>, AdapterError> {
        for index in 0..self.transitions.live.len() {
            let transition = &self.transitions.live[index];
            if transition.claimed
                || transition.state.phase != TransitionPhase::OldPending
                || transition.state.wait.is_none()
                || !self.transition_owner_live(index)?
            {
                continue;
            }
            let transition = &mut self.transitions.live[index];
            transition.claimed = true;
            return Ok(Some(TransitionRequest {
                id: TransitionId {
                    session: self.identity,
                    serial: transition.state.serial,
                },
                target: transition.target,
                presentation_request: transition.state.presentation_request,
            }));
        }
        Ok(None)
    }

    pub fn complete_transition_request(
        &mut self,
        id: TransitionId,
        result: Result<(Arc<RetainedScene>, Arc<SavedRetainedScene>), String>,
    ) -> Result<bool, AdapterError> {
        if !self.transition_request_pending(id)? {
            return Ok(false);
        }
        let index = self.transition_index(id).unwrap();
        let wait = self.transitions.live[index].state.wait.unwrap();
        let completion = match result {
            Ok((previous, saved)) => {
                self.transitions.live[index].previous = Some(previous);
                self.transitions.live[index].saved = Some(saved);
                VmCompletion::Ready(Value::NULL)
            }
            Err(message) => VmCompletion::Error(aside_vm::NewFault {
                value: text::alloc_string(self.vm.heap_mut(), &message, self.read_limit)?,
                code: aside_vm::FaultCode::core(FaultKind::InvalidState),
                cause: None,
            }),
        };
        self.vm.complete(wait, completion)?;
        self.transitions.live[index].state.wait = None;
        self.transitions.changed();
        Ok(true)
    }

    /// Called only for the transition metadata carried by the accepted candidate.
    /// Accept one transition projection at a monotonic session-logical time.
    /// The first zero-progress frame starts playback at its actual acceptance,
    /// independently of the earlier VM/activity sample used to prepare it.
    pub fn accept_transition(
        &mut self,
        id: TransitionId,
        phase: TransitionPhase,
        progress: f64,
        accepted_time: f64,
    ) -> Result<bool, AdapterError> {
        if !accepted_time.is_finite() || accepted_time < 0. {
            return Err(AdapterError::Input("invalid transition presentation time"));
        }
        let Some(index) = self.transition_index(id) else {
            return Ok(false);
        };
        if !self.transition_owner_live(index)? {
            return Ok(false);
        }
        let state = &mut self.transitions.live[index].state;
        if state.phase != phase || state.finished {
            return Ok(false);
        }
        match phase {
            TransitionPhase::NewPending if progress == 0. => {
                state.phase = TransitionPhase::Playing;
                state.last_time = accepted_time.max(self.activities.time());
            }
            TransitionPhase::FinalPending if progress == 1. => {
                let wait = state.wait.take().unwrap();
                self.vm.complete(wait, VmCompletion::Ready(Value::NULL))?;
                state.finished = true;
            }
            _ => return Ok(false),
        }
        self.transitions.changed();
        Ok(true)
    }

    pub fn pending_transition_pictures(
        &self,
    ) -> impl Iterator<Item = (TransitionId, Arc<SavedRetainedScene>)> + '_ {
        self.transitions.live.iter().filter_map(|entry| {
            if entry.state.finished || entry.previous.is_some() {
                return None;
            }
            entry.saved.as_ref().map(|saved| {
                (
                    TransitionId {
                        session: self.identity,
                        serial: entry.state.serial,
                    },
                    saved.clone(),
                )
            })
        })
    }

    pub fn install_transition_picture(
        &mut self,
        id: TransitionId,
        picture: Arc<RetainedScene>,
    ) -> Result<bool, AdapterError> {
        let Some(index) = self.transition_index(id) else {
            return Ok(false);
        };
        let transition = &mut self.transitions.live[index];
        if transition.previous.is_some() || transition.saved.is_none() {
            return Ok(false);
        }
        transition.previous = Some(picture);
        self.transitions.changed();
        Ok(true)
    }

    fn transition_target(&self, value: Value) -> Result<Option<NodeId>, AdapterError> {
        if value.tag() == ValueTag::Null {
            return Ok(None);
        }
        let target = self.stage.id(self.vm.heap(), value)?;
        let mut ancestor = target;
        loop {
            let node = self.stage.scene.node(ancestor).unwrap();
            if matches!(node.kind, NodeKind::Cursor) {
                return Err(AdapterError::Input(
                    "transition target must be in the stage body",
                ));
            }
            let Some(parent) = node.parent else {
                break;
            };
            ancestor = parent;
        }
        if Some(ancestor) != self.stage.scene.root() {
            return Err(AdapterError::Input("transition target must be mounted"));
        }
        Ok(Some(target))
    }

    fn transition_targets_overlap(&self, a: Option<NodeId>, b: Option<NodeId>) -> bool {
        let (Some(a), Some(b)) = (a, b) else {
            return true;
        };
        let contains = |ancestor, mut current| {
            loop {
                if ancestor == current {
                    return true;
                }
                let Some(node) = self.stage.scene.node(current) else {
                    return false;
                };
                if matches!(node.kind, NodeKind::Popup(_)) {
                    return false;
                }
                let Some(parent) = node.parent else {
                    return false;
                };
                current = parent;
            }
        };
        contains(a, b) || contains(b, a)
    }

    pub(super) fn dispatch_transition(&mut self, ticket: HostTicket) -> Result<(), AdapterError> {
        if ticket.input == NativeInput::Entry {
            if ticket.context.no_wait || ticket.context.init || ticket.context.derived {
                return Err(VmError::Effect.into());
            }
            let args = self.vm.host_arguments(ticket)?;
            let change = args[0];
            let target_value = args[1];
            let target = self.transition_target(target_value)?;
            let duration = stage::nonnegative(args[2])?;
            let easing = activity::easing::Easing::parse(&text::read_string(
                self.vm.heap(),
                args[3],
                self.read_limit,
            )?)?;
            let mask_value = args[4];
            stage::validate_mask(self.vm.heap(), mask_value)?;
            let mask = (mask_value.tag() != ValueTag::Null)
                .then(|| masks::project(self.vm.heap(), mask_value, self.read_limit))
                .transpose()?;
            let mask_softness = stage::nonnegative(args[5])?;
            let mask_invert = args[6]
                .as_bool()
                .ok_or(AdapterError::Input("mask_invert requires bool"))?;
            let mask_x = stage::mask_offset(args[7])?;
            let mask_y = stage::mask_offset(args[8])?;
            let owner = if args[9].tag() == ValueTag::Null {
                self.default_activity_owner(ticket.task)?
            } else {
                args[9]
            };
            scopes::validate_owner(self.vm.heap(), &self.stage, owner)?;
            if self.vm.callable_has_parameters(change)? {
                return Err(AdapterError::Input(
                    "transition change requires a zero-argument callback",
                ));
            }
            if self.transitions.live.iter().any(|entry| {
                !entry.state.finished && self.transition_targets_overlap(target, entry.target)
            }) {
                return Err(AdapterError::Input(
                    "transition target overlaps an active transition",
                ));
            }
            let serial = self
                .transitions
                .serial
                .checked_add(1)
                .ok_or(AdapterError::Input("transition identity exhausted"))?;
            results::reserve_queue(
                self.vm.heap(),
                &mut self.transitions.live,
                &mut self.transitions.charge,
            )?;
            let heap = self.vm.heap_mut();
            let state =
                heap.alloc_array(&[owner, target_value, change, Value::NULL, mask_value])?;
            let root = heap.add_root(self.root_owner, RootKind::Host, state)?;
            if let Err(error) = self.vm.respond(
                ticket,
                HostResponse::Continue(Continuation {
                    version: 1,
                    stage: 0,
                    values: &[state],
                    faults: FaultRetention::Preserve,
                }),
            ) {
                self.vm.heap_mut().release_root(root)?;
                return Err(error.into());
            }
            self.transitions.serial = serial;
            self.transitions.checked_lifetimes = None;
            self.transitions.live.push_back(Transition {
                state: SavedTransition {
                    serial,
                    task: ticket.task,
                    frame: ticket.frame,
                    root,
                    wait: None,
                    phase: if duration == 0. {
                        TransitionPhase::Changing
                    } else {
                        TransitionPhase::OldPending
                    },
                    duration,
                    easing: easing as i64,
                    elapsed: 0.,
                    last_time: self.activities.time(),
                    presentation_request: 0,
                    mask_softness,
                    mask_invert,
                    mask_x,
                    mask_y,
                    finished: false,
                },
                target,
                previous: None,
                saved: None,
                mask,
                claimed: false,
            });
            self.transitions.changed();
        } else if ticket.input == NativeInput::CallbackCancelled {
            let value = self.vm.heap_mut().alloc_string("TransitionCancelled")?;
            self.vm.respond(
                ticket,
                HostResponse::Error(aside_vm::NewFault {
                    value,
                    code: aside_vm::FaultCode::core(FaultKind::Cancelled),
                    cause: None,
                }),
            )?;
        } else {
            let index = self
                .transitions
                .live
                .iter()
                .position(|entry| {
                    entry.state.task == ticket.task && entry.state.frame == ticket.frame
                })
                .ok_or(AdapterError::Input("transition continuation is missing"))?;
            match ticket.input {
                NativeInput::CallbackFault | NativeInput::WaitingFault => {
                    let fault = self.vm.host_callback_fault(ticket)?.unwrap().id;
                    self.close_transition(index)?;
                    self.vm.respond(ticket, HostResponse::ForwardFault(fault))?;
                }
                NativeInput::CallbackReturned => {
                    let result = self.vm.host_callback_value(ticket)?;
                    let state = self
                        .vm
                        .heap()
                        .root_value(self.transitions.live[index].state.root)?;
                    self.vm.heap_mut().set_field(state, RETURNED, result)?;
                    let entry = &mut self.transitions.live[index];
                    entry.state.phase = if entry.state.duration == 0. {
                        TransitionPhase::FinalPending
                    } else {
                        TransitionPhase::NewPending
                    };
                    entry.state.wait = Some(ticket.wait_id());
                    self.vm
                        .respond(ticket, HostResponse::Pending(ticket.wait_id()))?;
                    self.transitions.changed();
                }
                NativeInput::WaitingReturned
                    if self.transitions.live[index].state.phase
                        == TransitionPhase::FinalPending =>
                {
                    let value = self.transition_value(index, RETURNED)?;
                    self.close_transition(index)?;
                    self.vm.respond(ticket, HostResponse::Ready(value))?;
                }
                NativeInput::Continue { .. } | NativeInput::WaitingReturned => {
                    let phase = self.transitions.live[index].state.phase;
                    if phase == TransitionPhase::OldPending
                        && self.transitions.live[index].previous.is_none()
                    {
                        let request = self.results.request_presentation()?;
                        let entry = &mut self.transitions.live[index];
                        entry.state.presentation_request = request;
                        entry.state.wait = Some(ticket.wait_id());
                        self.vm
                            .respond(ticket, HostResponse::Pending(ticket.wait_id()))?;
                    } else {
                        let closure = self.transition_value(index, CHANGE)?;
                        let value = self
                            .vm
                            .heap()
                            .root_value(self.transitions.live[index].state.root)?;
                        self.transitions.live[index].state.phase = TransitionPhase::Changing;
                        self.vm.respond(
                            ticket,
                            HostResponse::CallScript {
                                closure,
                                arguments: &[],
                                continuation: Continuation {
                                    version: 1,
                                    stage: 1,
                                    values: &[value],
                                    faults: FaultRetention::Preserve,
                                },
                            },
                        )?;
                    }
                    self.transitions.changed();
                }
                _ => return Err(AdapterError::Input("unexpected transition continuation")),
            }
        }
        self.issued = None;
        Ok(())
    }

    pub(super) fn reject_transition(&mut self, ticket: HostTicket) -> Result<(), AdapterError> {
        if let Some(index) =
            self.transitions.live.iter().position(|entry| {
                entry.state.task == ticket.task && entry.state.frame == ticket.frame
            })
        {
            self.close_transition(index)?;
        }
        Ok(())
    }

    pub(super) fn transition_snapshot_records(&self) -> snapshot::RecordStream<'_> {
        use snapshot::{Part, Record};
        Box::new(
            std::iter::once_with(|| {
                Record::new(
                    Part::Transitions,
                    &(self.transitions.serial, self.transitions.revision),
                )
            })
            .chain(
                self.transitions
                    .live
                    .iter()
                    .enumerate()
                    .map(|(index, entry)| {
                        Record::new(
                            Part::Transition,
                            &(
                                &entry.state,
                                (!entry.state.finished && entry.saved.is_some()).then_some(index),
                            ),
                        )
                    }),
            ),
        )
    }

    pub(super) fn transition_pictures(&self) -> Vec<Option<Arc<SavedRetainedScene>>> {
        self.transitions
            .live
            .iter()
            .map(|entry| {
                if entry.state.finished {
                    None
                } else {
                    entry.saved.clone()
                }
            })
            .collect()
    }

    pub(super) fn transition_picture_sizes(&self) -> impl Iterator<Item = u64> + '_ {
        std::iter::once(
            (self.transitions.live.len() * size_of::<Option<Arc<SavedRetainedScene>>>()) as u64,
        )
        .chain(self.transitions.live.iter().flat_map(|entry| {
            std::iter::once(0).chain(
                entry
                    .saved
                    .iter()
                    .filter(|_| !entry.state.finished)
                    .flat_map(|picture| picture.retained_size_steps()),
            )
        }))
    }

    pub(super) fn restore_transition_record(
        &mut self,
        record: &snapshot::Record,
        map: &aside_vm::snapshot::SnapshotMap,
        pictures: &[Option<Arc<SavedRetainedScene>>],
    ) -> Result<(), AdapterError> {
        use snapshot::Part;
        if matches!(record.part, Part::Transitions) {
            (self.transitions.serial, self.transitions.revision) = record.read()?;
            return Ok(());
        }
        let (mut state, picture): (SavedTransition, Option<usize>) = record.read()?;
        state.task = map.task(state.task)?;
        state.wait = state.wait.map(|wait| map.wait(wait)).transpose()?;
        state.root = map.root(state.root)?;
        let heap = self.vm.heap();
        let value = heap.root_value(state.root)?;
        let target_value = heap.sequence_get(value, TARGET)?;
        let target = if state.finished || target_value.tag() == ValueTag::Null {
            None
        } else {
            Some(self.stage.id(heap, target_value)?)
        };
        let mask_value = heap.sequence_get(value, MASK)?;
        let mask = (!state.finished && mask_value.tag() != ValueTag::Null)
            .then(|| masks::project(heap, mask_value, self.read_limit))
            .transpose()?;
        let saved = picture
            .map(|index| {
                pictures
                    .get(index)
                    .and_then(Option::as_ref)
                    .cloned()
                    .ok_or(AdapterError::Input("transition picture is missing"))
            })
            .transpose()?;
        results::reserve_queue(
            heap,
            &mut self.transitions.live,
            &mut self.transitions.charge,
        )?;
        self.transitions.live.push_back(Transition {
            state,
            target,
            previous: None,
            saved,
            mask,
            claimed: false,
        });
        Ok(())
    }
}
