use crate::stage::effect::{EffectDefinitionId, EffectInstance, EffectType, EffectValue};
use std::ops::Range;

/// Interface version shared by exported fragments and production source bindings.
pub const EFFECT_SHADER_ABI: u32 = 1;

/// A malformed or over-budget prepared shader envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectShaderPayloadError {
    Invalid,
    Limit,
}
impl std::fmt::Display for EffectShaderPayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Invalid => "invalid prepared effect shader",
            Self::Limit => "prepared effect shader exceeds its byte limit",
        })
    }
}
impl std::error::Error for EffectShaderPayloadError {}

/// Prepared fragment representation selected by the asset host for its backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EffectShaderFormat {
    GlslEs100,
    Wgsl,
    Sgx543,
}
impl EffectShaderFormat {
    /// Target representation suffix used by the logical prepared-asset path.
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::GlslEs100 => "glsl",
            Self::Wgsl => "wgsl",
            Self::Sgx543 => "sgx",
        }
    }
    /// Stable logical asset path shared by exporters and asynchronous hosts.
    pub fn asset_path(self, definition: EffectDefinitionId) -> String {
        use std::fmt::Write;
        let mut path = String::with_capacity(100);
        path.push_str("__reel/effects/");
        for byte in definition {
            write!(&mut path, "{byte:02x}").unwrap();
        }
        write!(&mut path, "/{}.reel-effect", self.suffix()).unwrap();
        path
    }
}

/// Typed interface of a complete exported fragment. The definition identity is
/// shared across platform products; the resource version identifies their bytes.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EffectShaderDescriptor {
    pub definition: EffectDefinitionId,
    /// Code and ABI identity, independent of author names, defaults and values.
    pub shader_key: [u8; 32],
    pub abi: u32,
    pub format: EffectShaderFormat,
    /// One sixteen-byte uniform slot per parameter, in declaration order.
    pub parameters: Vec<EffectType>,
    /// Number of separately typed whole-image bindings, in declaration order.
    pub textures: u32,
    /// Whether the compiled entry point references its subtree source sampler.
    pub source_sampler: bool,
    /// Float is scalar coverage; Vec4 is linear premultiplied RGBA.
    pub output: EffectType,
}
impl EffectShaderDescriptor {
    /// Build the canonical code identity for an exported target fragment.
    pub fn new(
        definition: EffectDefinitionId,
        format: EffectShaderFormat,
        parameters: Vec<EffectType>,
        output: EffectType,
        raw: &[u8],
    ) -> Result<Self, EffectShaderPayloadError> {
        Self::with_textures(definition, format, parameters, 0, true, output, raw)
    }

