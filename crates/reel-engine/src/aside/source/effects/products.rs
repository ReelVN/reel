//! Canonical effect definitions, bounded typed admission and linked function lookup.
use super::*;
use aside_vm::bytecode as bc;
use serde::{
    Deserialize,
    de::{self, DeserializeSeed, SeqAccess, Visitor},
};
use std::{fmt, marker::PhantomData};

const MAGIC: &[u8; 8] = b"REELFXD1";
pub use reel_ui::stage::effect::EffectDefinitionId;

/// One immutable, validated definition. Identity includes parameter names/defaults.
#[derive(Debug)]
pub struct EffectDefinition {
    pub id: EffectDefinitionId,
    pub program: Arc<EffectProgram>,
}
impl EffectDefinition {
    /// Definition and program capacity, including both shared Arc headers.
    pub fn storage_bytes(&self) -> usize {
        size_of::<Self>() + 2 * size_of::<usize>() + self.program.storage_bytes()
    }
}

/// Package-local function lookup. GPU artifacts and live values are separate owners.
#[derive(Debug, Default)]
pub struct EffectProducts {
    definitions: Vec<Arc<EffectDefinition>>,
    bindings: Vec<EffectBinding>,
}

#[derive(Debug)]
struct EffectBinding {
    function: bc::FunctionId,
    definition: EffectDefinitionId,
    origin: EffectOrigin,
    mask: Option<Arc<reel_ui::stage::MaskProgram>>,
}

