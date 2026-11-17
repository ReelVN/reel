//! Offline effect fragments in the ordinary prepared-resource namespace.
//!
//! Definition products skip code generation on cache hits. GXP compilation has
//! a second content key over complete typed Cg and the actual SDK tools, so changing
//! a parameter name or default can reuse the same compiled code. Runtime hosts
//! consume the resulting envelopes without loading a compiler or the typed IR.
use crate::resource_conversion::{Prepared, Report};
use reel_engine::aside::source::{EffectDefinition, EffectProducts};
use reel_package::archive::{ArchiveKind, ArchiveWriter};
use reel_render::effect::gxp_compiler::{Compiler, Stage};
use reel_render::effect::{
    FragmentCoverage, Language, OutputTail, compile_diagonal_fragment, compile_fragment,
    compile_fragment_variant, compile_fragment_with_tail,
};
use reel_ui::resources::{EffectShaderDescriptor, EffectShaderFormat};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

/// Maximum complete generated source and cached envelope size per effect.
pub const MAX_SHADER_BYTES: usize = 4 << 20;

/// Targets emitted for the host renderer or for an offline Vita distribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Desktop,
    Vita,
}
impl Target {
    /// Resource preview targets do not change a desktop renderer. Export selects
    /// this target from its actual destination; development always uses Desktop.
    pub fn for_export(name: &str) -> Self {
        if name == "vita" {
            Self::Vita
        } else {
            Self::Desktop
        }
    }
    fn formats(self) -> &'static [EffectShaderFormat] {
        match self {
            Self::Desktop => &[EffectShaderFormat::Wgsl],
            Self::Vita => &[EffectShaderFormat::Gxp],
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Vita => "vita",
        }
    }
}

/// Prepare immutable, typed shader resources with one compiler invocation at a
/// time. No directory is created for a project without effect definitions.
pub fn prepare(project: &Path, products: &EffectProducts, target: Target) -> io::Result<Prepared> {
    prepare_with_input(project, products, target, &project.join("src/main.aside"))
}

