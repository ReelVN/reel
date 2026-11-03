//! Stable scratch capacity classes for continuous changes of logical bounds.
use super::*;
use crate::common::target_plan::Requirements;

impl Plan {
    /// Stabilize the physical classes produced by lifetime sharing. The logical
    /// map is composed with the padded classes, preserving every source grid and
    /// the already established simultaneous-use multiplicity.
    pub(crate) fn padded_capacities<R>(
        &self,
        root: [u32; 2],
        max_dimension: u32,
        byte_limit: u64,
        cost: &dyn Fn([u32; 2]) -> Result<u64, GpuError>,
        resident: &Requirements,
        mut reserve: impl FnMut(u64) -> Result<R, GpuError>,
    ) -> Result<Option<(Self, Vec<R>)>, GpuError> {
        let Some((padding, _temporary)) = Self::capacities(
            &self.requirements,
            root,
            max_dimension,
            byte_limit,
            cost,
            resident,
            &mut reserve,
        )?
        else {
            return Ok(None);
        };
        let mut tickets = Vec::new();
        let mut mappings = Vec::new();
        for mapping in &self.mappings {
            push_stack(
                &mut mappings,
                Mapping {
                    logical: mapping.logical,
                    storage: padding.storage_size(mapping.storage),
                },
                |bytes| {
                    tickets.push(reserve(bytes)?);
                    Ok(())
                },
            )?;
        }
        for mapping in &padding.mappings {
            if self
                .mappings
                .binary_search_by_key(&mapping.logical, |entry| entry.logical)
                .is_err()
            {
                push_stack(
                    &mut mappings,
                    Mapping {
                        logical: mapping.logical,
                        storage: mapping.storage,
                    },
                    |bytes| {
                        tickets.push(reserve(bytes)?);
                        Ok(())
                    },
                )?;
            }
        }
        mappings.sort_unstable_by_key(|mapping| mapping.logical);
        // Padding's mapping capacity is temporary; retain only the composed map
        // and physical requirements. Its tickets end with this planning call.
        let requirements = Requirements::collect(padding.requirements.iter(), |bytes| {
            tickets.push(reserve(bytes)?);
            Ok(())
        })?
        .with_exact(padding.requirements.exact(), |bytes| {
            tickets.push(reserve(bytes)?);
            Ok(())
        })?;
        Ok(Some((
            Self {
                mappings,
                bytes: padding.bytes,
                requirements,
            },
            tickets,
        )))
    }

