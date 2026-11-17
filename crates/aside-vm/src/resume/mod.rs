//! Stable logical continuations verified against the current program.
//! Serialized keys never grant permission to inject a function or instruction PC.
mod analysis;
mod contracts;
mod declarations;
pub use contracts::{RootContract, StateContract, StateContracts};
mod plan;
mod targets;
mod verify;
pub use declarations::class_locator;

use crate::bytecode::{
    BindingId, DescriptorId, FunctionId, GlobalId, ModuleId, Pc, Reg, SymbolId, TypeId,
};
pub use crate::executor::resume::{
    CaptureProgress, CapturedBinding, ResumeCapture, ResumeCaptureCursor, ResumeDescriptor,
    ResumeDescriptorBound, ResumeFrameRef, ResumeFrameValues, ResumeFrames, ResumeProgress,
    ResumeRestore, ResumeRestoreCursor, ResumeSiteRef, ResumeValue,
};
use crate::typed_data::TypeKey;
pub use analysis::{FlowAnalysis, SiteLayout, analyze_site};
pub(crate) use plan::{PlannedFrame, PlannedSite};
pub use plan::{VerifiedResumePlan, VerifiedResumeTable};
use serde::{Deserialize, Serialize};
pub use verify::{static_call_target, static_global_function};

macro_rules! key {
    ($($name:ident),*) => {$(
        #[doc = "An explicit stable source identity, independent of numeric code layout."]
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub struct $name(pub String);
        impl From<&str> for $name { fn from(value: &str) -> Self { Self(value.into()) } }
    )*};
}
key!(FlowKey, PointKey, ReturnKey, BindingKey, RootKey);

/// Current immutable module binding and class descriptor, never written to a saved graph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassLocator {
    pub module: ModuleId,
    pub global: GlobalId,
    pub function: FunctionId,
    pub descriptor: DescriptorId,
    pub nominal: TypeId,
}
/// An explicit stable identity for one field declared by its owning State class.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldKeyOverride {
    pub field: SymbolId,
    pub key: String,
}
/// A module-level State protocol declaration attached to the current class definition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateDecl {
    pub key: TypeKey,
    pub class: ClassLocator,
    pub fields: Vec<FieldKeyOverride>,
}
/// The current bootstrap and its persistent State input contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootDecl {
    pub key: RootKey,
    pub function: FunctionId,
    pub state: TypeKey,
}

/// Current-code metadata. PCs are compact execution word offsets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeTable {
    pub version: u32,
    pub flows: Vec<ResumeFlow>,
    pub states: Vec<StateDecl>,
    pub root: Option<RootDecl>,
}
impl Default for ResumeTable {
    fn default() -> Self {
        Self {
            version: 1,
            flows: Vec::new(),
            states: Vec::new(),
            root: None,
        }
    }
}
impl ResumeTable {
    /// Whether the project declares no logical continuation, State or Root contracts.
    pub fn is_empty(&self) -> bool {
        self.flows.is_empty() && self.states.is_empty() && self.root.is_none()
    }
}
/// A plain function participating in logical continuation capture.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeFlow {
    pub key: FlowKey,
    pub function: FunctionId,
    pub layouts: Vec<ResumeLayout>,
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
    /// Initialized mutable registers whose old values are dead. Restoration
    /// preserves the right to assign them without retaining their old contents.
    pub initialized_dead: Vec<BindingId>,
}
/// A complete native call or await whose read-only inputs may be prepared again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumePoint {
    pub key: PointKey,
    pub prepare: Pc,
    /// Current word PC of the ordinary Call, CallNoWait or Await instruction.
    pub call: Pc,
    pub after_call: Pc,
    pub result: Reg,
    /// Index into the current flow's continuation layouts.
    pub layout: u32,
}
/// A suspended ordinary call; restoration skips its already evaluated arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeReturn {
    pub key: ReturnKey,
    /// Current declared function. Saved call chains retain the stable flow key.
    pub callee: FunctionId,
    pub call: Pc,
    pub after_call: Pc,
    pub result: Reg,
    /// Index into the current flow's continuation layouts.
    pub layout: u32,
}

// The ASB stores these high-volume records as positional sequences.
macro_rules! site_wire {
    ($site:ident { $($field:ident),+ }) => {
        impl Serialize for $site {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                ($(&self.$field,)+).serialize(serializer)
            }
        }

        impl<'de> Deserialize<'de> for $site {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let ($($field,)+) = Deserialize::deserialize(deserializer)?;
                Ok(Self { $($field,)+ })
            }
        }
    };
}
site_wire!(ResumePoint {
    key,
    prepare,
    call,
    after_call,
    result,
    layout
});
site_wire!(ResumeReturn {
    key,
    callee,
    call,
    after_call,
    result,
    layout
});
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