/// Prepare products obtained from an explicit source or ASB input. Stripped
/// packages use this path plus their retained function identity in diagnostics.
pub fn prepare_with_input(
    project: &Path,
    products: &EffectProducts,
    target: Target,
    input: &Path,
) -> io::Result<Prepared> {
    let mut prepared = Prepared {
        files: BTreeMap::new(),
        report: Report::default(),
        index: None,
    };
    if products.is_empty() {
        return Ok(prepared);
    }
    let compiler_diagnostic = |error| {
        diagnostic(
            products,
            products.definitions().next().unwrap(),
            EffectShaderFormat::Gxp,
            input,
            error,
        )
    };
    let compiler = (target == Target::Vita)
        .then(Compiler::new)
        .transpose()
        .map_err(compiler_diagnostic)?;
    let mut compiler_identity = blake3::Hasher::new();
    if let Some(compiler) = &compiler {
        compiler
            .fingerprint(|field| {
                compiler_identity.update(field);
            })
            .map_err(compiler_diagnostic)?;
    }
    let compiler_identity = compiler_identity.finalize();
    let cache = project.join("build/resources/objects");
    fs::create_dir_all(&cache)?;
    for definition in products.definitions() {
        for &format in target.formats() {
            let key = identity(&[
                b"effect-product-v1",
                env!("REEL_CONVERTER_EFFECT").as_bytes(),
                &definition.id,
                format.suffix().as_bytes(),
                compiler_identity.as_bytes(),
            ]);
            let path = cache.join(format!("effect-{key}.reel-effect"));
            let size = (|| -> io::Result<usize> {
                Ok(
                    if let Some(bytes) = cached(&path, &mut [])?
                        && valid_product(&bytes, definition, format)
                    {
                        prepared.report.reused += 1;
                        bytes.len()
                    } else {
                        let language = match format {
                            EffectShaderFormat::Wgsl => Language::Wgsl,
                            EffectShaderFormat::Gxp => Language::Cg,
                        };
                        let fragment =
                            compile_fragment(&definition.program, language, MAX_SHADER_BYTES)
                                .map_err(|error| {
                                    io::Error::other(format!(
                                        "effect shader generation failed: {error:?}"
                                    ))
                                })?;
                        prepared.report.source_bytes += fragment.source.len() as u64;
                        let mut raw = if format == EffectShaderFormat::Gxp {
                            gxp(
                                &cache,
                                &fragment.source,
                                compiler.as_ref().unwrap(),
                                compiler_identity.as_bytes(),
                            )?
                        } else {
                            fragment.source.into_bytes()
                        };
                        let coverage_offset = if OutputTail::supports_coverage(
                            fragment.output_type,
                            definition.program.textures().len() as u32,
                            fragment.source_sampler,
                        ) {
                            let masked = compile_fragment_with_tail(
                                &definition.program,
                                language,
                                OutputTail::AlphaCoverage,
                                MAX_SHADER_BYTES,
                            )
                            .map_err(io::Error::other)?;
                            prepared.report.source_bytes += masked.source.len() as u64;
                            let code = if format == EffectShaderFormat::Gxp {
                                gxp(
                                    &cache,
                                    &masked.source,
                                    compiler.as_ref().unwrap(),
                                    compiler_identity.as_bytes(),
                                )?
                            } else {
                                masked.source.into_bytes()
                            };
                            let offset = u32::try_from(raw.len()).map_err(io::Error::other)?;
                            raw.extend_from_slice(&code);
                            Some(offset)
                        } else {
                            None
                        };
                        let full = compile_fragment_variant(
                            &definition.program,
                            language,
                            OutputTail::Ordinary,
                            FragmentCoverage::FullTarget,
                            MAX_SHADER_BYTES,
                        )
                        .map_err(io::Error::other)?;
                        prepared.report.source_bytes += full.source.len() as u64;
                        let full_code = if format == EffectShaderFormat::Gxp {
                            gxp(
                                &cache,
                                &full.source,
                                compiler.as_ref().unwrap(),
                                compiler_identity.as_bytes(),
                            )?
                        } else {
                            full.source.into_bytes()
                        };
                        let full_offset =
                            if raw.len() + full_code.len() + 108 + fragment.parameters.slots.len()
                                <= MAX_SHADER_BYTES
                            {
                                let offset = u32::try_from(raw.len()).map_err(io::Error::other)?;
                                raw.extend_from_slice(&full_code);
                                Some(offset)
                            } else {
                                None
                            };
                        let descriptor = EffectShaderDescriptor::with_textures(
                            definition.id,
                            format,
                            fragment
                                .parameters
                                .slots
                                .iter()
                                .map(|slot| slot.ty)
                                .collect(),
                            definition.program.textures().len() as u32,
                            fragment.source_sampler,
                            fragment.output_type,
                            &raw,
                        )
                        .map_err(io::Error::other)?;
                        let descriptor = if let Some(offset) = coverage_offset {
                            descriptor
                                .with_coverage_variant(offset, &raw)
                                .map_err(io::Error::other)?
                        } else {
                            descriptor
                        };
                        let descriptor = if let Some(offset) = full_offset {
                            descriptor
                                .with_full_target_variant(offset, &raw)
                                .map_err(io::Error::other)?
                        } else {
                            descriptor
                        };
                        let footprint = reel_render::effect::source_footprint(&definition.program)
                            .filter(|_| {
                                raw.len() + 124 + descriptor.parameters.len() <= MAX_SHADER_BYTES
                            });
                        let mut descriptor = descriptor
                            .with_source_footprint(footprint, &raw)
                            .map_err(io::Error::other)?;
                        if format == EffectShaderFormat::Gxp {
                            let diagonal =
                                compile_diagonal_fragment(&definition.program, MAX_SHADER_BYTES)
                                    .map_err(io::Error::other)?;
                            if let Some(diagonal) = diagonal {
                                prepared.report.source_bytes += diagonal.source.len() as u64;
                                let code = gxp(
                                    &cache,
                                    &diagonal.source,
                                    compiler.as_ref().unwrap(),
                                    compiler_identity.as_bytes(),
                                )?;
                                descriptor =
                                    append_diagonal(descriptor, &mut raw, &code, MAX_SHADER_BYTES)?;
                            }
                        }
                        let bytes = descriptor.encode_payload(&raw).map_err(io::Error::other)?;
                        if bytes.len() > MAX_SHADER_BYTES {
                            return Err(io::Error::other(
                                "prepared effect shader exceeds its byte limit",
                            ));
                        }
                        publish(&path, &[&bytes])?;
                        prepared.report.converted += 1;
                        bytes.len()
                    },
                )
            })()
            .map_err(|error| diagnostic(products, definition, format, input, error))?;
            prepared.report.output_bytes += size as u64;
            prepared
                .files
                .insert(format.asset_path(definition.id), path);
        }
    }
    let directory = project.join("build/resources/effects");
    fs::create_dir_all(&directory)?;
    let index = directory.join(format!("{}.json", target.name()));
    publish(
        &index,
        &[&serde_json::to_vec(&prepared.files).map_err(io::Error::other)?],
    )?;
    prepared.index = Some(index);
    Ok(prepared)
}

