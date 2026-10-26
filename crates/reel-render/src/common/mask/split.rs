//! Separate draw-uniform work and independent samples from scalar pixel math.
use super::{MaskChannel, MaskOp, MaskProgram, NODES, registers::visit_reads};
use reel_core::budget::{BudgetPool, BudgetTicket};
use std::sync::{Arc, Weak};

pub(crate) const FETCHES: usize = 2;
pub(crate) const MATH: usize = 16;
pub(crate) const SLOTS: usize = 8;
const UNUSED: u8 = u8::MAX;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Fetch {
    pub image: u32,
    pub coordinates: [u32; 4],
    pub repeat: u8,
    pub alpha: Option<u8>,
    pub gray: Option<u8>,
}

#[derive(Clone, Debug)]
pub(crate) struct Instruction {
    #[cfg(test)]
    pub original: u8,
    pub destination: u8,
    pub operation: MaskOp,
}

/// Bounded metadata; no owned GPU resources or parameter-dependent structure.
pub(crate) struct SplitPlan {
    uniform: u32,
    seeds: [u8; SLOTS],
    fetches: [Option<Fetch>; FETCHES],
    instructions: [Option<Instruction>; MATH],
    count: u8,
    result: u8,
}

#[derive(Clone, Copy)]
pub(crate) struct SplitValues {
    pub coordinates: [[f32; 4]; FETCHES],
    pub seeds: [f32; SLOTS],
}

/// Four live structures, two exact parameter tuples each. Metadata is charged
/// before allocation; budget pressure simply retains the supported general path.
#[derive(Default)]
pub(crate) struct Cache {
    entries: [Option<Entry>; 4],
    next: usize,
}
struct Entry {
    owner: Weak<MaskProgram>,
    plan: Arc<AccountedPlan>,
    values: [Option<([u32; 5], Option<SplitValues>)>; 2],
    next: usize,
}
pub(crate) struct AccountedPlan {
    pub plan: SplitPlan,
    _storage: BudgetTicket,
}
pub(crate) struct Prepared {
    pub plan: Arc<AccountedPlan>,
    pub values: SplitValues,
}
impl Cache {
    pub(crate) fn prune(&mut self) {
        for entry in &mut self.entries {
            if entry
                .as_ref()
                .is_some_and(|entry| entry.owner.strong_count() == 0)
            {
                *entry = None;
            }
        }
    }
    pub(crate) fn get(
        &mut self,
        program: &Arc<MaskProgram>,
        parameters: [f32; 5],
        budget: &BudgetPool,
    ) -> Option<Prepared> {
        self.prune();
        let index = match self.entries.iter().position(|entry| {
            entry
                .as_ref()
                .is_some_and(|entry| entry.owner.as_ptr() == Arc::as_ptr(program))
        }) {
            Some(index) => index,
            None => {
                let plan = SplitPlan::new(program)?;
                let index = self
                    .entries
                    .iter()
                    .position(Option::is_none)
                    .unwrap_or(self.next);
                self.entries[index] = None;
                // Include cache entry capacity and Arc allocation counters.
                let storage = budget
                    .reserve(
                        (size_of::<Entry>() + size_of::<AccountedPlan>() + 2 * size_of::<usize>())
                            as u64,
                    )
                    .ok()?;
                self.entries[index] = Some(Entry {
                    owner: Arc::downgrade(program),
                    plan: Arc::new(AccountedPlan {
                        plan,
                        _storage: storage,
                    }),
                    values: [None; 2],
                    next: 0,
                });
                self.next = (index + 1) % self.entries.len();
                index
            }
        };
        let entry = self.entries[index].as_mut().unwrap();
        let key = parameters.map(f32::to_bits);
        let values = match entry.values.iter().flatten().find(|(bits, _)| *bits == key) {
            Some((_, values)) => *values,
            None => {
                let values = entry.plan.plan.evaluate(program, parameters);
                entry.values[entry.next] = Some((key, values));
                entry.next = (entry.next + 1) % entry.values.len();
                values
            }
        }?;
        Some(Prepared {
            plan: entry.plan.clone(),
            values,
        })
    }
}

