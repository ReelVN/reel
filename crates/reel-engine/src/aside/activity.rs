//! Reusable descriptions and traced execution graphs share the VM heap.
//! Rust keeps only scheduling indexes and bounded traversal cursors.
pub(super) mod action;

pub(super) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint;
    use aside_host::source::{Member, NamedType, named, number};
    let action = named("reel::types::Action");
    let activity = named("reel::types::Activity");
    for symbol in ["tween", "delay", "sequence", "parallel", "repeat"] {
        api.refine(&format!("reel::{symbol}"), action.clone(), &[]);
    }
    api.refine(
        "reel::tween",
        action.clone(),
        &[
            (
                "target",
                TypeHint::Union(vec![
                    named("reel::types::Node"),
                    named("reel::types::Override"),
                ]),
            ),
            ("duration", number()),
        ],
    );
    api.refine("reel::delay", action.clone(), &[("seconds", number())]);
    {
        use aside_host::PropertySchema;
        let fields = super::stage::NodeProperty::PROPERTIES
            .iter()
            .chain(
                super::stage::animation_prefixes()
                    .into_iter()
                    .flat_map(|(_, fields)| fields),
            )
            .filter(|p| p.animatable)
            .map(|p| (p.name, aside_host::source::nullable(p.source_type())))
            .collect::<Vec<_>>();
        api.refine("reel::tween", action.clone(), &fields);
    }
    api.refine(
        "reel::repeat",
        action.clone(),
        &[
            ("action", action.clone()),
            ("times", aside_host::source::nullable(TypeHint::Int)),
        ],
    );
    api.refine(
        "reel::run",
        activity.clone(),
        &[
            ("action", action.clone()),
            (
                "owner",
                aside_host::source::nullable(TypeHint::Union(vec![
                    named("reel::types::Node"),
                    named("reel::types::Scope"),
                ])),
            ),
            ("paused", TypeHint::Bool),
        ],
    );
    for symbol in ["then", "together"] {
        api.refine(
            &format!("reel::action::{symbol}"),
            action.clone(),
            &[("action", action.clone()), ("other", action.clone())],
        );
    }
    api.refine(
        "reel::clock",
        named("reel::types::Clock"),
        &[("interval", number())],
    );
    api.refine(
        "reel::clock::next",
        TypeHint::Int,
        &[("clock", named("reel::types::Clock"))],
    );
    let producer = TypeHint::Union(vec![
        activity,
        named("reel::types::Playback"),
        named("reel::types::ParticleActivity"),
    ]);
    for symbol in ["pause", "resume", "stop", "finish"] {
        api.refine(
            &format!("reel::{symbol}"),
            producer.clone(),
            &[("activity", producer.clone())],
        );
        api.native_mut(&format!("reel::{symbol}"))
            .returns_parameter("activity");
    }
    api.types.push(NamedType::new(
        "reel::types::Action",
        "An immutable action description. Sequence, parallel and repeat compose actions; run starts an independent activity.",
        [
            ("then", "reel::action::then"),
            ("together", "reel::action::together"),
            ("repeat", "reel::repeat"),
            ("run", "reel::run"),
        ]
        .into_iter()
        .map(|(name, symbol)| Member::method(api.native(symbol).clone().bound(name)))
        .collect(),
    ));
    let mut base = NamedType::new(
        "reel::types::Activity",
        "An awaitable execution of an action. Its controls preserve the activity's identity.",
        Vec::new(),
    );
    base.await_result = Some(TypeHint::String);
    base.await_adapter = Some(api.native("reel::activity::await").clone());
    api.types.push(base.clone());
    api.types.push(
        NamedType::new(
            "reel::types::Clock",
            "A periodic activity with observable accepted ticks and elapsed seconds.",
            vec![
                Member::method(api.native("reel::clock::next").clone().bound("next")),
                Member::field(
                    "reel::types::Clock",
                    "ticks",
                    TypeHint::Int,
                    None,
                    "Complete intervals elapsed while running.",
                    Some(aside_vm::bytecode::Const::Int(0)),
                ),
                Member::field(
                    "reel::types::Clock",
                    "elapsed",
                    TypeHint::Float,
                    None,
                    "Elapsed activity time in seconds.",
                    Some(aside_vm::bytecode::Const::FloatBits(0f64.to_bits())),
                ),
            ],
        )
        .extends(&base),
    );
}

