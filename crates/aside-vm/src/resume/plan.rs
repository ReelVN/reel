use super::*;
use crate::bytecode::VerifiedProgram;
use std::sync::Arc;
mod index;
use index::ResumeIndex;

/// Metadata proved against one immutable current program.
#[derive(Clone, Debug)]
pub struct VerifiedResumeTable {
    pub(crate) program: VerifiedProgram,
    table: Arc<ResumeTable>,
    index: Arc<ResumeIndex>,
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum PlannedSite {
    Point(usize),
    Return(usize),
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct PlannedFrame {
    pub flow: usize,
    pub site: PlannedSite,
}
pub(crate) struct Site<'a> {
    pub entry: Pc,
    pub call: Pc,
    pub result: Reg,
    pub layout: &'a ResumeLayout,
}
/// An executable continuation capability. Its numeric layout is never caller supplied.
#[derive(Clone, Debug)]
pub struct VerifiedResumePlan {
    pub(crate) verified: VerifiedResumeTable,
    pub(crate) frames: Vec<PlannedFrame>,
    descriptor: ContinuationDescriptor,
}
impl VerifiedResumeTable {
    pub(crate) fn new(
        program: VerifiedProgram,
        table: ResumeTable,
        point_targets: &[FunctionId],
        budget: &mut Budget,
    ) -> Result<Self, ResumeError> {
        let index = Arc::new(ResumeIndex::new(&table, point_targets, budget)?);
        Ok(Self {
            program,
            table: Arc::new(table),
            index,
        })
    }
    pub(crate) fn current_site(
        &self,
        function: FunctionId,
        call: Pc,
    ) -> Option<(PlannedFrame, Option<FunctionId>)> {
        self.index.current_site(function, call)
    }
    pub(crate) fn current_bounds(&self) -> (u64, u64) {
        (self.index.lookup_work, self.index.key_bytes)
    }
    /// Check whether this call is declared as a point. Capturing or hashing a
    /// resumable path still requires proving the actual active call chain.
    pub fn has_point_at(&self, function: FunctionId, call: Pc) -> bool {
        self.current_site(function, call)
            .is_some_and(|(frame, _)| matches!(frame.site, PlannedSite::Point(_)))
    }
    /// The validated current source contract.
    pub fn table(&self) -> &ResumeTable {
        &self.table
    }
    /// The verified program whose functions and continuation sites this table describes.
    pub fn program(&self) -> &VerifiedProgram {
        &self.program
    }
    /// Bind the same immutable text pool as the current program without revalidating
    /// the already checked continuation table.
    pub fn bind_text_pool(
        mut self,
        pool: crate::bytecode::TextPool,
    ) -> Result<Self, crate::bytecode::Error> {
        self.program = self.program.bind_text_pool(pool)?;
        Ok(self)
    }
    /// Resolve only stable keys and verify the complete ordinary-call chain.
    pub fn verify_plan(
        &self,
        descriptor: &ContinuationDescriptor,
    ) -> Result<VerifiedResumePlan, ResumeError> {
        if descriptor.frames.is_empty() {
            return Err(ResumeError::new("empty resume call chain"));
        }
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(descriptor.frames.len())
            .map_err(|_| ResumeError::new("resume plan allocation"))?;
        let mut expected_function = None;
        for (index, frame) in descriptor.frames.iter().enumerate() {
            let current = self.index.stable_site(&self.table, frame).ok_or_else(|| {
                ResumeError::new("saved flow or site key is absent from current code")
            })?;
            let definition = &self.table.flows[current.flow];
            if expected_function.is_some_and(|callee| callee != definition.function) {
                return Err(ResumeError::new(
                    "saved call chain differs from the current return contract",
                ));
            }
            match current.site {
                PlannedSite::Point(_) if index + 1 == descriptor.frames.len() => {}
                PlannedSite::Return(site) if index + 1 < descriptor.frames.len() => {
                    expected_function = Some(definition.returns[site].callee);
                }
                _ => {
                    return Err(ResumeError::new(
                        "resume chain requires return sites followed by one leaf point",
                    ));
                }
            }
            frames.push(current);
        }
        Ok(VerifiedResumePlan {
            verified: self.clone(),
            frames,
            descriptor: descriptor.clone(),
        })
    }
}
impl VerifiedResumePlan {
    pub(crate) fn from_matching_stack(
        verified: VerifiedResumeTable,
        frames: Vec<PlannedFrame>,
        descriptor: ContinuationDescriptor,
    ) -> Self {
        Self {
            verified,
            frames,
            descriptor,
        }
    }
    /// Stable source identities selected by this verified plan.
    pub fn descriptor(&self) -> &ContinuationDescriptor {
        &self.descriptor
    }
    /// Number of ordinary business activations to rebuild.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }
    /// Current plain entry function whose frames return to the live native callback.
    pub fn entry_function(&self) -> FunctionId {
        self.frame(0).0.function
    }
    pub(crate) fn frame(&self, index: usize) -> (&ResumeFlow, Site<'_>) {
        let frame = self.frames[index];
        let flow = &self.verified.table.flows[frame.flow];
        let site = match frame.site {
            PlannedSite::Point(index) => {
                let p = &flow.points[index];
                Site {
                    entry: p.prepare,
                    call: p.call,
                    result: p.result,
                    layout: &flow.layouts[p.layout as usize],
                }
            }
            PlannedSite::Return(index) => {
                let r = &flow.returns[index];
                Site {
                    entry: r.after_call,
                    call: r.call,
                    result: r.result,
                    layout: &flow.layouts[r.layout as usize],
                }
            }
        };
        (flow, site)
    }
}