/// Function-level diagnostic data reconstructed from ordinary bytecode metadata.
/// It is separate from definition identity and is absent from the effect wire data.
#[derive(Debug)]
pub struct EffectOrigin {
    pub function: Arc<str>,
    /// Debug source location; release packages retain the function name alone.
    pub location: Option<EffectLocation>,
}
#[derive(Debug)]
pub struct EffectLocation {
    pub file: Arc<str>,
    pub line: usize,
    pub column: usize,
}
impl EffectOrigin {
    fn from_function(program: &bc::VerifiedProgram, function: bc::FunctionId) -> Self {
        let package = program.package();
        let code = &package.functions[function.index()];
        let module = &package.modules[code.module.index()];
        let name = format!(
            "{}::{}",
            package.symbols[module.path.index()],
            package.symbols[code.name.index()]
        );
        let location = code.source_map.iter().next().and_then(|entry| {
            let source = &package.sources[entry.span.file.index()];
            let prefix = source.text.get(..entry.span.start as usize)?;
            Some(EffectLocation {
                file: package.symbols[source.path.index()].as_str().into(),
                line: prefix.bytes().filter(|&byte| byte == b'\n').count() + 1,
                column: prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1,
            })
        });
        Self {
            function: name.into(),
            location,
        }
    }
    fn storage_bytes(&self) -> usize {
        self.function.len()
            + 2 * size_of::<usize>()
            + self
                .location
                .as_ref()
                .map_or(0, |location| location.file.len() + 2 * size_of::<usize>())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Rejection before a malformed or over-budget effect enters the session.
pub struct EffectProductError(pub String);
impl fmt::Display for EffectProductError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for EffectProductError {}

/// Decode allowances use the host's byte/storage admission, not a fixed graph cap.
#[derive(Clone, Copy, Debug)]
pub struct EffectLoadLimits {
    /// Encoded bytes across all admitted definitions.
    pub bytes: usize,
    /// Definition decoding storage and retained typed program storage.
    pub storage_bytes: usize,
    /// Maximum nodes in one definition, also bounded by its encoded length.
    pub nodes: usize,
    /// Maximum parameters in one definition, also bounded by its encoded length.
    pub parameters: usize,
}
impl Default for EffectLoadLimits {
    fn default() -> Self {
        Self {
            bytes: isize::MAX as usize,
            storage_bytes: isize::MAX as usize,
            nodes: u32::MAX as usize,
            parameters: u32::MAX as usize,
        }
    }
}

impl EffectProducts {
    /// Resolve a real function from this package without executing its body.
    pub fn get(&self, function: bc::FunctionId) -> Option<&Arc<EffectDefinition>> {
        let first = self
            .bindings
            .partition_point(|binding| binding.function < function);
        let binding = self.bindings[first..]
            .iter()
            .take_while(|binding| binding.function == function)
            .find(|binding| binding.mask.is_none())?;
        self.by_id(binding.definition)
    }
    /// Definitions in lexicographic content-id order, shared by every binding.
    pub fn definitions(&self) -> impl ExactSizeIterator<Item = &Arc<EffectDefinition>> {
        self.definitions.iter()
    }
    /// Final linked FunctionId and content identity, in FunctionId order.
    pub fn bindings(&self) -> impl Iterator<Item = (bc::FunctionId, EffectDefinitionId)> + '_ {
        self.bindings
            .iter()
            .filter(|binding| binding.mask.is_none())
            .map(|binding| (binding.function, binding.definition))
    }
    /// Source owners and typed function/mask-template uses for ASB packaging.
    pub fn binding_records(
        &self,
    ) -> impl Iterator<
        Item = (
            bc::FunctionId,
            EffectDefinitionId,
            Option<&reel_ui::stage::MaskProgram>,
        ),
    > {
        self.bindings.iter().map(|binding| {
            (
                binding.function,
                binding.definition,
                binding.mask.as_deref(),
            )
        })
    }
    /// Resolve a construction once. Animation revisions retain the chosen definition.
    pub fn mask_definition(
        &self,
        mask: &reel_ui::stage::MaskProgram,
    ) -> Option<&Arc<EffectDefinition>> {
        self.by_id(self.mask_binding(mask)?.definition)
    }

    fn mask_binding(&self, mask: &reel_ui::stage::MaskProgram) -> Option<&EffectBinding> {
        self.bindings.iter().find(|binding| {
            binding.mask.as_ref().is_some_and(|template| {
                template.operations() == mask.operations()
                    && template.sources().len() == mask.sources().len()
            })
        })
    }
    /// Resolve a definition already retained by an accepted mask instance.
    pub fn by_id(&self, id: EffectDefinitionId) -> Option<&Arc<EffectDefinition>> {
        self.definitions
            .binary_search_by_key(&id, |definition| definition.id)
            .ok()
            .map(|index| &self.definitions[index])
    }

    /// Whether this source/package has no compiled effect functions.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Source or release function identity for an offline compiler diagnostic.
    pub fn origin(&self, definition: EffectDefinitionId) -> Option<&EffectOrigin> {
        self.bindings
            .iter()
            .find(|binding| binding.definition == definition)
            .map(|binding| &binding.origin)
    }

    /// Retained structural capacities, including the shared Arc control blocks.
    /// A runtime owner holds this charge for the lifetime of the shared products.
    pub fn storage_bytes(&self) -> usize {
        size_of::<Self>()
            + 2 * size_of::<usize>()
            + self.definitions.capacity() * size_of::<Arc<EffectDefinition>>()
            + self.bindings.capacity() * size_of::<EffectBinding>()
            + self
                .definitions
                .iter()
                .map(|definition| definition.storage_bytes())
                .sum::<usize>()
            + self
                .bindings
                .iter()
                .map(|binding| {
                    binding.origin.storage_bytes()
                        + binding.mask.as_ref().map_or(0, |mask| mask.storage_bytes())
                })
                .sum::<usize>()
    }
    fn bind(
        &mut self,
        program: &bc::VerifiedProgram,
        function: bc::FunctionId,
        definition: EffectDefinitionId,
        mask: Option<Arc<reel_ui::stage::MaskProgram>>,
    ) -> Result<(), EffectProductError> {
        let index = self
            .bindings
            .partition_point(|binding| binding.function <= function);
        if mask.is_none()
            && self.bindings[..index]
                .iter()
                .rev()
                .take_while(|binding| binding.function == function)
                .any(|binding| binding.mask.is_none())
        {
            return Err(EffectProductError(
                "duplicate effect function binding".into(),
            ));
        }
        if mask
            .as_ref()
            .is_some_and(|mask| self.mask_binding(mask).is_some())
        {
            return Ok(());
        }
        self.bindings
            .try_reserve(1)
            .map_err(|error| EffectProductError(error.to_string()))?;
        self.bindings.insert(
            index,
            EffectBinding {
                function,
                definition,
                origin: EffectOrigin::from_function(program, function),
                mask,
            },
        );
        Ok(())
    }

    pub(super) fn insert(
        &mut self,
        code: &bc::VerifiedProgram,
        function: bc::FunctionId,
        program: Arc<EffectProgram>,
        mask: Option<Arc<reel_ui::stage::MaskProgram>>,
    ) -> Result<(), EffectProductError> {
        let bytes = encode_definition(&program)?;
        let id = *blake3::hash(&bytes).as_bytes();
        self.bind(code, function, id, mask)?;
        if let Err(index) = self
            .definitions
            .binary_search_by_key(&id, |definition| definition.id)
        {
            self.definitions
                .try_reserve(1)
                .map_err(|error| EffectProductError(error.to_string()))?;
            self.definitions
                .insert(index, Arc::new(EffectDefinition { id, program }));
        }
        Ok(())
    }

    /// Admit opaque package products without making the engine depend on ASB.
    /// Bindings carry final module/function ids and their definition content id.
    pub fn decode(
        program: &bc::VerifiedProgram,
        definitions: impl IntoIterator<Item = (EffectDefinitionId, Vec<u8>)>,
        bindings: impl IntoIterator<
            Item = (
                bc::ModuleId,
                bc::FunctionId,
                EffectDefinitionId,
                Option<Vec<u8>>,
            ),
        >,
        mut limits: EffectLoadLimits,
    ) -> Result<Self, EffectProductError> {
        let storage_limit = limits.storage_bytes;
        let mut products = Self::default();
        for (id, bytes) in definitions {
            if blake3::hash(&bytes).as_bytes() != &id {
                return Err(EffectProductError(
                    "effect definition digest mismatch".into(),
                ));
            }
            let definition = decode_definition(&bytes, limits)?;
            limits.bytes = limits.bytes.checked_sub(bytes.len()).ok_or_else(|| {
                EffectProductError("effect definition byte budget exceeded".into())
            })?;
            limits.storage_bytes = limits
                .storage_bytes
                .checked_sub(definition.storage_bytes())
                .ok_or_else(|| {
                    EffectProductError("effect definition storage budget exceeded".into())
                })?;
            products
                .definitions
                .try_reserve(1)
                .map_err(|error| EffectProductError(error.to_string()))?;
            products.definitions.push(Arc::new(EffectDefinition {
                id,
                program: definition,
            }));
        }
        products
            .definitions
            .sort_unstable_by_key(|definition| definition.id);
        if products
            .definitions
            .windows(2)
            .any(|pair| pair[0].id == pair[1].id)
        {
            return Err(EffectProductError("duplicate effect definition".into()));
        }
        let mut functions = std::collections::BTreeSet::new();
        for (module, function, definition, mask_key) in bindings {
            if mask_key.is_none() && !functions.insert(function) {
                return Err(EffectProductError(
                    "duplicate effect function binding".into(),
                ));
            }
            let mask = mask_key.as_deref().map(decode_mask_template).transpose()?;
            let Some(code) = program.package().functions.get(function.index()) else {
                return Err(EffectProductError("effect function out of range".into()));
            };
            if code.module != module
                || (mask.is_none()
                    && (code.kind != bc::FunctionKind::Plain || !code.captures.is_empty()))
            {
                return Err(EffectProductError(
                    "effect function must be plain, unbound and in its declared module".into(),
                ));
            }
            let Some(effect) = products.by_id(definition) else {
                return Err(EffectProductError("effect definition is missing".into()));
            };
            if let Some(mask) = mask {
                let lowered = reel_ui::stage::effect::lower_mask(&mask)
                    .ok_or_else(|| EffectProductError("unsupported static mask template".into()))?;
                if lowered.as_ref() != effect.program.as_ref() {
                    return Err(EffectProductError(
                        "mask template differs from its compiled definition".into(),
                    ));
                }
                products.bindings.push(EffectBinding {
                    function,
                    definition,
                    origin: EffectOrigin::from_function(program, function),
                    mask: Some(mask),
                });
                continue;
            }
            let signature = &program.package().signatures[code.signature.index()];
            if signature.rest.is_some()
                || signature.parameters.len()
                    != effect.program.parameters().len() + effect.program.textures().len() + 1
            {
                return Err(EffectProductError(
                    "effect parameter count differs from its function".into(),
                ));
            }
            let package = program.package();
            let mut numeric = effect.program.parameters().iter();
            let mut textures = effect.program.textures().iter().peekable();
            for parameter in signature.parameters.iter().skip(1) {
                let name = package.symbols[parameter.name.index()].as_str();
                let valid = if textures
                    .next_if(|texture| texture.name.as_ref() == name)
                    .is_some()
                {
                    parameter.default.is_none()
                        && matches!(&package.types[parameter.ty.index()], bc::TypeDef::Named { name, .. } if matches!(package.symbols[name.index()].as_str(), "Texture" | "reel::types::Texture"))
                } else {
                    numeric.next().is_some_and(|effect| {
                        name == effect.name.as_ref()
                            && matches_type(package, parameter.ty, effect.ty)
                            && parameter.default.is_some() == effect.default.is_some()
                    })
                };
                if !valid {
                    return Err(EffectProductError(
                        "effect parameter schema differs from its function".into(),
                    ));
                }
            }
            let output = effect.program.nodes()[effect.program.output().0 as usize].ty;
            if package.types[signature.return_type.index()]
                != bc::TypeDef::Builtin(bc::BuiltinType::Any)
                && !matches_type(package, signature.return_type, output)
            {
                return Err(EffectProductError(
                    "effect output differs from its function".into(),
                ));
            }
            products
                .bindings
                .try_reserve(1)
                .map_err(|error| EffectProductError(error.to_string()))?;
            products.bindings.push(EffectBinding {
                function,
                definition,
                origin: EffectOrigin::from_function(program, function),
                mask: None,
            });
        }
        products
            .bindings
            .sort_unstable_by_key(|binding| binding.function);
        let used = products
            .bindings
            .iter()
            .map(|binding| binding.definition)
            .collect::<std::collections::BTreeSet<_>>();
        if used.len() != products.definitions.len() {
            return Err(EffectProductError("unbound effect definition".into()));
        }
        if products.storage_bytes() > storage_limit {
            return Err(EffectProductError(
                "effect products storage budget exceeded".into(),
            ));
        }
        Ok(products)
    }
}

fn matches_type(package: &bc::RuntimePackage, ty: bc::TypeId, effect: Ty) -> bool {
    let scalar =
        |ty: bc::TypeId, builtin| package.types[ty.index()] == bc::TypeDef::Builtin(builtin);
    match effect {
        Ty::Bool => scalar(ty, bc::BuiltinType::Bool),
        Ty::Float => scalar(ty, bc::BuiltinType::Float),
        Ty::Vec2 | Ty::Vec3 | Ty::Vec4 => {
            let bc::TypeDef::Tuple(lanes) = &package.types[ty.index()] else {
                return false;
            };
            let expected = match effect {
                Ty::Vec2 => 2,
                Ty::Vec3 => 3,
                _ => 4,
            };
            lanes.len() == expected
                && lanes
                    .iter()
                    .all(|lane| scalar(*lane, bc::BuiltinType::Float))
        }
    }
}

/// Canonical key for one existing mask topology, independent of asset paths.
pub fn encode_mask_template(
    mask: &reel_ui::stage::MaskProgram,
) -> Result<Vec<u8>, EffectProductError> {
    postcard::to_allocvec(&(mask.operations(), mask.sources().len() as u32))
        .map_err(|error| EffectProductError(error.to_string()))
}
fn decode_mask_template(
    bytes: &[u8],
) -> Result<Arc<reel_ui::stage::MaskProgram>, EffectProductError> {
    struct Key;
    impl<'de> Visitor<'de> for Key {
        type Value = Arc<reel_ui::stage::MaskProgram>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a bounded mask template")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut storage = 32 * size_of::<reel_ui::stage::MaskOp>();
            let operations = seq
                .next_element_seed(VectorSeed {
                    count: 32,
                    storage: &mut storage,
                    marker: PhantomData,
                })?
                .ok_or_else(|| de::Error::custom("missing mask operations"))?;
            let images: u32 = seq
                .next_element()?
                .ok_or_else(|| de::Error::custom("missing mask image count"))?;
            if images > 4 {
                return Err(de::Error::custom(
                    "mask template image count exceeds supported capacity",
                ));
            }
            reel_ui::stage::MaskProgram::new(
                operations,
                (0..images)
                    .map(|i| Arc::from(format!("image_{i}")))
                    .collect(),
            )
            .map_err(de::Error::custom)
        }
    }
    let mut de = postcard::Deserializer::from_bytes(bytes);
    let mask = de::Deserializer::deserialize_tuple(&mut de, 2, Key)
        .map_err(|e| EffectProductError(e.to_string()))?;
    if !de
        .finalize()
        .map_err(|e| EffectProductError(e.to_string()))?
        .is_empty()
    {
        return Err(EffectProductError("trailing mask template bytes".into()));
    }
    Ok(mask)
}