/// Publish the same controls for every producer that implements the activity protocol.
pub(super) fn describe_controls(api: &mut aside_host::source::Api) {
    use aside_host::source::{Member, named};
    for type_name in ["Activity", "Clock", "Playback", "ParticleActivity"] {
        let owner = format!("reel::types::{type_name}");
        let members = ["pause", "resume", "stop", "finish"]
            .into_iter()
            .map(|name| {
                Member::method(
                    api.native(&format!("reel::{name}"))
                        .clone()
                        .bound(name)
                        .types(named(&owner), &[]),
                )
            })
            .collect::<Vec<_>>();
        api.types
            .iter_mut()
            .find(|ty| ty.name == owner)
            .expect("registered activity producer")
            .members
            .extend(members);
    }
}

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Describe numeric changes on a stage node. `duration` is nonnegative seconds;
        /// `from` can supply starting values. Omitted properties remain unchanged.
        /// Easing accepts a named curve or `std::math::curve` value. `run` starts the action.
        "reel::tween";
        /// Describe a wait of nonnegative seconds. Compose it with `sequence` or
        /// `parallel`, then start it with `run`.
        "reel::delay";
        /// Describe actions played in argument order. The returned description can be
        /// started repeatedly; each run owns its execution state.
        "reel::sequence";
        /// Describe actions played together, completing when all children complete.
        "reel::parallel";
        /// Follow this action with `other`. Both descriptions remain reusable;
        /// the returned action shares them without copying their contents.
        "reel::action::then";
        /// Run this action and `other` together, completing when both finish.
        /// Each chained operation combines the complete preceding expression.
        "reel::action::together";
        /// Describe repeated execution of an action. `times: null` repeats indefinitely;
        /// a nonnegative integer count finishes after that many iterations.
        "reel::repeat";
        /// Start an action and return its awaitable activity. `paused` starts with its
        /// clock frozen. Default ownership follows the active scope, then the current
        /// task owner. Await returns Completed or Stopped; retirement cancels the activity.
        "reel::run";
        /// Start a periodic activity with observable integer ticks and elapsed seconds.
        /// Publish the latest complete interval when time advances. Pause freezes time;
        /// stop settles the activity. A clock has no finite endpoint for `finish`.
        "reel::clock";
        /// Wait for this clock to advance beyond its current complete interval and
        /// return its total integer ticks. Ordinary calls suspend the current task;
        /// no separate awaitable or condition is allocated per tick. Each consumer
        /// waits independently. Missed intervals are combined in the returned count.
        /// Pause retains the wait; stopping or retiring the clock cancels it.
        "reel::clock::next";
        /// Wait for the activity's terminal result. Multiple consumers share the result;
        /// cancelling a consumer removes its wait without stopping the producer.
        "reel::activity::await";
        /// Pause the activity at its current progress. Its owner and saved state remain
        /// active; `resume` continues from that progress.
        "reel::pause";
        /// Resume a paused activity from its saved progress.
        "reel::resume";
        /// Stop the activity at its current progress and settle its waiters as Stopped.
        "reel::stop";
        /// Apply the finite activity's endpoint and settle it as Completed. Indefinite
        /// clocks, repetitions and continuous emissions have no endpoint.
        "reel::finish";
    }
}
pub(super) mod easing;
mod graph;
pub(in crate::aside) mod resources;
mod runner;
mod service;
use super::{
    AdapterError, FaultKind, HostTicket,
    stage::{self, NodeProperty, StageBinding},
};
use aside_host::PropertySchema;
use aside_vm::executor::{Completion, HostResponse, TaskId, Vm, WaitId};
use aside_vm::{Heap, HeapError, PropertyId, RootKind, RootOwner, RootToken, Value, ValueTag};
use std::collections::BTreeMap;

const ACTION: i64 = -1001;
const ACTIVITY: i64 = -1002;
// Immutable description and stage-node schemas.
const TARGET: u32 = 1;
const DURATION: u32 = 2;
const EASING: u32 = 3;
const GOALS: u32 = 4;

#[derive(Clone, Copy)]
#[repr(u32)]
enum RootSlot {
    Tag,
    State,
    Description,
    Owner,
    Graph,
    Next,
    Previous,
    Results,
    Serial,
    PausedAt,
    Forcing,
}
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
enum State {
    Running,
    Paused,
    Cleaning,
    Completed,
    Stopped,
    Cancelled,
    Failed,
}

fn state(heap: &Heap, activity: Value) -> Result<State, AdapterError> {
    Ok(
        match root_get(heap, activity, RootSlot::State)?.as_int().unwrap() {
            0 => State::Running,
            1 => State::Paused,
            2 => State::Cleaning,
            3 => State::Completed,
            4 => State::Stopped,
            5 => State::Cancelled,
            6 => State::Failed,
            _ => unreachable!("private activity schema"),
        },
    )
}
fn terminal(status: State) -> bool {
    matches!(
        status,
        State::Completed | State::Stopped | State::Cancelled | State::Failed
    )
}
fn root_get(heap: &Heap, activity: Value, slot: RootSlot) -> Result<Value, AdapterError> {
    payload(heap, activity, slot as u32)
}
fn root_set(
    heap: &mut Heap,
    activity: Value,
    slot: RootSlot,
    value: Value,
) -> Result<(), AdapterError> {
    Ok(heap.set_host_payload(activity, slot as u32, value)?)
}
fn payload(heap: &Heap, value: Value, slot: u32) -> Result<Value, AdapterError> {
    Ok(heap.host_payload(value, slot)?)
}
fn tagged(heap: &Heap, value: Value, tag: i64) -> Result<(), AdapterError> {
    heap.host_fields(value)?;
    if heap.host_payload(value, 0)?.as_int() != Some(tag) {
        return Err(AdapterError::Input("expected action or activity"));
    }
    Ok(())
}
pub(super) static READONLY: aside_vm::HostPropertyValidator =
    aside_vm::HostPropertyValidator::new("reel.activity", 1, readonly);

