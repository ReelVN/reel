//! Sprite emission producers reuse the common stage clock and result protocol.
use super::{AdapterError, HostTicket, SessionVm, results, stage, text};
use aside_vm::executor::HostResponse;
use aside_vm::{Heap, HeapError, HeapReservation, RootKind, RootToken, Value, ValueTag};
use reel_ui::stage::{
    NodeId, NodeKind,
    particles::{ParticleBatch, ParticleConfig, ParticleFrame},
};
use std::{collections::VecDeque, sync::Arc};

pub(super) const PREFIX: &[&str] = &[
    "source",
    "rate",
    "lifetime",
    "seed",
    "velocity_x",
    "velocity_y",
    "spread_x",
    "spread_y",
    "acceleration_x",
    "acceleration_y",
    "size",
    "size_spread",
    "opacity_start",
    "opacity_end",
    "spin",
    "spin_spread",
    "burst",
    "lifetime_spread",
    "size_end",
];
pub(super) fn parameters() -> Vec<aside_host::NativeParameter> {
    use aside_vm::bytecode::{BuiltinType, Const};
    let defaults: [f64; 19] = [
        0., 60., 5., 0., 0., 80., 20., 20., 0., 0., 12., 4., 1., 0., 0., 30., 0., 0., 0.,
    ];
    PREFIX
        .iter()
        .enumerate()
        .map(|(i, name)| aside_host::NativeParameter {
            name: (*name).into(),
            ty: BuiltinType::Any,
            default: if i == 0 {
                None
            } else if i == 3 || i == 16 {
                Some(Const::Int(0))
            } else if i == 18 {
                Some(Const::Null)
            } else {
                Some(Const::FloatBits(defaults[i].to_bits()))
            },
        })
        .collect()
}
pub(super) fn kind(
    heap: &Heap,
    args: &[Value],
    origin: Option<String>,
    limit: usize,
) -> Result<NodeKind, AdapterError> {
    let n = |i| stage::number(args[i]);
    let source = text::read_string(heap, args[0], limit)?;
    if source.is_empty() {
        return Err(AdapterError::Input("particle source must not be empty"));
    }
    let config = ParticleConfig {
        source,
        origin,
        rate: n(1)?,
        burst: args[16]
            .as_int()
            .filter(|count| *count >= 0)
            .ok_or(AdapterError::Input(
                "particle burst must be a nonnegative int",
            ))? as u64,
        lifetime: n(2)?,
        lifetime_spread: n(17)?,
        seed: args[3]
            .as_int()
            .ok_or(AdapterError::Input("particle seed must be int"))? as u64,
        velocity: [n(4)?, n(5)?],
        spread: [n(6)?, n(7)?],
        acceleration: [n(8)?, n(9)?],
        size: n(10)?,
        size_spread: n(11)?,
        size_end: if args[18].tag() == ValueTag::Null {
            None
        } else {
            Some(n(18)?)
        },
        opacity: [n(12)?, n(13)?],
        spin: n(14)?,
        spin_spread: n(15)?,
    };
    if config.rate < 0.
        || config.lifetime <= 0.
        || config.lifetime_spread < 0.
        || config.lifetime_spread > config.lifetime
        || !config.max_lifetime().is_finite()
        || config.spread.iter().any(|v| *v < 0.)
        || config.size < 0.
        || config.size_spread < 0.
        || config.size_spread > config.size
        || config.size_end.is_some_and(|size| size < 0.)
        || config.spin_spread < 0.
        || config.opacity.iter().any(|v| !(0. ..=1.).contains(v))
        || !(config.rate * config.max_lifetime()).is_finite()
        || config.rate * config.max_lifetime() + config.burst as f64 >= u64::MAX as f64
    {
        return Err(AdapterError::Input(
            "invalid particle rate, lifetime, spread, size or opacity",
        ));
    }
    Ok(NodeKind::Particles(ParticleBatch {
        config: Arc::new(config),
        frame: None,
    }))
}
pub(super) fn validate(heap: &Heap, _: Value, index: u32, value: Value) -> Result<(), HeapError> {
    if index < PREFIX.len() as u32 {
        return Err(HeapError::ReadOnly);
    }
    stage::validate_layout_property(heap, index - PREFIX.len() as u32, value)
}