fn diagnostic(
    products: &EffectProducts,
    definition: &EffectDefinition,
    format: EffectShaderFormat,
    input: &Path,
    error: io::Error,
) -> io::Error {
    let origin = products.origin(definition.id);
    let location = origin
        .and_then(|origin| origin.location.as_ref())
        .map(|location| format!("{}:{}:{}", location.file, location.line, location.column))
        .unwrap_or_else(|| input.display().to_string());
    let function = origin.map_or("unknown function", |origin| origin.function.as_ref());
    let id = blake3::Hash::from(definition.id).to_hex();
    io::Error::new(
        error.kind(),
        format!(
            "{location}: {function} (effect {id}, {})\n{error}",
            format.suffix()
        ),
    )
}

fn valid_product(bytes: &[u8], definition: &EffectDefinition, format: EffectShaderFormat) -> bool {
    EffectShaderDescriptor::decode_payload(bytes, MAX_SHADER_BYTES).is_ok_and(
        |(descriptor, raw)| {
            descriptor.definition == definition.id
                && descriptor.format == format
                && descriptor.textures as usize == definition.program.textures().len()
                && descriptor.output
                    == definition.program.nodes()[definition.program.output().0 as usize].ty
                && descriptor.parameters.iter().copied().eq(definition
                    .program
                    .parameters()
                    .iter()
                    .map(|p| p.ty))
                && descriptor.matches_bytes(&bytes[raw])
        },
    )
}

// Optional code must not remove an existing fragment or source-footprint proof
// to fit the complete current envelope limit.
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

fn gxp(
    cache: &Path,
    source: &str,
    compiler: &Compiler,
    compiler_identity: &[u8; 32],
) -> io::Result<Vec<u8>> {
    let key = identity(&[
        b"effect-gxp-fragment-v1",
        compiler_identity,
        source.as_bytes(),
    ]);
    let path = cache.join(format!("effect-code-{key}.gxp"));
    // The cached binary carries a digest, so an interrupted or damaged cache is
    // rebuilt before it can enter a new typed shader envelope.
    let mut digest = [0; 32];
    if let Some(binary) = cached(&path, &mut digest)?
        && !binary.is_empty()
        && blake3::hash(&binary).as_bytes() == &digest
    {
        return Ok(binary);
    }
    let binary = compiler.compile(Stage::Fragment, source)?;
    if binary.len() > MAX_SHADER_BYTES - 32 {
        return Err(io::Error::other("effect GXP binary exceeds its byte limit"));
    }
    publish(&path, &[blake3::hash(&binary).as_bytes(), &binary])?;
    Ok(binary)
}

fn cached(path: &Path, header: &mut [u8]) -> io::Result<Option<Vec<u8>>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() > MAX_SHADER_BYTES as u64 {
        return Ok(None);
    }
    if let Err(error) = file.read_exact(header) {
        return match error.kind() {
            io::ErrorKind::UnexpectedEof => Ok(None),
            _ => Err(error),
        };
    }
    let limit = MAX_SHADER_BYTES - header.len();
    let mut payload = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut payload)?;
    Ok((payload.len() <= limit).then_some(payload))
}
fn identity(fields: &[&[u8]]) -> String {
    let mut hash = blake3::Hasher::new();
    for field in fields {
        hash.update(&(field.len() as u64).to_le_bytes());
        hash.update(field);
    }
    hash.finalize().to_hex().to_string()
}
fn publish(path: &Path, parts: &[&[u8]]) -> io::Result<()> {
    let result = reel_storage::publish(path, true, |file| {
        for part in parts {
            file.write_all(part)?;
        }
        Ok(())
    })?;
    if let Some(warning) = result.cleanup_warning {
        eprintln!("{warning}");
    }
    Ok(())
}