/// Canonical v1 bytes use the shared UI enums' declaration order and finite f32 bits.
/// Source locations, source function numbering and instance values are excluded.
pub fn encode_definition(program: &EffectProgram) -> Result<Vec<u8>, EffectProductError> {
    let payload = postcard::to_allocvec(&(
        program.parameters(),
        program.textures(),
        program.nodes(),
        program.output(),
    ))
    .map_err(|e| EffectProductError(e.to_string()))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(MAGIC.len() + payload.len())
        .map_err(|e| EffectProductError(e.to_string()))?;
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&payload);
    Ok(bytes)
}

#[derive(Deserialize)]
struct Parameter<'a> {
    #[serde(borrow)]
    name: &'a str,
    ty: Ty,
    default: Option<Constant>,
}

struct VectorSeed<'a, T> {
    count: usize,
    storage: &'a mut usize,
    marker: PhantomData<T>,
}
impl<'de, T: Deserialize<'de>> DeserializeSeed<'de> for VectorSeed<'_, T> {
    type Value = Vec<T>;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        struct Values<'a, T>(VectorSeed<'a, T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for Values<'_, T> {
            type Value = Vec<T>;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a bounded effect table")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let count = seq
                    .size_hint()
                    .ok_or_else(|| de::Error::custom("effect table needs a declared length"))?;
                if count > self.0.count {
                    return Err(de::Error::custom(
                        "effect table count exceeds input or host limit",
                    ));
                }
                let bytes = count
                    .checked_mul(size_of::<T>())
                    .ok_or_else(|| de::Error::custom("effect storage overflow"))?;
                *self.0.storage = self
                    .0
                    .storage
                    .checked_sub(bytes)
                    .ok_or_else(|| de::Error::custom("effect storage budget exceeded"))?;
                let mut values = Vec::new();
                values.try_reserve_exact(count).map_err(de::Error::custom)?;
                while let Some(value) = seq.next_element()? {
                    if values.len() == count {
                        return Err(de::Error::custom("effect table length mismatch"));
                    }
                    values.push(value);
                }
                Ok(values)
            }
        }
        deserializer.deserialize_seq(Values(self))
    }
}

