//! Versioned document overlays and cached compiler project analysis.
use crate::{
    HostContext, HostDeclaration, HostDocument, HostMember, NamedType, ServerOptions,
    SourceRootQuery,
    position::{Encoding, LineIndex},
};
use aside_compiler::{
    Parse,
    editor::{self, EditorProjectAnalysis, EditorSource},
    semantic::*,
};
use lsp_types::{
    Diagnostic, DiagnosticSeverity, NumberOrString, PublishDiagnosticsParams,
    TextDocumentContentChangeEvent, Uri,
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

pub(crate) type ReferenceKey = (String, String, Option<String>);
pub(crate) fn reference_key(reference: &SymbolReference) -> ReferenceKey {
    (
        reference.module.clone(),
        reference.version.clone(),
        reference.export.clone(),
    )
}

#[derive(Debug)]
pub(crate) struct HostIndex {
    pub globals: Vec<ExternalSymbol>,
    pub modules: MemoryResolver,
    pub types: BTreeMap<String, BTreeMap<String, Arc<HostMember>>>,
    pub members: BTreeMap<ReferenceKey, Arc<HostMember>>,
    pub bases: BTreeMap<String, Vec<String>>,
    pub calls: BTreeMap<String, ExternalSymbol>,
    pub awaits: BTreeMap<String, TypeHint>,
    pub declarations: BTreeMap<ReferenceKey, HostDeclaration>,
    pub documents: BTreeMap<String, HostDocument>,
}
impl From<HostContext> for HostIndex {
    fn from(host: HostContext) -> Self {
        let bases = host
            .named_types
            .iter()
            .map(|ty| (ty.name.clone(), ty.bases.clone()))
            .collect();
        let calls = host
            .named_types
            .iter()
            .filter_map(|ty| ty.call.clone().map(|call| (ty.name.clone(), call)))
            .collect();
        let awaits = host
            .named_types
            .iter()
            .filter_map(|ty| {
                ty.await_result
                    .clone()
                    .map(|result| (ty.name.clone(), result))
            })
            .collect();
        let mut members = BTreeMap::new();
        let mut types = BTreeMap::new();
        for NamedType {
            name,
            members: fields,
            ..
        } in host.named_types
        {
            let fields = fields
                .into_iter()
                .map(|member| {
                    let name = member.symbol.name.clone();
                    let member = if let Some(reference) = &member.symbol.reference {
                        members
                            .entry(reference_key(reference))
                            .or_insert_with(|| Arc::new(member))
                            .clone()
                    } else {
                        Arc::new(member)
                    };
                    (name, member)
                })
                .collect();
            types.insert(name, fields);
        }
        let mut modules = MemoryResolver::default();
        for module in host.modules {
            modules.insert(module);
        }
        Self {
            globals: host.globals,
            modules,
            bases,
            calls,
            awaits,
            types,
            members,
            declarations: host
                .declarations
                .into_iter()
                .map(|declaration| (reference_key(&declaration.reference), declaration))
                .collect(),
            documents: host
                .documents
                .into_iter()
                .map(|document| (document.uri.as_str().to_owned(), document))
                .collect(),
        }
    }
}
struct HostResolver<'a> {
    host: &'a HostIndex,
}
impl ModuleResolver for HostResolver<'_> {
    fn resolve(&mut self, request: &ImportRequest) -> Result<ResolvedImport, ResolveError> {
        self.host.modules.resolve_import(request)
    }
    fn resolve_type_member(&mut self, type_name: &str, member: &str) -> Option<HostMember> {
        self.host
            .types
            .get(type_name)?
            .get(member)
            .map(|member| member.as_ref().clone())
    }
    fn is_named_subtype(&self, actual: &str, expected: &str) -> Option<bool> {
        if !self.host.bases.contains_key(actual) || !self.host.bases.contains_key(expected) {
            return None;
        }
        let mut queue = VecDeque::from([actual]);
        let mut seen = BTreeSet::new();
        while let Some(name) = queue.pop_front() {
            if name == expected {
                return Some(true);
            }
            if !seen.insert(name) {
                continue;
            }
            if let Some(bases) = self.host.bases.get(name) {
                queue.extend(bases.iter().map(String::as_str));
            }
        }
        Some(false)
    }
    fn resolve_type_call(&mut self, name: &str) -> Option<ExternalSymbol> {
        self.host.calls.get(name).cloned()
    }
    fn resolve_type_await(&mut self, name: &str) -> Option<TypeHint> {
        self.host.awaits.get(name).cloned()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Source {
    pub uri: Uri,
    pub text: Arc<str>,
    pub version: Option<i32>,
    pub parsed: Parse,
    pub lines: LineIndex,
    // Discovery consumes textual paths. Binding supplies the canonical `from`
    // identity for this source in each project through the shared linker.
    imports: Arc<[(ImportRequest, aside_compiler::TextRange)]>,
}
impl Source {
    fn new(uri: Uri, text: Arc<str>, version: Option<i32>) -> Result<Self, String> {
        let parsed = aside_compiler::parse(&text).map_err(|error| error.to_string())?;
        let imports = editor::imports(&parsed, "", AnalysisLimits::default())
            .unwrap_or_default()
            .into();
        let lines = LineIndex::new(&text);
        Ok(Self {
            uri,
            text,
            version,
            parsed,
            lines,
            imports,
        })
    }
}
pub(crate) struct Snapshot {
    pub entry: String,
    pub project: EditorProjectAnalysis,
    pub sources: BTreeMap<String, Source>,
    dependencies: BTreeSet<String>,
}
impl Snapshot {
    pub fn module(&self) -> Option<editor::EditorModule<'_>> {
        self.project
            .modules()
            .find(|module| module.canonical_path() == self.entry)
    }
    pub fn source(&self) -> &Source {
        &self.sources[&self.entry]
    }
}
struct Document {
    source: Source,
    snapshot: Option<Arc<Snapshot>>,
}

