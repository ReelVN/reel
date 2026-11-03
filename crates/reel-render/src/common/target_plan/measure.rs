//! Shared measurement and storage admission on the target lifetime walk.
use super::*;

pub(crate) struct Measured {
    pub(crate) cropped: bool,
    pub bytes: u64,
    pub reusable: bool,
    pub local_backdrop: bool,
    pub sampled_backdrop: bool,
    pub coverage_bytes: u64,
    scratch: Scratch,
}
impl Measured {
    pub(in crate::common) fn output_crops(self) -> Vec<super::super::effect_output::Entry> {
        self.scratch.output_crops
    }
    pub(crate) fn coverage_requirements(
        &self,
        reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Requirements, GpuError> {
        Requirements::collect(
            self.scratch
                .coverage
                .iter()
                .map(|bucket| (bucket.size, bucket.peak)),
            reserve,
        )
    }
    pub(crate) fn requirements(
        &self,
        mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Requirements, GpuError> {
        Requirements::collect(
            self.scratch
                .buckets
                .iter()
                .map(|bucket| (bucket.size, bucket.peak)),
            &mut reserve,
        )?
        .with_exact(&self.scratch.exact, reserve)
    }
}

impl Input<'_> {
    /// Recount the same liveness events against the already selected ordinary
    /// storage map. A raw crop is an exact class; all other classes stay mapped.
    pub(crate) fn physical_measure(
        &self,
        mut reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Measured, GpuError> {
        let mut scratch = self.walk(true, false, None, &mut reserve)?;
        let events = scratch.events.take().expect("recorded physical lifetimes");
        recount(
            &events,
            std::iter::empty(),
            |size| self.schedule.storage_size(size),
            &mut scratch,
            &mut reserve,
        )?;
        scratch.buckets.retain(|bucket| bucket.peak != 0);
        let coverage_bytes = scratch.coverage_cost(self.coverage_cost)?;
        let bytes = scratch
            .cost(self.cost)?
            .checked_add(coverage_bytes)
            .and_then(|v| v.checked_add(self.selection.persistent_bytes()))
            .and_then(|v| v.checked_add(self.schedule.effect_inputs().pinned_bytes()))
            .ok_or(GpuError::InvalidSize)?;
        Ok(Measured {
            cropped: scratch.cropped,
            bytes,
            reusable: false,
            local_backdrop: scratch.local_backdrop,
            sampled_backdrop: scratch.sampled_backdrop,
            coverage_bytes,
            scratch,
        })
    }
    pub(crate) fn measure(
        &self,
        reserve: impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Measured, GpuError> {
        let scratch = self.walk(false, false, None, reserve)?;
        let reusable = scratch
            .buckets
            .iter()
            .filter(|bucket| bucket.size != self.size)
            .take(2)
            .count()
            == 2;
        let coverage_bytes = scratch.coverage_cost(self.coverage_cost)?;
        let bytes = scratch
            .cost(self.cost)?
            .checked_add(coverage_bytes)
            .and_then(|v| v.checked_add(self.selection.persistent_bytes()))
            .and_then(|v| v.checked_add(self.schedule.effect_inputs().pinned_bytes()))
            .ok_or(GpuError::InvalidSize)?;
        Ok(Measured {
            cropped: scratch.cropped,
            bytes,
            reusable,
            local_backdrop: scratch.local_backdrop,
            sampled_backdrop: scratch.sampled_backdrop,
            coverage_bytes,
            scratch,
        })
    }

    /// Derive a physical storage map from the executor's exact target lifetimes.
    /// Temporary planning tickets end here; mapping tickets remain charged through encoding.
    pub(crate) fn storage<R>(
        &self,
        mut reserve: impl FnMut(u64) -> Result<R, GpuError>,
    ) -> Result<(super::super::target_storage::Plan, u64, Vec<R>), GpuError> {
        let mut temporary = Vec::new();
        let scratch = self.walk(true, false, None, |bytes| {
            temporary.push(reserve(bytes)?);
            Ok(())
        })?;
        let (plan, retained) = super::super::target_storage::Plan::from_lifetimes_exact(
            scratch
                .events
                .as_deref()
                .expect("recorded target lifetimes"),
            self.size,
            &scratch.exact,
            self.cost,
            reserve,
        )?;
        let bytes = plan
            .bytes()
            .checked_add(scratch.coverage_cost(self.coverage_cost)?)
            .and_then(|v| v.checked_add(self.selection.persistent_bytes()))
            .and_then(|v| v.checked_add(self.schedule.effect_inputs().pinned_bytes()))
            .ok_or(GpuError::InvalidSize)?;
        Ok((plan, bytes, retained))
    }
}
