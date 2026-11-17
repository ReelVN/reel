//! Versioned document overlays and cached compiler project analysis.
use crate::{
    HostContext, HostDeclaration, HostDocument, HostMember, NamedType, ServerOptions,
    SourceAnalysis, SourceFeatures, SourceRootQuery,
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
    num::NonZeroUsize,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, OnceLock},
};

mod module_paths;
pub(crate) use module_paths::import_path_at;

pub(crate) type ReferenceKey = (String, String, Option<String>);
// Bound retained disk inputs across project-root and import-graph changes.
// Active semantic snapshots retain their own shared sources independently.
const MAX_CACHED_DISK_SOURCES: NonZeroUsize = NonZeroUsize::new(1024).unwrap();
const MAX_CACHED_DISK_BYTES: usize = 32 * 1024 * 1024;
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
    pub declarations: BTreeMap<ReferenceKey, Vec<HostDeclaration>>,
    pub documents: BTreeMap<String, IndexedHostDocument>,
}

#[derive(Debug, Clone)]
pub(crate) struct IndexedHostDocument {
    pub document: HostDocument,
    pub lines: LineIndex,
}
impl std::ops::Deref for IndexedHostDocument {
    type Target = HostDocument;
    fn deref(&self) -> &Self::Target {
        &self.document
    }
}
impl From<HostContext> for HostIndex {
    fn from(mut host: HostContext) -> Self {
        let catalog = builtin_catalog();
        let builtin_uri = Uri::from_str("aside-native://builtins/v1.aside").unwrap();
        host.documents.push(HostDocument {
            uri: builtin_uri.clone(),
            text: catalog.source.clone(),
            references: Vec::new(),
        });
        for group in &catalog.types {
            for member in &group.members {
                host.declarations.push(HostDeclaration {
                    reference: member.symbol.reference.as_deref().unwrap().clone(),
                    signature: member.symbol.signature.clone(),
                    markdown: member.documentation.into(),
                    definition: Some(crate::HostLocation {
                        uri: builtin_uri.clone(),
                        range: member.definition,
                    }),
                    parameter_defaults: member
                        .parameter_defaults
                        .iter()
                        .map(|(name, value)| {
                            (
                                (*name).into(),
                                crate::ParameterDefault::Value((*value).into()),
                            )
                        })
                        .collect(),
                    property_parameters: Vec::new(),
                    parameter_documentation: BTreeMap::new(),
                    property_groups: Vec::new(),
                    property_default: None,
                });
            }
        }
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
                    let member = Arc::new(member);
                    if let Some(reference) = &member.symbol.reference {
                        members
                            .entry(reference_key(reference))
                            .or_insert_with(|| member.clone());
                    }
                    (name, member)
                })
                .collect();
            types.insert(name, fields);
        }
        let mut modules = MemoryResolver::default();
        for module in host.modules {
            modules.insert(module);
        }
        let mut declarations: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for declaration in host.declarations {
            declarations
                .entry(reference_key(&declaration.reference))
                .or_default()
                .push(declaration);
        }
        Self {
            globals: host.globals,
            modules,
            bases,
            calls,
            awaits,
            types,
            members,
            declarations,
            documents: host
                .documents
                .into_iter()
                .map(|document| {
                    let lines = LineIndex::new(&document.text);
                    (
                        document.uri.as_str().to_owned(),
                        IndexedHostDocument { document, lines },
                    )
                })
                .collect(),
        }
    }
}
impl HostIndex {
    pub fn declaration(&self, symbol: &ExternalSymbol) -> Option<&HostDeclaration> {
        let candidates = self
            .declarations
            .get(&reference_key(symbol.reference.as_deref()?))?;
        candidates
            .iter()
            .find(|declaration| declaration.signature == symbol.signature)
            .or_else(|| candidates.first())
    }
}
struct HostResolver<'a> {
    host: &'a HostIndex,
    aliases: &'a BTreeMap<String, String>,
}
impl ModuleResolver for HostResolver<'_> {
    fn has_closed_members(&self, type_name: &str) -> bool {
        self.host.types.contains_key(type_name)
    }
    fn resolve(&mut self, request: &ImportRequest) -> Result<ResolvedImport, ResolveError> {
        self.host.modules.resolve_import(request)
    }
    fn canonical_request<'a>(
        &self,
        request: &'a ImportRequest,
    ) -> std::borrow::Cow<'a, ImportRequest> {
        canonical_import(request, self.aliases)
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
#[derive(Clone)]
pub(crate) struct Snapshot {
    pub entry: String,
    pub project: Arc<EditorProjectAnalysis>,
    pub sources: Arc<BTreeMap<String, Source>>,
    features: Arc<BTreeMap<String, OnceLock<SourceFeatures>>>,
    source_analysis: Option<SourceAnalysis>,
    dependencies: Arc<BTreeSet<String>>,
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
    /// Build host-owned facts once per module, shared by every entry view.
    pub fn source_features(&self, path: &str) -> Option<&SourceFeatures> {
        let analyze = self.source_analysis?;
        Some(self.features.get(path)?.get_or_init(|| {
            let module = self
                .project
                .modules()
                .find(|module| module.canonical_path() == path)
                .expect("feature slots belong to analyzed modules");
            analyze(module)
        }))
    }
}
struct Document {
    source: Source,
    snapshot: Option<Arc<Snapshot>>,
    analysis_size: usize,
}