    /// Build one v1 fragment interface with numeric and texture inputs separated.
    pub fn with_textures(
        definition: EffectDefinitionId,
        format: EffectShaderFormat,
        parameters: Vec<EffectType>,
        textures: u32,
        source_sampler: bool,
        output: EffectType,
        raw: &[u8],
    ) -> Result<Self, EffectShaderPayloadError> {
        let mut descriptor = Self {
            definition,
            shader_key: [0; 32],
            abi: EFFECT_SHADER_ABI,
            format,
            parameters,
            textures,
            source_sampler,
            output,
        };
        if !descriptor.is_valid() || raw.is_empty() {
            return Err(EffectShaderPayloadError::Invalid);
        }
        descriptor.shader_key = descriptor.content_key(raw);
        Ok(descriptor)
    }
    fn content_key(&self, raw: &[u8]) -> [u8; 32] {
        let mut hash = blake3::Hasher::new();
        hash.update(b"Reel effect shader\0");
        hash.update(&self.abi.to_le_bytes());
        hash.update(&[format_byte(self.format), type_byte(self.output)]);
        hash.update(&(self.parameters.len() as u32).to_le_bytes());
        hash.update(&self.textures.to_le_bytes());
        hash.update(&[u8::from(self.source_sampler)]);
        for ty in &self.parameters {
            hash.update(&[type_byte(*ty)]);
        }
        hash.update(raw);
        *hash.finalize().as_bytes()
    }
    /// Verify bytes once when entering the prepared resource registry.
    pub fn matches_bytes(&self, raw: &[u8]) -> bool {
        self.is_valid() && !raw.is_empty() && self.shader_key == self.content_key(raw)
    }
    /// Encode the version-one little-endian prepared shader envelope. The header
    /// carries this descriptor followed by the complete target fragment bytes.
    pub fn encode_payload(&self, raw: &[u8]) -> Result<Vec<u8>, EffectShaderPayloadError> {
        if !self.is_valid() || raw.is_empty() {
            return Err(EffectShaderPayloadError::Invalid);
        }
        let capacity = 96usize
            .checked_add(self.parameters.len())
            .and_then(|v| v.checked_add(raw.len()))
            .ok_or(EffectShaderPayloadError::Limit)?;
        let mut bytes = Vec::with_capacity(capacity);
        bytes.extend_from_slice(b"REELSHD1");
        bytes.extend_from_slice(&self.abi.to_le_bytes());
        bytes.push(format_byte(self.format));
        bytes.push(type_byte(self.output));
        bytes.extend_from_slice(&[u8::from(self.source_sampler), 0]);
        bytes.extend_from_slice(&(self.parameters.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&self.definition);
        bytes.extend_from_slice(&self.shader_key);
        bytes.extend_from_slice(&(raw.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&self.textures.to_le_bytes());
        bytes.extend(self.parameters.iter().copied().map(type_byte));
        bytes.extend_from_slice(raw);
        Ok(bytes)
    }

    /// Validate envelope structure before allocating its parameter schema.
    /// Resource registration verifies the content key against the raw bytes.
    /// The returned range borrows raw bytes from the caller's accounted buffer;
    /// asset hosts can move that range to the front and truncate in place.
    pub fn decode_payload(
        bytes: &[u8],
        max_payload_bytes: usize,
    ) -> Result<(Self, Range<usize>), EffectShaderPayloadError> {
        use EffectShaderPayloadError::{Invalid, Limit};
        if bytes.len() > max_payload_bytes {
            return Err(Limit);
        }
        if bytes.len() < 96 || &bytes[..8] != b"REELSHD1" || (bytes[14] > 1 || bytes[15] != 0) {
            return Err(Invalid);
        }
        let source_sampler = bytes[14] != 0;
        let abi = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let format = match bytes[12] {
            0 => EffectShaderFormat::GlslEs100,
            1 => EffectShaderFormat::Wgsl,
            2 => EffectShaderFormat::Sgx543,
            _ => return Err(Invalid),
        };
        let output = byte_type(bytes[13]).ok_or(Invalid)?;
        let count = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
        let definition = bytes[20..52].try_into().unwrap();
        let shader_key = bytes[52..84].try_into().unwrap();
        let size = usize::try_from(u64::from_le_bytes(bytes[84..92].try_into().unwrap()))
            .map_err(|_| Invalid)?;
        let textures = u32::from_le_bytes(bytes[92..96].try_into().unwrap());
        let start = 96usize.checked_add(count).ok_or(Invalid)?;
        if size == 0 || start.checked_add(size) != Some(bytes.len()) {
            return Err(Invalid);
        }
        if abi != EFFECT_SHADER_ABI || !matches!(output, EffectType::Float | EffectType::Vec4) {
            return Err(Invalid);
        }
        let parameters = bytes[96..start]
            .iter()
            .map(|&v| byte_type(v).ok_or(Invalid))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((
            Self {
                definition,
                shader_key,
                abi,
                format,
                parameters,
                textures,
                source_sampler,
                output,
            },
            start..bytes.len(),
        ))
    }
    /// Validate the portable interface before registering its immutable bytes.
    pub fn is_valid(&self) -> bool {
        self.abi == EFFECT_SHADER_ABI
            && self.parameters.len() <= u32::MAX as usize
            && matches!(self.output, EffectType::Float | EffectType::Vec4)
    }
    /// Check the original typed definition when an author instance is prepared.
    pub fn matches_instance(&self, instance: &EffectInstance) -> bool {
        self.is_valid()
            && self.definition == instance.definition()
            && self.textures as usize == instance.textures().len()
            && self.output == instance.program().nodes()[instance.program().output().0 as usize].ty
            && self.parameters.iter().copied().eq(instance
                .program()
                .parameters()
                .iter()
                .map(|p| p.ty))
            && self.accepts(instance.values())
    }
    /// Validate restored frame values against the shader's fixed slot schema.
    pub fn accepts(&self, values: &[EffectValue]) -> bool {
        values.len() == self.parameters.len()
            && values
                .iter()
                .zip(&self.parameters)
                .all(|(value, ty)| value.ty() == *ty && value.is_finite())
    }
}

fn type_byte(ty: EffectType) -> u8 {
    match ty {
        EffectType::Bool => 0,
        EffectType::Float => 1,
        EffectType::Vec2 => 2,
        EffectType::Vec3 => 3,
        EffectType::Vec4 => 4,
    }
}
fn format_byte(format: EffectShaderFormat) -> u8 {
    match format {
        EffectShaderFormat::GlslEs100 => 0,
        EffectShaderFormat::Wgsl => 1,
        EffectShaderFormat::Sgx543 => 2,
    }
}
fn byte_type(value: u8) -> Option<EffectType> {
    Some(match value {
        0 => EffectType::Bool,
        1 => EffectType::Float,
        2 => EffectType::Vec2,
        3 => EffectType::Vec3,
        4 => EffectType::Vec4,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prepared_envelope_preserves_schema_and_raw_range() {
        let descriptor = EffectShaderDescriptor::new(
            [17; 32],
            EffectShaderFormat::GlslEs100,
            vec![
                EffectType::Bool,
                EffectType::Float,
                EffectType::Vec2,
                EffectType::Vec3,
                EffectType::Vec4,
            ],
            EffectType::Vec4,
            b"void main() {}",
        )
        .unwrap();
        let bytes = descriptor.encode_payload(b"void main() {}").unwrap();
        let (decoded, range) = EffectShaderDescriptor::decode_payload(&bytes, bytes.len()).unwrap();
        assert_eq!(decoded, descriptor);
        assert_eq!(&bytes[range], b"void main() {}");
        let mut other = descriptor.clone();
        other.definition = [99; 32];
        assert!(other.matches_bytes(b"void main() {}"));
        assert!(!other.matches_bytes(b"void main() { discard; }"));
        assert_eq!(
            EffectShaderDescriptor::decode_payload(&bytes, bytes.len() - 1),
            Err(EffectShaderPayloadError::Limit)
        );
        assert_eq!(
            EffectShaderDescriptor::decode_payload(&bytes[..bytes.len() - 1], bytes.len()),
            Err(EffectShaderPayloadError::Invalid)
        );
    }
}
