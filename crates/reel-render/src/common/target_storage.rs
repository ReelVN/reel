//! Physical storage shared by scratch extents with compatible lifetimes.
use super::{GpuError, scopes::push_stack};
mod capacities;

#[derive(Clone, Copy)]
pub(crate) struct Event {
    pub size: [u32; 2],
    pub acquired: bool,
}

#[derive(Default)]
pub(crate) struct Plan {
    mappings: Vec<Mapping>,
    bytes: u64,
    requirements: super::target_plan::Requirements,
}

struct Mapping {
    logical: [u32; 2],
    storage: [u32; 2],
}

struct Class {
    size: [u32; 2],
    peak: u64,
    cost: u64,
    group: usize,
}

struct Group {
    size: [u32; 2],
    peak: u64,
    cost: u64,
    protected: bool,
    live: u64,
    combined_peak: u64,
}

impl Plan {
    pub(crate) fn requirements(&self) -> &super::target_plan::Requirements {
        &self.requirements
    }
    pub(crate) fn storage_size(&self, logical: [u32; 2]) -> [u32; 2] {
        self.mappings
            .binary_search_by_key(&logical, |mapping| mapping.logical)
            .map_or(logical, |index| self.mappings[index].storage)
    }

    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Fold smaller size classes into existing storage only when their exact
    /// combined lifetime peak occupies fewer bytes. Root output stays exact.
    /// Requests keep their logical size and therefore their rasterization grid.
    pub(crate) fn from_lifetimes<R>(
        events: &[Event],
        root: [u32; 2],
        cost: &dyn Fn([u32; 2]) -> Result<u64, GpuError>,
        mut reserve: impl FnMut(u64) -> Result<R, GpuError>,
    ) -> Result<(Self, Vec<R>), GpuError> {
        let mut temporary = Vec::new();
        let mut planning = |bytes| {
            temporary.push(reserve(bytes)?);
            Ok(())
        };
        let mut classes = Vec::<Class>::new();
        for event in events {
            if !classes.iter().any(|class| class.size == event.size) {
                push_stack(
                    &mut classes,
                    Class {
                        size: event.size,
                        peak: 0,
                        cost: cost(event.size)?,
                        group: 0,
                    },
                    &mut planning,
                )?;
            }
        }
        classes.sort_unstable_by_key(|class| class.size);
        let mut timeline = Vec::new();
        let mut live = Vec::new();
        for _ in &classes {
            push_stack(&mut live, 0u64, &mut planning)?;
        }
        for event in events {
            let index = classes
                .binary_search_by_key(&event.size, |class| class.size)
                .unwrap();
            push_stack(&mut timeline, (index, event.acquired), &mut planning)?;
            if event.acquired {
                live[index] += 1;
                classes[index].peak = classes[index].peak.max(live[index]);
            } else {
                live[index] = live[index].checked_sub(1).expect("planned target release");
            }
        }
        let mut order = Vec::new();
        for index in 0..classes.len() {
            push_stack(&mut order, index, &mut planning)?;
        }
        order.sort_unstable_by_key(|&index| {
            (std::cmp::Reverse(classes[index].cost), classes[index].size)
        });
        let mut groups = Vec::<Group>::new();
        for index in order {
            let class = &classes[index];
            let size = class.size;
            let peak = class.peak;
            let original = class.cost.checked_mul(peak).ok_or(GpuError::InvalidSize)?;
            let mut best = None;
            if size != root {
                // One timeline sweep evaluates all existing groups. A new
                // candidate acquisition changes every combined live count;
                // existing group events change only that group's count.
                for group in &mut groups {
                    group.live = 0;
                    group.combined_peak = 0;
                }
                let mut current = 0u64;
                for &(event_class, acquired) in &timeline {
                    if event_class == index {
                        if acquired {
                            current += 1;
                            for group in &mut groups {
                                group.combined_peak = group.combined_peak.max(group.live + current);
                            }
                        } else {
                            current = current.checked_sub(1).expect("planned class release");
                        }
                    } else if let Some(group_id) = classes[event_class].group.checked_sub(1) {
                        let group = &mut groups[group_id];
                        if acquired {
                            group.live += 1;
                            group.combined_peak = group.combined_peak.max(group.live + current);
                        } else {
                            group.live = group.live.checked_sub(1).expect("planned group release");
                        }
                    }
                }
                for (group_id, group) in groups
                    .iter()
                    .enumerate()
                    .filter(|(_, group)| !group.protected)
                {
                    let combined_size = [size[0].max(group.size[0]), size[1].max(group.size[1])];
                    let combined_peak = group.combined_peak;
                    let separate = original
                        .checked_add(
                            group
                                .cost
                                .checked_mul(group.peak)
                                .ok_or(GpuError::InvalidSize)?,
                        )
                        .ok_or(GpuError::InvalidSize)?;
                    let combined_cost = if combined_size == group.size {
                        group.cost
                    } else if combined_size == size {
                        class.cost
                    } else {
                        cost(combined_size)?
                    };
                    let combined = combined_cost
                        .checked_mul(combined_peak)
                        .ok_or(GpuError::InvalidSize)?;
                    let saving = separate.saturating_sub(combined);
                    if saving > best.map_or(0, |(_, _, _, _, saving)| saving) {
                        best = Some((
                            group_id,
                            combined_size,
                            combined_peak,
                            combined_cost,
                            saving,
                        ));
                    }
                }
            }
            if let Some((group_id, size, peak, combined_cost, _)) = best {
                groups[group_id].cost = combined_cost;
                groups[group_id].size = size;
                groups[group_id].peak = peak;
                classes[index].group = group_id + 1;
            } else {
                push_stack(
                    &mut groups,
                    Group {
                        size,
                        peak,
                        cost: class.cost,
                        protected: size == root,
                        live: 0,
                        combined_peak: 0,
                    },
                    &mut planning,
                )?;
                classes[index].group = groups.len();
            }
        }
        let mut plan = Self::default();
        let mut retained = Vec::new();
        plan.requirements = super::target_plan::Requirements::collect(
            groups.iter().map(|group| (group.size, group.peak)),
            |bytes| {
                retained.push(reserve(bytes)?);
                Ok(())
            },
        )?;
        for group in &groups {
            plan.bytes = plan
                .bytes
                .checked_add(
                    group
                        .cost
                        .checked_mul(group.peak)
                        .ok_or(GpuError::InvalidSize)?,
                )
                .ok_or(GpuError::InvalidSize)?;
        }
        for class in &classes {
            let storage = groups[class.group - 1].size;
            if class.size != storage {
                push_stack(
                    &mut plan.mappings,
                    Mapping {
                        logical: class.size,
                        storage,
                    },
                    |bytes| {
                        retained.push(reserve(bytes)?);
                        Ok(())
                    },
                )?;
            }
        }
        Ok((plan, retained))
    }
}

