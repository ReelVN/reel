//! Opaque effect definitions and verified bytecode-function bindings in ASB v1.
use super::{Budget, Error, ErrorKind, Limits, require};
use aside_vm::bytecode as bc;
use std::collections::BTreeSet;

/// Content identity of one canonical effect definition, including its parameters.
pub type EffectDefinitionId = [u8; 32];

/// The engine owns the definition schema and validates its typed expression graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectDefinition {
    pub id: EffectDefinitionId,
    pub bytes: Vec<u8>,
}

/// The entry that selects a shared compiled definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffectBindingKind {
    /// A typed effect function called by the author.
    Function,
    /// A pure mask construction template; bytes are validated by the engine.
    MaskTemplate { key: Vec<u8> },
}
/// A final linked source owner and typed use of one shared definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectBinding {
    pub module: u32,
    pub function: u32,
    pub definition: u32,
    pub kind: EffectBindingKind,
}

/// Compiler products retained independently of debug information and GPU targets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EffectProducts {
    pub definitions: Vec<EffectDefinition>,
    pub bindings: Vec<EffectBinding>,
}

impl EffectProducts {
    /// Empty products omit the effect section entirely.
    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty() && self.bindings.is_empty()
    }

    pub(crate) fn validate(
        &self,
        program: &bc::VerifiedProgram,
        limits: Limits,
        budget: &mut Budget,
    ) -> Result<(), Error> {
        require(
            self.definitions.len().saturating_add(self.bindings.len())
                <= limits.max_metadata_records,
            ErrorKind::Limit,
            "effect product record budget exceeded",
        )?;
        let mut identities = BTreeSet::new();
        for definition in &self.definitions {
            budget.charge(definition.bytes.len())?;
            require(
                !definition.bytes.is_empty(),
                ErrorKind::Encoding,
                "empty effect definition",
            )?;
            require(
                blake3::hash(&definition.bytes).as_bytes() == &definition.id,
                ErrorKind::Integrity,
                "effect definition digest mismatch",
            )?;
            require(
                identities.insert(definition.id),
                ErrorKind::Metadata,
                "duplicate effect definition",
            )?;
        }
        let mut functions = BTreeSet::new();
        let mut used = BTreeSet::new();
        for binding in &self.bindings {
            budget.charge(1)?;
            let function = program
                .package()
                .functions
                .get(binding.function as usize)
                .ok_or_else(|| Error::new(ErrorKind::Metadata, "effect function out of range"))?;
            require(
                function.module.0 == binding.module
                    && (!matches!(binding.kind, EffectBindingKind::Function)
                        || (function.kind == bc::FunctionKind::Plain
                            && function.captures.is_empty())),
                ErrorKind::Metadata,
                "effect function must belong to its module and be plain without captures",
            )?;
            if let EffectBindingKind::MaskTemplate { key } = &binding.kind {
                budget.charge(key.len())?;
                require(
                    !key.is_empty(),
                    ErrorKind::Metadata,
                    "empty mask template key",
                )?;
            }
            require(
                !matches!(binding.kind, EffectBindingKind::Function)
                    || functions.insert(binding.function),
                ErrorKind::Metadata,
                "duplicate effect function binding",
            )?;
            require(
                (binding.definition as usize) < self.definitions.len(),
                ErrorKind::Metadata,
                "effect definition index out of range",
            )?;
            used.insert(binding.definition);
        }
        require(
            used.len() == self.definitions.len(),
            ErrorKind::Metadata,
            "unbound effect definition",
        )
    }

    pub(crate) fn encode(&self, limits: Limits) -> Result<Vec<u8>, Error> {
        let count =
            |n| u32::try_from(n).map_err(|_| Error::new(ErrorKind::Limit, "effect count overflow"));
        let definitions = count(self.definitions.len())?;
        let bindings = count(self.bindings.len())?;
        let mut length = 8usize
            .checked_add(self.bindings.len().checked_mul(17).ok_or_else(size_error)?)
            .ok_or_else(size_error)?;
        for binding in &self.bindings {
            if let EffectBindingKind::MaskTemplate { key } = &binding.kind {
                length = length.checked_add(key.len()).ok_or_else(size_error)?;
            }
        }
        for definition in &self.definitions {
            length = length
                .checked_add(40)
                .and_then(|n| n.checked_add(definition.bytes.len()))
                .ok_or_else(size_error)?;
        }
        require(
            length <= limits.max_metadata_bytes,
            ErrorKind::Limit,
            "effect product byte budget exceeded",
        )?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|e| Error::new(ErrorKind::Limit, e.to_string()))?;
        bytes.extend_from_slice(&definitions.to_le_bytes());
        bytes.extend_from_slice(&bindings.to_le_bytes());
        for definition in &self.definitions {
            bytes.extend_from_slice(&definition.id);
            bytes.extend_from_slice(&(definition.bytes.len() as u64).to_le_bytes());
            bytes.extend_from_slice(&definition.bytes);
        }
        for binding in &self.bindings {
            let key = match &binding.kind {
                EffectBindingKind::Function => &[][..],
                EffectBindingKind::MaskTemplate { key } => key.as_slice(),
            };
            bytes.push(u8::from(matches!(
                binding.kind,
                EffectBindingKind::MaskTemplate { .. }
            )));
            for value in [binding.module, binding.function, binding.definition] {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
            bytes.extend_from_slice(&count(key.len())?.to_le_bytes());
            bytes.extend_from_slice(key);
        }
        Ok(bytes)
    }

    pub(crate) fn decode(bytes: &[u8], limits: Limits, budget: &mut Budget) -> Result<Self, Error> {
        require(
            bytes.len() <= limits.max_metadata_bytes,
            ErrorKind::Limit,
            "effect product byte budget exceeded",
        )?;
        budget.charge(bytes.len())?;
        let mut input = bytes;
        let definitions = u32::from_le_bytes(take(&mut input, 4)?.try_into().unwrap()) as usize;
        let bindings = u32::from_le_bytes(take(&mut input, 4)?.try_into().unwrap()) as usize;
        require(
            definitions.saturating_add(bindings) <= limits.max_metadata_records,
            ErrorKind::Limit,
            "effect product record budget exceeded",
        )?;
        let fixed = definitions
            .checked_mul(40)
            .and_then(|n| bindings.checked_mul(17).and_then(|b| n.checked_add(b)))
            .ok_or_else(size_error)?;
        require(
            fixed <= input.len(),
            ErrorKind::Encoding,
            "truncated effect product tables",
        )?;
        // Scan every length before reserving either table or copying any payload.
        let mut scan = input;
        for _ in 0..definitions {
            take(&mut scan, 32)?;
            let length =
                usize::try_from(u64::from_le_bytes(take(&mut scan, 8)?.try_into().unwrap()))
                    .map_err(|_| size_error())?;
            take(&mut scan, length)?;
        }
        for _ in 0..bindings {
            let kind = take(&mut scan, 1)?[0];
            take(&mut scan, 12)?;
            let length = u32::from_le_bytes(take(&mut scan, 4)?.try_into().unwrap()) as usize;
            require(
                kind <= 1 && ((kind == 0) == (length == 0)),
                ErrorKind::Encoding,
                "invalid effect binding kind",
            )?;
            take(&mut scan, length)?;
        }
        require(
            scan.is_empty(),
            ErrorKind::Encoding,
            "effect product length mismatch",
        )?;
        let mut products = Self::default();
        products
            .definitions
            .try_reserve_exact(definitions)
            .map_err(|e| Error::new(ErrorKind::Limit, e.to_string()))?;
        products
            .bindings
            .try_reserve_exact(bindings)
            .map_err(|e| Error::new(ErrorKind::Limit, e.to_string()))?;
        for _ in 0..definitions {
            let id = take(&mut input, 32)?.try_into().unwrap();
            let length =
                usize::try_from(u64::from_le_bytes(take(&mut input, 8)?.try_into().unwrap()))
                    .map_err(|_| size_error())?;
            let payload = take(&mut input, length)?;
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(length)
                .map_err(|e| Error::new(ErrorKind::Limit, e.to_string()))?;
            bytes.extend_from_slice(payload);
            products.definitions.push(EffectDefinition { id, bytes });
        }
        for _ in 0..bindings {
            let kind = take(&mut input, 1)?[0];
            let mut word = || -> Result<u32, Error> {
                Ok(u32::from_le_bytes(take(&mut input, 4)?.try_into().unwrap()))
            };
            let module = word()?;
            let function = word()?;
            let definition = word()?;
            let length = word()? as usize;
            let key = take(&mut input, length)?;
            let kind = if kind == 0 {
                EffectBindingKind::Function
            } else {
                EffectBindingKind::MaskTemplate {
                    key: {
                        let mut owned = Vec::new();
                        owned
                            .try_reserve_exact(length)
                            .map_err(|error| Error::new(ErrorKind::Limit, error.to_string()))?;
                        owned.extend_from_slice(key);
                        owned
                    },
                }
            };
            products.bindings.push(EffectBinding {
                module,
                function,
                definition,
                kind,
            });
        }
        Ok(products)
    }
}

