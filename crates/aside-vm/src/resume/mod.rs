//! Stable logical continuations verified against the current program.
//! Serialized keys never grant permission to inject a function or instruction PC.
mod analysis;
mod plan;
mod targets;
mod verify;

use crate::bytecode::{BindingId, FunctionId, Pc, Reg};
pub use crate::executor::resume::{
    CaptureProgress, CapturedBinding, ResumeCapture, ResumeCaptureCursor, ResumeFrameValues,
    ResumeProgress, ResumeRestore, ResumeRestoreCursor, ResumeValue,
};
pub use analysis::{SiteLayout, analyze_site};
pub use plan::{VerifiedResumePlan, VerifiedResumeTable};
use serde::{Deserialize, Serialize};
pub use verify::static_call_target;

macro_rules! key {
    ($($name:ident),*) => {$(
        #[doc = "An explicit stable source identity, independent of numeric code layout."]
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(pub String);
        impl From<&str> for $name { fn from(value: &str) -> Self { Self(value.into()) } }
    )*};
}
key!(FlowKey, PointKey, ReturnKey, BindingKey);

/// Current-code metadata. PCs are compact execution word offsets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeTable {
    pub version: u32,
    pub flows: Vec<ResumeFlow>,
}
impl Default for ResumeTable {
    fn default() -> Self {
        Self {
            version: 1,
            flows: Vec::new(),
        }
    }
}
/// A plain function participating in logical continuation capture.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeFlow {
    pub key: FlowKey,
    pub function: FunctionId,
    pub points: Vec<ResumePoint>,
    pub returns: Vec<ResumeReturn>,
}
/// A live binding selected by current control-flow analysis.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeBinding {
    pub key: BindingKey,
    pub binding: BindingId,
}
/// Register and lexical-cell state required at one continuation entry.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeLayout {
    pub bindings: Vec<ResumeBinding>,
    /// Existing lexical cells, including cells whose binding is not initialized yet.
    pub cells: Vec<BindingId>,
}
/// A complete interaction call whose read-only arguments may be prepared again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumePoint {
    pub key: PointKey,
    pub prepare: Pc,
    pub call: Pc,
    pub after_call: Pc,
    pub result: Reg,
    pub layout: ResumeLayout,
}
/// A suspended ordinary call; restoration skips its already evaluated arguments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeReturn {
    pub key: ReturnKey,
    pub callee: FlowKey,
    pub call: Pc,
    pub after_call: Pc,
    pub result: Reg,
    pub layout: ResumeLayout,
}
/// A stable site in one saved activation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResumeSite {
    Point(PointKey),
    Return(ReturnKey),
}
/// One activation, ordered from the outermost flow to the waiting point.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameDescriptor {
    pub flow: FlowKey,
    pub site: ResumeSite,
}
/// A code-independent call chain. Values belong to the separate persistent graph.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuationDescriptor {
    pub frames: Vec<FrameDescriptor>,
}
/// Verification and analysis quotas; defaults follow addressable format limits.
#[derive(Clone, Copy, Debug)]
pub struct ResumeLimits {
    pub work: u64,
    pub allocation_bytes: usize,
}
impl Default for ResumeLimits {
    fn default() -> Self {
        Self {
            work: u64::MAX,
            allocation_bytes: isize::MAX as usize,
        }
    }
}
/// A rejected metadata entry or source continuation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeError {
    pub function: Option<FunctionId>,
    pub pc: Option<Pc>,
    pub detail: String,
}
impl ResumeError {
    pub(crate) fn new(detail: impl Into<String>) -> Self {
        Self {
            function: None,
            pc: None,
            detail: detail.into(),
        }
    }
    pub(crate) fn at(mut self, function: FunctionId, pc: Pc) -> Self {
        self.function = Some(function);
        self.pc = Some(pc);
        self
    }
}
impl std::fmt::Display for ResumeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.detail.fmt(f)
    }
}
impl std::error::Error for ResumeError {}

pub(crate) struct Budget {
    work: u64,
    bytes: usize,
    limits: ResumeLimits,
}
impl Budget {
    pub(crate) fn new(limits: ResumeLimits) -> Self {
        Self {
            work: 0,
            bytes: 0,
            limits,
        }
    }
    pub(crate) fn work(&mut self, n: u64) -> Result<(), ResumeError> {
        self.work = self
            .work
            .checked_add(n)
            .ok_or_else(|| ResumeError::new("resume work overflow"))?;
        if self.work > self.limits.work {
            return Err(ResumeError::new("resume work limit"));
        }
        Ok(())
    }
    pub(crate) fn bytes(&mut self, n: usize) -> Result<(), ResumeError> {
        self.bytes = self
            .bytes
            .checked_add(n)
            .ok_or_else(|| ResumeError::new("resume allocation overflow"))?;
        if self.bytes > self.limits.allocation_bytes {
            return Err(ResumeError::new("resume allocation limit"));
        }
        Ok(())
    }
}
