use super::{
    FragmentCoverage, Language, OutputTail, compile_diagonal_fragment, compile_fragment,
    compile_fragment_variant, compile_fragment_with_tail, source_footprint,
};
use reel_ui::{
    resources::{EffectShaderDescriptor, EffectShaderFormat},
    stage::effect::{EffectDefinitionId, EffectProgram},
};
use std::io;

/// Maximum generated source and encoded asset size for one effect.
pub const MAX_SHADER_BYTES: usize = 4 << 20;

/// Browser and desktop consume WGSL directly. Vita supplies its offline compiler.
pub enum ShaderTarget<'a> {
    Wgsl,
    Gxp(&'a mut dyn FnMut(&str) -> io::Result<Vec<u8>>),
}

/// Encoded shader asset and the bytes of source generated to produce it.
pub struct PreparedShader {
    pub bytes: Vec<u8>,
    pub source_bytes: u64,
}

/// Generate all admitted variants and encode the shader asset consumed by the player.
pub fn prepare_shader(
    id: EffectDefinitionId,
    program: &EffectProgram,
    mut target: ShaderTarget<'_>,
) -> io::Result<PreparedShader> {
    let (language, format) = match &target {
        ShaderTarget::Wgsl => (Language::Wgsl, EffectShaderFormat::Wgsl),
        ShaderTarget::Gxp(_) => (Language::Cg, EffectShaderFormat::Gxp),
    };
    let mut source_bytes = 0;
    let mut encode = |source: String| {
        source_bytes += source.len() as u64;
        match &mut target {
            ShaderTarget::Wgsl => Ok(source.into_bytes()),
            ShaderTarget::Gxp(compile) => compile(&source),
        }
    };

    let fragment = compile_fragment(program, language, MAX_SHADER_BYTES)
        .map_err(|error| io::Error::other(format!("effect shader generation failed: {error:?}")))?;
    let mut raw = encode(fragment.source)?;
    let textures = program.textures().len() as u32;
    let coverage_offset =
        if OutputTail::supports_coverage(fragment.output_type, textures, fragment.source_sampler) {
            let masked = compile_fragment_with_tail(
                program,
                language,
                OutputTail::AlphaCoverage,
                MAX_SHADER_BYTES,
            )
            .map_err(io::Error::other)?;
            let code = encode(masked.source)?;
            let offset = u32::try_from(raw.len()).map_err(io::Error::other)?;
            raw.extend_from_slice(&code);
            Some(offset)
        } else {
            None
        };
    let full = compile_fragment_variant(
        program,
        language,
        OutputTail::Ordinary,
        FragmentCoverage::FullTarget,
        MAX_SHADER_BYTES,
    )
    .map_err(io::Error::other)?;
    let full_code = encode(full.source)?;
    let parameter_count = fragment.parameters.slots.len();
    let full_offset = if raw.len() + full_code.len() + 108 + parameter_count <= MAX_SHADER_BYTES {
        let offset = u32::try_from(raw.len()).map_err(io::Error::other)?;
        raw.extend_from_slice(&full_code);
        Some(offset)
    } else {
        None
    };

    let mut descriptor = EffectShaderDescriptor::with_textures(
        id,
        format,
        fragment
            .parameters
            .slots
            .iter()
            .map(|slot| slot.ty)
            .collect(),
        textures,
        fragment.source_sampler,
        fragment.output_type,
        &raw,
    )
    .map_err(io::Error::other)?;
    if let Some(offset) = coverage_offset {
        descriptor = descriptor
            .with_coverage_variant(offset, &raw)
            .map_err(io::Error::other)?;
    }
    if let Some(offset) = full_offset {
        descriptor = descriptor
            .with_full_target_variant(offset, &raw)
            .map_err(io::Error::other)?;
    }
    let footprint =
        source_footprint(program).filter(|_| raw.len() + 124 + parameter_count <= MAX_SHADER_BYTES);
    descriptor = descriptor
        .with_source_footprint(footprint, &raw)
        .map_err(io::Error::other)?;
    if format == EffectShaderFormat::Gxp
        && let Some(diagonal) =
            compile_diagonal_fragment(program, MAX_SHADER_BYTES).map_err(io::Error::other)?
    {
        let code = encode(diagonal.source)?;
        descriptor = append_diagonal(descriptor, &mut raw, &code, MAX_SHADER_BYTES)?;
    }

    let bytes = descriptor.encode_payload(&raw).map_err(io::Error::other)?;
    if bytes.len() > MAX_SHADER_BYTES {
        return Err(io::Error::other(
            "prepared effect shader exceeds its byte limit",
        ));
    }
    Ok(PreparedShader {
        bytes,
        source_bytes,
    })
}

// Optional code must fit alongside the existing fragment and source-footprint proof.
fn append_diagonal(
    descriptor: EffectShaderDescriptor,
    raw: &mut Vec<u8>,
    code: &[u8],
    limit: usize,
) -> io::Result<EffectShaderDescriptor> {
    let header = 108 + 16 * usize::from(descriptor.source_footprint.is_some());
    let size = raw
        .len()
        .checked_add(code.len())
        .and_then(|size| size.checked_add(header))
        .and_then(|size| size.checked_add(descriptor.parameters.len()));
    if size.is_none_or(|size| size > limit) {
        return Ok(descriptor);
    }
    let offset = u32::try_from(raw.len()).map_err(io::Error::other)?;
    raw.extend_from_slice(code);
    descriptor
        .with_diagonal_variant(offset, raw)
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_diagonal_code_keeps_all_existing_segments_at_the_complete_limit() {
        let mut raw = b"ordinaryfull".to_vec();
        let original = EffectShaderDescriptor::new(
            [7; 32],
            EffectShaderFormat::Gxp,
            vec![reel_ui::stage::effect::EffectType::Float],
            reel_ui::stage::effect::EffectType::Vec4,
            &raw,
        )
        .unwrap()
        .with_full_target_variant(8, &raw)
        .unwrap()
        .with_source_footprint(
            Some(reel_ui::resources::EffectSourceFootprint {
                logical_halo: [30, 0, 30, 0],
            }),
            &raw,
        )
        .unwrap();
        let limit = 108 + 16 + original.parameters.len() + raw.len() + 8;
        let refused = append_diagonal(original.clone(), &mut raw, b"diagonal", limit - 1).unwrap();
        assert_eq!(refused, original);
        assert_eq!(raw, b"ordinaryfull");
        assert_eq!(&refused.encode_payload(&raw).unwrap()[..8], b"REELSHD2");
        let admitted = append_diagonal(original, &mut raw, b"diagonal", limit).unwrap();
        assert_eq!(admitted.code(&raw, false), Some(&b"ordinary"[..]));
        assert_eq!(admitted.full_target_code(&raw), Some(&b"full"[..]));
        assert_eq!(admitted.diagonal_code(&raw), Some(&b"diagonal"[..]));
        assert_eq!(admitted.encode_payload(&raw).unwrap().len(), limit);
    }
}
