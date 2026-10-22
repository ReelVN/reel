//! Module-prefix navigation uses registered declarations and ordinary source discovery.
use super::{
    HostDeclaration, HostDocument, Source, Workspace, file_uri, import_name, module_file,
    source_key, uri_path,
};
use crate::position::{Encoding, LineIndex};
use aside_compiler::{SyntaxKind as K, TextRange, TextSize};
use lsp_types::{Location, Position, Range, Uri};
use std::{str::FromStr, sync::Arc};

pub(crate) struct ImportPath {
    name: String,
    pub range: TextRange,
}

/// Retain the selected prefix, including enclosing grouped paths. Leaf symbols
/// and aliases continue through the compiler's canonical symbol query.
pub(crate) fn import_path_at(source: &Source, offset: usize) -> Option<ImportPath> {
    let token = source
        .parsed
        .syntax()
        .token_at_offset(TextSize::from(offset as u32))
        .into_iter()
        .find(|token| token.kind() == K::Ident)?;
    let path = token
        .parent_ancestors()
        .find(|node| node.kind() == K::UsePath)?;
    let next = std::iter::successors(token.next_token(), |token| token.next_token())
        .find(|token| !token.kind().is_trivia());
    if next.is_none_or(|token| token.kind() != K::ColonColon)
        && path
            .parent()
            .is_some_and(|parent| parent.kind() == K::UseGroup)
    {
        return None;
    }
    let mut parts = Vec::new();
    let paths: Vec<_> = path
        .ancestors()
        .filter(|node| node.kind() == K::UsePath)
        .collect();
    for path in paths.iter().rev() {
        for part in path
            .children_with_tokens()
            .filter_map(|part| part.into_token())
        {
            if part.text_range().start() > token.text_range().start() {
                break;
            }
            if part.kind() == K::As {
                return None;
            }
            if part.kind() == K::Ident {
                parts.push(part.text().to_owned());
            }
        }
    }
    Some(ImportPath {
        name: parts.join("::"),
        range: token.text_range(),
    })
}

struct IndexDocument {
    document: HostDocument,
    range: TextRange,
}
enum ModuleTarget<'a> {
    Native(Option<&'a HostDeclaration>),
    Source(Uri),
    Directory { origin: Uri, names: Vec<String> },
}

fn index_context(uri: &Uri) -> Option<(Uri, String)> {
    if !uri.as_str().starts_with("aside-native://source-index/") {
        return None;
    }
    let url = url::Url::parse(uri.as_str()).ok()?;
    if url.scheme() != "aside-native" || url.host_str() != Some("source-index") {
        return None;
    }
    let source = url.query_pairs().find(|(name, _)| name == "source")?.1;
    let module = url
        .query_pairs()
        .find(|(name, _)| name == "module")?
        .1
        .into_owned();
    if !module.split("::").all(import_name) {
        return None;
    }
    let source = Uri::from_str(&source).ok()?;
    uri_path(&source)?;
    Some((source, module))
}

/// Index documents resolve their real imports in the originating source project.
pub(super) fn index_origin(uri: &Uri) -> Option<Uri> {
    index_context(uri).map(|(source, _)| source)
}

fn directory_index(origin: &Uri, module: &str, names: Vec<String>) -> IndexDocument {
    let mut uri = url::Url::parse("aside-native://source-index/modules/").unwrap();
    uri.set_path(&format!("/modules/{}.aside", module.replace("::", "/")));
    uri.query_pairs_mut()
        .append_pair("source", origin.as_str())
        .append_pair("module", module);
    let uri = Uri::from_str(uri.as_str()).expect("generated module index URI");
    let mut text = format!("/// Source modules in `{module}`.\nuse ");
    let end = text.len() + module.len();
    let start = end - module.rsplit("::").next().unwrap().len();
    text.push_str(module);
    text.push_str(";\n\n");
    for name in names {
        text.push_str(&format!("use {module}::{name};\n"));
    }
    IndexDocument {
        document: HostDocument {
            uri,
            text: Arc::from(text),
        },
        range: TextRange::new(
            TextSize::try_from(start).unwrap(),
            TextSize::try_from(end).unwrap(),
        ),
    }
}

impl Workspace {
    fn module_target(&self, uri: &Uri, path: &ImportPath) -> Option<ModuleTarget<'_>> {
        if let Some(module) = self.host.modules.modules.get(&path.name) {
            let declaration = self
                .host
                .declarations
                .get(&(module.canonical_path.clone(), module.version.clone(), None))
                .and_then(|declarations| declarations.first());
            return Some(ModuleTarget::Native(declaration));
        }
        let (roots, _) = self.effective_roots(uri);
        if let Some(file) = module_file(&path.name, &roots) {
            let file_uri = file_uri(&file).ok()?;
            if let Some(open) = self.documents.get(&source_key(&file_uri)) {
                return Some(ModuleTarget::Source(open.source.uri.clone()));
            }
            if file.is_file() {
                return Some(ModuleTarget::Source(file_uri));
            }
        }
        let prefix: Vec<_> = path.name.split("::").collect();
        let names = self.import_modules(uri, &prefix);
        if names.is_empty() {
            return None;
        }
        let origin = index_origin(uri).unwrap_or_else(|| uri.clone());
        Some(ModuleTarget::Directory { origin, names })
    }

    pub(crate) fn module_hover(&self, uri: &Uri, path: &ImportPath) -> Option<String> {
        let target = self.module_target(uri, path)?;
        let mut text = format!("```aside\nuse {}\n```", path.name);
        if let ModuleTarget::Native(Some(declaration)) = target
            && !declaration.markdown.is_empty()
        {
            text.push_str("\n\n");
            text.push_str(&declaration.markdown);
        }
        Some(text)
    }

    pub(crate) fn module_definition(
        &self,
        uri: &Uri,
        path: &ImportPath,
        encoding: Encoding,
    ) -> Option<Location> {
        let (document, range) = match self.module_target(uri, path)? {
            ModuleTarget::Native(declaration) => {
                let definition = declaration?.definition.as_ref()?;
                (
                    self.host.documents.get(definition.uri.as_str())?.clone(),
                    definition.range,
                )
            }
            ModuleTarget::Source(uri) => {
                return Some(Location {
                    uri,
                    range: Range::new(Position::default(), Position::default()),
                });
            }
            ModuleTarget::Directory { origin, names } => {
                let index = directory_index(&origin, &path.name, names);
                (index.document, index.range)
            }
        };
        Some(Location {
            uri: document.uri,
            range: LineIndex::new(&document.text).range(&document.text, range, encoding),
        })
    }

    /// Serve host declarations unchanged, or discover one level of source paths
    /// for a directory view. Index text is never registered as a runtime module.
    pub(crate) fn native_document(&self, uri: &Uri) -> Option<HostDocument> {
        if let Some(document) = self.host.documents.get(uri.as_str()) {
            return Some(document.clone());
        }
        let (origin, module) = index_context(uri)?;
        let prefix: Vec<_> = module.split("::").collect();
        let names = self.import_modules(&origin, &prefix);
        if names.is_empty() {
            return None;
        }
        Some(directory_index(&origin, &module, names).document)
    }
}
