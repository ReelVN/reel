//! Stable data contracts and current-class reconstruction without executing constructors.
use crate::bytecode::{BuiltinType, BytecodePackage, TypeDef, TypeId};
use crate::{Heap, HeapError, HeapReservation, RootKind, RootOwner, RootToken, Value};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod classes;
mod defaults;
pub use defaults::{DefaultBuild, DefaultPlan};

/// A persistent identity, independent of a package's indices and content version.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TypeKey {
    pub module: String,
    pub name: String,
}
/// A field identity scoped to the class which declared it, including inherited fields.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FieldKey {
    pub owner: TypeKey,
    pub name: String,
}
/// Serializable value contracts. Dynamic data is checked by graph capture.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DataType {
    Builtin(BuiltinType),
    Tuple(Vec<DataType>),
    Union(Vec<DataType>),
    State(TypeKey),
    LogicalRef(String),
}
impl PartialOrd for DataType {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for DataType {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let tag = |v: &Self| match v {
            Self::Builtin(_) => 0,
            Self::Tuple(_) => 1,
            Self::Union(_) => 2,
            Self::State(_) => 3,
            Self::LogicalRef(_) => 4,
        };
        tag(self)
            .cmp(&tag(other))
            .then_with(|| match (self, other) {
                (Self::Builtin(a), Self::Builtin(b)) => (*a as u8).cmp(&(*b as u8)),
                (Self::Tuple(a), Self::Tuple(b)) | (Self::Union(a), Self::Union(b)) => a.cmp(b),
                (Self::State(a), Self::State(b)) => a.cmp(b),
                (Self::LogicalRef(a), Self::LogicalRef(b)) => a.cmp(b),
                _ => std::cmp::Ordering::Equal,
            })
    }
}
impl DataType {
    /// Owned storage retained by a normalized contract, excluding its inline value.
    pub fn retained_bytes(&self) -> usize {
        match self {
            Self::Builtin(_) => 0,
            Self::Tuple(v) | Self::Union(v) => {
                v.capacity() * size_of::<Self>() + v.iter().map(Self::retained_bytes).sum::<usize>()
            }
            Self::State(k) => k.module.capacity() + k.name.capacity(),
            Self::LogicalRef(s) => s.capacity(),
        }
    }
}
/// The stable portion of one field declaration. Display names are diagnostic metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldSchema {
    pub key: FieldKey,
    pub ty: DataType,
}
/// A saved class contract; declaration order is unrelated to compatibility.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSchema {
    pub key: TypeKey,
    pub fields: Vec<FieldSchema>,
}
/// Explicit identities permit source renames while preserving saved type and field keys.
#[derive(Clone, Debug, Default)]
pub struct Identities {
    pub types: BTreeMap<TypeKey, TypeKey>,
    pub fields: BTreeMap<FieldKey, FieldKey>,
    pub logical: BTreeMap<TypeKey, String>,
}
impl Identities {
    fn ty(&self, source: &TypeKey) -> TypeKey {
        self.types.get(source).unwrap_or(source).clone()
    }
    fn field(&self, source: &FieldKey) -> FieldKey {
        self.fields
            .get(source)
            .cloned()
            .unwrap_or_else(|| FieldKey {
                owner: self.ty(&source.owner),
                name: source.name.clone(),
            })
    }
    fn remap(&self, ty: DataType) -> DataType {
        match ty {
            DataType::State(source) => self.logical.get(&source).map_or_else(
                || DataType::State(self.ty(&source)),
                |kind| DataType::LogicalRef(kind.clone()),
            ),
            DataType::Tuple(v) => DataType::Tuple(v.into_iter().map(|t| self.remap(t)).collect()),
            DataType::Union(v) => {
                let mut v = v.into_iter().map(|t| self.remap(t)).collect::<Vec<_>>();
                v.sort();
                v.dedup();
                DataType::Union(v)
            }
            other => other,
        }
    }
}
/// A failure identifies the stable declaration involved in a data operation.
#[derive(Debug)]
pub enum SchemaError {
    Heap(HeapError),
    Allocation,
    Contract(&'static str),
    Field { key: FieldKey, reason: &'static str },
}
impl From<HeapError> for SchemaError {
    fn from(value: HeapError) -> Self {
        Self::Heap(value)
    }
}
impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Field { key, reason } => write!(
                f,
                "{}::{}.{}: {reason}",
                key.owner.module, key.owner.name, key.name
            ),
            Self::Heap(e) => write!(f, "{e}"),
            Self::Allocation => f.write_str("typed data allocation failed"),
            Self::Contract(s) => f.write_str(s),
        }
    }
}
impl std::error::Error for SchemaError {}

