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
        mut reserve: impl FnMut(u64) -> Result<R, GpuError>,
    ) -> Result<Option<(Self, Vec<R>)>, GpuError> {
        let Some((padding, _temporary)) = Self::capacities(
            &self.requirements,
            root,
            max_dimension,
            byte_limit,
            cost,
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
        for (size, count) in required.iter().filter(|(size, _)| *size != root) {
            let largest = size[0].max(size[1]);
            let quantum = largest
                .checked_next_power_of_two()
                .ok_or(GpuError::InvalidSize)?
                / 32;
            let quantum = quantum.max(32);
            let extent = |value: u32| {
                value
                    .div_ceil(quantum)
                    .checked_mul(quantum)
                    .ok_or(GpuError::InvalidSize)
            };
            let storage = [extent(size[0])?, extent(size[1])?];
            if storage == size || storage[0] > max_dimension || storage[1] > max_dimension {
                continue;
            }
            let before = cost(size)?;
            let after = cost(storage)?;
            let extra = after.checked_sub(before).ok_or(GpuError::InvalidSize)?;
            let total_extra = extra.checked_mul(count).ok_or(GpuError::InvalidSize)?;
            if extra > before / 8 || total_extra > spare {
                continue;
            }
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
        if plan.mappings.is_empty() {
            return Ok(None);
        }
        plan.requirements = Requirements::collect(
            required
                .iter()
                .map(|(size, count)| (plan.storage_size(size), count)),
            |bytes| {
                tickets.push(reserve(bytes)?);
                Ok(())
            },
        )?;
        Ok(Some((plan, tickets)))
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
                .padded_capacities(root, 4096, exact.bytes(), &cost, |_| Ok(()))
                .unwrap()
                .is_none()
        );
        let pool = crate::test_support::r01_fixture::pool();
        let limit = cost(root).unwrap() + 2 * cost([512, 384]).unwrap();
        let (padded, tickets) = exact
            .padded_capacities(root, 4096, limit, &cost, |bytes| {
                pool.reserve(bytes).map_err(|_| GpuError::InvalidSize)
            })
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
                .padded_capacities(root, 4096, limit, &cost, |_| Err::<(), _>(
                    GpuError::InvalidSize
                ))
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
            let (plan, _) = Plan::capacities(&inputs, root, 4096, u64::MAX, &cost, |_| Ok(()))
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
            Plan::capacities(&inputs, root, 4096, exact, &cost, |_| Ok(()))
                .unwrap()
                .is_none()
        );
        assert!(
            Plan::capacities(&inputs, root, 500, u64::MAX, &cost, |_| Ok(()))
                .unwrap()
                .is_none()
        );
        let small = required(&[([1, 1], 1)]);
        assert!(
            Plan::capacities(&small, root, 4096, u64::MAX, &cost, |_| Ok(()))
                .unwrap()
                .is_none()
        );
        let padded =
            |size: [u32; 2]| Ok(u64::from(size[0].div_ceil(32) * 32) * u64::from(size[1]) * 4);
        let budget = padded(root).unwrap() + 2 * padded(size).unwrap();
        let (plan, _) = Plan::capacities(&inputs, root, 4096, budget, &padded, |_| Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(plan.bytes(), budget);
        assert_eq!(plan.storage_size(size), [512, 384]);
        assert_eq!(plan.requirements().count(root), 1);
    }
}