fn size_error() -> Error {
    Error::new(ErrorKind::Limit, "effect product size overflow")
}

fn take<'a>(input: &mut &'a [u8], length: usize) -> Result<&'a [u8], Error> {
    let value = input
        .get(..length)
        .ok_or_else(|| Error::new(ErrorKind::Encoding, "truncated effect product"))?;
    *input = &input[length..];
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn effect_lengths_are_preflighted_before_payload_allocation() {
        let limits = Limits::default();
        let definition = b"opaque engine definition".to_vec();
        let products = EffectProducts {
            definitions: vec![EffectDefinition {
                id: *blake3::hash(&definition).as_bytes(),
                bytes: definition,
            }],
            bindings: vec![EffectBinding {
                module: 1,
                function: 2,
                definition: 0,
                kind: EffectBindingKind::Function,
            }],
        };
        let encoded = products.encode(limits).unwrap();
        assert_eq!(
            EffectProducts::decode(&encoded, limits, &mut Budget::new(limits)).unwrap(),
            products
        );
        for length in 0..encoded.len() {
            assert!(
                EffectProducts::decode(&encoded[..length], limits, &mut Budget::new(limits))
                    .is_err()
            );
        }
        let mut corrupted = encoded.clone();
        corrupted[40..48].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(EffectProducts::decode(&corrupted, limits, &mut Budget::new(limits)).is_err());
        let mut trailing = encoded;
        trailing.push(0);
        assert!(EffectProducts::decode(&trailing, limits, &mut Budget::new(limits)).is_err());
    }
}