impl SplitPlan {
    pub(crate) fn new(program: &MaskProgram) -> Option<Self> {
        let operations = program.operations();
        if operations.len() > NODES || program.is_image_threshold() {
            return None;
        }
        let mut result = Self {
            uniform: 0,
            seeds: [UNUSED; SLOTS],
            fetches: [None; FETCHES],
            instructions: std::array::from_fn(|_| None),
            count: 0,
            result: 0,
        };
        let mut locations = [UNUSED; NODES];
        let mut next_seed = 0_u8;
        for (index, operation) in operations.iter().enumerate() {
            let uniform_bits = result.uniform;
            let uniform = |node: u32| uniform_bits & (1 << node) != 0;
            let is_uniform = match *operation {
                MaskOp::Constant(_) | MaskOp::Parameter(_) => true,
                MaskOp::Add(a, b) | MaskOp::Subtract(a, b) | MaskOp::Multiply(a, b) => {
                    uniform(a) && uniform(b)
                }
                MaskOp::Clamp(a, _, _) => uniform(a),
                // GPU power and smoothstep approximations remain on the GPU.
                _ => false,
            };
            if is_uniform {
                result.uniform |= 1 << index;
            }
            if let MaskOp::Sample {
                image,
                channel,
                coordinates,
                repeat,
            } = *operation
            {
                if !coordinates.iter().all(|&node| uniform(node)) {
                    return None;
                }
                let fetch = result.fetches.iter().position(|fetch| {
                    fetch.is_some_and(|fetch| {
                        fetch.image == image
                            && fetch.coordinates == coordinates
                            && fetch.repeat == repeat
                    })
                });
                let fetch = match fetch {
                    Some(index) => index,
                    None => {
                        let index = result.fetches.iter().position(Option::is_none)?;
                        result.fetches[index] = Some(Fetch {
                            image,
                            coordinates,
                            repeat,
                            alpha: None,
                            gray: None,
                        });
                        index
                    }
                };
                let fetch = result.fetches[fetch].as_mut().unwrap();
                let destination = match channel {
                    MaskChannel::Alpha => &mut fetch.alpha,
                    MaskChannel::Gray => &mut fetch.gray,
                };
                locations[index] = *destination.get_or_insert_with(|| {
                    let slot = next_seed;
                    next_seed += 1;
                    slot
                });
            }
        }
        if next_seed == 0 {
            return None;
        }
        // Only values crossing the CPU/pixel boundary occupy initial banks.
        let mut boundary = 0_u32;
        for (index, operation) in operations.iter().enumerate() {
            if result.uniform & (1 << index) == 0 && !matches!(operation, MaskOp::Sample { .. }) {
                visit_reads(operation, |node| boundary |= result.uniform & (1 << node));
            }
        }
        boundary |= result.uniform & (1 << (operations.len() - 1));
        for (index, location) in locations.iter_mut().enumerate().take(operations.len()) {
            if boundary & (1 << index) != 0 {
                *result.seeds.get_mut(usize::from(next_seed))? = index as u8;
                *location = next_seed;
                next_seed += 1;
            }
        }
        if usize::from(next_seed) > SLOTS {
            return None;
        }
        let mut last_read: [u8; NODES] = std::array::from_fn(|i| i as u8);
        for (index, operation) in operations.iter().enumerate() {
            visit_reads(operation, |node| last_read[node as usize] = index as u8);
        }
        let mut expires = [0_u8; SLOTS];
        let mut occupied = 0_u8;
        for (index, &slot) in locations.iter().enumerate().take(operations.len()) {
            if slot != UNUSED {
                // Identical sample channels share storage and the latest read.
                expires[usize::from(slot)] = expires[usize::from(slot)].max(last_read[index]);
                occupied |= 1 << slot;
            }
        }
        for (index, operation) in operations.iter().enumerate() {
            if result.uniform & (1 << index) != 0 || matches!(operation, MaskOp::Sample { .. }) {
                continue;
            }
            let mapped = remap(operation, |node| u32::from(locations[node as usize]));
            // All reads finish before an expiring slot receives this output.
            for (slot, &last) in expires.iter().enumerate() {
                if usize::from(last) <= index {
                    occupied &= !(1 << slot);
                }
            }
            let slot = (!occupied).trailing_zeros() as u8;
            if usize::from(slot) >= SLOTS {
                return None;
            }
            *result.instructions.get_mut(usize::from(result.count))? = Some(Instruction {
                #[cfg(test)]
                original: index as u8,
                destination: slot,
                operation: mapped,
            });
            result.count += 1;
            locations[index] = slot;
            expires[usize::from(slot)] = last_read[index];
            if usize::from(last_read[index]) > index {
                occupied |= 1 << slot;
            }
        }
        // The small single-fetch variant has only eight arithmetic words.
        if result.fetch_count() == 1 && usize::from(result.count) > 8 {
            return None;
        }
        result.result = locations[operations.len() - 1];
        Some(result)
    }

