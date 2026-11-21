//! Stable State contracts are derived once from current class and default metadata.
use super::declarations::{ClassDefinition, definition};
use super::*;
use crate::bytecode as bc;
use crate::typed_data::{DataType, FieldKey, FieldSchema, Identities, StateSchema, TypeKey};
use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::mem::size_of;

/// A normalized persistent contract, ordered by stable field key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateContract {
    pub schema: StateSchema,
    /// Indices of fields that require a saved value because they have no default.
    pub required: Vec<u32>,
}
/// Code-independent Root input identity for build-baseline comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootContract {
    pub key: RootKey,
    pub state: TypeKey,
}
/// Current normalized contracts; code indices and source names do not affect comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateContracts {
    pub states: Vec<StateContract>,
    pub root: Option<RootContract>,
}

fn source_key(p: &bc::RuntimePackage, ty: TypeId) -> Result<TypeKey, ResumeError> {
    match crate::typed_data::data_type(p, ty).map_err(|e| ResumeError::new(e.to_string()))? {
        DataType::State(key) => Ok(key),
        _ => Err(ResumeError::new("State class nominal type is not named")),
    }
}
fn owned_global(
    program: &bc::VerifiedProgram,
    module: ModuleId,
    global: GlobalId,
    b: &mut Budget,
) -> Result<(ModuleId, GlobalId), ResumeError> {
    let binding = program
        .global_binding(module, global)
        .map_err(|e| ResumeError::new(e.to_string()))?;
    if !binding.imported {
        return Ok((module, global));
    }
    for (index, m) in program.package().modules.iter().enumerate() {
        b.work(1)?;
        if m.globals.is_empty() {
            continue;
        }
        let start = program
            .global_binding(ModuleId(index as u32), GlobalId(0))
            .map_err(|e| ResumeError::new(e.to_string()))?
            .offset;
        if binding.offset >= start && binding.offset - start < m.globals.len() as u32 {
            return Ok((ModuleId(index as u32), GlobalId(binding.offset - start)));
        }
    }
    Err(ResumeError::new("State base has no owned module binding"))
}
fn desc<'a>(p: &'a bc::RuntimePackage, class: &ClassLocator) -> &'a bc::ClassDesc {
    let bc::Descriptor::Class(desc) =
        &p.functions[class.function.index()].descriptors[class.descriptor.index()]
    else {
        unreachable!()
    };
    desc
}
fn check_named_types(
    ty: &DataType,
    scripted: &BTreeSet<(&str, &str)>,
    identities: &Identities,
    native_admission: bool,
    b: &mut Budget,
) -> Result<(), ResumeError> {
    b.work(1)?;
    match ty {
        DataType::State(key)
            if scripted.contains(&(key.module.as_str(), key.name.as_str()))
                && !identities.types.contains_key(key) =>
        {
            return Err(ResumeError::new(format!(
                "class {}::{} has no state declaration",
                key.module, key.name
            )));
        }
        DataType::State(key)
            if native_admission
                && !identities.types.contains_key(key)
                && !identities.logical.contains_key(key) =>
        {
            return Err(ResumeError::new(format!(
                "named type {}::{} is neither a declared State nor a trusted native type",
                key.module, key.name
            )));
        }
        DataType::Union(items) | DataType::Tuple(items) => {
            for item in items {
                check_named_types(item, scripted, identities, native_admission, b)?;
            }
        }
        DataType::Array(item) | DataType::OptionalField(item) => {
            check_named_types(item, scripted, identities, native_admission, b)?;
        }
        DataType::Record(fields) => {
            for (_, ty) in fields {
                check_named_types(ty, scripted, identities, native_admission, b)?;
            }
        }
        _ => {}
    }
    Ok(())
}
pub(super) fn normalize(
    program: &bc::VerifiedProgram,
    table: &ResumeTable,
    host_types: Option<&[crate::HostTypeSupport]>,
    b: &mut Budget,
) -> Result<StateContracts, ResumeError> {
    let p = program.package();
    if table.states.is_empty() && table.root.is_none() {
        return Ok(StateContracts {
            states: Vec::new(),
            root: None,
        });
    }
    let mut identities = Identities::default();
    let mut stable = BTreeSet::new();
    let mut definitions = BTreeMap::<u32, ClassDefinition>::new();
    let mut declarations = BTreeMap::new();
    let mut scripted = BTreeSet::new();
    for function in &p.functions {
        for descriptor in &function.descriptors {
            b.work(1)?;
            if let bc::Descriptor::Class(class) = descriptor
                && let Some(nominal) = class.nominal
                && let bc::TypeDef::Named { module, name, .. } = p.types[nominal.index()]
            {
                b.bytes(4 * (size_of::<(&str, &str)>() + 64))?;
                scripted.insert((
                    p.symbols[p.modules[module.index()].path.index()].as_str(),
                    p.symbols[name.index()].as_str(),
                ));
            }
        }
    }
    if let Some(host_types) = host_types {
        let mut names = BTreeSet::new();
        for host in host_types {
            b.work(1)?;
            b.bytes(4 * (size_of::<(&str, &str)>() + 64))?;
            let Some((module, name)) = host.name.rsplit_once("::") else {
                return Err(ResumeError::new(
                    "trusted native type requires a canonical public name",
                ));
            };
            if module.is_empty()
                || name.is_empty()
                || host.version == 0
                || !names.insert((module, name))
            {
                return Err(ResumeError::new("invalid or duplicate trusted native type"));
            }
        }
        for ty in &p.types {
            b.work(1)?;
            if let bc::TypeDef::Named { module, name, .. } = *ty {
                let module = p.symbols[p.modules[module.index()].path.index()].as_str();
                let name = p.symbols[name.index()].as_str();
                if !names.contains(&(module, name)) {
                    continue;
                }
                if scripted.contains(&(module, name)) {
                    return Err(ResumeError::new(format!(
                        "script class {module}::{name} conflicts with a trusted native type"
                    )));
                }
                let canonical = format!("{module}::{name}");
                b.bytes(
                    4 * (size_of::<(TypeKey, String)>() + 64)
                        + module.len()
                        + name.len()
                        + canonical.capacity(),
                )?;
                identities.logical.insert(
                    TypeKey {
                        module: module.into(),
                        name: name.into(),
                    },
                    canonical,
                );
            }
        }
    }
    for state in &table.states {
        b.work(1)?;
        b.bytes(state.key.module.len() + state.key.name.len() + size_of::<StateContract>())?;
        if state.key.name.is_empty() || !stable.insert(&state.key) {
            return Err(ResumeError::new("duplicate or empty State key"));
        }
        let current = definition(program, state.class.module, state.class.global, b)?;
        if current.locator != state.class {
            return Err(ResumeError::new(
                "State class locator differs from its current module declaration",
            ));
        }
        let source = source_key(p, state.class.nominal)?;
        if identities
            .types
            .insert(source.clone(), state.key.clone())
            .is_some()
        {
            return Err(ResumeError::new(
                "one class has multiple State declarations",
            ));
        }
        let offset = program
            .global_binding(state.class.module, state.class.global)
            .map_err(|e| ResumeError::new(e.to_string()))?
            .offset;
        if declarations.insert(offset, state).is_some() {
            return Err(ResumeError::new(
                "one class binding has multiple State declarations",
            ));
        }
        let fields = desc(p, &state.class)
            .fields
            .iter()
            .filter(|f| !f.is_static)
            .map(|f| f.name)
            .collect::<BTreeSet<_>>();
        let mut overrides = BTreeSet::new();
        let mut keys = BTreeSet::new();
        for field in &state.fields {
            b.work(1)?;
            b.bytes(field.key.len())?;
            if !fields.contains(&field.field)
                || !overrides.insert(field.field)
                || field.key.is_empty()
                || !keys.insert(&field.key)
            {
                return Err(ResumeError::new(
                    "State field override is unknown, repeated or has an empty/repeated key",
                ));
            }
            identities.fields.insert(
                FieldKey {
                    owner: source.clone(),
                    name: p.symbols[field.field.index()].clone(),
                },
                FieldKey {
                    owner: state.key.clone(),
                    name: field.key.clone(),
                },
            );
        }
        definitions.insert(offset, current);
    }
    let mut states = Vec::new();
    for state in &table.states {
        let mut lineage = Vec::new();
        let mut visiting = BTreeSet::new();
        let mut target = Some((state.class.module, state.class.global));
        while let Some((module, global)) = target {
            b.work(1)?;
            let (module, global) = owned_global(program, module, global, b)?;
            let offset = program
                .global_binding(module, global)
                .map_err(|e| ResumeError::new(e.to_string()))?
                .offset;
            if !visiting.insert(offset) {
                return Err(ResumeError::new("State inheritance is cyclic"));
            }
            let current = match definitions.entry(offset) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => entry.insert(definition(program, module, global, b)?),
            };
            target = current.base;
            lineage.push(offset);
        }
        let mut entries = Vec::new();
        let mut names = BTreeSet::new();
        for offset in lineage.into_iter().rev() {
            let current = &definitions[&offset];
            let definition = desc(p, &current.locator);
            let owner = declarations.get(&offset);
            for field in definition.fields.iter().filter(|f| !f.is_static) {
                b.work(1)?;
                let owner = owner.ok_or_else(|| {
                    ResumeError::new(
                        "a base class that declares persistent fields also requires state(...)",
                    )
                })?;
                if !names.insert(field.name) {
                    return Err(ResumeError::new("State inheritance repeats a field name"));
                }
                let key = identities.field(&FieldKey {
                    owner: source_key(p, current.locator.nominal)?,
                    name: p.symbols[field.name.index()].clone(),
                });
                let ty = crate::typed_data::data_type(p, field.ty).map_err(|e| {
                    ResumeError::new(format!(
                        "State {} field {}: {e}",
                        owner.key.name,
                        p.symbols[field.name.index()]
                    ))
                })?;
                check_named_types(&ty, &scripted, &identities, host_types.is_some(), b).map_err(
                    |error| {
                        ResumeError::new(format!(
                            "State {} field {}: {error}",
                            owner.key.name,
                            p.symbols[field.name.index()]
                        ))
                    },
                )?;
                let ty = identities.remap(ty);
                b.bytes(
                    size_of::<FieldSchema>()
                        + key.owner.module.len()
                        + key.owner.name.len()
                        + key.name.len()
                        + ty.retained_bytes(),
                )?;
                if let Some(function) = field.initializer {
                    let usage = crate::HeapBudgetUsage::new(
                        b.limits.allocation_bytes.saturating_sub(b.bytes) as u64,
                    );
                    let mut proof = crate::typed_data::Proof::new(program, &usage, function)
                        .map_err(|e| {
                            ResumeError::new(format!(
                                "State {} field {} default: {e}",
                                owner.key.name,
                                p.symbols[field.name.index()]
                            ))
                            .at(function, Pc(0))
                        })?;
                    loop {
                        b.work(1)?;
                        if proof.step().map_err(|e| {
                            ResumeError::new(format!(
                                "State {} field {} default: {e}",
                                owner.key.name,
                                p.symbols[field.name.index()]
                            ))
                            .at(function, Pc(0))
                        })? {
                            break;
                        }
                    }
                }
                entries.push((FieldSchema { key, ty }, field.initializer.is_none()));
            }
        }
        b.work(entries.len() as u64 * (usize::BITS - entries.len().max(1).leading_zeros()) as u64)?;
        entries.sort_by(|a, b| a.0.key.cmp(&b.0.key));
        if entries.windows(2).any(|v| v[0].0.key == v[1].0.key) {
            return Err(ResumeError::new("State fields reuse a persistent key"));
        }
        let mut required = Vec::new();
        let mut fields = Vec::new();
        for (field, (schema, is_required)) in entries.into_iter().enumerate() {
            if is_required {
                required.push(field as u32);
            }
            fields.push(schema);
        }
        states.push(StateContract {
            schema: StateSchema {
                key: state.key.clone(),
                fields,
            },
            required,
        });
    }
    b.work(states.len() as u64 * (usize::BITS - states.len().max(1).leading_zeros()) as u64)?;
    states.sort_by(|a, b| a.schema.key.cmp(&b.schema.key));
    let root = if let Some(root) = &table.root {
        if root.key.0.is_empty() {
            return Err(ResumeError::new("Root key cannot be empty"));
        }
        let state = table
            .states
            .iter()
            .find(|s| s.key == root.state)
            .ok_or_else(|| ResumeError::new("Root State has no state declaration"))?;
        let f = p
            .functions
            .get(root.function.index())
            .ok_or_else(|| ResumeError::new("Root bootstrap function is missing"))?;
        let signature = &p.signatures[f.signature.index()];
        if f.kind != bc::FunctionKind::Plain
            || f.context != bc::Context::Ordinary
            || !f.captures.is_empty()
            || signature.parameters.len() != 1
            || signature.rest.is_some()
        {
            return Err(ResumeError::new(
                "Root bootstrap requires one ordinary State parameter with a default value",
            )
            .at(root.function, Pc(0)));
        }
        let parameter = &signature.parameters[0];
        let expected = DataType::State(state.key.clone());
        let actual = identities.remap(
            crate::typed_data::data_type(p, parameter.ty)
                .map_err(|e| ResumeError::new(e.to_string()))?,
        );
        if actual != expected || parameter.default.is_none() {
            return Err(ResumeError::new(
                "Root bootstrap parameter must be its direct State type with a default value",
            )
            .at(root.function, Pc(0)));
        }
        Some(RootContract {
            key: root.key.clone(),
            state: root.state.clone(),
        })
    } else {
        None
    };
    Ok(StateContracts { states, root })
}
impl VerifiedResumeTable {
    /// Read normalized persistent contracts for release-baseline comparison.
    /// Names, layout indices and function positions remain current-code details.
    /// Native field identities use the embedder's existing trusted declarations;
    /// a Named bytecode record alone does not establish native identity.
    pub fn normalized_state_contracts(
        &self,
        host_types: &[crate::HostTypeSupport],
        limits: ResumeLimits,
    ) -> Result<StateContracts, ResumeError> {
        normalize(
            &self.program,
            self.table(),
            Some(host_types),
            &mut Budget::new(limits),
        )
    }
}
