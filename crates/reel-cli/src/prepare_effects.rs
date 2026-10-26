//! Offline effect fragments in the ordinary prepared-resource namespace.
//!
//! Definition products skip code generation on cache hits. SGX compilation has
//! a second content key over complete GLSL and the pinned compiler, so changing
//! a parameter name or default can reuse the same compiled code. Runtime hosts
//! consume the resulting envelopes without loading a compiler or the typed IR.
use crate::resource_conversion::{Prepared, Report};
use reel_engine::aside::source::{EffectDefinition, EffectProducts};
use reel_package::archive::{ArchiveKind, ArchiveWriter};
use reel_render::effect::{Language, compile_fragment};
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
            Self::Desktop => &[EffectShaderFormat::Wgsl, EffectShaderFormat::GlslEs100],
            Self::Vita => &[EffectShaderFormat::Sgx543],
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
    if target == Target::Vita && !pvr_compiler::AVAILABLE {
        return Err(io::Error::other(
            "Vita effect shaders require the pinned PVR compiler on a Linux build host; export this project with the Linux Reel CLI",
        ));
    }
    let cache = project.join("build/resources/objects");
    fs::create_dir_all(&cache)?;
    let mut compiler = None;
    for definition in products.definitions() {
        for &format in target.formats() {
            let key = identity(&[
                b"effect-product-v1",
                env!("REEL_CONVERTER_EFFECT").as_bytes(),
                &definition.id,
                format.suffix().as_bytes(),
            ]);
            let path = cache.join(format!("effect-{key}.reel-effect"));
            let size = (|| -> io::Result<usize> {
                Ok(
                    if let Some(bytes) = cached(&path)?
                        && valid_product(&bytes, definition, format)
                    {
                        prepared.report.reused += 1;
                        bytes.len()
                    } else {
                        let language = if format == EffectShaderFormat::Wgsl {
                            Language::Wgsl
                        } else {
                            Language::GlslEs100
                        };
                        let fragment =
                            compile_fragment(&definition.program, language, MAX_SHADER_BYTES)
                                .map_err(|error| {
                                    io::Error::other(format!(
                                        "effect shader generation failed: {error:?}"
                                    ))
                                })?;
                        prepared.report.source_bytes += fragment.source.len() as u64;
                        let raw = if format == EffectShaderFormat::Sgx543 {
                            sgx(&cache, &fragment.source, &mut compiler)?
                        } else {
                            fragment.source.into_bytes()
                        };
                        let descriptor = EffectShaderDescriptor::new(
                            definition.id,
                            format,
                            fragment
                                .parameters
                                .slots
                                .iter()
                                .map(|slot| slot.ty)
                                .collect(),
                            fragment.output_type,
                            &raw,
                        )
                        .map_err(io::Error::other)?;
                        let bytes = descriptor.encode_payload(&raw).map_err(io::Error::other)?;
                        if bytes.len() > MAX_SHADER_BYTES {
                            return Err(io::Error::other(
                                "prepared effect shader exceeds its byte limit",
                            ));
                        }
                        publish(&path, &bytes)?;
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
        &serde_json::to_vec(&prepared.files).map_err(io::Error::other)?,
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

fn sgx(
    cache: &Path,
    source: &str,
    compiler: &mut Option<pvr_compiler::Compiler>,
) -> io::Result<Vec<u8>> {
    let key = identity(&[
        b"effect-sgx543-fragment-v1",
        env!("REEL_EFFECT_COMPILER").as_bytes(),
        source.as_bytes(),
    ]);
    let path = cache.join(format!("effect-code-{key}.sgx"));
    // The cached binary carries a digest, so an interrupted or damaged cache is
    // rebuilt before it can enter a new typed shader envelope.
    if let Some(bytes) = cached(&path)?
        && bytes.len() > 32
        && blake3::hash(&bytes[32..]).as_bytes() == &bytes[..32]
    {
        return Ok(bytes[32..].to_vec());
    }
    if compiler.is_none() {
        *compiler = Some(pvr_compiler::Compiler::new().map_err(io::Error::other)?);
    }
    let result = compiler
        .as_mut()
        .unwrap()
        .compile_binary(pvr_compiler::Stage::Fragment, source)
        .map_err(io::Error::other)?;
    if !result.success || result.binary.is_empty() {
        return Err(io::Error::other(format!(
            "effect SGX compilation failed: {}",
            result.log
        )));
    }
    if result.binary.len() > MAX_SHADER_BYTES - 32 {
        return Err(io::Error::other("effect SGX binary exceeds its byte limit"));
    }
    let mut bytes = blake3::hash(&result.binary).as_bytes().to_vec();
    bytes.extend_from_slice(&result.binary);
    publish(&path, &bytes)?;
    Ok(result.binary)
}

fn cached(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() > MAX_SHADER_BYTES as u64 {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.take(MAX_SHADER_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok((bytes.len() <= MAX_SHADER_BYTES).then_some(bytes))
}
fn identity(fields: &[&[u8]]) -> String {
    let mut hash = blake3::Hasher::new();
    for field in fields {
        hash.update(&(field.len() as u64).to_le_bytes());
        hash.update(field);
    }
    hash.finalize().to_hex().to_string()
}
fn publish(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let result = reel_storage::publish(path, true, |file| file.write_all(bytes))?;
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
                EffectShaderFormat::Sgx543,
                input,
                io::Error::new(io::ErrorKind::InvalidData, original),
            );
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            let text = error.to_string();
            assert!(text.ends_with(original));
            assert!(
                text.contains("main::sample (effect ") && text.contains(", sgx)"),
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