/// Owns open text buffers. Every query for an unchanged dependency graph reuses
/// the same lossless trees and semantic snapshot; no game session is started.
pub(crate) struct Workspace {
    pub host: Arc<HostIndex>,
    source_root: Option<SourceRootQuery>,
    project_prefix: Option<String>,
    documents: BTreeMap<String, Document>,
    disk_sources: BTreeMap<String, Source>,
    pub analyses: usize,
    pub disk_reads: usize,
}
impl Workspace {
    pub fn new(options: ServerOptions) -> Self {
        Self {
            host: Arc::new(options.host.into()),
            source_root: options.source_root,
            project_prefix: options.project_prefix,
            documents: BTreeMap::new(),
            disk_sources: BTreeMap::new(),
            analyses: 0,
            disk_reads: 0,
        }
    }
    pub fn open(&mut self, uri: Uri, version: i32, text: String) -> Result<(), String> {
        let source = Source::new(uri.clone(), text.into(), Some(version))?;
        self.invalidate(&uri);
        self.documents.insert(
            source_key(&uri),
            Document {
                source,
                snapshot: None,
            },
        );
        Ok(())
    }
    pub fn change(
        &mut self,
        uri: &Uri,
        version: i32,
        changes: Vec<TextDocumentContentChangeEvent>,
        encoding: Encoding,
    ) -> Result<bool, String> {
        let key = source_key(uri);
        let Some(document) = self.documents.get(&key) else {
            return Err("document is not open".into());
        };
        if document
            .source
            .version
            .is_some_and(|current| version <= current)
        {
            return Ok(false);
        }
        let mut text = document.source.text.to_string();
        for change in changes {
            if let Some(range) = change.range {
                let index = LineIndex::new(&text);
                let start = index
                    .offset(&text, range.start, encoding)
                    .ok_or("invalid change start position")?;
                let end = index
                    .offset(&text, range.end, encoding)
                    .ok_or("invalid change end position")?;
                if start > end {
                    return Err("change range is reversed".into());
                }
                text.replace_range(start..end, &change.text);
            } else {
                text = change.text;
            }
        }
        let source = Source::new(uri.clone(), text.into(), Some(version))?;
        self.invalidate(uri);
        self.documents.insert(
            key,
            Document {
                source,
                snapshot: None,
            },
        );
        Ok(true)
    }
    pub fn close(&mut self, uri: &Uri) {
        self.documents.remove(&source_key(uri));
        self.invalidate(uri);
        if self.documents.is_empty() {
            self.disk_sources.clear();
        }
    }
    pub fn open_uris(&self) -> Vec<Uri> {
        self.documents
            .values()
            .map(|document| document.source.uri.clone())
            .collect()
    }
    /// Format the cached lossless tree without constructing a semantic snapshot.
    /// Syntax errors already have diagnostics and leave the editor buffer unchanged.
    pub fn format(
        &self,
        uri: &Uri,
        encoding: Encoding,
    ) -> Result<Vec<lsp_types::TextEdit>, String> {
        let document = self
            .documents
            .get(&source_key(uri))
            .ok_or("document is not open")?;
        let source = &document.source;
        let Ok(formatted) = aside_compiler::format_parsed(&source.parsed) else {
            return Ok(Vec::new());
        };
        if formatted == source.text.as_ref() {
            return Ok(Vec::new());
        }
        let length = u32::try_from(source.text.len()).expect("parsed source has a u32 byte range");
        Ok(vec![lsp_types::TextEdit {
            range: source.lines.range(
                &source.text,
                aside_compiler::TextRange::new(0.into(), length.into()),
                encoding,
            ),
            new_text: formatted,
        }])
    }
    pub fn invalidate(&mut self, uri: &Uri) {
        let uri = source_key(uri);
        self.disk_sources.remove(&uri);
        for (key, document) in &mut self.documents {
            if key == &uri
                || document
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.dependencies.contains(&uri))
            {
                document.snapshot = None;
            }
        }
    }
    pub fn snapshot(&mut self, uri: &Uri) -> Result<Arc<Snapshot>, String> {
        let key = source_key(uri);
        let document = self.documents.get(&key).ok_or("document is not open")?;
        if let Some(snapshot) = &document.snapshot {
            return Ok(snapshot.clone());
        }
        let (roots, mut dependencies) = self.effective_roots(uri);
        let entry = module_path(uri, &roots).unwrap_or_else(|| uri.as_str().to_owned());
        let mut sources = BTreeMap::from([(entry.clone(), document.source.clone())]);
        dependencies.insert(key.clone());
        let mut queue = VecDeque::from([entry.clone()]);
        let mut visited = BTreeSet::new();
        while let Some(module) = queue.pop_front() {
            if !visited.insert(module.clone()) {
                continue;
            }
            let requests = sources[&module].imports.clone();
            for (request, _) in requests.iter() {
                if self
                    .host
                    .modules
                    .modules
                    .contains_key(&request.path.join("::"))
                    || request.path.split_last().is_some_and(|(_, prefix)| {
                        self.host.modules.modules.contains_key(&prefix.join("::"))
                    })
                {
                    continue;
                }
                for candidate in [
                    request.path.join("::"),
                    request
                        .path
                        .split_last()
                        .map(|(_, prefix)| prefix.join("::"))
                        .unwrap_or_default(),
                ] {
                    if sources.contains_key(&candidate) {
                        break;
                    }
                    let Some(path) = module_file(&candidate, &roots) else {
                        continue;
                    };
                    let Ok(candidate_uri) = file_uri(&path) else {
                        continue;
                    };
                    let candidate_key = source_key(&candidate_uri);
                    dependencies.insert(candidate_key.clone());
                    let source = if let Some(open) = self.documents.get(&candidate_key) {
                        Some(open.source.clone())
                    } else if let Some(cached) = self.disk_sources.get(&candidate_key) {
                        Some(cached.clone())
                    } else {
                        match std::fs::read_to_string(&path) {
                            Ok(text) => {
                                let source = Source::new(candidate_uri, text.into(), None)?;
                                self.disk_reads += 1;
                                self.disk_sources.insert(candidate_key, source.clone());
                                Some(source)
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                            Err(error) => {
                                return Err(format!(
                                    "cannot read {}: {error}",
                                    candidate_uri.as_str()
                                ));
                            }
                        }
                    };
                    if let Some(source) = source {
                        sources.insert(candidate.clone(), source);
                        queue.push_back(candidate);
                        break;
                    }
                }
            }
        }
        let inputs: Vec<_> = sources
            .iter()
            .map(|(module, source)| EditorSource {
                source: SourceModule {
                    canonical_path: module.clone(),
                    source_path: Some(source.uri.as_str().to_owned()),
                    version: source
                        .version
                        .map_or_else(|| "disk".into(), |version| version.to_string()),
                    source: source.text.clone(),
                },
                parsed: source.parsed.clone(),
            })
            .collect();
        let options = AnalysisOptions {
            module: entry.clone(),
            hosts: self.host.globals.clone(),
            ..Default::default()
        };
        let project =
            editor::analyze_project(&inputs, &options, &mut HostResolver { host: &self.host })
                .map_err(|error| error.to_string())?;
        let snapshot = Arc::new(Snapshot {
            entry,
            project,
            sources,
            dependencies,
        });
        self.analyses += 1;
        self.documents.get_mut(&key).unwrap().snapshot = Some(snapshot.clone());
        Ok(snapshot)
    }
    fn effective_roots(&self, uri: &Uri) -> (Vec<ModuleRoot>, BTreeSet<String>) {
        let Some(path) = uri_path(uri) else {
            return (Vec::new(), BTreeSet::new());
        };
        let Some(parent) = path.parent() else {
            return (Vec::new(), BTreeSet::new());
        };
        let dependencies = RefCell::new(BTreeSet::new());
        let project = self.source_root.and_then(|query| {
            query(parent, &|candidate| {
                let open = file_uri(candidate).ok().is_some_and(|uri| {
                    let key = source_key(&uri);
                    dependencies.borrow_mut().insert(key.clone());
                    self.documents.contains_key(&key)
                });
                open || candidate.is_file()
            })
        });
        let source = normalize_drive(project.clone().unwrap_or_else(|| parent.to_path_buf()));
        let mut roots = vec![ModuleRoot {
            prefix: String::new(),
            path: source.clone(),
        }];
        if project.is_some()
            && let Some(prefix) = &self.project_prefix
        {
            roots.push(ModuleRoot {
                prefix: prefix.clone(),
                path: source,
            });
        }
        (roots, dependencies.into_inner())
    }
    pub fn diagnostics(
        &mut self,
        uri: &Uri,
        encoding: Encoding,
    ) -> Result<Vec<PublishDiagnosticsParams>, String> {
        let snapshot = self.snapshot(uri)?;
        Ok(snapshot
            .sources
            .iter()
            .map(|(module, source)| {
                let diagnostics = snapshot
                    .project
                    .diagnostics
                    .iter()
                    .filter(|diagnostic| &diagnostic.module == module)
                    .map(|diagnostic| {
                        let diagnostic = &diagnostic.diagnostic;
                        Diagnostic {
                            range: source.lines.range(&source.text, diagnostic.range, encoding),
                            severity: Some(match diagnostic.severity {
                                aside_compiler::Severity::Error => DiagnosticSeverity::ERROR,
                                aside_compiler::Severity::Warning => DiagnosticSeverity::WARNING,
                            }),
                            code: Some(NumberOrString::String(diagnostic.code.as_str().into())),
                            source: Some("aside".into()),
                            message: diagnostic.message.clone(),
                            ..Default::default()
                        }
                    })
                    .collect();
                PublishDiagnosticsParams {
                    uri: source.uri.clone(),
                    version: source.version,
                    diagnostics,
                }
            })
            .collect())
    }
}

