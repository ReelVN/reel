//! Freeze bounded physical owners and remove only their own ordinary events.
use super::*;

fn grid(cell: [u32; 2], cells: usize, max_dimension: u32) -> Option<[u32; 2]> {
    if cells > OWNER_MAX_CELLS {
        return None;
    }
    (1..=4)
        .flat_map(|x| (1..=4).map(move |y| [x, y]))
        .filter(|g| {
            g[0] * g[1] >= cells as u32 && (0..2).all(|axis| g[axis] * cell[axis] <= max_dimension)
        })
        .min_by_key(|g| (g[0] * g[1], (g[0] * cell[0]).max(g[1] * cell[1])))
}

fn recount_selected<'a>(
    events: &[Event],
    candidates: impl Iterator<Item = &'a Candidate>,
    excluded: &mut Vec<Range<usize>>,
    storage: &impl Fn([u32; 2]) -> [u32; 2],
    scratch: &mut Scratch,
    reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
) -> Result<(), GpuError> {
    excluded.clear();
    for candidate in candidates {
        for span in [
            candidate.events.clone(),
            candidate.release..candidate.release + 1,
        ] {
            crate::common::scopes::push_stack(excluded, span, &mut *reserve)?;
        }
    }
    // Deferred releases follow all window producers, rather than their own
    // producer span. Sorting preserves unrelated owners between those events.
    excluded.sort_unstable_by_key(|span| span.start);
    recount(events, excluded.iter().cloned(), storage, scratch, reserve)
}

impl Selection {
    #[expect(clippy::too_many_arguments, reason = "frozen physical planning inputs")]
    pub(super) fn choose(
        events: &[Event],
        mut candidates: Vec<Candidate>,
        storage: impl Fn([u32; 2]) -> [u32; 2],
        pixel_cost: &dyn Fn([u32; 2]) -> Result<u64, GpuError>,
        fixed: u64,
        limit: u64,
        max_dimension: u32,
        reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Option<Self>, GpuError> {
        if candidates.is_empty() {
            return Ok(None);
        }
        candidates.sort_unstable_by_key(|candidate| candidate.events.start);
        let mut entries: Vec<Candidate> = Vec::new();
        let mut owners = 0;
        let mut atlas_bytes = 0u64;
        let mut chosen = None;
        let mut scratch = Scratch::default();
        let mut excluded = Vec::new();
        recount(events, std::iter::empty(), &storage, &mut scratch, reserve)?;
        for mut candidate in candidates {
            recount_selected(
                events,
                entries.iter().chain(std::iter::once(&candidate)),
                &mut excluded,
                &storage,
                &mut scratch,
                reserve,
            )?;
            let base = scratch
                .cost(pixel_cost)?
                .checked_add(atlas_bytes)
                .and_then(|value| value.checked_add(fixed))
                .ok_or(GpuError::InvalidSize)?;

            // Only adjacent producers in one independent window can continue
            // the same macro scene. Parent draws between them are deferred.
            let append = entries.last().filter(|previous| {
                candidate
                    .window
                    .zip(previous.window)
                    .is_some_and(|(a, b)| a.0 == b.0 && a.1 == b.1 + 1)
                    && candidate.key.drawing == previous.key.drawing
                    && candidate.key.commands == previous.key.commands
                    && candidate.cell == previous.cell
                    && candidate.storage == previous.storage
            });
            let mut placement = None;
            if let Some(previous) = append {
                let first = previous.first_cell + previous.count + 1;
                let cells = previous.owner_cells.max(first + candidate.count + 1);
                if let Some(layout) = grid(candidate.cell, cells, max_dimension) {
                    let extent = [candidate.cell[0] * layout[0], candidate.cell[1] * layout[1]];
                    let additional = pixel_cost(extent)?
                        .checked_sub(pixel_cost(previous.extent())?)
                        .ok_or(GpuError::InvalidSize)?;
                    if base
                        .checked_add(additional)
                        .is_some_and(|bytes| bytes <= limit)
                    {
                        placement = Some((previous.owner, first, cells, layout, additional));
                    }
                }
            }
            if placement.is_none() {
                let reuse = entries.iter().find(|entry| {
                    entry.last_use
                        && entry.cell == candidate.cell
                        && entry.storage == candidate.storage
                        && entry.owner_cells > candidate.count
                        && entries
                            .iter()
                            .filter(|other| other.owner == entry.owner)
                            .all(|other| other.release < candidate.events.start)
                });
                placement = if let Some(previous) = reuse {
                    Some((previous.owner, 0, previous.owner_cells, previous.grid, 0))
                } else {
                    Some((
                        owners,
                        0,
                        candidate.count + 1,
                        candidate.grid,
                        pixel_cost(candidate.extent())?,
                    ))
                };
            }
            let (owner, first, cells, layout, additional) = placement.unwrap();
            let bytes = base.checked_add(additional).ok_or(GpuError::InvalidSize)?;
            if bytes > limit {
                continue;
            }
            for previous in entries.iter_mut().filter(|entry| entry.owner == owner) {
                previous.grid = layout;
                previous.owner_cells = cells;
                if first == 0 {
                    previous.last_use = false;
                }
            }
            if owner == owners {
                owners += 1;
            }
            atlas_bytes += additional;
            candidate.owner = owner;
            candidate.first_cell = first;
            candidate.owner_cells = cells;
            candidate.grid = layout;
            crate::common::scopes::push_stack(&mut entries, candidate, &mut *reserve)?;
            chosen = Some(bytes);
        }
        let Some(bytes) = chosen else { return Ok(None) };
        recount_selected(
            events,
            entries.iter(),
            &mut excluded,
            &storage,
            &mut scratch,
            reserve,
        )?;
        let mut requirements = Requirements::collect(
            scratch
                .buckets
                .iter()
                .map(|bucket| (bucket.size, bucket.peak)),
            reserve,
        )?;
        requirements.0.retain(|entry| entry.count != 0);
        Ok(Some(Self {
            entries,
            requirements,
            bytes,
        }))
    }
}