/// Exact finite end time, found backwards without retaining individual particles.
/// Earlier births cannot improve `end` once even their maximum lifetime ends before it.
#[derive(serde::Serialize, serde::Deserialize)]
struct Completion {
    end: f64,
    next: u64,
}
impl Completion {
    fn new(config: &ParticleConfig, duration: f64) -> Self {
        let last = config.last_index(duration);
        if last == 0 {
            Self { end: 0., next: 0 }
        } else if config.lifetime_spread == 0. {
            Self {
                end: config.birth_time(last) + config.lifetime,
                next: 0,
            }
        } else {
            Self {
                end: 0.,
                next: last,
            }
        }
    }
    fn advance(&mut self, config: &ParticleConfig, budget: u32) -> u32 {
        let mut work = 0;
        while self.next != 0 && work < budget {
            work += 1;
            let birth = config.birth_time(self.next);
            if birth + config.max_lifetime() <= self.end {
                self.next = 0;
                break;
            }
            self.end = self.end.max(birth + config.lifetime_at(self.next));
            self.next -= 1;
        }
        work
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Live {
    root: RootToken,
    target: NodeId,
    owner: Option<NodeId>,
    frame: ParticleFrame,
    last_time: f64,
    paused: bool,
    completion: Option<Completion>,
}
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Particles {
    #[serde(skip)]
    live: VecDeque<Live>,
    #[serde(skip)]
    charge: Option<HeapReservation>,
    time: f64,
    remaining: usize,
    revision: u64,
    woke: bool,
}
impl Particles {
    pub(super) fn snapshot_records(&self) -> super::snapshot::RecordStream<'_> {
        use super::snapshot::{Part, Record};
        Box::new(
            std::iter::once_with(|| Record::new(Part::Particles, self)).chain(
                self.live
                    .iter()
                    .map(|live| Record::new(Part::Particle, live)),
            ),
        )
    }
    pub(super) fn restore_record(
        &mut self,
        record: &super::snapshot::Record,
        map: &aside_vm::snapshot::SnapshotMap,
        heap: &Heap,
        stage: &mut reel_ui::stage::Stage,
    ) -> Result<(), AdapterError> {
        use super::snapshot::Part;
        match record.part {
            Part::Particles => {
                *self = record.read()?;
            }
            Part::Particle => {
                let mut live: Live = record.read()?;
                live.root = map.root(live.root)?;
                stage.set_particle_frame(live.target, Some(live.frame))?;
                results::reserve_queue(heap, &mut self.live, &mut self.charge)?;
                self.live.push_back(live);
            }
            _ => unreachable!(),
        }
        self.revision = stage.revision();
        Ok(())
    }
    pub fn has_active(&self) -> bool {
        !self.live.is_empty()
    }
    pub fn take_wake(&mut self) -> bool {
        std::mem::take(&mut self.woke)
    }
    pub fn needs_time(&self) -> bool {
        self.live.iter().any(|live| !live.paused)
    }
    pub fn needs_work(&self, revision: u64) -> bool {
        self.remaining != 0 || (!self.live.is_empty() && self.revision != revision)
    }
    pub fn begin(&mut self, time: f64) -> Result<(), AdapterError> {
        if !time.is_finite() || time < self.time {
            return Err(AdapterError::Input(
                "activity clock must be finite and monotonic",
            ));
        }
        if self.remaining == 0 && time != self.time {
            self.time = time;
            self.remaining = self.live.len();
        }
        Ok(())
    }
    pub fn clear(&mut self) {
        // Session owner-root retirement releases the producer roots.
        self.live = VecDeque::new();
        self.charge = None;
        self.remaining = 0;
    }
}

impl SessionVm {
    pub(super) fn maintain_particles(&mut self, budget: u32) -> Result<u32, AdapterError> {
        if !self.particles.needs_work(self.stage.scene.revision()) {
            return Ok(0);
        }
        if self.particles.remaining == 0 {
            self.particles.remaining = self.particles.live.len();
        }
        let mut work = 0;
        while work < budget && self.particles.remaining != 0 {
            let live = self.particles.live.front_mut().unwrap();
            let result = self.vm.heap().root_value(live.root)?;
            let node = self.stage.scene.node(live.target);
            let cancelled = node.is_none()
                || live
                    .owner
                    .is_some_and(|id| self.stage.scene.node(id).is_none());
            let mut terminal = cancelled.then_some(results::TaskResult::Cancelled);
            let mut pending_completion = false;
            if !cancelled {
                if !live.paused {
                    live.frame.elapsed += self.particles.time - live.last_time;
                }
                live.last_time = self.particles.time;
                let NodeKind::Particles(batch) = &node.unwrap().kind else {
                    unreachable!()
                };
                if let Some(completion) = &mut live.completion {
                    work += completion.advance(&batch.config, budget - work).max(1);
                    pending_completion = completion.next != 0;
                    if !pending_completion && live.frame.elapsed >= completion.end {
                        terminal = Some(results::TaskResult::Returned(
                            self.vm.heap_mut().alloc_string("Completed")?,
                        ));
                    }
                } else {
                    work += 1;
                    if live.frame.elapsed * batch.config.rate + batch.config.burst as f64
                        >= u64::MAX as f64
                    {
                        terminal = Some(results::TaskResult::Rejected(
                            self.vm
                                .heap_mut()
                                .alloc_string("particle birth index exceeds representable range")?,
                        ));
                    }
                }
            } else {
                work += 1;
            }
            if let Some(terminal) = terminal {
                if self.stage.scene.node(live.target).is_some() {
                    self.stage.scene.set_particle_frame(live.target, None)?;
                }
                results::finish_task(self.vm.heap_mut(), result, terminal)?;
                self.vm.heap_mut().release_root(live.root)?;
                self.results.changed();
                self.particles.woke = true;
                self.particles.live.pop_front();
            } else {
                self.stage
                    .scene
                    .set_particle_frame(live.target, Some(live.frame))?;
                if pending_completion {
                    // Retain this producer at the pending prefix until its
                    // bounded completion scan has drained. No clock advance
                    // or per-particle allocation is needed between slices.
                    break;
                }
                let live = self.particles.live.pop_front().unwrap();
                self.particles.live.push_back(live);
            }
            self.particles.remaining -= 1;
        }
        if self.particles.remaining == 0 {
            self.particles.revision = self.stage.scene.revision();
        }
        if self.particles.live.is_empty() {
            self.particles.clear();
        }
        Ok(work)
    }

