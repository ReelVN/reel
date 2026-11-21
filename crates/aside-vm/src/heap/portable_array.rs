//! A saved contract can outlive the annotation that first created the array.
//! Reuse verified types where possible and keep any remaining structure in the heap.
use super::*;
use crate::HeapReservation;
use crate::bytecode::{BuiltinType, TypeDef, TypeId};
use crate::typed_data::{DataType, Registry, SchemaError, data_type};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct ArrayShape {
    pub(super) element: DataType,
    package_types: BTreeMap<DataType, TypeId>,
}

pub(super) struct PortableArrayItem {
    pub(super) shape: ArrayShape,
    pub(super) work: u64,
    node: Check,
    _charge: HeapReservation,
}

enum Check {
    Package(TypeId),
    Builtin(BuiltinType),
    String(String),
    Tuple(Vec<Check>),
    Union(Vec<Check>),
    Record(Vec<(String, Check, bool)>),
    Array {
        saved: DataType,
        package_type: Option<TypeId>,
    },
}

impl Heap {
    pub(super) fn typed_array_base(&self) -> usize {
        self.code
            .as_ref()
            .map_or(0, |code| code.package().types.len())
    }

    pub(super) fn portable_item(&self, item: TypeId) -> Option<&PortableArrayItem> {
        let index = item.index().checked_sub(self.typed_array_base())?;
        self.portable_arrays.get(index)
    }

    pub(super) fn matching_array_item(&self, a: TypeId, b: TypeId) -> bool {
        if a == b {
            return true;
        }
        self.code
            .as_ref()
            .is_some_and(|code| code.checks.same_type(a, b))
    }

    fn match_package_type(&self, saved: &DataType, registry: Option<&Registry>) -> Option<TypeId> {
        let package = self.code.as_ref()?.package();
        package
            .types
            .iter()
            .enumerate()
            .find_map(|(index, definition)| {
                let matches_kind = match (definition, saved) {
                    (TypeDef::Builtin(a), DataType::Builtin(b)) => {
                        return (a == b).then_some(TypeId(index as u32));
                    }
                    (TypeDef::Tuple(a), DataType::Tuple(b)) => a.len() == b.len(),
                    (TypeDef::Union(_), DataType::Union(_))
                    | (TypeDef::Named { .. }, DataType::State(_) | DataType::LogicalRef(_))
                    | (TypeDef::Array(_), DataType::Array(_)) => true,
                    (TypeDef::StringLiteral(a), DataType::StringLiteral(b)) => {
                        return (a == b).then_some(TypeId(index as u32));
                    }
                    (TypeDef::Record(a), DataType::Record(b)) => a.len() == b.len(),
                    _ => false,
                };
                if !matches_kind {
                    return None;
                }
                let id = TypeId(index as u32);
                let normalized = if let Some(registry) = registry {
                    registry.data_type(package, id)
                } else {
                    data_type(package, id)
                };
                normalized
                    .as_ref()
                    .is_ok_and(|ty| ty == saved)
                    .then_some(id)
            })
    }

    pub(super) fn resolve_array_item(
        &mut self,
        element: &DataType,
        registry: Option<&Registry>,
    ) -> Result<TypeId, SchemaError> {
        if let Some(item) = self.match_package_type(element, registry) {
            return Ok(item);
        }
        self.restore_array_shape(
            ArrayShape {
                element: element.clone(),
                package_types: BTreeMap::new(),
            },
            registry,
        )
    }

    pub(super) fn restore_array_shape(
        &mut self,
        mut shape: ArrayShape,
        registry: Option<&Registry>,
    ) -> Result<TypeId, SchemaError> {
        let base = self.typed_array_base();
        if let Some(index) = self
            .portable_arrays
            .iter()
            .position(|item| item.shape.element == shape.element)
        {
            return Ok(TypeId(
                u32::try_from(base + index).map_err(|_| SchemaError::Allocation)?,
            ));
        }
        let id = TypeId(
            u32::try_from(base + self.portable_arrays.len())
                .map_err(|_| SchemaError::Allocation)?,
        );
        if id.0 == u32::MAX {
            return Err(SchemaError::Allocation);
        }
        let mut charge = self.budget_usage().reserve(
            (2 * size_of::<PortableArrayItem>() + shape.element.retained_bytes()) as u64,
        )?;
        let (node, work) = Check::build(
            self,
            &shape.element,
            &mut shape.package_types,
            registry,
            &mut charge,
            0,
        )?;
        self.portable_arrays
            .try_reserve(1)
            .map_err(|_| SchemaError::Allocation)?;
        self.portable_arrays.push(PortableArrayItem {
            shape,
            work,
            node,
            _charge: charge,
        });
        Ok(id)
    }
}

impl PortableArrayItem {
    pub(super) fn accepts(&self, heap: &Heap, value: Value) -> Result<bool, HeapError> {
        self.node.accepts(heap, value)
    }
}