    /// Evaluate the eligible subgraph in source f32 order. Exceptional values
    /// select the existing GPU path; they never enter an optimized coordinate.
    pub(crate) fn evaluate(
        &self,
        program: &MaskProgram,
        parameters: [f32; 5],
    ) -> Option<SplitValues> {
        let mut values = [0_f32; NODES];
        for (index, operation) in program.operations().iter().enumerate() {
            if self.uniform & (1 << index) == 0 {
                continue;
            }
            let at = |node: u32| values[node as usize];
            let value = match *operation {
                MaskOp::Constant(bits) => f32::from_bits(bits),
                MaskOp::Parameter(index) => parameters[usize::from(index)],
                MaskOp::Add(a, b) => at(a) + at(b),
                MaskOp::Subtract(a, b) => at(a) - at(b),
                MaskOp::Multiply(a, b) => at(a) * at(b),
                MaskOp::Clamp(a, low, high) => {
                    at(a).clamp(f32::from_bits(low), f32::from_bits(high))
                }
                _ => unreachable!("uniform classification admits only basic scalar arithmetic"),
            };
            if !value.is_finite() {
                return None;
            }
            values[index] = value;
        }
        Some(SplitValues {
            coordinates: std::array::from_fn(|i| {
                self.fetches[i].map_or([0.; 4], |fetch| {
                    fetch.coordinates.map(|n| values[n as usize])
                })
            }),
            seeds: self.seeds.map(|node| {
                if node == UNUSED {
                    0.
                } else {
                    values[usize::from(node)]
                }
            }),
        })
    }

    pub(crate) fn fetches(&self) -> impl Iterator<Item = &Fetch> {
        self.fetches.iter().flatten()
    }
    pub(crate) fn fetch_count(&self) -> usize {
        self.fetches().count()
    }
    pub(crate) fn instructions(&self) -> impl Iterator<Item = &Instruction> {
        self.instructions[..usize::from(self.count)]
            .iter()
            .flatten()
    }
    pub(crate) fn result(&self) -> u8 {
        self.result
    }
}