/// Normalize a verified package type without retaining package-local IDs.
/// Named types receive their default module/name identity; a Registry applies overrides.
pub fn data_type<C, S>(p: &BytecodePackage<C, S>, id: TypeId) -> Result<DataType, SchemaError> {
    fn convert<C, S>(
        p: &BytecodePackage<C, S>,
        id: TypeId,
        depth: u32,
    ) -> Result<DataType, SchemaError> {
        if depth > 128 {
            return Err(SchemaError::Contract("persistent type nesting exceeds 128"));
        }
        Ok(match &p.types[id.index()] {
            TypeDef::Builtin(
                kind @ (BuiltinType::Any
                | BuiltinType::Null
                | BuiltinType::Bool
                | BuiltinType::Int
                | BuiltinType::Float
                | BuiltinType::String
                | BuiltinType::Bytes
                | BuiltinType::Array
                | BuiltinType::Tuple
                | BuiltinType::Table
                | BuiltinType::Instance),
            ) => DataType::Builtin(*kind),
            TypeDef::Tuple(v) => DataType::Tuple(
                v.iter()
                    .map(|&t| convert(p, t, depth + 1))
                    .collect::<Result<_, _>>()?,
            ),
            TypeDef::Union(v) => {
                let mut v = v
                    .iter()
                    .map(|&t| convert(p, t, depth + 1))
                    .collect::<Result<Vec<_>, _>>()?;
                v.sort();
                v.dedup();
                DataType::Union(v)
            }
            TypeDef::Named { module, name, .. } => DataType::State(TypeKey {
                module: p.symbols[p.modules[module.index()].path.index()].clone(),
                name: p.symbols[name.index()].clone(),
            }),
            _ => {
                return Err(SchemaError::Contract(
                    "executable and weak-reference types are not persistent data",
                ));
            }
        })
    }
    convert(p, id, 0)
}

struct BoundField {
    contract: crate::CellContract,
    default: Option<DefaultPlan>,
    default_error: Option<&'static str>,
}
struct BoundClass {
    schema: StateSchema,
    fields: Vec<BoundField>,
    next: u32,
    ready: bool,
    pending: Option<defaults::Proof>,
    keys: BTreeMap<FieldKey, u32>,
}
/// Current class bindings owned by one candidate heap. Release after capture/restore,
/// including cancellation, to retire the registry's strong class root.
pub struct Registry {
    root: RootToken,
    identities: Identities,
    classes: Vec<BoundClass>,
    by_type: BTreeMap<TypeKey, u32>,
    by_handle: BTreeMap<u64, u32>,
    charge: HeapReservation,
}
impl Registry {
    pub fn new(
        heap: &mut Heap,
        owner: RootOwner,
        identities: Identities,
    ) -> Result<Self, SchemaError> {
        let identity_bytes = identities
            .types
            .iter()
            .map(|(a, b)| {
                128 + a.module.capacity()
                    + a.name.capacity()
                    + b.module.capacity()
                    + b.name.capacity()
            })
            .sum::<usize>()
            + identities
                .fields
                .iter()
                .map(|(a, b)| {
                    128 + a.owner.module.capacity()
                        + a.owner.name.capacity()
                        + a.name.capacity()
                        + b.owner.module.capacity()
                        + b.owner.name.capacity()
                        + b.name.capacity()
                })
                .sum::<usize>()
            + identities
                .logical
                .iter()
                .map(|(a, b)| 96 + a.module.capacity() + a.name.capacity() + b.capacity())
                .sum::<usize>();
        let charge = heap
            .budget_usage()
            .reserve((size_of::<Self>() + identity_bytes) as u64)?;
        let values = heap.alloc_array(&[])?;
        let root = heap.add_root(owner, RootKind::Host, values)?;
        Ok(Self {
            root,
            identities,
            classes: Vec::new(),
            by_type: BTreeMap::new(),
            by_handle: BTreeMap::new(),
            charge,
        })
    }
    pub fn data_type<C, S>(
        &self,
        p: &BytecodePackage<C, S>,
        id: TypeId,
    ) -> Result<DataType, SchemaError> {
        Ok(self.identities.remap(data_type(p, id)?))
    }
    pub fn schema(&self, class: u32) -> Result<&StateSchema, SchemaError> {
        let c = self
            .classes
            .get(class as usize)
            .ok_or(SchemaError::Contract("unknown State class"))?;
        if !c.ready {
            return Err(SchemaError::Contract(
                "State class registration is incomplete",
            ));
        }
        Ok(&c.schema)
    }
    pub fn find(&self, key: &TypeKey) -> Result<u32, SchemaError> {
        self.by_type
            .get(key)
            .copied()
            .filter(|&n| self.classes[n as usize].ready)
            .ok_or(SchemaError::Contract(
                "saved State class is missing from the current registry",
            ))
    }
    pub fn release(self, heap: &mut Heap) -> Result<(), SchemaError> {
        heap.release_root(self.root)?;
        Ok(())
    }
    fn grow(&mut self, bytes: usize) -> Result<(), SchemaError> {
        self.charge.resize(
            self.charge
                .bytes()
                .checked_add(bytes as u64)
                .ok_or(SchemaError::Allocation)?,
        )?;
        Ok(())
    }
}