#[cfg(test)]
fn area(size: [u32; 2]) -> u64 {
    u64::from(size[0]) * u64::from(size[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(size: [u32; 2], acquired: bool) -> Event {
        Event { size, acquired }
    }

    #[test]
    fn disjoint_source_shapes_share_storage_without_changing_the_output_extent() {
        let root = [960, 544];
        let a = [2240, 1922];
        let b = [2522, 1000];
        let events = [
            event(root, true),
            event(a, true),
            event(a, true),
            event(a, false),
            event(a, false),
            event(b, true),
            event(b, true),
            event(b, false),
            event(b, false),
        ];
        let (plan, _) =
            Plan::from_lifetimes(&events, root, &|size| Ok(area(size)), |_| Ok(())).unwrap();
        assert_eq!(plan.storage_size(root), root);
        assert_eq!(plan.storage_size(a), [2522, 1922]);
        assert_eq!(plan.storage_size(b), [2522, 1922]);
        assert_eq!(plan.bytes(), area(root) + 2 * area([2522, 1922]));
        assert!(plan.bytes() < area(root) + 2 * area(a) + 2 * area(b));
    }

    #[test]
    fn simultaneous_sources_keep_separate_storage_when_padding_would_cost_more() {
        let a = [600, 200];
        let b = [200, 600];
        let (plan, _) = Plan::from_lifetimes(
            &[event(a, true), event(b, true)],
            [960, 544],
            &|size| Ok(area(size)),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(plan.storage_size(a), a);
        assert_eq!(plan.storage_size(b), b);
        assert_eq!(plan.bytes(), area(a) + area(b));
    }

    #[test]
    fn storage_sharing_uses_the_same_padded_byte_cost_as_allocation() {
        let a = [9, 1];
        let b = [8, 2];
        let events = [event(a, true), event(a, false), event(b, true)];
        let stride =
            |size: [u32; 2]| Ok(u64::from(size[0]).div_ceil(8) * 8 * u64::from(size[1]) * 4);
        let (area_plan, _) =
            Plan::from_lifetimes(&events, [64; 2], &|size| Ok(area(size)), |_| Ok(())).unwrap();
        assert_eq!(area_plan.storage_size(a), [9, 2]);
        let (plan, _) = Plan::from_lifetimes(&events, [64; 2], &stride, |_| Ok(())).unwrap();
        assert_eq!(plan.storage_size(a), a);
        assert_eq!(plan.storage_size(b), b);
        assert_eq!(plan.bytes(), 128);
        assert_eq!(stride([9, 2]).unwrap(), 128);
    }

    #[test]
    fn overlap_uses_the_combined_live_peak_and_accounts_metadata() {
        let a = [400, 400];
        let b = [380, 380];
        let events = [
            event(a, true),
            event(a, true),
            event(a, false),
            event(b, true),
            event(a, false),
            event(b, true),
            event(b, false),
            event(b, false),
        ];
        let pool = crate::test_support::r01_fixture::pool();
        let (plan, tickets) =
            Plan::from_lifetimes(&events, [960, 544], &|size| Ok(area(size)), |bytes| {
                pool.reserve(bytes).map_err(|_| GpuError::InvalidSize)
            })
            .unwrap();
        assert_eq!(plan.storage_size(b), a);
        assert_eq!(plan.bytes(), 2 * area(a));
        assert_eq!(plan.requirements().count(a), 2);
        assert_eq!(plan.requirements().count(b), 0);
        assert_eq!(
            pool.usage().bytes,
            (plan.mappings.capacity() * size_of::<Mapping>()) as u64
                + plan.requirements.capacity_bytes(),
            "only storage mappings and physical requirements remain reserved"
        );
        drop(plan);
        drop(tickets);
        assert_eq!(pool.usage().bytes, 0);
        assert!(
            Plan::from_lifetimes(
                &events,
                [960, 544],
                &|size| Ok(area(size)),
                |_| Err::<(), _>(GpuError::InvalidSize)
            )
            .is_err()
        );
    }
}
