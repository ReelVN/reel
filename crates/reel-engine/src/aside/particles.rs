//! Sprite emission producers reuse the common stage clock and result protocol.
use super::{AdapterError, HostTicket, SessionVm, results, stage, text};
use aside_vm::executor::HostResponse;
use aside_vm::{Heap, HeapError, HeapReservation, RootKind, RootToken, Value, ValueTag};
use reel_core::budget::{BudgetCategory, BudgetPool, QueueLimits};
use reel_ui::stage::{
    NodeId, NodeKind,
    particles::{ParticleBatches, ParticleConfig, ParticleEmission, ParticleFrame, ParticleNode},
};
use std::collections::VecDeque;

pub(super) const PREFIX: &[&str] = &["source", "seed"];
/// Shared node payload: 0 identity, 1 children, 2 parent, 3 derived view;
/// particles alone add 4, the next deterministic emission sequence.
const SEQUENCE: u32 = 4;
pub(super) fn parameters() -> Vec<aside_host::NativeParameter> {
    use aside_vm::bytecode::{BuiltinType, Const};
    vec![
        aside_host::NativeParameter {
            name: "source".into(),
            ty: BuiltinType::Any,
            default: None,
        },
        aside_host::NativeParameter {
            name: "seed".into(),
            ty: BuiltinType::Any,
            default: Some(Const::Int(0)),
        },
    ]
}
pub(super) fn emission_parameters() -> Vec<aside_host::NativeParameter> {
    use aside_vm::bytecode::{BuiltinType, Const};
    let number = |value: f64| Some(Const::FloatBits(value.to_bits()));
    [
        ("node", None),
        ("burst", Some(Const::Int(1))),
        ("rate", number(0.)),
        ("duration", Some(Const::Null)),
        ("lifetime", number(5.)),
        ("lifetime_spread", number(0.)),
        ("velocity_x", number(0.)),
        ("velocity_y", number(80.)),
        ("spread_x", number(20.)),
        ("spread_y", number(20.)),
        ("acceleration_x", number(0.)),
        ("acceleration_y", number(0.)),
        ("size", number(12.)),
        ("size_spread", number(4.)),
        ("size_end", Some(Const::Null)),
        ("opacity_start", number(1.)),
        ("opacity_end", number(0.)),
        ("spin", number(0.)),
        ("spin_spread", number(30.)),
        ("x", number(0.)),
        ("y", number(0.)),
        ("owner", Some(Const::Null)),
    ]
    .into_iter()
    .map(|(name, default)| aside_host::NativeParameter {
        name: name.into(),
        ty: BuiltinType::Any,
        default,
    })
    .collect()
}
pub(super) fn kind(
    heap: &Heap,
    args: &[Value],
    origin: Option<String>,
    limit: usize,
) -> Result<NodeKind, AdapterError> {
    let source = text::read_string(heap, args[0], limit)?;
    if source.is_empty() {
        return Err(AdapterError::Input("particle source must not be empty"));
    }
    let seed = args[1]
        .as_int()
        .ok_or(AdapterError::Input("particle seed must be int"))? as u64;
    Ok(NodeKind::Particles(ParticleNode {
        source,
        origin,
        seed,
        batches: Default::default(),
        time: 0.,
    }))
}
fn configuration(args: &[Value], seed: u64) -> Result<ParticleConfig, AdapterError> {
    let n = |i: usize| {
        args[i]
            .as_int()
            .map(|v| v as f64)
            .or_else(|| args[i].as_float())
            .ok_or(AdapterError::Input("particle parameter must be a number"))
    };
    let config = ParticleConfig {
        rate: n(2)?,
        burst: args[1]
            .as_int()
            .filter(|count| *count >= 0)
            .ok_or(AdapterError::Input(
                "particle burst must be a nonnegative int",
            ))? as u64,
        lifetime: n(4)?,
        lifetime_spread: n(5)?,
        seed,
        velocity: [n(6)?, n(7)?],
        spread: [n(8)?, n(9)?],
        acceleration: [n(10)?, n(11)?],
        size: n(12)?,
        size_spread: n(13)?,
        size_end: if args[14].tag() == ValueTag::Null {
            None
        } else {
            Some(n(14)?)
        },
        opacity: [n(15)?, n(16)?],
        spin: n(17)?,
        spin_spread: n(18)?,
    };
    validate_configuration(&config)?;
    Ok(config)
}
fn validate_configuration(config: &ParticleConfig) -> Result<(), AdapterError> {
    let numbers = [
        config.rate,
        config.lifetime,
        config.lifetime_spread,
        config.size,
        config.size_spread,
        config.spin,
        config.spin_spread,
    ]
    .into_iter()
    .chain(config.velocity)
    .chain(config.spread)
    .chain(config.acceleration)
    .chain(config.opacity)
    .chain(config.size_end);
    if numbers.into_iter().any(|value| !value.is_finite())
        || config.burst > i64::MAX as u64
        || config.rate < 0.
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
    Ok(())
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
    sequence: u64,
    config: ParticleConfig,
    frame: ParticleFrame,
    started: f64,
    paused: bool,
    completion: Option<Completion>,
}
impl Live {
    fn projection(&self) -> ParticleEmission {
        ParticleEmission {
            sequence: self.sequence,
            config: self.config,
            frame: self.frame,
            started: self.started,
            paused: self.paused,
        }
    }
}
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Particles {
    #[serde(skip)]
    live: VecDeque<Live>,
    #[serde(skip)]
    charge: Option<HeapReservation>,
    #[serde(skip)]
    projection_pool: Option<BudgetPool>,
    time: f64,
    remaining: usize,
    // Last node-retirement version checked; ordinary property changes keep this pass valid.
    revision: u64,
    woke: bool,
}
impl Particles {
    pub(super) fn owners_changed(&mut self) {
        self.remaining = self.live.len();
    }
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
        retirement_revision: u64,
    ) -> Result<(), AdapterError> {
        use super::snapshot::Part;
        match record.part {
            Part::Particles => {
                *self = record.read()?;
            }
            Part::Particle => {
                let mut live: Live = record.read()?;
                // Emission configuration now belongs to adapter records rather
                // than constructor fields: restoration uses the same boundary.
                validate_configuration(&live.config)?;
                live.root = map.root(live.root)?;
                let batches = self.project(
                    heap,
                    stage,
                    live.target,
                    live.sequence,
                    Some(live.projection()),
                )?;
                stage.set_particle_batches(live.target, batches)?;
                stage.set_particle_time(live.target, self.time)?;
                results::reserve_queue(heap, &mut self.live, &mut self.charge)?;
                self.live.push_back(live);
            }
            _ => unreachable!(),
        }
        self.revision = retirement_revision;
        Ok(())
    }
    /// Admission prepares the replacement before changing either the activity
    /// or the stage. Old UI views retain their independent CPU-output lease.
    fn project(
        &mut self,
        heap: &Heap,
        stage: &reel_ui::stage::Stage,
        target: NodeId,
        sequence: u64,
        replacement: Option<ParticleEmission>,
    ) -> Result<ParticleBatches, AdapterError> {
        let NodeKind::Particles(node) = &stage.node(target).unwrap().kind else {
            unreachable!()
        };
        let pool = self.projection_pool.get_or_insert_with(|| {
            BudgetPool::new(
                BudgetCategory::CpuOutput,
                QueueLimits {
                    max_requests: u32::MAX,
                    max_bytes: heap.budget_usage().limit(),
                },
            )
        });
        // Maintenance rotates its queue; saved records therefore need not
        // arrive in paint order. Sequence remains the single ordering source.
        let at = node
            .batches
            .partition_point(|batch| batch.sequence < sequence);
        let replacing = node
            .batches
            .get(at)
            .is_some_and(|batch| batch.sequence == sequence);
        let count =
            node.batches.len() - usize::from(replacing) + usize::from(replacement.is_some());
        let entries = node.batches[..at]
            .iter()
            .copied()
            .chain(replacement)
            .chain(node.batches[at + usize::from(replacing)..].iter().copied());
        ParticleBatches::try_new(pool, count, entries).map_err(|_| AdapterError::Allocation)
    }
    #[cfg(test)]
    pub(super) fn projection_pool(&self) -> &BudgetPool {
        self.projection_pool.as_ref().unwrap()
    }
    #[cfg(test)]
    pub(super) fn set_projection_pool(&mut self, pool: BudgetPool) {
        self.projection_pool = Some(pool);
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
    pub fn needs_work(&self, retirement_revision: u64) -> bool {
        self.remaining != 0 || (!self.live.is_empty() && self.revision != retirement_revision)
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
        if !self.particles.needs_work(self.stage.retirement_revision) {
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
                || !super::scopes::owner_live(
                    self.vm.heap(),
                    results::owner(self.vm.heap(), result)?,
                );
            let mut terminal = cancelled.then_some(results::TaskResult::Cancelled);
            let mut pending_completion = false;
            if !cancelled {
                // Keep the clock origin unchanged between controls so a
                // snapshot does not introduce a different floating-point sum.
                let elapsed = live.frame.elapsed
                    + if live.paused {
                        0.
                    } else {
                        self.particles.time - live.started
                    };
                if let Some(completion) = &mut live.completion {
                    work += completion.advance(&live.config, budget - work).max(1);
                    pending_completion = completion.next != 0;
                    if !pending_completion && elapsed >= completion.end {
                        terminal = Some(results::TaskResult::Returned(
                            self.vm.heap_mut().alloc_string("Completed")?,
                        ));
                    }
                } else {
                    work += 1;
                    if elapsed * live.config.rate + live.config.burst as f64 >= u64::MAX as f64 {
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
            let target = live.target;
            let sequence = live.sequence;
            if let Some(terminal) = terminal {
                if self.stage.scene.node(target).is_some() {
                    let batches = self.particles.project(
                        self.vm.heap(),
                        &self.stage.scene,
                        target,
                        sequence,
                        None,
                    )?;
                    self.stage.scene.set_particle_batches(target, batches)?;
                }
                results::finish_task(self.vm.heap_mut(), result, terminal)?;
                self.vm
                    .heap_mut()
                    .release_root(self.particles.live.front().unwrap().root)?;
                self.results.changed();
                self.particles.woke = true;
                self.particles.live.pop_front();
            } else {
                self.stage
                    .scene
                    .set_particle_time(target, self.particles.time)?;
                if pending_completion {
                    break;
                }
                let live = self.particles.live.pop_front().unwrap();
                self.particles.live.push_back(live);
            }
            self.particles.remaining -= 1;
        }
        if self.particles.remaining == 0 {
            self.particles.revision = self.stage.retirement_revision;
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
        let duration = if args[3].tag() == ValueTag::Null {
            None
        } else {
            let duration = stage::number(args[3])?;
            if duration < 0. {
                return Err(AdapterError::Input("emission duration must be nonnegative"));
            }
            Some(duration)
        };
        let owner = if args[21].tag() == ValueTag::Null {
            self.default_activity_owner(ticket.task)?
        } else {
            args[21]
        };
        let owner_value = owner;
        super::scopes::validate_owner(self.vm.heap(), &self.stage, owner)?;
        let node = self.stage.scene.node(id).unwrap();
        let NodeKind::Particles(batch) = &node.kind else {
            return Err(AdapterError::Input("emit requires a particles node"));
        };
        let sequence = (self
            .vm
            .heap()
            .host_payload(target, SEQUENCE)?
            .as_int()
            .unwrap() as u64)
            .checked_add(1)
            .ok_or(AdapterError::Input("particle emission sequence exhausted"))?;
        let seed = batch
            .seed
            .wrapping_add(sequence.wrapping_mul(0xa0761d6478bd642f));
        let config = configuration(args, seed)?;
        let origin = [stage::number(args[19])?, stage::number(args[20])?];
        let duration = if config.rate == 0. {
            Some(0.)
        } else {
            duration
        };
        if duration.is_some_and(|d| {
            !(d + config.max_lifetime()).is_finite()
                || d * config.rate + config.burst as f64 >= u64::MAX as f64
        }) {
            return Err(AdapterError::Input(
                "emission duration exceeds representable range",
            ));
        }
        let completion = duration.map(|duration| Completion::new(&config, duration));
        // Capture current effective dimensions, including scope overrides and
        // writes since the last stage projection.
        let dimension = |field: stage::NodeProperty| -> Result<f64, AdapterError> {
            let value = self
                .stage
                .effective_property(self.vm.heap(), target, field)?;
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
        let frame = ParticleFrame {
            origin,
            elapsed: 0.,
            duration,
            extent,
        };
        let projection = ParticleEmission {
            sequence,
            config,
            frame,
            started: self.particles.time,
            paused: false,
        };
        let batches = if completed.is_none() {
            Some(self.particles.project(
                self.vm.heap(),
                &self.stage.scene,
                id,
                sequence,
                Some(projection),
            )?)
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
        self.vm
            .heap_mut()
            .set_host_payload(target, SEQUENCE, Value::int(sequence as i64))?;
        if let Some(value) = completed {
            results::finish_task(
                self.vm.heap_mut(),
                result,
                results::TaskResult::Returned(value),
            )?;
            self.vm.heap_mut().release_root(root)?;
        } else {
            self.stage
                .scene
                .set_particle_batches(id, batches.unwrap())?;
            self.stage
                .scene
                .set_particle_time(id, self.particles.time)?;
            self.particles.live.push_back(Live {
                root,
                target: id,
                sequence,
                config,
                frame,
                started: self.particles.time,
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
        let target = live.target;
        let sequence = live.sequence;
        let batches = if self.stage.scene.node(target).is_some() {
            Some(self.particles.project(
                self.vm.heap(),
                &self.stage.scene,
                target,
                sequence,
                None,
            )?)
        } else {
            None
        };
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
        if let Some(batches) = batches {
            self.stage.scene.set_particle_batches(target, batches)?;
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
                "reel::pause" | "reel::resume" => {
                    let live = &self.particles.live[index];
                    let paused = symbol == "reel::pause";
                    let mut projection = live.projection();
                    projection.frame = projection.frame_at(self.particles.time);
                    projection.started = self.particles.time;
                    projection.paused = paused;
                    let target = live.target;
                    let sequence = live.sequence;
                    let batches = self.particles.project(
                        self.vm.heap(),
                        &self.stage.scene,
                        target,
                        sequence,
                        Some(projection),
                    )?;
                    let live = &mut self.particles.live[index];
                    live.frame = projection.frame;
                    live.started = projection.started;
                    live.paused = paused;
                    self.stage.scene.set_particle_batches(target, batches)?;
                }
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