    /// Keep output exact. Scratch padding is limited to one eighth of its
    /// backend byte cost and admitted against the same physical frame peak.
    pub(crate) fn capacities<R>(
        required: &Requirements,
        root: [u32; 2],
        max_dimension: u32,
        byte_limit: u64,
        cost: &dyn Fn([u32; 2]) -> Result<u64, GpuError>,
        resident: &Requirements,
        mut reserve: impl FnMut(u64) -> Result<R, GpuError>,
    ) -> Result<Option<(Self, Vec<R>)>, GpuError> {
        let mut baseline = 0u64;
        for (size, count) in required.iter() {
            baseline = baseline
                .checked_add(
                    cost(size)?
                        .checked_mul(count)
                        .ok_or(GpuError::InvalidSize)?,
                )
                .ok_or(GpuError::InvalidSize)?;
        }
        let mut spare = byte_limit.saturating_sub(baseline);
        if baseline > byte_limit {
            return Ok(None);
        }
        let mut plan = Self {
            bytes: baseline,
            ..Default::default()
        };
        let mut tickets = Vec::new();
        for (size, count) in required
            .iter()
            .filter(|(size, _)| *size != root && !required.exact().contains(size))
        {
            let Some((storage, total_extra)) =
                padded_size(size, count, max_dimension, spare, cost)?
            else {
                continue;
            };
            spare -= total_extra;
            plan.bytes += total_extra;
            push_stack(
                &mut plan.mappings,
                Mapping {
                    logical: size,
                    storage,
                },
                |bytes| {
                    tickets.push(reserve(bytes)?);
                    Ok(())
                },
            )?;
        }
        plan.requirements = Requirements::collect(
            required
                .iter()
                .map(|(size, count)| (plan.storage_size(size), count)),
            |bytes| {
                tickets.push(reserve(bytes)?);
                Ok(())
            },
        )?
        .with_exact(required.exact(), |bytes| {
            tickets.push(reserve(bytes)?);
            Ok(())
        })?;
        resident_capacities(
            &mut plan,
            required,
            resident,
            root,
            max_dimension,
            byte_limit,
            cost,
            |bytes| {
                tickets.push(reserve(bytes)?);
                Ok(())
            },
        )?;
        if plan.mappings.is_empty() {
            return Ok(None);
        }
        Ok(Some((plan, tickets)))
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one complete scratch capacity selection"
)]
fn resident_capacities(
    plan: &mut Plan,
    required: &Requirements,
    resident: &Requirements,
    root: [u32; 2],
    max_dimension: u32,
    byte_limit: u64,
    cost: &dyn Fn([u32; 2]) -> Result<u64, GpuError>,
    mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
) -> Result<(), GpuError> {
    for (logical, count) in required
        .iter()
        .filter(|(size, _)| *size != root && !required.exact().contains(size))
    {
        let previous = plan.storage_size(logical);
        let original_bytes = cost(logical)?;
        let previous_bytes = cost(previous)?;
        let available = resident
            .count(previous)
            .saturating_sub(plan.requirements.count(previous) - count);
        // Compare the incremental allocations for this class, after preserving
        // every other logical class's multiplicity. A partially resident pool
        // still avoids native texture and scheduling-owner construction.
        let mut selected = (
            previous_bytes
                .checked_mul(count.saturating_sub(available))
                .ok_or(GpuError::InvalidSize)?,
            previous_bytes,
            previous,
        );
        for (capacity, available) in resident.iter() {
            if capacity == previous
                || (0..2)
                    .any(|axis| capacity[axis] < logical[axis] || capacity[axis] > max_dimension)
            {
                continue;
            }
            let available = available.saturating_sub(plan.requirements.count(capacity));
            if available == 0 {
                continue;
            }
            let bytes = cost(capacity)?;
            if bytes.saturating_sub(original_bytes) > original_bytes / 8 {
                continue;
            }
            let total = plan.bytes - previous_bytes * count;
            let Some(total) = bytes
                .checked_mul(count)
                .and_then(|extra| total.checked_add(extra))
            else {
                continue;
            };
            let missing = bytes
                .checked_mul(count.saturating_sub(available))
                .ok_or(GpuError::InvalidSize)?;
            if total <= byte_limit && (missing, bytes, capacity) < selected {
                selected = (missing, bytes, capacity);
            }
        }
        let (_, bytes, capacity) = selected;
        if capacity == previous {
            continue;
        }
        plan.bytes = plan.bytes - previous_bytes * count + bytes * count;
        plan.requirements.relocate(previous, capacity, count);
        match plan
            .mappings
            .binary_search_by_key(&logical, |entry| entry.logical)
        {
            Ok(index) if logical == capacity => {
                plan.mappings.remove(index);
            }
            Ok(index) => plan.mappings[index].storage = capacity,
            Err(_) => {
                push_stack(
                    &mut plan.mappings,
                    Mapping {
                        logical,
                        storage: capacity,
                    },
                    &mut reserve,
                )?;
                plan.mappings.sort_unstable_by_key(|entry| entry.logical);
            }
        }
    }
    Ok(())
}

