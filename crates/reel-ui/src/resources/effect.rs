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
    /// WGSL source loaded by the desktop WGPU renderer.
    Wgsl,
    /// Native Sony GXM program compiled offline from typed Cg source.
    Gxp,
}
impl EffectShaderFormat {
    /// Target representation suffix used by the logical prepared-asset path.
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Wgsl => "wgsl",
            Self::Gxp => "gxp",
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

/// Compiler-proved logical neighborhood of explicit subtree samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EffectSourceFootprint {
    /// Maximum source offsets in original effect logical pixels, ordered left,
    /// top, right, bottom. The renderer adds the texture filtering footprint.
    pub logical_halo: [u32; 4],
}

/// Typed interface and compiler-proved source sampling of an exported fragment.
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
    /// Byte offset of the optional fixed alpha-coverage variant; the prefix is ordinary code.
    /// Only color effects with a spare portable sampler carry this variant.
    pub coverage_offset: Option<u32>,
    /// Byte offset of an optional final fragment with unconditional output over
    /// the target, returning transparent outside the local effect domain.
    /// It has the ordinary output tail. This metadata never authorizes undefined
    /// attachment contents; registered shaders still run on initialized targets.
    pub full_target_offset: Option<u32>,
    /// Byte offset of an optional final ordinary-color GXP segment with one
    /// compiler-proved unchanged source-coordinate axis. The renderer selects
    /// it only when the final uploaded source mapping is finite and diagonal.
    /// Sampling order, local coverage and uniform ABI remain unchanged.
    pub diagonal_offset: Option<u32>,
    /// Compiler-proved fixed source neighborhood. Absence keeps the full source
    /// domain. This proof changes neither shader coordinates nor logical Size.
    #[serde(default)]
    pub source_footprint: Option<EffectSourceFootprint>,
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
            coverage_offset: None,
            full_target_offset: None,
            diagonal_offset: None,
            source_footprint: None,
        };
        if !descriptor.is_valid() || raw.is_empty() {
            return Err(EffectShaderPayloadError::Invalid);
        }
        descriptor.shader_key = descriptor.content_key(raw);
        Ok(descriptor)
    }
    /// Bind a compiler-proved source neighborhood into the code identity. Only
    /// ordinary color fragments can use this optional work-reduction contract.
    pub fn with_source_footprint(
        mut self,
        footprint: Option<EffectSourceFootprint>,
        raw: &[u8],
    ) -> Result<Self, EffectShaderPayloadError> {
        self.source_footprint = footprint;
        if !self.is_valid() || raw.is_empty() {
            return Err(EffectShaderPayloadError::Invalid);
        }
        self.shader_key = self.content_key(raw);
        Ok(self)
    }
    /// Attach the offline coverage variant and bind both segments into the content key.
    pub fn with_coverage_variant(
        mut self,
        offset: u32,
        raw: &[u8],
    ) -> Result<Self, EffectShaderPayloadError> {
        self.coverage_offset = Some(offset);
        if !self.is_valid() || offset as usize >= raw.len() {
            return Err(EffectShaderPayloadError::Invalid);
        }
        self.shader_key = self.content_key(raw);
        Ok(self)
    }
    /// Borrow the selected immutable code segment without allocating or compiling shaders.
    pub fn code<'a>(&self, raw: &'a [u8], coverage: bool) -> Option<&'a [u8]> {
        let end = self.full_target_offset.or(self.diagonal_offset);
        let raw = raw.get(..end.map_or(raw.len(), |v| v as usize))?;
        match (self.coverage_offset, coverage) {
            (Some(offset), false) => raw.get(..offset as usize),
            (Some(offset), true) => raw.get(offset as usize..),
            (None, false) => Some(raw),
            (None, true) => None,
        }
    }
    /// Attach the final full-target fragment and bind its range into code identity.
    pub fn with_full_target_variant(
        mut self,
        offset: u32,
        raw: &[u8],
    ) -> Result<Self, EffectShaderPayloadError> {
        self.full_target_offset = Some(offset);
        if !self.is_valid() || offset as usize >= raw.len() {
            return Err(EffectShaderPayloadError::Invalid);
        }
        self.shader_key = self.content_key(raw);
        Ok(self)
    }
    /// Borrow the full-target variant after the renderer proves complete coverage.
    pub fn full_target_code<'a>(&self, raw: &'a [u8]) -> Option<&'a [u8]> {
        let raw = raw.get(..self.diagonal_offset.map_or(raw.len(), |v| v as usize))?;
        raw.get(self.full_target_offset? as usize..)
    }
    /// Attach a proved ordinary-color diagonal-source GXP segment. Its separate
    /// boundary is encoded in the current REELSHD2 envelope.
    pub fn with_diagonal_variant(
        mut self,
        offset: u32,
        raw: &[u8],
    ) -> Result<Self, EffectShaderPayloadError> {
        self.diagonal_offset = Some(offset);
        if !self.is_valid() || offset as usize >= raw.len() {
            return Err(EffectShaderPayloadError::Invalid);
        }
        self.shader_key = self.content_key(raw);
        Ok(self)
    }
    /// Borrow the diagonal-source segment after proving its runtime mapping.
    pub fn diagonal_code<'a>(&self, raw: &'a [u8]) -> Option<&'a [u8]> {
        raw.get(self.diagonal_offset? as usize..)
    }
    fn content_key(&self, raw: &[u8]) -> [u8; 32] {
        let mut hash = blake3::Hasher::new();
        hash.update(b"Reel effect shader\0");
        hash.update(&self.abi.to_le_bytes());
        hash.update(&[format_byte(self.format), type_byte(self.output)]);
        hash.update(&(self.parameters.len() as u32).to_le_bytes());
        hash.update(&self.textures.to_le_bytes());
        hash.update(&[u8::from(self.source_sampler)]);
        hash.update(&self.coverage_offset.unwrap_or(0).to_le_bytes());
        hash.update(&self.full_target_offset.unwrap_or(0).to_le_bytes());
        hash.update(&self.diagonal_offset.unwrap_or(0).to_le_bytes());
        if let Some(footprint) = self.source_footprint {
            hash.update(b"source-footprint\0");
            for value in footprint.logical_halo {
                hash.update(&value.to_le_bytes());
            }
        }
        for ty in &self.parameters {
            hash.update(&[type_byte(*ty)]);
        }
        hash.update(raw);
        *hash.finalize().as_bytes()
    }
    /// Verify bytes once when entering the prepared resource registry.
    pub fn matches_bytes(&self, raw: &[u8]) -> bool {
        self.is_valid()
            && !raw.is_empty()
            && self
                .coverage_offset
                .is_none_or(|o| (o as usize) < raw.len())
            && self
                .full_target_offset
                .is_none_or(|o| (o as usize) < raw.len())
            && self
                .diagonal_offset
                .is_none_or(|o| (o as usize) < raw.len())
            && self.shader_key == self.content_key(raw)
    }
    /// Encode the little-endian prepared shader envelope followed by complete
    /// target fragment bytes. REELSHD2 gives every optional code segment its own
    /// boundary; zero denotes an absent segment.
    pub fn encode_payload(&self, raw: &[u8]) -> Result<Vec<u8>, EffectShaderPayloadError> {
        if !self.matches_bytes(raw) {
            return Err(EffectShaderPayloadError::Invalid);
        }
        let capacity = 108usize
            .checked_add(16 * usize::from(self.source_footprint.is_some()))
            .ok_or(EffectShaderPayloadError::Limit)?
            .checked_add(self.parameters.len())
            .and_then(|v| v.checked_add(raw.len()))
            .ok_or(EffectShaderPayloadError::Limit)?;
        let mut bytes = Vec::with_capacity(capacity);
        bytes.extend_from_slice(b"REELSHD2");
        bytes.extend_from_slice(&self.abi.to_le_bytes());
        bytes.push(format_byte(self.format));
        bytes.push(type_byte(self.output));
        bytes.extend_from_slice(&[
            u8::from(self.source_sampler),
            u8::from(self.source_footprint.is_some()),
        ]);
        bytes.extend_from_slice(&(self.parameters.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&self.definition);
        bytes.extend_from_slice(&self.shader_key);
        bytes.extend_from_slice(&(raw.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&self.textures.to_le_bytes());
        bytes.extend_from_slice(&self.coverage_offset.unwrap_or(0).to_le_bytes());
        bytes.extend_from_slice(&self.full_target_offset.unwrap_or(0).to_le_bytes());
        bytes.extend_from_slice(&self.diagonal_offset.unwrap_or(0).to_le_bytes());
        if let Some(footprint) = self.source_footprint {
            for value in footprint.logical_halo {
                bytes.extend_from_slice(&value.to_le_bytes());
            }
        }
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
        if bytes.len() < 108 || &bytes[..8] != b"REELSHD2" || bytes[14] > 1 || bytes[15] > 1 {
            return Err(Invalid);
        }
        let source_sampler = bytes[14] != 0;
        let abi = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let format = match bytes[12] {
            1 => EffectShaderFormat::Wgsl,
            3 => EffectShaderFormat::Gxp,
            _ => return Err(Invalid),
        };
        let output = byte_type(bytes[13]).ok_or(Invalid)?;
        let count = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
        let definition = bytes[20..52].try_into().unwrap();
        let shader_key = bytes[52..84].try_into().unwrap();
        let size = usize::try_from(u64::from_le_bytes(bytes[84..92].try_into().unwrap()))
            .map_err(|_| Invalid)?;
        let textures = u32::from_le_bytes(bytes[92..96].try_into().unwrap());
        let offset = u32::from_le_bytes(bytes[96..100].try_into().unwrap());
        let coverage_offset = (offset != 0).then_some(offset);
        let full = u32::from_le_bytes(bytes[100..104].try_into().unwrap());
        let full_target_offset = (full != 0).then_some(full);
        let diagonal = u32::from_le_bytes(bytes[104..108].try_into().unwrap());
        let diagonal_offset = (diagonal != 0).then_some(diagonal);
        if diagonal_offset.is_some_and(|offset| {
            offset as usize >= size
                || offset <= full.max(coverage_offset.unwrap_or(0))
                || format != EffectShaderFormat::Gxp
                || output != EffectType::Vec4
                || !source_sampler
        }) {
            return Err(Invalid);
        }
        if coverage_offset.is_some_and(|v| v as usize >= size)
            || full_target_offset.is_some_and(|v| v as usize >= size || v <= offset)
        {
            return Err(Invalid);
        }
        let schema = 108 + 16 * usize::from(bytes[15] != 0);
        if bytes.len() < schema {
            return Err(Invalid);
        }
        let source_footprint = (bytes[15] != 0).then(|| EffectSourceFootprint {
            logical_halo: std::array::from_fn(|i| {
                u32::from_le_bytes(bytes[108 + i * 4..112 + i * 4].try_into().unwrap())
            }),
        });
        if source_footprint.is_some() && (!source_sampler || output != EffectType::Vec4) {
            return Err(Invalid);
        }
        let start = schema.checked_add(count).ok_or(Invalid)?;
        if size == 0 || start.checked_add(size) != Some(bytes.len()) {
            return Err(Invalid);
        }
        if abi != EFFECT_SHADER_ABI || !matches!(output, EffectType::Float | EffectType::Vec4) {
            return Err(Invalid);
        }
        let parameters = bytes[schema..start]
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
                coverage_offset,
                full_target_offset,
                diagonal_offset,
                source_footprint,
            },
            start..bytes.len(),
        ))
    }
    /// Validate the portable interface before registering its immutable bytes.
    pub fn is_valid(&self) -> bool {
        self.abi == EFFECT_SHADER_ABI
            && self
                .source_footprint
                .is_none_or(|_| self.source_sampler && self.output == EffectType::Vec4)
            && self
                .full_target_offset
                .is_none_or(|offset| offset > self.coverage_offset.unwrap_or(0))
            && self.diagonal_offset.is_none_or(|offset| {
                offset
                    > self
                        .full_target_offset
                        .or(self.coverage_offset)
                        .unwrap_or(0)
                    && self.format == EffectShaderFormat::Gxp
                    && self.output == EffectType::Vec4
                    && self.source_sampler
            })
            && self.coverage_offset.is_none_or(|offset| {
                offset > 0
                    && self.output == EffectType::Vec4
                    && self.textures.saturating_add(u32::from(self.source_sampler)) < 16
            })
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
        EffectShaderFormat::Wgsl => 1,
        EffectShaderFormat::Gxp => 3,
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
    fn current_envelope_roundtrips_with_and_without_diagonal_code() {
        let original_raw = b"ordinarymaskedfull";
        let original = EffectShaderDescriptor::new(
            [7; 32],
            EffectShaderFormat::Gxp,
            vec![],
            EffectType::Vec4,
            original_raw,
        )
        .unwrap()
        .with_coverage_variant(8, original_raw)
        .unwrap()
        .with_full_target_variant(14, original_raw)
        .unwrap()
        .with_source_footprint(
            Some(EffectSourceFootprint {
                logical_halo: [30, 0, 30, 0],
            }),
            original_raw,
        )
        .unwrap();
        let base = original.encode_payload(original_raw).unwrap();
        assert_eq!(&base[..8], b"REELSHD2");
        assert_eq!(&base[104..108], &0u32.to_le_bytes());
        let (decoded, range) = EffectShaderDescriptor::decode_payload(&base, base.len()).unwrap();
        assert_eq!(decoded, original);
        assert_eq!(range.start, 124);
        assert_eq!(decoded.encode_payload(&base[range]).unwrap(), base);

        let raw = b"ordinarymaskedfulldiagonal";
        let descriptor = original.clone().with_diagonal_variant(18, raw).unwrap();
        assert_eq!(descriptor.code(raw, false), Some(&b"ordinary"[..]));
        assert_eq!(descriptor.code(raw, true), Some(&b"masked"[..]));
        assert_eq!(descriptor.full_target_code(raw), Some(&b"full"[..]));
        assert_eq!(descriptor.diagonal_code(raw), Some(&b"diagonal"[..]));
        let bytes = descriptor.encode_payload(raw).unwrap();
        assert_eq!(&bytes[..8], b"REELSHD2");
        let (decoded, range) = EffectShaderDescriptor::decode_payload(&bytes, bytes.len()).unwrap();
        assert_eq!(range.start, 124);
        assert_eq!(decoded, descriptor);
        assert!(decoded.matches_bytes(&bytes[range]));
        assert_ne!(original.shader_key, descriptor.shader_key);
        let mut changed = descriptor;
        changed.diagonal_offset = Some(19);
        assert!(!changed.matches_bytes(raw));
    }

    #[test]
    fn diagonal_segment_rejects_empty_overlap_format_and_truncated_v2_headers() {
        let raw = b"ordinarymaskedfulldiagonal";
        let base = EffectShaderDescriptor::new(
            [9; 32],
            EffectShaderFormat::Gxp,
            vec![],
            EffectType::Vec4,
            raw,
        )
        .unwrap()
        .with_coverage_variant(8, raw)
        .unwrap()
        .with_full_target_variant(14, raw)
        .unwrap();
        let descriptor = base.clone().with_diagonal_variant(18, raw).unwrap();
        let bytes = descriptor.encode_payload(raw).unwrap();
        assert!(base.clone().with_diagonal_variant(0, raw).is_err());
        for offset in [8u32, 14, raw.len() as u32, raw.len() as u32 + 1] {
            assert!(base.clone().with_diagonal_variant(offset, raw).is_err());
            let mut invalid = bytes.clone();
            invalid[104..108].copy_from_slice(&offset.to_le_bytes());
            assert!(EffectShaderDescriptor::decode_payload(&invalid, invalid.len()).is_err());
        }
        for length in [103, 104, 107] {
            assert!(EffectShaderDescriptor::decode_payload(&bytes[..length], bytes.len()).is_err());
        }
        for (index, value) in [(12, 1), (13, 1), (14, 0)] {
            let mut invalid = bytes.clone();
            invalid[index] = value;
            assert!(EffectShaderDescriptor::decode_payload(&invalid, invalid.len()).is_err());
        }
        let ordinary = EffectShaderDescriptor::new(
            [9; 32],
            EffectShaderFormat::Gxp,
            vec![],
            EffectType::Vec4,
            raw,
        )
        .unwrap()
        .with_diagonal_variant(18, raw)
        .unwrap();
        assert_eq!(ordinary.code(raw, false), Some(&raw[..18]));
        assert!(ordinary.full_target_code(raw).is_none());
        assert_eq!(ordinary.diagonal_code(raw), Some(&raw[18..]));
    }
    #[test]
    fn footprint_envelope_is_optional_identity_bound_and_length_checked() {
        let raw = b"ordinary fragment";
        let original = EffectShaderDescriptor::new(
            [7; 32],
            EffectShaderFormat::Wgsl,
            Vec::new(),
            EffectType::Vec4,
            raw,
        )
        .unwrap();
        let proved = original
            .clone()
            .with_source_footprint(
                Some(EffectSourceFootprint {
                    logical_halo: [30, 0, 30, 0],
                }),
                raw,
            )
            .unwrap();
        assert_ne!(original.shader_key, proved.shader_key);
        let old = original.encode_payload(raw).unwrap();
        let bytes = proved.encode_payload(raw).unwrap();
        assert_eq!(bytes.len(), old.len() + 16);
        let (decoded, code) = EffectShaderDescriptor::decode_payload(&bytes, bytes.len()).unwrap();
        assert_eq!(decoded, proved);
        assert_eq!(&bytes[code], raw);
        assert!(decoded.matches_bytes(raw));
        let mut changed = decoded;
        changed.source_footprint.as_mut().unwrap().logical_halo[0] = 29;
        assert!(!changed.matches_bytes(raw));
        assert!(EffectShaderDescriptor::decode_payload(&bytes[..119], bytes.len()).is_err());
        let mut missing = old.clone();
        missing[15] = 1;
        assert!(EffectShaderDescriptor::decode_payload(&missing, missing.len()).is_err());
        assert_eq!(
            EffectShaderDescriptor::decode_payload(&old, old.len())
                .unwrap()
                .0,
            original
        );
    }
    #[test]
    fn gxp_envelopes_have_a_distinct_native_format_identity() {
        let raw = b"GXP\0test-program";
        let gxp = EffectShaderDescriptor::new(
            [1; 32],
            EffectShaderFormat::Gxp,
            vec![EffectType::Float],
            EffectType::Vec4,
            raw,
        )
        .unwrap();
        let wgsl = EffectShaderDescriptor::new(
            [1; 32],
            EffectShaderFormat::Wgsl,
            vec![EffectType::Float],
            EffectType::Vec4,
            raw,
        )
        .unwrap();
        assert_ne!(gxp.shader_key, wgsl.shader_key);
        let bytes = gxp.encode_payload(raw).unwrap();
        assert_eq!(bytes[12], 3);
        let (decoded, range) = EffectShaderDescriptor::decode_payload(&bytes, bytes.len()).unwrap();
        assert_eq!(decoded, gxp);
        assert_eq!(&bytes[range], raw);
        assert!(decoded.matches_bytes(raw));
        assert_eq!(wgsl.encode_payload(raw).unwrap()[12], 1);
        for retired in [0, 2] {
            let mut retired_payload = bytes.clone();
            retired_payload[12] = retired;
            assert_eq!(
                EffectShaderDescriptor::decode_payload(&retired_payload, retired_payload.len()),
                Err(EffectShaderPayloadError::Invalid)
            );
        }
        assert!(
            EffectShaderFormat::Gxp
                .asset_path([1; 32])
                .ends_with("/gxp.reel-effect")
        );
    }
    #[test]
    fn coverage_code_segments_are_bounded_and_content_keyed() {
        let raw = b"ordinarymasked";
        let base = EffectShaderDescriptor::new(
            [1; 32],
            EffectShaderFormat::Wgsl,
            vec![],
            EffectType::Vec4,
            raw,
        )
        .unwrap();
        let descriptor = base.clone().with_coverage_variant(8, raw).unwrap();
        assert_ne!(base.shader_key, descriptor.shader_key);
        assert_eq!(descriptor.code(raw, false), Some(&b"ordinary"[..]));
        assert_eq!(descriptor.code(raw, true), Some(&b"masked"[..]));
        let bytes = descriptor.encode_payload(raw).unwrap();
        let (decoded, range) = EffectShaderDescriptor::decode_payload(&bytes, bytes.len()).unwrap();
        assert!(decoded.matches_bytes(&bytes[range]));
        assert!(base.clone().with_coverage_variant(0, raw).is_err());
        assert!(base.with_coverage_variant(raw.len() as u32, raw).is_err());
        let mut invalid = bytes.clone();
        invalid[96..100].copy_from_slice(&(raw.len() as u32).to_le_bytes());
        assert!(EffectShaderDescriptor::decode_payload(&invalid, invalid.len()).is_err());
    }
    #[test]
    fn full_target_segment_keeps_ordinary_and_coverage_ranges_distinct() {
        let raw = b"ordinarymaskedfull";
        let descriptor = EffectShaderDescriptor::new(
            [7; 32],
            EffectShaderFormat::Wgsl,
            vec![],
            EffectType::Vec4,
            raw,
        )
        .unwrap()
        .with_coverage_variant(8, raw)
        .unwrap()
        .with_full_target_variant(14, raw)
        .unwrap();
        assert_eq!(descriptor.code(raw, false), Some(&b"ordinary"[..]));
        assert_eq!(descriptor.code(raw, true), Some(&b"masked"[..]));
        assert_eq!(descriptor.full_target_code(raw), Some(&b"full"[..]));
        let encoded = descriptor.encode_payload(raw).unwrap();
        let (decoded, range) =
            EffectShaderDescriptor::decode_payload(&encoded, encoded.len()).unwrap();
        assert_eq!(decoded, descriptor);
        assert!(decoded.matches_bytes(&encoded[range]));
        assert!(descriptor.clone().with_full_target_variant(8, raw).is_err());
        assert!(
            descriptor
                .clone()
                .with_full_target_variant(raw.len() as u32, raw)
                .is_err()
        );
        assert_ne!(
            descriptor.shader_key,
            descriptor
                .clone()
                .with_full_target_variant(15, raw)
                .unwrap()
                .shader_key
        );
    }
    #[test]
    fn prepared_envelope_preserves_schema_and_raw_range() {
        let descriptor = EffectShaderDescriptor::new(
            [17; 32],
            EffectShaderFormat::Wgsl,
            vec![
                EffectType::Bool,
                EffectType::Float,
                EffectType::Vec2,
                EffectType::Vec3,
                EffectType::Vec4,
            ],
            EffectType::Vec4,
            b"fn main() {}",
        )
        .unwrap();
        let bytes = descriptor.encode_payload(b"fn main() {}").unwrap();
        let (decoded, range) = EffectShaderDescriptor::decode_payload(&bytes, bytes.len()).unwrap();
        assert_eq!(decoded, descriptor);
        assert_eq!(&bytes[range], b"fn main() {}");
        let mut other = descriptor.clone();
        other.definition = [99; 32];
        assert!(other.matches_bytes(b"fn main() {}"));
        assert!(!other.matches_bytes(b"fn main() { discard; }"));
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
