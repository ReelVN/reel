//! Source-only value summaries over bound HIR. Runtime contracts remain unchanged.
use super::{ExternalSymbol, TypeHint};
use crate::{
    TextRange,
    hir::{NodeId, SymbolId},
};
use std::{collections::HashMap, sync::Arc};

mod infer;
pub(crate) use infer::analyze;

/// The original declaration of a source record field, including through imports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDefinition {
    pub module: Arc<str>,
    pub range: TextRange,
}

/// A known field of an ordinary writable table. This is editor information,
/// not a structural runtime contract or a promise that other keys are absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMember {
    pub name: String,
    pub value: Arc<SourceType>,
    pub definition: SourceDefinition,
}

/// Additional information retained for source values without changing runtime
/// representation or the permissions of an explicit annotation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceShape {
    Record(Vec<SourceMember>),
    /// Fixed slots preserve element summaries, including iterator result pairs.
    Tuple(Arc<[Arc<SourceType>]>),
    Array {
        element: Arc<SourceType>,
        /// Literal slots preserve shallow aliases. None means only the element summary is known.
        values: Option<Arc<[Arc<SourceType>]>>,
    },
    /// The yielded element and final return value belong to separate contracts.
    Generator {
        element: Arc<SourceType>,
        result: Arc<SourceType>,
    },
}

/// A shared source value summary. `ty` is a normal language type; the optional
/// shape and call result refine editor queries and exported module metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceType {
    pub ty: TypeHint,
    /// An opaque mutable escape keeps this container dynamic in subsequent flows.
    pub escaped: bool,
    pub shape: Option<SourceShape>,
    pub call_result: Option<Arc<SourceType>>,
}
impl SourceType {
    pub fn new(ty: TypeHint) -> Self {
        Self {
            ty,
            escaped: false,
            shape: None,
            call_result: None,
        }
    }
    pub fn member(&self, name: &str) -> Option<&SourceMember> {
        match &self.shape {
            Some(SourceShape::Record(fields)) => fields.iter().find(|field| field.name == name),
            _ => None,
        }
    }
    pub fn element(&self) -> Option<&Arc<Self>> {
        match &self.shape {
            Some(SourceShape::Array { element, .. } | SourceShape::Generator { element, .. }) => {
                Some(element)
            }
            _ => None,
        }
    }
}

/// Compiler-owned query facts, indexed by the original bound nodes/symbols.
/// Shared summaries retain no VM values and are discarded with analysis.
#[derive(Debug, Clone, Default)]
pub struct SourceTypes {
    pub(crate) interface: Option<super::ModuleInterface>,
    pub(crate) work_units: usize,
    pub(crate) values: Vec<Option<Arc<SourceType>>>,
    pub(crate) symbols: Vec<Option<Arc<SourceType>>>,
    pub(crate) functions: Vec<Option<Arc<SourceType>>>,
    pub(crate) definitions: HashMap<NodeId, SourceDefinition>,
    pub(crate) external: HashMap<NodeId, Arc<ExternalSymbol>>,
}
impl SourceTypes {
    /// Work used to build this module's source-only summaries. Zero means no
    /// summary was retained, including a bounded-analysis fallback.
    pub fn work_units(&self) -> usize {
        self.work_units
    }
    /// Export the same inferred values through an ordinary module resolver.
    pub fn module_interface(&self) -> Option<&super::ModuleInterface> {
        self.interface.as_ref()
    }
    pub fn value(&self, node: NodeId) -> Option<&Arc<SourceType>> {
        self.values.get(node.0 as usize)?.as_ref()
    }
    pub fn symbol(&self, symbol: SymbolId) -> Option<&Arc<SourceType>> {
        self.symbols.get(symbol.0 as usize)?.as_ref()
    }
    pub fn function_result(&self, function: crate::hir::FunctionId) -> Option<&Arc<SourceType>> {
        self.functions.get(function.0 as usize)?.as_ref()
    }
    pub fn definition(&self, node: NodeId) -> Option<&SourceDefinition> {
        self.definitions.get(&node)
    }
    pub fn external(&self, node: NodeId) -> Option<&ExternalSymbol> {
        self.external.get(&node).map(AsRef::as_ref)
    }
}