pub(super) fn readonly(
    _: &aside_vm::PropertyRead<'_>,
    _: Value,
    _: u32,
    _: Value,
) -> Result<(), HeapError> {
    Err(HeapError::TypeMismatch)
}
fn present(value: Value) -> bool {
    value.tag() != ValueTag::Null
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActivityProgress {
    pub work: u32,
    pub pending: bool,
    pub woke: bool,
}

#[derive(Clone, Copy)]
struct Scan {
    activity: Value,
    index: u32,
}
#[derive(Clone, Copy)]
enum Round {
    Reconcile(Option<Scan>),
    Events,
    Sample(Option<Scan>),
}

struct Cleanup {
    activity: Value,
    index: u32,
    result: State,
    freeze: Option<f64>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Command {
    Pause,
    Resume,
    Stop,
    Finish,
    Await,
}
struct Control {
    activity: Value,
    command: Command,
    index: u32,
    shift: f64,
}
struct NativeWork {
    task: TaskId,
    root: RootToken,
    build: Option<graph::Build>,
    command: Option<Command>,
}

type EventKey = (u64, u64, u32);
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Activities {
    #[serde(skip)]
    logical: resources::Restoring,
    skipping: bool,
    head: Option<RootToken>,
    jobs: Option<RootToken>,
    job_tail: Option<Value>,
    // Every indexed leaf is also traced through the active root's graph.
    #[serde(skip)]
    events: BTreeMap<EventKey, Value>,
    sampling: usize,
    #[serde(skip)]
    clock_events: usize,
    #[serde(skip)]
    deadline_cache: std::cell::Cell<Option<(u64, Option<f64>)>>,
    #[serde(skip)]
    native: Option<NativeWork>,
    #[serde(skip)]
    cleanup: Option<Cleanup>,
    #[serde(skip)]
    control: Option<Control>,
    #[serde(skip)]
    round: Option<Round>,
    clock_round: bool,
    time: f64,
    observed_time: f64,
    target_time: f64,
    serial: u64,
    #[serde(skip)]
    waits: Vec<(WaitId, Value, Option<i64>)>,
    #[serde(skip)]
    waits_dirty: bool,
    #[serde(skip)]
    checked_wait_revision: u64,
    #[cfg(test)]
    #[serde(skip)]
    pub wait_checks: u64,
}

impl Activities {
    pub(super) fn time(&self) -> f64 {
        self.time
    }
    pub(super) fn snapshot_records(&self) -> super::snapshot::RecordStream<'_> {
        use super::snapshot::{Part, Record};
        Box::new(
            std::iter::once_with(|| Record::new(Part::Activities, self))
                .chain(
                    self.events
                        .iter()
                        .map(|event| Record::new(Part::ActivityEvent, &event)),
                )
                .chain(
                    self.waits
                        .iter()
                        .map(|wait| Record::new(Part::ActivityWait, wait)),
                ),
        )
    }
    pub(super) fn restore_record(
        &mut self,
        record: &super::snapshot::Record,
        map: &aside_vm::snapshot::SnapshotMap,
        heap: &Heap,
    ) -> Result<(), AdapterError> {
        use super::snapshot::Part;
        match record.part {
            Part::Activities => {
                *self = record.read()?;
                self.restore(map)?;
            }
            Part::ActivityEvent => {
                self.deadline_cache.set(None);
                let (key, value): (EventKey, Value) = record.read()?;
                let value = map.value(value)?;
                self.clock_events += usize::from(graph::kind(heap, value)? == action::Kind::Clock);
                if let Some(previous) = self.events.insert(key, value) {
                    self.clock_events -=
                        usize::from(graph::kind(heap, previous)? == action::Kind::Clock);
                }
            }
            Part::ActivityWait => {
                self.deadline_cache.set(None);
                let (wait, value, tick): (WaitId, Value, Option<i64>) = record.read()?;
                self.waits.push((map.wait(wait)?, map.value(value)?, tick));
                self.waits_dirty = true;
            }
            _ => unreachable!(),
        }
        Ok(())
    }
    pub(super) fn snapshot_ready(&self) -> bool {
        self.native.is_none()
            && self.cleanup.is_none()
            && self.control.is_none()
            && self.round.is_none()
    }
    pub(super) fn restore(
        &mut self,
        map: &aside_vm::snapshot::SnapshotMap,
    ) -> Result<(), aside_vm::snapshot::SnapshotError> {
        self.head = self.head.map(|r| map.root(r)).transpose()?;
        self.jobs = self.jobs.map(|r| map.root(r)).transpose()?;
        self.job_tail = self.job_tail.map(|v| map.value(v)).transpose()?;
        self.skipping = false;
        Ok(())
    }
}