pub(crate) struct DiagnosticWarmup {
    projects: VecDeque<Arc<Snapshot>>,
    last_module: Option<String>,
}
impl DiagnosticWarmup {
    /// Initialize at most one module, yielding to the transport between calls.
    /// Return true after every current module's host features are ready.
    pub fn step(&mut self) -> bool {
        use std::ops::Bound;
        while let Some(snapshot) = self.projects.front() {
            let start = self
                .last_module
                .as_deref()
                .map_or(Bound::Unbounded, Bound::Excluded);
            let next = snapshot
                .features
                .range::<str, _>((start, Bound::Unbounded))
                .find(|(_, features)| features.get().is_none())
                .map(|(path, _)| path.clone());
            if let Some(path) = next {
                snapshot.source_features(&path);
                self.last_module = Some(path);
                return false;
            }
            self.projects.pop_front();
            self.last_module = None;
        }
        true
    }
}

/// Owns open text buffers. Every query for an unchanged dependency graph reuses
/// the same lossless trees and semantic snapshot; no game session is started.
/// Recent disk parses retain at most 1,024 inputs and 32 MiB of source text.
pub(crate) struct Workspace {
    pub host: Arc<HostIndex>,
    source_root: Option<SourceRootQuery>,
    project_prefix: Option<String>,
    source_analysis: Option<SourceAnalysis>,
    documents: BTreeMap<String, Document>,
    disk_sources: lru::LruCache<String, Source>,
    disk_bytes: usize,
    pub analyses: usize,
    pub disk_reads: usize,
}
impl Workspace {
    pub fn new(options: ServerOptions) -> Self {
        Self {
            host: Arc::new(options.host.into()),
            source_root: options.source_root,
            project_prefix: options.project_prefix,
            source_analysis: options.source_analysis,
            documents: BTreeMap::new(),
            disk_sources: lru::LruCache::new(MAX_CACHED_DISK_SOURCES),
            disk_bytes: 0,
            analyses: 0,
            disk_reads: 0,
        }
    }
    pub fn open(&mut self, uri: Uri, version: i32, text: String) -> Result<(), String> {
        let key = source_key(&uri);
        let analysis_size = self
            .documents
            .get(&key)
            .map_or(0, |document| document.analysis_size);
        let cached = self
            .documents
            .get(&key)
            .map(|document| &document.source)
            .or_else(|| self.disk_sources.peek(&key));
        let unchanged = cached.is_some_and(|source| source.text.as_ref() == text);
        let source = if let Some(cached) = cached.filter(|_| unchanged) {
            Source {
                uri: uri.clone(),
                version: Some(version),
                ..cached.clone()
            }
        } else {
            Source::new(uri.clone(), text.into(), Some(version))?
        };
        if unchanged {
            self.refresh_sources(&key, &source);
        } else {
            self.invalidate_key(&key);
        }
        self.documents.insert(
            key,
            Document {
                source,
                snapshot: None,
                analysis_size,
            },
        );
        Ok(())
    }