/// Keep an already affordable square bucket. A rejected thin/small source tries
/// axis-sized buckets, reducing the more inflated axis until the same byte cap
/// admits it. Exact sizing is the final candidate, with no changed quality tier.
fn padded_size(
    size: [u32; 2],
    count: u64,
    max_dimension: u32,
    spare: u64,
    cost: &dyn Fn([u32; 2]) -> Result<u64, GpuError>,
) -> Result<Option<([u32; 2], u64)>, GpuError> {
    let before = cost(size)?;
    let bucket = |value: u32, divisor: u32| {
        value
            .checked_next_power_of_two()
            .map(|power| (power / divisor).max(1))
            .ok_or(GpuError::InvalidSize)
    };
    let square = bucket(size[0].max(size[1]), 32)?.max(32);
    let mut quantum = [square; 2];
    let mut square_candidate = true;
    loop {
        let round = |axis: usize| {
            size[axis]
                .div_ceil(quantum[axis])
                .checked_mul(quantum[axis])
                .ok_or(GpuError::InvalidSize)
        };
        let storage = [round(0)?, round(1)?];
        if storage == size {
            return Ok(None);
        }
        if storage.iter().all(|&extent| extent <= max_dimension) {
            let extra = cost(storage)?
                .checked_sub(before)
                .ok_or(GpuError::InvalidSize)?;
            let total = extra.checked_mul(count).ok_or(GpuError::InvalidSize)?;
            if extra <= before / 8 && total <= spare {
                return Ok(Some((storage, total)));
            }
        }
        if square_candidate {
            quantum = [bucket(size[0], 32)?, bucket(size[1], 32)?];
            square_candidate = false;
        } else {
            let inflation = [
                u64::from(storage[0] - size[0]) * u64::from(size[1]),
                u64::from(storage[1] - size[1]) * u64::from(size[0]),
            ];
            let axis = usize::from(inflation[1] > inflation[0]);
            let axis = if quantum[axis] > 1 { axis } else { 1 - axis };
            quantum[axis] = (quantum[axis] / 2).max(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn required(values: &[([u32; 2], u64)]) -> Requirements {
        Requirements::collect(values.iter().copied(), |_| Ok(())).unwrap()
    }
    fn cost(size: [u32; 2]) -> Result<u64, GpuError> {
        Ok(area(size) * 4)
    }

    #[test]
    fn resident_capacity_reuses_complete_classes_and_accounts_shared_multiplicity() {
        let root = [960, 544];
        let a = [481, 380];
        let b = [491, 380];
        let capacity = [512, 400];
        let inputs = required(&[(root, 1), (a, 1), (b, 1)]);
        for count in [1, 2] {
            let resident = required(&[(capacity, count)]);
            let (plan, _) =
                Plan::capacities(&inputs, root, 4096, u64::MAX, &cost, &resident, |_| Ok(()))
                    .unwrap()
                    .unwrap();
            assert_eq!(plan.storage_size(a), capacity);
            assert_eq!(plan.requirements.count(capacity), count);
            assert_eq!(plan.storage_size(b) == capacity, count == 2);
            let physical = inputs
                .iter()
                .map(|(size, n)| cost(plan.storage_size(size)).unwrap() * n)
                .sum::<u64>();
            assert_eq!(plan.bytes, physical);
        }
        let resident = required(&[(capacity, 2)]);
        let exact = required(&[(root, 1), (a, 1), (b, 1)])
            .with_exact(&[a], |_| Ok(()))
            .unwrap();
        let (plan, _) =
            Plan::capacities(&exact, root, 4096, u64::MAX, &cost, &resident, |_| Ok(()))
                .unwrap()
                .unwrap();
        assert_eq!(plan.storage_size(a), a);
        assert_eq!(plan.storage_size(root), root);
        assert_eq!(plan.storage_size(b), capacity);
        let (baseline, _) = Plan::capacities(
            &inputs,
            root,
            4096,
            u64::MAX,
            &cost,
            &Requirements::default(),
            |_| Ok(()),
        )
        .unwrap()
        .unwrap();
        let (tight, _) = Plan::capacities(
            &inputs,
            root,
            4096,
            baseline.bytes,
            &cost,
            &resident,
            |_| Ok(()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(tight.requirements.count(capacity), 0);
        assert_eq!(tight.bytes, baseline.bytes);
    }

    #[test]
    fn partial_resident_classes_minimize_new_storage_within_the_same_byte_cap() {
        let root = [960, 544];
        let a = [481, 380];
        let b = [491, 380];
        let padded = [512, 384];
        let resident_size = [512, 400];
        let inputs = required(&[(root, 1), (a, 3)]);
        let resident = required(&[(root, 1), (resident_size, 2)]);
        let limit = cost(root).unwrap() + 3 * cost(resident_size).unwrap();
        let (plan, _) = Plan::capacities(&inputs, root, 4096, limit, &cost, &resident, |_| Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(plan.storage_size(a), resident_size);
        assert_eq!(plan.requirements.count(resident_size), 3);
        assert_eq!(plan.bytes, limit);
        assert_eq!(
            plan.requirements
                .iter()
                .map(|(size, count)| count.saturating_sub(resident.count(size)))
                .sum::<u64>(),
            1,
            "two existing owners satisfy the enlarged simultaneous-use class"
        );
        let competing = required(&[(padded, 2), (resident_size, 2)]);
        let (preferred, _) =
            Plan::capacities(&inputs, root, 4096, limit, &cost, &competing, |_| Ok(()))
                .unwrap()
                .unwrap();
        assert_eq!(preferred.storage_size(a), padded);

        let inputs = required(&[(root, 1), (a, 2), (b, 2)]);
        let resident = required(&[(root, 1), (resident_size, 3)]);
        let limit = cost(root).unwrap() + 4 * cost(resident_size).unwrap();
        let (shared, _) =
            Plan::capacities(&inputs, root, 4096, limit, &cost, &resident, |_| Ok(()))
                .unwrap()
                .unwrap();
        assert_eq!(shared.storage_size(a), resident_size);
        assert_eq!(shared.storage_size(b), resident_size);
        assert_eq!(shared.requirements.count(resident_size), 4);
        assert_eq!(shared.bytes, limit);
        assert!(cost(resident_size).unwrap() - cost(a).unwrap() <= cost(a).unwrap() / 8);
        let tight = cost(root).unwrap() + 4 * cost(padded).unwrap();
        let (fallback, _) =
            Plan::capacities(&inputs, root, 4096, tight, &cost, &resident, |_| Ok(()))
                .unwrap()
                .unwrap();
        assert_eq!(fallback.requirements.count(resident_size), 0);
        assert_eq!(fallback.bytes, tight);
    }

    #[test]
    fn lifetime_padding_preserves_overlap_and_exact_pressure_fallback() {
        let root = [960, 544];
        let a = [481, 384];
        let b = [400, 380];
        let events = [
            Event {
                size: root,
                acquired: true,
            },
            Event {
                size: a,
                acquired: true,
            },
            Event {
                size: a,
                acquired: true,
            },
            Event {
                size: a,
                acquired: false,
            },
            Event {
                size: b,
                acquired: true,
            },
            Event {
                size: a,
                acquired: false,
            },
            Event {
                size: b,
                acquired: true,
            },
        ];
        let (exact, _) = Plan::from_lifetimes(&events, root, &cost, |_| Ok(())).unwrap();
        assert_eq!(exact.requirements().count(a), 2);
        assert!(
            exact
                .padded_capacities(
                    root,
                    4096,
                    exact.bytes(),
                    &cost,
                    &Requirements::default(),
                    |_| Ok(())
                )
                .unwrap()
                .is_none()
        );
        let pool = crate::test_support::r01_fixture::pool();
        let limit = cost(root).unwrap() + 2 * cost([512, 384]).unwrap();
        let (padded, tickets) = exact
            .padded_capacities(
                root,
                4096,
                limit,
                &cost,
                &Requirements::default(),
                |bytes| pool.reserve(bytes).map_err(|_| GpuError::InvalidSize),
            )
            .unwrap()
            .unwrap();
        assert_eq!(padded.storage_size(root), root);
        assert_eq!(padded.storage_size(a), [512, 384]);
        assert_eq!(padded.storage_size(b), [512, 384]);
        assert_eq!(padded.requirements().count([512, 384]), 2);
        assert_eq!(padded.bytes(), limit);
        assert_eq!(
            pool.usage().bytes,
            (padded.mappings.capacity() * size_of::<Mapping>()) as u64
                + padded.requirements.capacity_bytes()
        );
        drop(padded);
        drop(tickets);
        assert_eq!(pool.usage().bytes, 0);
        assert!(
            exact
                .padded_capacities(
                    root,
                    4096,
                    limit,
                    &cost,
                    &Requirements::default(),
                    |_| Err::<(), _>(GpuError::InvalidSize)
                )
                .is_err()
        );
        assert_eq!(
            exact.storage_size(b),
            a,
            "optional metadata failure keeps the exact shared plan"
        );
    }

    #[test]
    fn moving_bounds_share_capacity_and_keep_physical_multiplicity() {
        let mut sizes = std::collections::BTreeSet::new();
        let root = [960, 544];
        for width in 481..=512 {
            let logical = [width, 384];
            let inputs = required(&[(root, 1), (logical, 2)]);
            let (plan, _) = Plan::capacities(
                &inputs,
                root,
                4096,
                u64::MAX,
                &cost,
                &Requirements::default(),
                |_| Ok(()),
            )
            .unwrap()
            .unwrap_or_else(|| {
                let (plan, tickets) = Plan::from_lifetimes(
                    &[
                        Event {
                            size: logical,
                            acquired: true,
                        },
                        Event {
                            size: logical,
                            acquired: true,
                        },
                    ],
                    root,
                    &cost,
                    |_| Ok(()),
                )
                .unwrap();
                (plan, tickets)
            });
            sizes.insert(plan.storage_size(logical));
            assert_eq!(plan.storage_size(root), root);
            assert_eq!(plan.requirements().count([512, 384]), 2);
            assert!(cost(plan.storage_size(logical)).unwrap() <= cost(logical).unwrap() * 9 / 8);
        }
        assert_eq!(sizes.into_iter().collect::<Vec<_>>(), [[512, 384]]);
    }

    #[test]
    fn padding_respects_full_frame_budget_and_backend_layout_cost() {
        let root = [960, 544];
        let size = [481, 384];
        let inputs = required(&[(root, 1), (size, 2)]);
        let exact = cost(root).unwrap() + 2 * cost(size).unwrap();
        assert!(
            Plan::capacities(
                &inputs,
                root,
                4096,
                exact,
                &cost,
                &Requirements::default(),
                |_| Ok(())
            )
            .unwrap()
            .is_none()
        );
        let (limited, _) = Plan::capacities(
            &inputs,
            root,
            500,
            u64::MAX,
            &cost,
            &Requirements::default(),
            |_| Ok(()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(limited.storage_size(size), [496, 384]);
        assert!(
            limited
                .storage_size(size)
                .iter()
                .all(|&extent| extent <= 500)
        );
        let small = required(&[([1, 1], 1)]);
        assert!(
            Plan::capacities(
                &small,
                root,
                4096,
                u64::MAX,
                &cost,
                &Requirements::default(),
                |_| Ok(())
            )
            .unwrap()
            .is_none()
        );
        let padded =
            |size: [u32; 2]| Ok(u64::from(size[0].div_ceil(32) * 32) * u64::from(size[1]) * 4);
        let budget = padded(root).unwrap() + 2 * padded(size).unwrap();
        let (plan, _) = Plan::capacities(
            &inputs,
            root,
            4096,
            budget,
            &padded,
            &Requirements::default(),
            |_| Ok(()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(plan.bytes(), budget);
        assert_eq!(plan.storage_size(size), [512, 384]);
        assert_eq!(plan.requirements().count(root), 1);
    }
}
