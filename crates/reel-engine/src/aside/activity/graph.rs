//! One mutable execution node per description occurrence, never per repeat.
//! Admission is iterative and checks parallel ownership before any takeover.
use super::*;
use std::collections::HashMap;

#[derive(Clone, Copy)]
#[repr(u32)]
pub(super) enum Slot {
    Action,
    Parent,
    FirstChild,
    Sibling,
    Tracks,
    Phase,
    StartedAt,
    Activity,
    Iteration,
    Remaining,
    JobNext,
    JobKind,
    JobTime,
    Event,
    Index,
    Depth,
    ChildCount,
}

/// One record per animated property, including the actual reserved source.
#[derive(Clone, Copy)]
#[repr(u32)]
pub(super) enum Track {
    Field,
    Start,
    End,
    Version,
    SourceHost,
    SourceField,
}
impl Track {
    pub const LEN: usize = 6;
}
pub(super) fn track(
    heap: &Heap,
    tracks: Value,
    index: u32,
    slot: Track,
) -> Result<Value, AdapterError> {
    Ok(heap.sequence_get(tracks, index + slot as u32)?)
}
pub(super) fn track_source(
    heap: &Heap,
    tracks: Value,
    index: u32,
) -> Result<Option<PropertyId>, AdapterError> {
    let host = track(heap, tracks, index, Track::SourceHost)?;
    if host.tag() == ValueTag::Null {
        return Ok(None);
    }
    Ok(Some(PropertyId {
        host,
        field: track(heap, tracks, index, Track::SourceField)?
            .as_int()
            .unwrap() as u32,
    }))
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub(super) enum Phase {
    Inactive,
    Starting,
    Active,
    Done,
}
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub(super) enum Job {
    Activate,
    Complete,
}

pub(super) fn phase(heap: &Heap, node: Value) -> Result<Phase, AdapterError> {
    Ok(match int(heap, node, Slot::Phase)? {
        0 => Phase::Inactive,
        1 => Phase::Starting,
        2 => Phase::Active,
        3 => Phase::Done,
        _ => unreachable!("private execution node schema"),
    })
}

pub(super) fn get(heap: &Heap, node: Value, field: Slot) -> Result<Value, AdapterError> {
    Ok(heap.sequence_get(node, field as u32)?)
}
pub(super) fn set(
    heap: &mut Heap,
    node: Value,
    field: Slot,
    value: Value,
) -> Result<(), AdapterError> {
    Ok(heap.set_field(node, field as u32, value)?)
}
pub(super) fn int(heap: &Heap, node: Value, field: Slot) -> Result<i64, AdapterError> {
    Ok(get(heap, node, field)?.as_int().unwrap())
}
pub(super) fn node(heap: &Heap, activity: Value, index: i64) -> Result<Value, AdapterError> {
    Ok(heap.sequence_get(root_get(heap, activity, RootSlot::Graph)?, index as u32)?)
}
pub(super) fn kind(heap: &Heap, node: Value) -> Result<action::Kind, AdapterError> {
    action::kind(heap, get(heap, node, Slot::Action)?)
}

struct Branch {
    index: u32,
    next: u32,
    previous: i64,
}
struct Conflict {
    leaf: u32,
    field: u32,
    left: Option<u32>,
    right: u32,
}

pub(super) struct Build {
    pub activity: Value,
    branches: Vec<Branch>,
    writers: HashMap<(aside_vm::HeapHandle, u32), u32>,
    conflict: Option<Conflict>,
    started: bool,
}

impl Build {
    pub fn new(activity: Value) -> Self {
        Self {
            activity,
            branches: Vec::new(),
            writers: HashMap::new(),
            conflict: None,
            started: false,
        }
    }

    fn append(
        &mut self,
        heap: &mut Heap,
        stage: &StageBinding,
        action: Value,
        parent: i64,
    ) -> Result<u32, AdapterError> {
        tagged(heap, action, ACTION)?;
        let graph = root_get(heap, self.activity, RootSlot::Graph)?;
        let index = heap.sequence_len(graph)?;
        let depth = if parent < 0 {
            0
        } else {
            int(heap, node(heap, self.activity, parent)?, Slot::Depth)? + 1
        };
        let tracks = heap.alloc_array(&[])?;
        let mut slots = [Value::NULL; Slot::ChildCount as usize + 1];
        slots[Slot::Action as usize] = action;
        slots[Slot::Parent as usize] = Value::int(parent);
        slots[Slot::FirstChild as usize] = Value::int(-1);
        slots[Slot::Sibling as usize] = Value::int(-1);
        slots[Slot::Tracks as usize] = tracks;
        slots[Slot::Phase as usize] = Value::int(Phase::Inactive as i64);
        slots[Slot::Activity as usize] = self.activity;
        slots[Slot::Index as usize] = Value::int(i64::from(index));
        slots[Slot::Depth as usize] = Value::int(depth);
        slots[Slot::ChildCount as usize] = Value::int(0);
        let current = heap.alloc_array(&slots)?;
        heap.array_push(graph, current)?;
        if kind(heap, current)? == action::Kind::Tween {
            let target = payload(heap, action, TARGET)?;
            stage.id(heap, target)?;
            heap.track_host_writes(target)?;
            let goals = payload(heap, action, GOALS)?;
            for i in (0..heap.sequence_len(goals)?).step_by(2) {
                let field = heap.sequence_get(goals, i)?;
                let end = Value::float(stage::number(heap.sequence_get(goals, i + 1)?)?);
                for value in [
                    field,
                    Value::NULL,
                    end,
                    Value::int(0),
                    Value::NULL,
                    Value::NULL,
                ] {
                    heap.array_push(tracks, value)?;
                }
            }
            self.conflict = Some(Conflict {
                leaf: index,
                field: 0,
                left: None,
                right: index,
            });
        } else if !(kind(heap, current)? == action::Kind::Repeat
            && payload(heap, action, action::TIMES)?.as_int() == Some(0))
        {
            self.branches.push(Branch {
                index,
                next: 0,
                previous: -1,
            });
        }
        Ok(index)
    }

    /// Preorder means the previous writer of a property is sufficient: if two
    /// parallel branches write it, their boundary has an adjacent writer pair.
    /// Walking to that pair's LCA costs one ancestor per work unit.
    fn check_writer(&mut self, heap: &Heap) -> Result<(), AdapterError> {
        let conflict = self.conflict.as_mut().unwrap();
        let leaf = node(heap, self.activity, i64::from(conflict.leaf))?;
        let tracks = get(heap, leaf, Slot::Tracks)?;
        if conflict.field == heap.sequence_len(tracks)? {
            self.conflict = None;
            return Ok(());
        }
        if let Some(left) = conflict.left {
            let a = node(heap, self.activity, i64::from(left))?;
            let b = node(heap, self.activity, i64::from(conflict.right))?;
            if left == conflict.right {
                if matches!(kind(heap, a)?, action::Kind::Parallel | action::Kind::Tween) {
                    return Err(AdapterError::Input(
                        "parallel branches write the same property",
                    ));
                }
                conflict.left = None;
                conflict.field += Track::LEN as u32;
            } else if int(heap, a, Slot::Depth)? >= int(heap, b, Slot::Depth)? {
                conflict.left = Some(int(heap, a, Slot::Parent)? as u32);
            } else {
                conflict.right = int(heap, b, Slot::Parent)? as u32;
            }
        } else {
            let field = heap.sequence_get(tracks, conflict.field)?.as_int().unwrap() as u32;
            let target = payload(heap, get(heap, leaf, Slot::Action)?, TARGET)?;
            let (source, writable) = heap.property_source(PropertyId {
                host: target,
                field,
            })?;
            if !writable {
                return Err(HeapError::ReadOnly.into());
            }
            conflict.left = self
                .writers
                .insert((heap.handle(source.host)?, source.field), conflict.leaf);
            conflict.right = conflict.leaf;
            if conflict.left.is_none() {
                conflict.field += Track::LEN as u32;
            }
        }
        Ok(())
    }

    pub fn step(
        &mut self,
        heap: &mut Heap,
        stage: &StageBinding,
        budget: u32,
    ) -> Result<(bool, u32), AdapterError> {
        let mut work = 0;
        while work < budget {
            work += 1;
            if self.conflict.is_some() {
                self.check_writer(heap)?;
            } else if !self.started {
                self.append(
                    heap,
                    stage,
                    root_get(heap, self.activity, RootSlot::Description)?,
                    -1,
                )?;
                self.started = true;
            } else if let Some(branch) = self.branches.last_mut() {
                let parent = node(heap, self.activity, i64::from(branch.index))?;
                let children = payload(heap, get(heap, parent, Slot::Action)?, GOALS)?;
                if branch.next == heap.sequence_len(children)? {
                    self.branches.pop();
                    continue;
                }
                let action = heap.sequence_get(children, branch.next)?;
                let previous = branch.previous;
                let parent_index = branch.index;
                branch.next += 1;
                let index = heap.sequence_len(root_get(heap, self.activity, RootSlot::Graph)?)?;
                branch.previous = i64::from(index);
                self.append(heap, stage, action, i64::from(parent_index))?;
                if previous < 0 {
                    set(heap, parent, Slot::FirstChild, Value::int(i64::from(index)))?;
                } else {
                    let previous = node(heap, self.activity, previous)?;
                    set(heap, previous, Slot::Sibling, Value::int(i64::from(index)))?;
                }
                let count = int(heap, parent, Slot::ChildCount)? + 1;
                set(heap, parent, Slot::ChildCount, Value::int(count))?;
            } else {
                return Ok((true, work));
            }
        }
        Ok((
            self.started && self.branches.is_empty() && self.conflict.is_none(),
            work,
        ))
    }
}
