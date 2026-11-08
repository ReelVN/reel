use super::*;
use crate::bytecode::VerifiedProgram;
use std::sync::Arc;

/// Metadata proved against one immutable current program.
#[derive(Clone, Debug)]
pub struct VerifiedResumeTable {
    pub(crate) program: VerifiedProgram,
    table: Arc<ResumeTable>,
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
    pub(crate) fn new(program: VerifiedProgram, table: ResumeTable) -> Self {
        Self {
            program,
            table: Arc::new(table),
        }
    }
    /// The validated current source contract.
    pub fn table(&self) -> &ResumeTable {
        &self.table
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
        for (index, frame) in descriptor.frames.iter().enumerate() {
            let flow = self
                .table
                .flows
                .iter()
                .position(|flow| flow.key == frame.flow)
                .ok_or_else(|| ResumeError::new("saved flow key is absent from current code"))?;
            let definition = &self.table.flows[flow];
            let site = match &frame.site {
                ResumeSite::Point(key) if index + 1 == descriptor.frames.len() => {
                    PlannedSite::Point(
                        definition
                            .points
                            .iter()
                            .position(|site| site.key == *key)
                            .ok_or_else(|| {
                                ResumeError::new("saved point key is absent from current code")
                            })?,
                    )
                }
                ResumeSite::Return(key) if index + 1 < descriptor.frames.len() => {
                    let site = definition
                        .returns
                        .iter()
                        .position(|site| site.key == *key)
                        .ok_or_else(|| {
                            ResumeError::new("saved return key is absent from current code")
                        })?;
                    if definition.returns[site].callee != descriptor.frames[index + 1].flow {
                        return Err(ResumeError::new(
                            "saved call chain differs from the current return contract",
                        ));
                    }
                    PlannedSite::Return(site)
                }
                _ => {
                    return Err(ResumeError::new(
                        "resume chain requires return sites followed by one leaf point",
                    ));
                }
            };
            frames.push(PlannedFrame { flow, site });
        }
        Ok(VerifiedResumePlan {
            verified: self.clone(),
            frames,
            descriptor: descriptor.clone(),
        })
    }
}
impl VerifiedResumePlan {
    /// Stable source identities selected by this verified plan.
    pub fn descriptor(&self) -> &ContinuationDescriptor {
        &self.descriptor
    }
    /// Number of ordinary business activations to rebuild.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
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
                    layout: &p.layout,
                }
            }
            PlannedSite::Return(index) => {
                let r = &flow.returns[index];
                Site {
                    entry: r.after_call,
                    call: r.call,
                    result: r.result,
                    layout: &r.layout,
                }
            }
        };
        (flow, site)
    }
}