    // Opening an unchanged imported file changes its protocol URI and version,
    // while retaining compiler identities and the shared semantic project.
    fn refresh_sources(&mut self, key: &str, source: &Source) {
        let mut refreshed = BTreeMap::new();
        for document in self.documents.values_mut() {
            let Some(snapshot) = &document.snapshot else {
                continue;
            };
            if !snapshot
                .sources
                .values()
                .any(|source| source_key(&source.uri) == key)
            {
                continue;
            }
            let sources = refreshed
                .entry(Arc::as_ptr(&snapshot.sources) as usize)
                .or_insert_with(|| {
                    let mut sources = snapshot.sources.as_ref().clone();
                    for cached in sources.values_mut() {
                        if source_key(&cached.uri) == key {
                            *cached = source.clone();
                        }
                    }
                    Arc::new(sources)
                })
                .clone();
            document.snapshot = Some(Arc::new(Snapshot {
                sources,
                ..snapshot.as_ref().clone()
            }));
        }
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
        let analysis_size = document.analysis_size;
        self.invalidate_key(&key);
        self.documents.insert(
            key,
            Document {
                source,
                snapshot: None,
                analysis_size,
            },
        );
        Ok(true)
    }
    pub fn close(&mut self, uri: &Uri) {
        let key = source_key(uri);
        self.documents.remove(&key);
        self.invalidate_key(&key);
        if self.documents.is_empty() {
            self.disk_sources.clear();
            self.disk_bytes = 0;
        }
    }
    pub fn open_uris(&self) -> Vec<Uri> {
        // Rebuild the widest known root first so dependent entry views reuse it.
        let mut documents: Vec<_> = self.documents.values().collect();
        documents.sort_by_key(|document| std::cmp::Reverse(document.analysis_size));
        documents
            .into_iter()
            .map(|document| document.source.uri.clone())
            .collect()
    }

