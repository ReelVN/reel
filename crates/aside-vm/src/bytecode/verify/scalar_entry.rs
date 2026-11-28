//! Fixed Value entries need neither parameter cells nor default evaluation.
use super::*;

impl VerifiedProgram {
    #[cfg(test)]
    pub(crate) fn scalar_entry(&self, function: FunctionId) -> bool {
        self.0.scalar_entries.values[function.index()].primitive
    }
    pub(crate) fn value_entry(&self, function: FunctionId) -> Option<&ValueEntry> {
        let entry = &self.0.scalar_entries.values[function.index()];
        entry.eligible.then_some(entry)
    }
    #[cfg(test)]
    pub(crate) fn scalar_entry_capacity_bytes(&self) -> usize {
        self.0.scalar_entries.capacity_bytes()
    }
}

#[derive(Debug)]
pub(super) struct ScalarEntries {
    values: Vec<ValueEntry>,
}
impl ScalarEntries {
    pub(super) fn capacity_bytes(&self) -> usize {
        self.values.capacity() * size_of::<ValueEntry>()
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ValueEntry {
    pub(crate) locals: u64,
    pub(crate) bindings: u16,
    #[cfg(any(test, feature = "execution-counts"))]
    pub(crate) clears: u16,
    pub(crate) primitive: bool,
    eligible: bool,
}

pub(super) fn build(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence, impl DescriptorSequence>,
    b: &mut Budget,
) -> Result<ScalarEntries, Error> {
    let mut values = {
        let mut values = b.vec(p.functions.len())?;
        values.resize(p.functions.len(), ValueEntry::default());
        values
    };
    for (index, f) in p.functions.iter().enumerate() {
        b.work(1)?;
        let signature = &p.signatures[f.signature.index()];
        if f.kind != FunctionKind::Plain
            || !f.captures.is_empty()
            || signature.parameters.len() > 4
            || signature.rest.is_some()
            || f.bindings.len() > 32
        {
            continue;
        }
        b.work((signature.parameters.len() + f.bindings.len()) as u64)?;
        if signature
            .parameters
            .iter()
            .any(|parameter| parameter.default.is_some())
        {
            continue;
        }
        let primitive_parameters = signature.parameters.iter().all(|parameter| {
            matches!(
                p.types[parameter.ty.index()],
                TypeDef::Builtin(
                    BuiltinType::Null | BuiltinType::Bool | BuiltinType::Int | BuiltinType::Float
                )
            )
        });
        let mut locals = 0;
        let mut clears = 0;
        let valid =
            f.bindings
                .iter()
                .all(|binding| match (binding.initialization, binding.storage) {
                    (
                        BindingInitialization::Parameter(parameter),
                        BindingStorage::Value(register),
                    ) => parameter == register.0,
                    (BindingInitialization::Explicit, BindingStorage::Value(register))
                        if register.index() >= signature.parameters.len() && register.0 < 64 =>
                    {
                        locals |= 1 << register.0;
                        clears += 1;
                        true
                    }
                    _ => false,
                });
        if valid {
            values[index] = ValueEntry {
                locals,
                #[cfg(any(test, feature = "execution-counts"))]
                clears,
                bindings: f.bindings.len() as u16,
                primitive: primitive_parameters && clears == 0 && f.bindings.len() <= 8,
                eligible: true,
            };
        }
    }
    Ok(ScalarEntries { values })
}