impl Check {
    fn build(
        heap: &Heap,
        ty: &DataType,
        package_types: &mut BTreeMap<DataType, TypeId>,
        registry: Option<&Registry>,
        charge: &mut HeapReservation,
        depth: usize,
    ) -> Result<(Self, u64), SchemaError> {
        if depth >= 128 {
            return Err(SchemaError::Contract(
                "array element type nesting exceeds 128",
            ));
        }
        charge.resize(charge.bytes() + size_of::<Check>() as u64)?;
        let attached = if let Some(&id) = package_types.get(ty) {
            Some(id)
        } else if let Some(id) = heap.match_package_type(ty, registry) {
            charge.resize(
                charge.bytes() + (size_of::<DataType>() + ty.retained_bytes() + 128) as u64,
            )?;
            package_types.insert(ty.clone(), id);
            Some(id)
        } else {
            None
        };
        if let Some(id) = attached {
            let code = heap
                .code
                .as_ref()
                .ok_or(SchemaError::Contract("array restore requires code"))?;
            if id.index() >= code.package().types.len() {
                return Err(SchemaError::Contract(
                    "array snapshot type is outside the code package",
                ));
            }
            return Ok((Self::Package(id), code.checks.extra(id) + 1));
        }
        let mut child = |ty| Self::build(heap, ty, package_types, registry, charge, depth + 1);
        Ok(match ty {
            DataType::Builtin(kind) => (Self::Builtin(*kind), 1),
            DataType::StringLiteral(text) => {
                charge.resize(charge.bytes() + text.len() as u64)?;
                (Self::String(text.clone()), 1 + text.len() as u64)
            }
            DataType::Array(element) => {
                let bytes = element.retained_bytes() as u64;
                charge.resize(charge.bytes() + bytes)?;
                let package_type = if let Some(&ty) = package_types.get(element.as_ref()) {
                    Some(ty)
                } else if let Some(ty) = heap.match_package_type(element, registry) {
                    charge.resize(charge.bytes() + bytes + 128)?;
                    package_types.insert((**element).clone(), ty);
                    Some(ty)
                } else {
                    None
                };
                (
                    Self::Array {
                        saved: (**element).clone(),
                        package_type,
                    },
                    2 + bytes,
                )
            }
            DataType::Union(items) | DataType::Tuple(items) => {
                let mut work = 3u64;
                let mut checks = Vec::new();
                checks
                    .try_reserve_exact(items.len())
                    .map_err(|_| SchemaError::Allocation)?;
                for item in items {
                    let (check, units) = child(item)?;
                    checks.push(check);
                    work = work.checked_add(2 + units).ok_or(SchemaError::Allocation)?;
                }
                let node = if matches!(ty, DataType::Tuple(_)) {
                    Self::Tuple(checks)
                } else {
                    Self::Union(checks)
                };
                (node, work)
            }
            DataType::Record(fields) => {
                let mut work = 3u64;
                let mut checks = Vec::new();
                checks
                    .try_reserve_exact(fields.len())
                    .map_err(|_| SchemaError::Allocation)?;
                for (name, item) in fields {
                    let optional = matches!(item, DataType::OptionalField(_));
                    let item = if let DataType::OptionalField(item) = item {
                        item.as_ref()
                    } else {
                        item
                    };
                    let (check, units) = child(item)?;
                    work = work
                        .checked_add(units)
                        .and_then(|n| n.checked_add(fields.len() as u64 * (3 + name.len() as u64)))
                        .ok_or(SchemaError::Allocation)?;
                    checks.push((name.clone(), check, optional));
                }
                let names = fields
                    .iter()
                    .map(|(name, _)| name.len() as u64)
                    .sum::<u64>();
                let keys = fields.len() as u64 * (size_of::<String>() + size_of::<bool>()) as u64;
                charge.resize(charge.bytes() + names + keys)?;
                (Self::Record(checks), work)
            }
            DataType::State(_) | DataType::LogicalRef(_) => {
                return Err(SchemaError::Contract(
                    "saved array element refers to an absent State or native type",
                ));
            }
            DataType::OptionalField(_) => {
                return Err(SchemaError::Contract(
                    "optional array item requires a record field",
                ));
            }
        })
    }

    fn accepts(&self, heap: &Heap, value: Value) -> Result<bool, HeapError> {
        use crate::type_checks::{record_field, string_equal};
        match self {
            Self::Package(ty) => {
                let code = heap.code.as_ref().unwrap();
                code.checks.accepts(code.package(), heap, *ty, value)
            }
            Self::Builtin(ty) => heap.accepts_builtin(*ty, value),
            Self::String(text) => string_equal(heap, value, text),
            Self::Array {
                saved,
                package_type,
            } => Ok(heap.array_item(value)?.is_some_and(|item| {
                package_type.is_some_and(|expected| heap.matching_array_item(item, expected))
                    || heap
                        .portable_item(item)
                        .is_some_and(|item| item.shape.element == *saved)
            })),
            Self::Tuple(items) => {
                if !heap.accepts_builtin(BuiltinType::Tuple, value)?
                    || heap.sequence_len(value)? as usize != items.len()
                {
                    return Ok(false);
                }
                for (index, item) in items.iter().enumerate() {
                    if !item.accepts(heap, heap.sequence_get(value, index as u32)?)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Self::Union(items) => {
                for item in items {
                    if item.accepts(heap, value)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Self::Record(fields) => {
                if !heap.accepts_builtin(BuiltinType::Table, value)?
                    || heap.table_len(value)? as usize > fields.len()
                {
                    return Ok(false);
                }
                let mut found = 0;
                for (index, (name, field, optional)) in fields.iter().enumerate() {
                    if let Some(item) = record_field(heap, value, index, name)? {
                        if !field.accepts(heap, item)? {
                            return Ok(false);
                        }
                        found += 1;
                    } else if !optional {
                        return Ok(false);
                    }
                }
                Ok(heap.table_len(value)? == found)
            }
        }
    }
}