    /// Capture one cursor per current shared graph for cooperative publication.
    pub fn diagnostic_warmup(&mut self) -> DiagnosticWarmup {
        let mut projects = VecDeque::new();
        let mut seen = BTreeSet::new();
        if self.source_analysis.is_some() {
            for uri in self.open_uris() {
                if self.host.documents.contains_key(uri.as_str())
                    || module_paths::index_origin(&uri).is_some()
                {
                    continue;
                }
                // Failed snapshots retain the normal diagnostic failure path;
                // they have no host features to warm before publication.
                if let Ok(snapshot) = self.snapshot(&uri)
                    && seen.insert(Arc::as_ptr(&snapshot.features) as usize)
                {
                    projects.push_back(snapshot);
                }
            }
        }
        DiagnosticWarmup {
            projects,
            last_module: None,
        }
    }
    /// Format the cached lossless tree without constructing a semantic snapshot.
    /// Syntax errors already have diagnostics and leave the editor buffer unchanged.
    pub fn format(
        &self,
        uri: &Uri,
        encoding: Encoding,
    ) -> Result<Vec<lsp_types::TextEdit>, String> {
        let source = self.open_source(uri)?;
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
    pub(crate) fn open_source(&self, uri: &Uri) -> Result<&Source, String> {
        self.documents
            .get(&source_key(uri))
            .map(|document| &document.source)
            .ok_or_else(|| "document is not open".into())
    }
    pub fn invalidate(&mut self, uri: &Uri) {
        self.invalidate_key(&source_key(uri));
    }
    fn invalidate_key(&mut self, key: &str) {
        if let Some(source) = self.disk_sources.pop(key) {
            self.disk_bytes -= source.text.len();
        }
        for (document_key, document) in &mut self.documents {
            if document_key == key
                || document
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.dependencies.contains(key))
            {
                document.snapshot = None;
            }
        }
    }
    fn cached_disk_source(&mut self, key: &str) -> Option<Source> {
        self.disk_sources.get(key).cloned()
    }
    fn cache_disk_source(&mut self, key: String, source: Source) {
        self.disk_bytes += source.text.len();
        if let Some((_, removed)) = self.disk_sources.push(key, source) {
            self.disk_bytes -= removed.text.len();
        }
        // Recent disk parses survive temporary project-root/import changes.
        while self.disk_bytes > MAX_CACHED_DISK_BYTES {
            let (_, source) = self
                .disk_sources
                .pop_lru()
                .expect("disk bytes count retained sources");
            self.disk_bytes -= source.text.len();
        }
    }
    pub fn snapshot(&mut self, uri: &Uri) -> Result<Arc<Snapshot>, String> {
        let key = source_key(uri);
        let document = self.documents.get(&key).ok_or("document is not open")?;
        if let Some(snapshot) = &document.snapshot {
            return Ok(snapshot.clone());
        }
        self.snapshot_from(document.source.clone(), key)
    }
    pub(crate) fn reference_project(
        &mut self,
        uri: &Uri,
        current: &Snapshot,
    ) -> Result<Option<Arc<Snapshot>>, String> {
        let (roots, _) = self.effective_roots(uri);
        let Some(root) = roots.first() else {
            return Ok(None);
        };
        let path = root.path.join("main.aside");
        let entry_uri = file_uri(&path)?;
        let key = source_key(&entry_uri);
        if module_path(&entry_uri, &roots).is_some_and(|entry| current.sources.contains_key(&entry))
        {
            return Ok(None);
        }
        self.file_source(&path, entry_uri, &key)?
            .map(|source| self.snapshot_from(source, key))
            .transpose()
    }
    fn file_source(&mut self, path: &Path, uri: Uri, key: &str) -> Result<Option<Source>, String> {
        if let Some(open) = self.documents.get(key) {
            return Ok(Some(open.source.clone()));
        }
        if let Some(cached) = self.cached_disk_source(key) {
            return Ok(Some(cached));
        }
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let source = Source::new(uri, text.into(), None)?;
                self.disk_reads += 1;
                self.cache_disk_source(key.to_owned(), source.clone());
                Ok(Some(source))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("cannot read {}: {error}", uri.as_str())),
        }
    }
    fn snapshot_from(&mut self, source: Source, key: String) -> Result<Arc<Snapshot>, String> {
        let uri = &source.uri;
        let (roots, mut dependencies) = self.effective_roots(uri);
        let entry = module_path(uri, &roots).unwrap_or_else(|| uri.as_str().to_owned());
        if let Some(shared) = self
            .documents
            .values()
            .filter_map(|document| document.snapshot.as_ref())
            .find(|snapshot| {
                snapshot.sources.get(&entry).is_some_and(|candidate| {
                    source_key(&candidate.uri) == key && candidate.text == source.text
                })
            })
        {
            let snapshot = Arc::new(Snapshot {
                entry,
                ..shared.as_ref().clone()
            });
            if let Some(document) = self.documents.get_mut(&key) {
                document.snapshot = Some(snapshot.clone());
            }
            return Ok(snapshot);
        }
        let mut sources = BTreeMap::from([(entry.clone(), source)]);
        dependencies.insert(key.clone());
        let mut queue = VecDeque::from([entry.clone()]);
        let mut visited = BTreeSet::new();
        let mut aliases = BTreeMap::new();
        let prefix = roots.last().map_or("", |root| root.prefix.as_str());
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
                if let Some(first) = request.path.first()
                    && !aliases.contains_key(first)
                    && let Some(alias) = self.project_import_name(first, prefix)
                {
                    aliases.insert(first.clone(), alias);
                }
                let request = canonical_import(request, &aliases);
                let path = request.path.as_slice();
                for candidate in [
                    (request.kind != ImportKind::Symbol).then_some(path),
                    (request.kind != ImportKind::Wildcard && path.len() > 1)
                        .then(|| &path[..path.len() - 1]),
                ]
                .into_iter()
                .flatten()
                {
                    let candidate = candidate.join("::");
                    if sources.contains_key(&candidate) {
                        continue;
                    }
                    let Some(path) = module_file(&candidate, &roots) else {
                        continue;
                    };
                    let Ok(candidate_uri) = file_uri(&path) else {
                        continue;
                    };
                    let candidate_key = source_key(&candidate_uri);
                    dependencies.insert(candidate_key.clone());
                    if let Some(source) = self.file_source(&path, candidate_uri, &candidate_key)? {
                        sources.insert(candidate.clone(), source);
                        queue.push_back(candidate);
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
        let project = editor::analyze_project(
            &inputs,
            &options,
            &mut HostResolver {
                host: &self.host,
                aliases: &aliases,
            },
        )
        .map_err(|error| error.to_string())?;
        let features = self.source_analysis.map_or_else(BTreeMap::new, |_| {
            project
                .modules()
                .map(|module| (module.canonical_path().to_owned(), OnceLock::new()))
                .collect()
        });
        let snapshot = Arc::new(Snapshot {
            entry,
            project: Arc::new(project),
            sources: Arc::new(sources),
            features: Arc::new(features),
            source_analysis: self.source_analysis,
            dependencies: Arc::new(dependencies),
        });
        self.analyses += 1;
        // Each open buffer gets an entry view into this one project. Rebuilding
        // a changed dependency therefore performs one analysis for its callers.
        let views: Vec<_> = snapshot
            .sources
            .iter()
            .filter_map(|(module, source)| {
                let key = source_key(&source.uri);
                let document = self.documents.get(&key)?;
                let (roots, _) = self.effective_roots(&document.source.uri);
                let entry = module_path(&document.source.uri, &roots)
                    .unwrap_or_else(|| document.source.uri.as_str().to_owned());
                (module == &entry).then_some((key, entry))
            })
            .collect();
        for (key, module) in views {
            if let Some(document) = self.documents.get_mut(&key) {
                if module == snapshot.entry {
                    document.analysis_size = snapshot.sources.len();
                }
                document.snapshot = Some(if module == snapshot.entry {
                    snapshot.clone()
                } else {
                    Arc::new(Snapshot {
                        entry: module.clone(),
                        ..snapshot.as_ref().clone()
                    })
                });
            }
        }
        Ok(snapshot)
    }
    fn effective_roots(&self, uri: &Uri) -> (Vec<ModuleRoot>, BTreeSet<String>) {
        let origin = module_paths::index_origin(uri);
        let uri = origin.as_ref().unwrap_or(uri);
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
    fn project_import_name(&self, path: &str, prefix: &str) -> Option<String> {
        if path.is_empty() || prefix.is_empty() {
            return None;
        }
        let root = path.split("::").next().unwrap();
        if root == prefix || self.host.modules.modules.contains_key(root) {
            return None;
        }
        let host_prefix = format!("{root}::");
        if self
            .host
            .modules
            .modules
            .range(host_prefix.clone()..)
            .next()
            .is_some_and(|(module, _)| module.starts_with(&host_prefix))
        {
            return None;
        }
        Some(format!("{prefix}::{path}"))
    }

    pub(crate) fn canonical_source_import(&self, uri: &Uri, path: &str) -> Option<String> {
        let (roots, _) = self.effective_roots(uri);
        self.project_import_name(path, &roots.last()?.prefix)
    }

    /// Discover one import-path level without loading or analyzing its sources.
    pub(crate) fn import_modules(&self, uri: &Uri, prefix: &[&str]) -> Vec<String> {
        let (roots, _) = self.effective_roots(uri);
        let explicit_root = roots
            .iter()
            .any(|root| !root.prefix.is_empty() && prefix.first() == Some(&root.prefix.as_str()));
        let mut names = BTreeSet::new();
        for root in roots {
            let suffix = if root.prefix.is_empty() {
                if explicit_root {
                    continue;
                }
                prefix
            } else {
                if prefix.is_empty() {
                    names.insert(root.prefix);
                    continue;
                }
                if prefix.first() != Some(&root.prefix.as_str()) {
                    continue;
                }
                &prefix[1..]
            };
            let directory = suffix.iter().fold(root.path, |path, part| path.join(part));
            if let Ok(entries) = std::fs::read_dir(&directory) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = if path.is_dir() {
                        path.file_name()
                    } else if path
                        .extension()
                        .is_some_and(|extension| extension == "aside")
                    {
                        path.file_stem()
                    } else {
                        None
                    };
                    if let Some(name) = name.and_then(|name| name.to_str())
                        && import_name(name)
                    {
                        names.insert(name.to_owned());
                    }
                }
            }
            for document in self.documents.values() {
                let Some(path) = uri_path(&document.source.uri) else {
                    continue;
                };
                let Ok(relative) = path.strip_prefix(&directory) else {
                    continue;
                };
                let Some(first) = relative.components().next() else {
                    continue;
                };
                let name = if relative.components().count() > 1 {
                    Some(first.as_os_str())
                } else if relative
                    .extension()
                    .is_some_and(|extension| extension == "aside")
                {
                    relative.file_stem()
                } else {
                    None
                };
                if let Some(name) = name.and_then(|name| name.to_str())
                    && import_name(name)
                {
                    names.insert(name.to_owned());
                }
            }
        }
        names.into_iter().collect()
    }

    pub fn diagnostics(
        &mut self,
        uri: &Uri,
        encoding: Encoding,
    ) -> Result<Vec<PublishDiagnosticsParams>, String> {
        if self.host.documents.contains_key(uri.as_str())
            || module_paths::index_origin(uri).is_some()
        {
            // Generated declarations and directory indexes are navigable views,
            // not executable author modules with imports or function bodies.
            let document = self
                .documents
                .get(&source_key(uri))
                .ok_or("document is not open")?;
            return Ok(vec![PublishDiagnosticsParams {
                uri: uri.clone(),
                version: document.source.version,
                diagnostics: Vec::new(),
            }]);
        }
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
                    .map(|diagnostic| &diagnostic.diagnostic)
                    .chain(
                        snapshot
                            .source_features(module)
                            .into_iter()
                            .flat_map(|features| &features.diagnostics),
                    )
                    .map(|diagnostic| Diagnostic {
                        range: source.lines.range(&source.text, diagnostic.range, encoding),
                        severity: Some(match diagnostic.severity {
                            aside_compiler::Severity::Error => DiagnosticSeverity::ERROR,
                            aside_compiler::Severity::Warning => DiagnosticSeverity::WARNING,
                        }),
                        code: Some(NumberOrString::String(diagnostic.code.as_str().into())),
                        source: Some("aside".into()),
                        message: diagnostic.message.clone(),
                        ..Default::default()
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

fn import_name(name: &str) -> bool {
    aside_compiler::lex(name).is_ok_and(|lexed| {
        lexed.tokens.first().is_some_and(|token| {
            token.kind == aside_compiler::SyntaxKind::Ident && token.text(name) == name
        })
    })
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

#[cfg(test)]
mod import_tests {
    use super::*;

    #[test]
    fn disk_cache_accounts_for_lru_eviction_and_invalidation() {
        let mut workspace = Workspace::new(ServerOptions::default());
        workspace.disk_sources.resize(NonZeroUsize::new(2).unwrap());
        let uri = Uri::from_str("file:///helper.aside").unwrap();
        let source = |text: &'static str| Source::new(uri.clone(), text.into(), None).unwrap();

        workspace.cache_disk_source("first".into(), source(" "));
        workspace.cache_disk_source("second".into(), source("  "));
        assert!(workspace.cached_disk_source("first").is_some());
        workspace.cache_disk_source("third".into(), source("   "));

        assert!(workspace.cached_disk_source("second").is_none());
        assert_eq!(workspace.disk_bytes, 4);
        workspace.invalidate_key("first");
        assert_eq!(workspace.disk_bytes, 3);
    }

    #[test]
    fn host_features_are_lazy_shared_and_diagnostics_complete_the_graph() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        fn analyze(module: editor::EditorModule<'_>) -> SourceFeatures {
            CALLS.fetch_add(1, Ordering::SeqCst);
            SourceFeatures {
                diagnostics: vec![aside_compiler::Diagnostic {
                    code: aside_compiler::DiagnosticCode::InvalidCall,
                    severity: aside_compiler::Severity::Error,
                    range: aside_compiler::TextRange::empty(0.into()),
                    message: format!("host check: {}", module.canonical_path()),
                    related: Vec::new(),
                    fix: None,
                }],
                ..Default::default()
            }
        }
        let project = tempfile::tempdir().unwrap();
        let root = project.path().join("src");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("main.aside"), "fn main(){}").unwrap();
        for name in ["helper", "other"] {
            std::fs::write(root.join(format!("{name}.aside")), "pub fn read()->int{1}").unwrap();
        }
        let main = file_uri(&root.join("main.aside")).unwrap();
        let helper = file_uri(&root.join("helper.aside")).unwrap();
        let source = "use game::helper::read; use game::other as other; fn main(){read();}";
        let mut workspace = Workspace::new(ServerOptions {
            source_analysis: Some(analyze),
            ..crate::tests::project_options()
        });
        workspace.open(main.clone(), 1, source.into()).unwrap();
        workspace.snapshot(&main).unwrap();
        assert_eq!(CALLS.load(Ordering::SeqCst), 0);
        let at = LineIndex::new(source)
            .position(source, source.rfind("read();").unwrap(), Encoding::Utf16)
            .unwrap();
        workspace.hover(&main, at, Encoding::Utf16).unwrap();
        workspace.hover(&main, at, Encoding::Utf16).unwrap();
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        workspace
            .open(helper.clone(), 1, "pub fn read()->int{1}".into())
            .unwrap();
        let mut warmup = workspace.diagnostic_warmup();
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        assert!(!warmup.step());
        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
        assert!(!warmup.step());
        assert_eq!(CALLS.load(Ordering::SeqCst), 3);
        assert!(warmup.step());
        workspace.diagnostics(&helper, Encoding::Utf16).unwrap();
        assert_eq!(CALLS.load(Ordering::SeqCst), 3);
        let diagnostics = workspace.diagnostics(&main, Encoding::Utf16).unwrap();
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            3,
            "entry views share host results"
        );
        assert_eq!(diagnostics.len(), 3);
        for document in diagnostics {
            assert_eq!(document.diagnostics.len(), 1);
            assert!(
                document.diagnostics[0]
                    .message
                    .starts_with("host check: game::")
            );
        }
    }

    #[test]
    fn imported_open_buffers_share_analysis_and_keep_overlay_versions() {
        let project = tempfile::tempdir().unwrap();
        let source = project.path().join("src");
        std::fs::create_dir(&source).unwrap();
        let helper_text = "pub fn read() -> int { 1 }";
        std::fs::write(source.join("main.aside"), "fn main(){}").unwrap();
        std::fs::write(source.join("helper.aside"), helper_text).unwrap();
        let main = file_uri(&source.join("main.aside")).unwrap();
        let helper = file_uri(&source.join("helper.aside")).unwrap();
        let mut workspace = Workspace::new(crate::tests::project_options());
        workspace
            .open(
                main.clone(),
                1,
                "use helper as plain; use game::helper::read; fn main(){let value:int=read();}"
                    .into(),
            )
            .unwrap();
        let first = workspace.snapshot(&main).unwrap();
        assert_eq!(workspace.analyses, 1);
        workspace
            .open(helper.clone(), 7, helper_text.into())
            .unwrap();
        let leaf = workspace.snapshot(&helper).unwrap();
        let caller = workspace.snapshot(&main).unwrap();
        assert!(Arc::ptr_eq(&first.project, &leaf.project));
        assert!(Arc::ptr_eq(&caller.project, &leaf.project));
        assert_eq!(leaf.entry, "game::helper");
        assert_eq!(leaf.source().version, Some(7));
        assert_eq!(caller.sources["game::helper"].version, Some(7));
        assert_eq!(caller.sources["game::helper"].uri, helper);
        assert_eq!(caller.sources.len(), 2);
        assert_eq!(workspace.analyses, 1);
        workspace
            .change(
                &helper,
                8,
                vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "pub fn read() -> string { \"changed\" }".into(),
                }],
                Encoding::Utf16,
            )
            .unwrap();
        for uri in workspace.open_uris() {
            workspace.diagnostics(&uri, Encoding::Utf16).unwrap();
        }
        let caller = workspace.snapshot(&main).unwrap();
        let leaf = workspace.snapshot(&helper).unwrap();
        assert_eq!(
            workspace.analyses, 2,
            "one rebuilt graph serves both buffers"
        );
        assert!(Arc::ptr_eq(&caller.project, &leaf.project));
        assert_eq!(caller.sources["game::helper"].version, Some(8));
        assert!(
            workspace
                .diagnostics(&main, Encoding::Utf16)
                .unwrap()
                .iter()
                .any(|params| params.uri == main && !params.diagnostics.is_empty())
        );
    }

    #[test]
    fn import_modules_include_disk_and_unsaved_children_without_analysis() {
        let project = tempfile::tempdir().unwrap();
        let source = project.path().join("src");
        std::fs::create_dir_all(source.join("ui")).unwrap();
        for name in ["旁白.aside", "bad-name.aside", "readme.md"] {
            std::fs::write(source.join("ui").join(name), "invalid source").unwrap();
        }
        let mut workspace = Workspace::new(ServerOptions {
            source_root: Some(|directory, exists| {
                directory
                    .ancestors()
                    .find(|root| exists(&root.join("src/main.aside")))
                    .map(|root| root.join("src"))
            }),
            project_prefix: Some("game".into()),
            ..Default::default()
        });
        let main = file_uri(&source.join("main.aside")).unwrap();
        workspace
            .open(main.clone(), 1, "fn main() {}".into())
            .unwrap();
        workspace
            .open(
                file_uri(&source.join("ui/draft.aside")).unwrap(),
                1,
                "".into(),
            )
            .unwrap();
        assert_eq!(workspace.import_modules(&main, &[]), ["game", "main", "ui"]);
        assert_eq!(
            workspace.import_modules(&main, &["game", "ui"]),
            ["draft", "旁白"]
        );
        std::fs::write(source.join("ui/added.aside"), "").unwrap();
        assert_eq!(
            workspace.import_modules(&main, &["ui"]),
            ["added", "draft", "旁白"]
        );
        assert_eq!((workspace.analyses, workspace.disk_reads), (0, 0));
    }
}