struct ModuleRoot {
    prefix: String,
    path: PathBuf,
}

pub(crate) fn file_uri(path: &Path) -> Result<Uri, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(path)
    };
    let url = url::Url::from_file_path(absolute).map_err(|_| "invalid source file path")?;
    Uri::from_str(url.as_str()).map_err(|error| error.to_string())
}
pub(crate) fn uri_path(uri: &Uri) -> Option<PathBuf> {
    Some(normalize_drive(
        url::Url::parse(uri.as_str()).ok()?.to_file_path().ok()?,
    ))
}
fn normalize_drive(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    let path = {
        use std::path::{Component, Prefix};
        let mut components = path.components();
        if let Some(Component::Prefix(prefix)) = components.next()
            && let Prefix::VerbatimDisk(letter) = prefix.kind()
        {
            let mut normalized = PathBuf::from(format!("{}:", char::from(letter)));
            normalized.push(components.as_path());
            normalized
        } else {
            path
        }
    };
    #[cfg(windows)]
    if let Some(text) = path.to_str()
        && text.as_bytes().get(1) == Some(&b':')
        && text.as_bytes()[0].is_ascii_alphabetic()
    {
        return PathBuf::from(format!("{}{}", text[..1].to_ascii_lowercase(), &text[1..]));
    }
    path
}
// File URI spelling is a protocol boundary: VS Code escapes the drive colon
// and lowercases its letter. Buffers retain their client URI for all responses.
fn source_key(uri: &Uri) -> String {
    let Some(path) = uri_path(uri) else {
        return uri.as_str().to_owned();
    };
    let mut url =
        url::Url::from_file_path(path).expect("a decoded absolute file URI remains a file path");
    let mut path = url.path().to_owned();
    if path.as_bytes().get(2) == Some(&b':')
        && path.as_bytes().get(1).is_some_and(u8::is_ascii_alphabetic)
    {
        path.replace_range(1..2, &path[1..2].to_ascii_lowercase());
        url.set_path(&path);
    }
    url.into()
}
fn module_path(uri: &Uri, roots: &[ModuleRoot]) -> Option<String> {
    let path = uri_path(uri)?;
    roots
        .iter()
        .filter_map(|root| {
            let relative = path.strip_prefix(&root.path).ok()?;
            let mut parts: Vec<_> = relative
                .with_extension("")
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect();
            if !root.prefix.is_empty() {
                parts.insert(0, root.prefix.clone());
            }
            Some((
                (root.path.components().count(), root.prefix.len()),
                parts.join("::"),
            ))
        })
        .max_by_key(|(priority, _)| *priority)
        .map(|(_, module)| module)
}
fn module_file(module: &str, roots: &[ModuleRoot]) -> Option<PathBuf> {
    roots
        .iter()
        .filter_map(|root| {
            let suffix = if root.prefix.is_empty() {
                module
            } else {
                module.strip_prefix(&format!("{}::", root.prefix))?
            };
            if suffix.is_empty() {
                return None;
            }
            let mut path = root.path.clone();
            for part in suffix.split("::") {
                if part.is_empty() {
                    return None;
                }
                path.push(part);
            }
            path.set_extension("aside");
            Some((root.prefix.len(), path))
        })
        .max_by_key(|(length, _)| *length)
        .map(|(_, path)| path)
}