fn remap(operation: &MaskOp, at: impl Fn(u32) -> u32) -> MaskOp {
    match *operation {
        MaskOp::Add(a, b) => MaskOp::Add(at(a), at(b)),
        MaskOp::Subtract(a, b) => MaskOp::Subtract(at(a), at(b)),
        MaskOp::Multiply(a, b) => MaskOp::Multiply(at(a), at(b)),
        MaskOp::Pow(a, p) => MaskOp::Pow(at(a), p),
        MaskOp::Clamp(a, low, high) => MaskOp::Clamp(at(a), low, high),
        MaskOp::Smoothstep(a, b, x) => MaskOp::Smoothstep(at(a), at(b), at(x)),
        _ => unreachable!("samples and uniform nodes are evaluated outside the arithmetic stream"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::operation_value;
    use super::*;

    #[allow(dead_code, reason = "shared full-DAG oracle fixture")]
    mod fixture {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/mask_dag.rs"
        ));
    }

    fn program(operations: Vec<MaskOp>) -> std::sync::Arc<MaskProgram> {
        MaskProgram::new(
            operations,
            vec!["a".into(), "b".into(), "c".into(), "d".into()],
        )
        .unwrap()
    }

    fn compare(program: &MaskProgram, parameters: [f32; 5]) {
        let plan = SplitPlan::new(program).expect("eligible graph");
        let evaluated = plan
            .evaluate(program, parameters)
            .expect("finite uniform values");
        let mut original = Vec::new();
        for operation in program.operations() {
            original.push(operation_value(operation, parameters, 0.37, 0.73, |n| {
                original[n as usize]
            }));
        }
        let mut slots = evaluated.seeds;
        for (fetch, coordinates) in plan.fetches().zip(evaluated.coordinates) {
            for (actual, node) in coordinates.into_iter().zip(fetch.coordinates) {
                assert_eq!(actual.to_bits(), original[node as usize].to_bits());
            }
            if let Some(slot) = fetch.alpha {
                slots[usize::from(slot)] = 0.37;
            }
            if let Some(slot) = fetch.gray {
                slots[usize::from(slot)] = 0.73;
            }
        }
        for instruction in plan.instructions() {
            let source = &program.operations()[usize::from(instruction.original)];
            let mut expected = Vec::new();
            visit_reads(source, |n| expected.push(original[n as usize].to_bits()));
            let mut actual = Vec::new();
            visit_reads(&instruction.operation, |n| {
                actual.push(slots[n as usize].to_bits())
            });
            assert_eq!(
                actual, expected,
                "operand order at {}",
                instruction.original
            );
            let value = operation_value(&instruction.operation, parameters, 0.37, 0.73, |n| {
                slots[n as usize]
            });
            assert_eq!(
                value.to_bits(),
                original[usize::from(instruction.original)].to_bits()
            );
            slots[usize::from(instruction.destination)] = value;
        }
        assert_eq!(
            slots[usize::from(plan.result())].to_bits(),
            original.last().unwrap().to_bits()
        );
    }

    #[test]
    fn split_preserves_arithmetic_operands_results_and_coordinates() {
        for length in [8, 12, 13] {
            let program = program(fixture::operations()[..length].to_vec());
            for progress in [-0., 0.35, 1., f32::MAX] {
                compare(&program, [progress, 0.2, -0.25, 0.75, 0.]);
            }
        }
        let program = program(vec![
            MaskOp::Constant(0),
            MaskOp::Constant(1_f32.to_bits()),
            MaskOp::Sample {
                image: 0,
                channel: MaskChannel::Alpha,
                coordinates: [0, 0, 1, 1],
                repeat: 0,
            },
            MaskOp::Constant((-0_f32).to_bits()),
            MaskOp::Parameter(0),
            MaskOp::Multiply(3, 4),
        ]);
        compare(&program, [1., 0., 0., 0., 0.]);
    }

    #[test]
    fn split_shares_only_identical_fetches_and_keeps_latest_channel_read() {
        let mut operations = vec![MaskOp::Constant(0), MaskOp::Constant(1_f32.to_bits())];
        let sample = |channel, repeat| MaskOp::Sample {
            image: 0,
            channel,
            coordinates: [0, 0, 1, 1],
            repeat,
        };
        operations.extend([
            sample(MaskChannel::Alpha, 0),
            sample(MaskChannel::Gray, 0),
            MaskOp::Add(2, 3),
            sample(MaskChannel::Alpha, 0),
            MaskOp::Multiply(4, 5),
        ]);
        let graph = program(operations.clone());
        assert_eq!(SplitPlan::new(&graph).unwrap().fetch_count(), 1);
        compare(&graph, [0.; 5]);
        operations[5] = sample(MaskChannel::Alpha, 1);
        let graph = program(operations);
        assert_eq!(SplitPlan::new(&graph).unwrap().fetch_count(), 2);
        compare(&graph, [0.; 5]);
    }

    #[test]
    fn exceptional_uniforms_and_texture_dependent_coordinates_keep_general_path() {
        assert!(SplitPlan::new(&program(fixture::operations())).is_none());
        let mut operations = fixture::operations()[..8].to_vec();
        operations.extend([
            MaskOp::Constant(2_f32.to_bits()),
            MaskOp::Multiply(2, 8),
            MaskOp::Add(7, 9),
        ]);
        let graph = program(operations);
        let plan = SplitPlan::new(&graph).unwrap();
        assert!(plan.evaluate(&graph, [f32::MAX, 0., 0., 0., 0.]).is_none());
        compare(&graph, [0.4, 0., 0., 0., 0.]);
        let graph = program(vec![
            MaskOp::Constant(0),
            MaskOp::Constant(1_f32.to_bits()),
            MaskOp::Pow(1, 2_f32.to_bits()),
            MaskOp::Sample {
                image: 0,
                channel: MaskChannel::Alpha,
                coordinates: [2, 0, 1, 1],
                repeat: 0,
            },
        ]);
        assert!(SplitPlan::new(&graph).is_none());
    }

    #[test]
    fn cache_keys_float_bits_and_keeps_charges_until_last_borrow() {
        use reel_core::budget::{BudgetCategory, QueueLimits};
        let budget = BudgetPool::new(
            BudgetCategory::GpuResource,
            QueueLimits {
                max_requests: 8,
                max_bytes: 16384,
            },
        );
        let graph = program(fixture::operations()[..13].to_vec());
        let mut cache = Cache::default();
        let positive = cache.get(&graph, [0.; 5], &budget).unwrap();
        let usage = budget.usage().bytes;
        let mut parameters = [0.; 5];
        parameters[0] = -0.;
        let negative = cache.get(&graph, parameters, &budget).unwrap();
        assert!(Arc::ptr_eq(&positive.plan, &negative.plan));
        let entry = cache.entries.iter().flatten().next().unwrap();
        assert_ne!(entry.values[0].unwrap().0, entry.values[1].unwrap().0);
        let next = entry.next;
        drop(cache.get(&graph, [0.; 5], &budget).unwrap());
        assert_eq!(cache.entries.iter().flatten().next().unwrap().next, next);
        assert_eq!(budget.usage().bytes, usage);
        drop(graph);
        cache.prune();
        assert!(cache.entries.iter().all(Option::is_none));
        assert_eq!(budget.usage().bytes, usage);
        drop((positive, negative));
        assert_eq!(budget.usage().bytes, 0);
        let tiny = BudgetPool::new(
            BudgetCategory::GpuResource,
            QueueLimits {
                max_requests: 1,
                max_bytes: 1,
            },
        );
        assert!(
            cache
                .get(
                    &program(fixture::operations()[..8].to_vec()),
                    [0.; 5],
                    &tiny
                )
                .is_none()
        );
        assert_eq!(tiny.usage().bytes, 0);
    }
}