    pub(super) fn dispatch_emit(&mut self, ticket: HostTicket) -> Result<(), AdapterError> {
        let args = self.vm.host_arguments(ticket)?;
        let target = args[0];
        let id = self.stage.id(self.vm.heap(), target)?;
        let duration = if args[1].tag() == ValueTag::Null {
            None
        } else {
            let duration = stage::number(args[1])?;
            if duration < 0. {
                return Err(AdapterError::Input("emission duration must be nonnegative"));
            }
            Some(duration)
        };
        let owner = if args[2].tag() == ValueTag::Null {
            self.default_activity_owner(ticket.task)?
        } else {
            args[2]
        };
        let owner_value = owner;
        let owner = if owner.tag() == ValueTag::Null {
            None
        } else {
            Some(self.stage.id(self.vm.heap(), owner)?)
        };
        let node = self.stage.scene.node(id).unwrap();
        let NodeKind::Particles(batch) = &node.kind else {
            return Err(AdapterError::Input("emit requires a particles node"));
        };
        let duration = if batch.config.rate == 0. {
            Some(0.)
        } else {
            duration
        };
        if duration.is_some_and(|d| {
            !(d + batch.config.max_lifetime()).is_finite()
                || d * batch.config.rate + batch.config.burst as f64 >= u64::MAX as f64
        }) {
            return Err(AdapterError::Input(
                "emission duration exceeds representable range",
            ));
        }
        let completion = duration.map(|duration| Completion::new(&batch.config, duration));
        // Read current authored dimensions, including writes since the last projection.
        let offset = self.stage.property_offset(self.vm.heap(), target)?;
        let dimension = |field: stage::NodeProperty| -> Result<f64, AdapterError> {
            let value = self.vm.heap().host_property(aside_vm::PropertyId {
                host: target,
                field: offset + field.index() as u32,
            })?;
            if value.tag() == ValueTag::Null {
                Ok(0.)
            } else {
                stage::number(value)
            }
        };
        let extent = [
            dimension(stage::NodeProperty::Width)?,
            dimension(stage::NodeProperty::Height)?,
        ];
        results::reserve_queue(
            self.vm.heap(),
            &mut self.particles.live,
            &mut self.particles.charge,
        )?;
        let completed = if completion
            .as_ref()
            .is_some_and(|end| end.next == 0 && end.end == 0.)
        {
            Some(self.vm.heap_mut().alloc_string("Completed")?)
        } else {
            None
        };
        let result = self
            .results
            .create_particle(&mut self.vm, owner_value, target)?;
        let root = self
            .vm
            .heap_mut()
            .add_root(self.root_owner, RootKind::Host, result)?;
        if let Some(index) = self
            .particles
            .live
            .iter()
            .position(|live| live.target == id)
            && let Err(error) = self.end_emission(index, "Stopped")
        {
            self.vm.heap_mut().release_root(root)?;
            return Err(error);
        }
        if let Some(value) = completed {
            results::finish_task(
                self.vm.heap_mut(),
                result,
                results::TaskResult::Returned(value),
            )?;
            self.vm.heap_mut().release_root(root)?;
        } else {
            let frame = ParticleFrame {
                elapsed: 0.,
                duration,
                extent,
            };
            self.stage.scene.set_particle_frame(id, Some(frame))?;
            self.particles.live.push_back(Live {
                root,
                target: id,
                owner,
                frame,
                last_time: self.particles.time,
                paused: false,
                completion,
            });
        }
        if self.particles.live.is_empty() {
            self.particles.clear();
        }
        self.vm.respond(ticket, HostResponse::Ready(result))?;
        self.issued = None;
        Ok(())
    }
    fn end_emission(&mut self, index: usize, status: &str) -> Result<(), AdapterError> {
        let live = &self.particles.live[index];
        let result = self.vm.heap().root_value(live.root)?;
        let value = self.vm.heap_mut().alloc_string(status)?;
        results::finish_task(
            self.vm.heap_mut(),
            result,
            results::TaskResult::Returned(value),
        )?;
        let live = self.particles.live.remove(index).unwrap();
        // A sliced clock pass keeps unprocessed producers at the front and
        // rotates survivors behind them. Controls may arrive between slices.
        if index < self.particles.remaining {
            self.particles.remaining -= 1;
        }
        self.vm.heap_mut().release_root(live.root)?;
        if self.stage.scene.node(live.target).is_some() {
            self.stage.scene.set_particle_frame(live.target, None)?;
        }
        self.results.changed();
        Ok(())
    }
    pub(super) fn control_particles(
        &mut self,
        ticket: HostTicket,
        symbol: &str,
    ) -> Result<(), AdapterError> {
        let result = self.vm.host_arguments(ticket)?[0];
        if let Some(index) = self.particles.live.iter().position(|live| {
            self.vm
                .heap()
                .root_value(live.root)
                .is_ok_and(|v| v.same_bits(result))
        }) {
            match symbol {
                "reel::pause" => self.particles.live[index].paused = true,
                "reel::resume" => self.particles.live[index].paused = false,
                "reel::stop" => self.end_emission(index, "Stopped")?,
                "reel::finish" => {
                    if self.particles.live[index].frame.duration.is_none() {
                        return Err(AdapterError::Input(
                            "continuous emission has no final value; use stop",
                        ));
                    }
                    self.end_emission(index, "Completed")?;
                }
                _ => unreachable!(),
            }
        }
        if self.particles.live.is_empty() {
            self.particles.clear();
        }
        self.vm.respond(ticket, HostResponse::Ready(result))?;
        self.issued = None;
        Ok(())
    }
}
