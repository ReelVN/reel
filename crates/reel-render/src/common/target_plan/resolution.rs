//! The low-density output remains live through full-grid reconstruction.
use super::*;
impl Scratch {
    pub(super) fn begin_resolution(
        &mut self,
        original: [u32; 2],
        effect: Option<&reel_ui::frame::FrameEffect>,
        reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<Option<[u32; 2]>, GpuError> {
        let reduced = effect
            .map(|effect| super::super::effect_resolution::size(original, effect.resolution_scale))
            .filter(|&size| size != original);
        if let Some(size) = reduced {
            self.acquire(size, reserve)?;
        }
        Ok(reduced)
    }
    pub(super) fn end_resolution(
        &mut self,
        reduced: Option<[u32; 2]>,
        reserve: &mut impl FnMut(u64) -> Result<(), GpuError>,
    ) -> Result<(), GpuError> {
        if let Some(size) = reduced {
            self.release(size, reserve)?;
        }
        Ok(())
    }
}