struct DefinitionSeed {
    limits: EffectLoadLimits,
    input_len: usize,
}
impl<'de> DeserializeSeed<'de> for DefinitionSeed {
    type Value = Arc<EffectProgram>;
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_tuple(4, self)
    }
}
impl<'de> Visitor<'de> for DefinitionSeed {
    type Value = Arc<EffectProgram>;
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a v1 effect definition")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut storage = self.limits.storage_bytes;
        let parameters: Vec<Parameter<'de>> = seq
            .next_element_seed(VectorSeed {
                count: self.limits.parameters.min(self.input_len),
                storage: &mut storage,
                marker: PhantomData,
            })?
            .ok_or_else(|| de::Error::custom("missing effect parameters"))?;
        #[derive(Deserialize)]
        struct Texture<'a> {
            #[serde(borrow)]
            name: &'a str,
        }
        let textures: Vec<Texture<'de>> = seq
            .next_element_seed(VectorSeed {
                count: self
                    .limits
                    .parameters
                    .saturating_sub(parameters.len())
                    .min(self.input_len),
                storage: &mut storage,
                marker: PhantomData,
            })?
            .ok_or_else(|| de::Error::custom("missing effect textures"))?;
        let texture_bytes = textures
            .iter()
            .try_fold(
                textures.len() * size_of::<EffectTextureParameter>(),
                |sum, texture| sum.checked_add(texture.name.len() + 2 * size_of::<usize>()),
            )
            .ok_or_else(|| de::Error::custom("effect texture storage overflow"))?;
        storage = storage
            .checked_sub(texture_bytes)
            .ok_or_else(|| de::Error::custom("effect texture storage budget exceeded"))?;
        let textures = textures
            .into_iter()
            .map(|texture| EffectTextureParameter {
                name: texture.name.into(),
            })
            .collect();
        let nodes: Vec<EffectNode> = seq
            .next_element_seed(VectorSeed {
                count: self.limits.nodes.min(self.input_len),
                storage: &mut storage,
                marker: PhantomData,
            })?
            .ok_or_else(|| de::Error::custom("missing effect nodes"))?;
        let output: Id = seq
            .next_element()?
            .ok_or_else(|| de::Error::custom("missing effect output"))?;
        let mut owned = Vec::new();
        let bytes = parameters
            .len()
            .checked_mul(size_of::<EffectParameter>())
            .ok_or_else(|| de::Error::custom("effect storage overflow"))?;
        storage = storage
            .checked_sub(bytes)
            .ok_or_else(|| de::Error::custom("effect storage budget exceeded"))?;
        for parameter in &parameters {
            storage = storage
                .checked_sub(parameter.name.len() + 2 * size_of::<usize>())
                .ok_or_else(|| de::Error::custom("effect name storage budget exceeded"))?;
        }
        owned
            .try_reserve_exact(parameters.len())
            .map_err(de::Error::custom)?;
        owned.extend(parameters.into_iter().map(|p| EffectParameter {
            name: p.name.into(),
            ty: p.ty,
            default: p.default,
        }));
        EffectProgram::with_textures(owned, textures, nodes, output).map_err(de::Error::custom)
    }
}