/// Combine generated shaders with a selected resource view, rejecting author
/// files that occupy the same reserved logical names.
pub fn merge(files: &mut BTreeMap<String, PathBuf>, prepared: Prepared) -> io::Result<()> {
    for (name, path) in prepared.files {
        if files.contains_key(&name) {
            return Err(io::Error::other(format!(
                "resource conflicts with generated effect shader: {name}"
            )));
        }
        files.insert(name, path);
    }
    Ok(())
}

/// Publish shaders next to a portable ASB using the existing seekable resource
/// archive. Earlier ASBs retain their immutable archives across later builds.
pub fn archive(prepared: &Prepared, output: &Path) -> io::Result<Option<PathBuf>> {
    if prepared.files.is_empty() {
        return Ok(None);
    }
    fs::create_dir_all(output)?;
    let mut candidate = tempfile::NamedTempFile::new_in(output)?;
    let mut writer = ArchiveWriter::new(candidate.as_file_mut(), ArchiveKind::Resources)?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"Reel effect resources v1\0");
    for (name, path) in &prepared.files {
        let entry = writer.add(name, &mut File::open(path)?)?;
        hash.update(&(name.len() as u64).to_le_bytes());
        hash.update(name.as_bytes());
        hash.update(&entry.length.to_le_bytes());
        hash.update(&entry.version);
    }
    writer.finish()?.sync_all()?;
    let path = output.join(format!("effects-{}.rpk", hash.finalize().to_hex()));
    match candidate.persist_noclobber(&path) {
        Ok(_) => {}
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.error),
    }
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_shader_separates_digest_and_payload_and_rejects_short_headers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("effect.gxp");
        let binary = b"compiled fragment";
        let digest = blake3::hash(binary);
        publish(&path, &[digest.as_bytes(), binary]).unwrap();
        let mut header = [0; 32];
        assert_eq!(cached(&path, &mut header).unwrap().unwrap(), binary);
        assert_eq!(&header, digest.as_bytes());
        fs::write(&path, &header[..31]).unwrap();
        assert!(cached(&path, &mut header).unwrap().is_none());
    }

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

    #[test]
    fn effect_diagnostics_preserve_origin_and_original_compiler_error() {
        let checked = crate::check("\nfn sample(pixel, amount: float = 0.5) -> float { amount }\nfn main() { effect(sample); }", "src/shader.aside").unwrap();
        for profile in [crate::BuildProfile::Debug, crate::BuildProfile::Release] {
            let artifact =
                crate::build_with_profile(&checked, Some("main::main"), profile).unwrap();
            let story = reel_player::distribution::decode_story(&artifact.bytes).unwrap();
            let definition = story.effects.definitions().next().unwrap();
            let input = Path::new("compiled-input.asb");
            let original = "shader line 7: original compiler diagnostic\nsecond line";
            let error = diagnostic(
                &story.effects,
                definition,
                EffectShaderFormat::Gxp,
                input,
                io::Error::new(io::ErrorKind::InvalidData, original),
            );
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            let text = error.to_string();
            assert!(text.ends_with(original));
            assert!(
                text.contains("main::sample (effect ") && text.contains(", gxp)"),
                "{text}"
            );
            if profile == crate::BuildProfile::Debug {
                let location = story
                    .effects
                    .origin(definition.id)
                    .unwrap()
                    .location
                    .as_ref()
                    .unwrap();
                assert_eq!(location.file.as_ref(), "src/shader.aside");
                assert_eq!(location.line, 2);
                assert!(location.column > 0);
                assert!(text.starts_with("src/shader.aside:2:"), "{text}");
            } else {
                assert!(
                    story
                        .effects
                        .origin(definition.id)
                        .unwrap()
                        .location
                        .is_none()
                );
                assert!(text.starts_with("compiled-input.asb:"), "{text}");
            }
            assert!(story.effects.storage_bytes() > definition.storage_bytes());
        }
    }
}
