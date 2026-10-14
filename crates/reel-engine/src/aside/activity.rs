//! Reusable descriptions and traced execution graphs share the VM heap.
//! Rust keeps only scheduling indexes and bounded traversal cursors.
pub(super) mod action;
mod graph;
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
fn readonly(_: &Heap, _: Value, _: u32, _: Value) -> Result<(), HeapError> {
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
#[derive(Default)]
pub(super) struct Activities {
    skipping: bool,
    head: Option<RootToken>,
    jobs: Option<RootToken>,
    job_tail: Option<Value>,
    // Every indexed leaf is also traced through the active root's graph.
    events: BTreeMap<EventKey, Value>,
    native: Option<NativeWork>,
    cleanup: Option<Cleanup>,
    control: Option<Control>,
    round: Option<Round>,
    clock_round: bool,
    time: f64,
    observed_time: f64,
    target_time: f64,
    serial: u64,
    running: usize,
    waits: Vec<(WaitId, Value)>,
}
