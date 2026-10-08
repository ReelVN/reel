//! Explicit blocks and source spans layered on the shared instruction ABI.
use super::{Metadata, Options, Stats, budget::Budget};
use crate::{TextRange, hir};
use aside_vm::bytecode as bc;
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockId(pub u32);
#[derive(Debug)]
pub struct Operation {
    pub instruction: bc::Instruction,
    pub span: TextRange,
}
#[derive(Debug)]
pub enum Terminator {
    Jump(BlockId),
    Leave(BlockId),
    BranchBool {
        value: bc::Reg,
        yes: BlockId,
        no: BlockId,
    },
    BranchNull {
        value: bc::Reg,
        null: BlockId,
        value_block: BlockId,
    },
    Return(bc::Reg),
    Throw(bc::Reg),
    Rethrow,
    EndFinally(bc::RegionId),
}
#[derive(Debug)]
pub struct Block {
    pub operations: Vec<Operation>,
    pub terminator: Option<(Terminator, TextRange)>,
}
/// Ranges refer to positions in the function's explicit layout sequence.
/// Emission converts these boundaries to B01 PcRange, without routing cleanup.
#[derive(Debug)]
pub struct Region {
    pub protected: Range<u32>,
    pub catch: Option<(Range<u32>, bc::Reg)>,
    pub finally: Option<Range<u32>>,
}
#[derive(Debug)]
pub enum Origin {
    Module,
    Function(hir::FunctionId),
    Default {
        function: hir::FunctionId,
        parameter: u32,
    },
}
#[derive(Debug)]
pub struct Function {
    pub output: bc::FunctionId,
    pub origin: Origin,
    pub span: TextRange,
    pub blocks: Vec<Block>,
    pub layout: Vec<BlockId>,
    pub regions: Vec<Region>,
}
#[derive(Debug)]
pub struct IrModule {
    /// Shared tables and function contracts. Lowered function code is populated by emit.
    pub(crate) package: bc::BytecodePackage,
    pub(crate) functions: Vec<Function>,
    pub source: bc::SourceId,
    pub module: bc::ModuleId,
    pub entry: bc::FunctionId,
    pub(crate) stats: Stats,
    pub(crate) metadata: Metadata,
    pub(crate) options: Options,
    pub(crate) budget: Budget,
}
impl IrModule {
    pub fn tables(&self) -> &bc::BytecodePackage {
        &self.package
    }
    pub fn functions(&self) -> &[Function] {
        &self.functions
    }
    pub fn stats(&self) -> Stats {
        self.stats
    }
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}
