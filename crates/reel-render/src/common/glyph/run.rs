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
            .lease(run.font)
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

    #[cfg(any(feature = "gxm", test))]
    pub fn glyph(
        &self,
        context: &mut Rasterizer,
        index: usize,
        limits: RasterLimits,
    ) -> Result<Option<GlyphBitmap>, GpuError> {
        let mut result = Ok(None);
        self.rasterize(
            context,
            std::slice::from_ref(&index),
            std::slice::from_mut(&mut result),
            limits,
        );
        result
    }

    fn rasterize(
        &self,
        context: &mut Rasterizer,
        indices: &[usize],
        results: &mut [Result<Option<GlyphBitmap>, GpuError>],
        limits: RasterLimits,
    ) {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::GlyphRaster);
        let mut prepared = match context.prepare(self.source, self.run, self.density) {
            Ok(prepared) => prepared,
            Err(record) => {
                for result in results {
                    *result = Err(GpuError::Unsupported {
                        at: self.at,
                        record,
                    });
                }
                return;
            }
        };
        for (result, &index) in results.iter_mut().zip(indices) {
            *result = super::rasterize_glyph(
                &mut prepared,
                self.run,
                self.density,
                index,
                self.at,
                limits.clone(),
            );
        }
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
            self.rasterize(&mut context, indices, &mut results, limits);
        } else {
            let lanes = (indices.len() / 4).min(contexts.0.len());
            let chunk = indices.len().div_ceil(lanes);
            results
                .par_chunks_mut(chunk)
                .zip(indices.par_chunks(chunk))
                .zip(contexts.0.par_iter())
                .for_each(|((results, indices), context)| {
                    let mut context = context.lock().unwrap();
                    self.rasterize(&mut context, indices, results, limits.clone());
                });
        }
        results
    }
}