/// Decode shared typed IR only after length admission; reject trailing data.
pub fn decode_definition(
    bytes: &[u8],
    limits: EffectLoadLimits,
) -> Result<Arc<EffectProgram>, EffectProductError> {
    if bytes.len() > limits.bytes {
        return Err(EffectProductError(
            "effect definition byte budget exceeded".into(),
        ));
    }
    let payload = bytes
        .strip_prefix(MAGIC)
        .ok_or_else(|| EffectProductError("invalid effect definition version".into()))?;
    let mut deserializer = postcard::Deserializer::from_bytes(payload);
    let program = DefinitionSeed {
        limits,
        input_len: payload.len(),
    }
    .deserialize(&mut deserializer)
    .map_err(|e| EffectProductError(e.to_string()))?;
    if !deserializer
        .finalize()
        .map_err(|e| EffectProductError(e.to_string()))?
        .is_empty()
    {
        return Err(EffectProductError(
            "trailing effect definition bytes".into(),
        ));
    }
    Ok(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> Arc<EffectProgram> {
        EffectProgram::new(
            vec![EffectParameter {
                name: "progress".into(),
                ty: Ty::Float,
                default: Some(Constant::Float(0.25_f32.to_bits())),
            }],
            vec![EffectNode {
                ty: Ty::Float,
                op: Op::Parameter(0),
            }],
            Id(0),
        )
        .unwrap()
    }

    #[test]
    fn canonical_definition_roundtrip_and_bounded_tables() {
        let program = definition();
        let bytes = encode_definition(&program).unwrap();
        assert_eq!(
            decode_definition(&bytes, Default::default()).unwrap(),
            program
        );
        assert_eq!(
            encode_definition(&decode_definition(&bytes, Default::default()).unwrap()).unwrap(),
            bytes
        );
        for limits in [
            EffectLoadLimits {
                bytes: bytes.len() - 1,
                ..Default::default()
            },
            EffectLoadLimits {
                storage_bytes: 0,
                ..Default::default()
            },
            EffectLoadLimits {
                parameters: 0,
                ..Default::default()
            },
            EffectLoadLimits {
                nodes: 0,
                ..Default::default()
            },
        ] {
            assert!(decode_definition(&bytes, limits).is_err());
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(decode_definition(&trailing, Default::default()).is_err());
        let mut forged = MAGIC.to_vec();
        forged.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0x0f]);
        assert!(decode_definition(&forged, Default::default()).is_err());
    }

    #[test]
    fn typed_graph_validation_runs_after_wire_decoding() {
        let parameters: Vec<EffectParameter> = Vec::new();
        for node in [
            EffectNode {
                ty: Ty::Float,
                op: Op::Constant(Constant::Float(f32::NAN.to_bits())),
            },
            EffectNode {
                ty: Ty::Float,
                op: Op::Unary {
                    op: Unary::Abs,
                    value: Id(0),
                },
            },
        ] {
            let mut bytes = MAGIC.to_vec();
            bytes.extend(
                postcard::to_allocvec(&(
                    parameters.clone(),
                    Vec::<EffectTextureParameter>::new(),
                    vec![node],
                    Id(0),
                ))
                .unwrap(),
            );
            assert!(decode_definition(&bytes, Default::default()).is_err());
        }
    }
}
