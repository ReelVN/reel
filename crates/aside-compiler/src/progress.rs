//! Compile-time observations owned by the caller; events never run author code.

/// A real source-compilation boundary. Discovery has an initially unknown size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressStage {
    Discover,
    Parse,
    Analyze,
    Lower,
    Verify,
}

/// Completed modules in one stage, or one final verification operation.
/// A zero total means the dependency graph is still being discovered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub stage: ProgressStage,
    pub completed: usize,
    pub total: usize,
}
