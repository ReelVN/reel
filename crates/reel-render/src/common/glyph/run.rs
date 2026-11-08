//! CPU-only glyph preparation; device adapters own atlas allocation and uploads.
use super::{GlyphBitmap, GpuError, RasterLimits, Rasterizer};
use reel_ui::{
    frame::{FrameDrawing, FrameGlyphRun, RecordLocation},
    resources::ResourceLease,
};

/// Renderer-owned contexts retain parsed font data across batches and release
/// their scratch storage with the renderer. Small runs use the first context.
#[cfg(feature = "wgpu")]
pub(crate) struct Rasterizers(Box<[std::sync::Mutex<Rasterizer>]>);
#[cfg(feature = "wgpu")]
impl Default for Rasterizers {
    fn default() -> Self {
        Self(
            (0..rayon::current_num_threads().min(8))
                .map(|_| std::sync::Mutex::new(Rasterizer::default()))
                .collect(),
        )
    }
}

/// Resolve the font lease once for all glyphs in a shaped run.
pub(crate) struct RasterRun<'a> {
    pub source: &'a ResourceLease,
    run: &'a FrameGlyphRun,
    density: f64,
    at: RecordLocation,
}

impl<'a> RasterRun<'a> {
    pub fn new(
        drawing: &'a FrameDrawing,
        run: &'a FrameGlyphRun,
        density: f64,
        at: RecordLocation,
    ) -> Result<Self, GpuError> {
        let source = drawing
            .resources()
            .leases()
            .iter()
            .find(|resource| resource.handle() == run.font)
            .ok_or(GpuError::Unsupported {
                at,
                record: "missing font lease",
            })?;
        Ok(Self {
            source,
            run,
            density,
            at,
        })
    }

    pub fn glyph(
        &self,
        context: &mut Rasterizer,
        index: usize,
        limits: RasterLimits,
    ) -> Result<Option<GlyphBitmap>, GpuError> {
        super::rasterize_resource(
            context,
            self.source,
            self.run,
            self.density,
            index,
            self.at,
            limits,
        )
    }

    /// Rasterize a bounded set of distinct missing glyphs. Each bitmap owns its
    /// shared-budget reservation; ordering and errors follow the input indices.
    #[cfg(feature = "wgpu")]
    pub fn batch(
        &self,
        contexts: &Rasterizers,
        indices: &[usize],
        limits: RasterLimits,
    ) -> smallvec::SmallVec<[Result<Option<GlyphBitmap>, GpuError>; 32]> {
        use rayon::prelude::*;
        let mut results: smallvec::SmallVec<[_; 32]> = indices.iter().map(|_| Ok(None)).collect();
        if indices.len() < 8 {
            let mut context = contexts.0[0].lock().unwrap();
            for (result, &index) in results.iter_mut().zip(indices) {
                *result = self.glyph(&mut context, index, limits.clone());
            }
        } else {
            let lanes = (indices.len() / 4).min(contexts.0.len());
            let chunk = indices.len().div_ceil(lanes);
            results
                .par_chunks_mut(chunk)
                .zip(indices.par_chunks(chunk))
                .zip(contexts.0.par_iter())
                .for_each(|((results, indices), context)| {
                    let mut context = context.lock().unwrap();
                    for (result, &index) in results.iter_mut().zip(indices) {
                        *result = self.glyph(&mut context, index, limits.clone());
                    }
                });
        }
        results
    }
}
