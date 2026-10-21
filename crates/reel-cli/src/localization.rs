//! Source-first localization tools sharing the author compiler and Fluent runtime.
mod project;
use aside_compiler::{
    SourceMap, SourcePosition, SyntaxKind,
    hir::{AnalyzedModule, FieldKey, Form, HirKind, NodeId, SymbolId},
};
use aside_vm::bytecode as bc;
use fluent_syntax::{ast::Entry, parser};
pub(crate) use project::bundle_project;
pub use project::{PreparedCatalogs, check_project, prepare_project};
use reel_engine::{
    aside::source,
    localization::{Catalog, message_id},
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt::Write as _,
    io::{self, Write as _},
    path::{Path, PathBuf},
};

pub struct Extraction {
    pub messages: usize,
    pub occurrences: usize,
    pub skipped: Vec<String>,
    pub stale_catalogs: Vec<PathBuf>,
    pub cleanup_warning: Option<String>,
}

struct Message {
    id: String,
    source: String,
    template: bool,
    references: Vec<String>,
    module: PathBuf,
}

struct SourceCatalog {
    messages: Vec<Message>,
    occurrences: usize,
    skipped: Vec<String>,
}

const GENERATED_HEADER: &str =
    "## Generated source catalog. Translate a separate locale catalog.\n";

/// Parse the same complete catalog that the player installs, without running a story.
pub fn check(path: &Path, locale: &str) -> Result<(), String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    Catalog::parse(locale, &path.to_string_lossy(), text).map(|_| ())
}

/// Extract only statically identified calls; dynamic calls are reported individually.
pub fn extract(input: &Path, output: &Path, overwrite: bool) -> Result<Extraction, String> {
    let catalog = collect(input)?;
    let text = render(catalog.messages.iter())?;
    let target = output_path(output)?;
    let published =
        reel_storage::publish(&target, overwrite, |file| file.write_all(text.as_bytes()))
            .map_err(|error| format!("{}: {error}", target.display()))?;
    Ok(catalog.result(published.cleanup_warning))
}

/// Mirror imported module paths without making those paths part of message identity.
pub fn extract_directory(
    input: &Path,
    output: &Path,
    overwrite: bool,
) -> Result<Extraction, String> {
    let catalog = collect(input)?;
    let mut modules: BTreeMap<&Path, Vec<&Message>> = BTreeMap::new();
    for message in &catalog.messages {
        modules.entry(&message.module).or_default().push(message);
    }
    // Validate all patterns before touching outputs, including template fragments.
    let rendered = modules
        .into_iter()
        .map(|(module, messages)| Ok((module.with_extension("ftl"), render(messages.into_iter())?)))
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    std::fs::create_dir_all(output).map_err(|error| format!("{}: {error}", output.display()))?;
    let output = output.canonicalize().map_err(|error| error.to_string())?;
    let old_files = catalog_files(&output)?;
    if !overwrite {
        for path in rendered.keys().map(|path| output.join(path)) {
            if path.exists() {
                return Err(format!(
                    "{}: source catalog already exists; use --overwrite",
                    path.display()
                ));
            }
        }
    }
    let mut warnings = Vec::new();
    for (module, text) in &rendered {
        let target = output_path(&output.join(module))?;
        let published =
            reel_storage::publish(&target, overwrite, |file| file.write_all(text.as_bytes()))
                .map_err(|error| format!("{}: {error}", target.display()))?;
        warnings.extend(published.cleanup_warning);
    }
    let mut result = catalog.result((!warnings.is_empty()).then(|| warnings.join("\n")));
    result.stale_catalogs = old_files
        .into_iter()
        .filter(|path| !rendered.contains_key(path.strip_prefix(&output).unwrap()))
        .collect();
    Ok(result)
}

fn collect(input: &Path) -> Result<SourceCatalog, String> {
    let loaded = crate::project::load(input)
        .map_err(|error| describe_failure(&error.failure, &error.sources))?;
    let compiled = source::compile_project(&loaded.sources, &loaded.aliases, 1)
        .map_err(|error| describe_failure(&crate::source_failure(error), &loaded.sources))?;
    let package = compiled.compilation.program.package();
    // Use actual compiler-decoded constants, including raw/multiline strings and
    // Unicode escapes. No second Aside literal parser belongs in an author tool.
    let mut constants = HashMap::new();
    for function in &package.functions {
        let mut spans = function.source_map.iter().peekable();
        let mut current = None;
        for (pc, instruction) in function.code.iter().enumerate() {
            while spans.peek().is_some_and(|entry| entry.pc.0 as usize <= pc) {
                current = spans.next().map(|entry| entry.span);
            }
            if instruction.opcode == bc::Opcode::LoadConst as u16
                && let bc::Const::Utf8(value) = &package.constants[instruction.operands[1] as usize]
                && let Some(span) = current
            {
                constants.insert((span.file.0, span.start, span.end), value.as_str());
            }
        }
    }
    let mut modules = compiled.modules.iter().collect::<Vec<_>>();
    // Catalog paths mirror the project's source root; source references retain src/.
    let module_path = |module: &AnalyzedModule| {
        Path::new(module.source_path())
            .strip_prefix("src")
            .expect("project sources are relative to src")
            .to_path_buf()
    };
    modules.sort_by_key(|module| module_path(module));
    let mut messages: Vec<Message> = Vec::new();
    let mut identities: HashMap<String, usize> = HashMap::new();
    let mut skipped = Vec::new();
    let mut occurrences = 0;
    for module in modules {
        let source_id = compiled
            .compilation
            .modules
            .iter()
            .find(|output| output.metadata.canonical_path == module.canonical_path())
            .expect("compiled module metadata")
            .source
            .0;
        let context = Context::new(module, source_id, &constants);
        for (index, node) in module.hir().nodes.iter().enumerate() {
            let Form::Call {
                callee, arguments, ..
            } = node.form
            else {
                continue;
            };
            if context.native(callee) != Some("tr") {
                continue;
            }
            let call = NodeId(index as u32);
            let mut reference = context.reference(call);
            let arguments =
                match context.arguments(arguments, &["source", "id", "params", "locale"]) {
                    Ok(arguments) => arguments,
                    Err(reason) => {
                        skipped.push(format!("{reference}: {reason}"));
                        continue;
                    }
                };
            let Some(original) = arguments[0].and_then(|value| context.string(value)) else {
                skipped.push(format!(
                    "{reference}: dynamic source text; use a static tr source at its declaration"
                ));
                continue;
            };
            let id = match arguments[1] {
                None => message_id(original),
                Some(value) if context.is_null(value) => message_id(original),
                Some(value) => match context.string(value) {
                    Some(id) => id.to_owned(),
                    None => {
                        skipped.push(format!("{reference}: dynamic message id"));
                        continue;
                    }
                },
            };
            let template = arguments[2].is_some_and(|value| !context.is_null(value));
            if template {
                write!(
                    reference,
                    " | {}",
                    context.parameter_names(arguments[2].unwrap())
                )
                .unwrap();
            }
            if let Some(&existing) = identities.get(&id) {
                let message = &mut messages[existing];
                if message.source != original || message.template != template {
                    return Err(format!(
                        "{reference}: message id {id:?} has conflicting source text or parameter/template intent\nfirst declaration: {}",
                        message.references[0]
                    ));
                }
                message.references.push(reference);
            } else {
                identities.insert(id.clone(), messages.len());
                messages.push(Message {
                    id,
                    source: original.into(),
                    template,
                    references: vec![reference],
                    module: module_path(module),
                });
            }
            occurrences += 1;
        }
    }
    Ok(SourceCatalog {
        messages,
        occurrences,
        skipped,
    })
}

impl SourceCatalog {
    fn result(self, cleanup_warning: Option<String>) -> Extraction {
        Extraction {
            messages: self.messages.len(),
            occurrences: self.occurrences,
            skipped: self.skipped,
            stale_catalogs: Vec::new(),
            cleanup_warning,
        }
    }
}

fn render<'a>(messages: impl Iterator<Item = &'a Message>) -> Result<String, String> {
    let mut text = String::from(GENERATED_HEADER);
    text.push_str("## IDs without an explicit id follow the exact source text; source edits create a new ID.\n\n");
    for message in messages {
        for reference in &message.references {
            writeln!(text, "# {reference}").unwrap();
        }
        writeln!(
            text,
            "# {}",
            if message.template {
                "Fluent pattern with parameters"
            } else {
                "Literal text"
            }
        )
        .unwrap();
        let pattern = if message.template {
            message.source.clone()
        } else {
            literal_pattern(&message.source)
        };
        let entry = format!("{} = {}\n", message.id, pattern.replace('\n', "\n    "));
        Catalog::parse("und", &message.references[0], entry.clone())?;
        text.push_str(&entry);
        text.push('\n');
    }
    Ok(text)
}

fn output_path(output: &Path) -> Result<PathBuf, String> {
    if output
        .extension()
        .is_none_or(|extension| extension != "ftl")
    {
        return Err("localization output must be an explicit .ftl file".into());
    }
    let name = output.file_name().ok_or("output filename is missing")?;
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    let target = parent
        .canonicalize()
        .map_err(|error| format!("{}: {error}", parent.display()))?
        .join(name);
    if let Ok(metadata) = std::fs::symlink_metadata(&target)
        && !metadata.is_file()
    {
        return Err(format!(
            "{}: output must be a regular file",
            target.display()
        ));
    }
    Ok(target)
}

fn catalog_files(directory: &Path) -> Result<Vec<PathBuf>, String> {
    fn visit(directory: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                visit(&entry.path(), files)?;
            } else if kind.is_file() && entry.path().extension().is_some_and(|ext| ext == "ftl") {
                files.push(entry.path());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    visit(directory, &mut files).map_err(|error| format!("{}: {error}", directory.display()))?;
    files.sort();
    Ok(files)
}

pub struct Bundle {
    pub files: usize,
    pub entries: usize,
    pub cleanup_warning: Option<String>,
}

/// Validate and stream a language's module catalogs into one atomic build artifact.
pub fn bundle(input: &Path, output: &Path, overwrite: bool) -> Result<Bundle, String> {
    let input = input
        .canonicalize()
        .map_err(|error| format!("{}: {error}", input.display()))?;
    let output = output_path(output)?;
    if output.starts_with(&input) {
        return Err("bundled output must be outside the source catalog directory".into());
    }
    let files = catalog_files(&input)?;
    if files.is_empty() {
        return Err(format!("{}: no .ftl catalogs found", input.display()));
    }
    let mut entries = 0;
    let published = reel_storage::publish(&output, overwrite, |file| {
        entries = write_bundle(&files, file)?;
        Ok(())
    })
    .map_err(|error| error.to_string())?;
    Ok(Bundle {
        files: files.len(),
        entries,
        cleanup_warning: published.cleanup_warning,
    })
}

// The explicit bundle command and automatic project preparation share one
// parser, namespace check, source-location mapping, and streaming writer.
fn write_bundle(files: &[PathBuf], file: &mut impl io::Write) -> io::Result<usize> {
    let mut identities = HashMap::<String, (usize, SourcePosition)>::new();
    for (file_index, path) in files.iter().enumerate() {
        let text = std::fs::read_to_string(path)
            .map_err(|error| io::Error::other(format!("{}: {error}", path.display())))?;
        let positions = SourceMap::new(&text);
        let resource = parser::parse_runtime(text.as_str()).map_err(|(_, errors)| {
            io::Error::other(
                errors
                    .iter()
                    .map(|error| {
                        format!(
                            "{}: {error}",
                            location(path, positions.position(error.pos.start).unwrap())
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        })?;
        for entry in &resource.body {
            // Fluent uses one shared namespace for messages and terms.
            let id = match entry {
                Entry::Message(message) => message.id.name,
                Entry::Term(term) => term.id.name,
                _ => continue,
            };
            let offset = id.as_ptr().addr() - text.as_ptr().addr();
            let position = positions.position(offset).unwrap();
            if let Some(&(first_file, first_position)) = identities.get(id) {
                let declaration = location(path, position);
                let first = location(&files[first_file], first_position);
                return Err(io::Error::other(format!(
                    "{declaration}: duplicate Fluent id {id:?}\nfirst declaration: {first}"
                )));
            }
            identities.insert(id.into(), (file_index, position));
        }
        // A blank line prevents the next file from continuing a comment/pattern.
        file.write_all(text.as_bytes())?;
        file.write_all(b"\n\n")?;
    }
    Ok(identities.len())
}

fn location(path: &Path, position: SourcePosition) -> String {
    format!(
        "{}:{}:{}",
        path.display(),
        position.line + 1,
        position.scalar_column + 1
    )
}

/// Preserve prose and paragraph breaks, quoting only syntax and significant
/// whitespace. Escape-heavy text uses one placeable instead of exceeding
/// Fluent's per-pattern resolution limit.
fn literal_pattern(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut placeables = 0;
    let mut lines = value.split('\n').enumerate().peekable();
    while let Some((index, line)) = lines.next() {
        if index != 0 {
            output.push('\n');
        }
        if line.is_empty() {
            if index == 0 || lines.peek().is_none() {
                push_literal(&mut output, "", &mut placeables);
            }
        } else {
            let leading = line.len() - line.trim_start_matches([' ', '\t', '\r']).len();
            let trailing = line.trim_end_matches([' ', '\t', '\r']).len();
            if leading == line.len() {
                push_literal(&mut output, line, &mut placeables);
            } else {
                let mut body_start = leading;
                if leading != 0 {
                    push_literal(&mut output, &line[..leading], &mut placeables);
                } else if line.starts_with(['.', '*', '[']) {
                    push_literal(&mut output, &line[..1], &mut placeables);
                    body_start = 1;
                }
                let body = &line[body_start..trailing];
                let mut characters = body.char_indices().peekable();
                let mut plain_start = 0;
                while let Some((start, character)) = characters.next() {
                    if matches!(character, '{' | '}') || character.is_control() {
                        output.push_str(&body[plain_start..start]);
                        let mut end = start + character.len_utf8();
                        while let Some(&(next, character)) = characters.peek() {
                            if !matches!(character, '{' | '}') && !character.is_control() {
                                break;
                            }
                            end = next + character.len_utf8();
                            characters.next();
                        }
                        push_literal(&mut output, &body[start..end], &mut placeables);
                        plain_start = end;
                    }
                }
                output.push_str(&body[plain_start..]);
                if trailing < line.len() {
                    push_literal(&mut output, &line[trailing..], &mut placeables);
                }
            }
        }
        if placeables > 100 {
            return fluent_string(value);
        }
    }
    output
}

fn push_literal(output: &mut String, value: &str, placeables: &mut usize) {
    *placeables += 1;
    output.push_str(&fluent_string(value));
}

fn fluent_string(value: &str) -> String {
    let mut output = String::from("{ \"");
    for character in value.chars() {
        match character {
            '\\' | '"' => {
                output.push('\\');
                output.push(character);
            }
            character if character.is_control() && (character as u32) <= 0xffff => {
                write!(output, "\\u{:04X}", character as u32).unwrap();
            }
            character => output.push(character),
        }
    }
    output.push_str("\" }");
    output
}

type Constants<'a> = HashMap<(u32, u32, u32), &'a str>;
struct Context<'a> {
    module: &'a AnalyzedModule,
    source: u32,
    constants: &'a Constants<'a>,
    positions: SourceMap<'a>,
    native_nodes: HashMap<NodeId, &'a str>,
    native_symbols: HashMap<SymbolId, &'a str>,
    character_names: HashMap<SymbolId, &'a str>,
}
impl<'a> Context<'a> {
    fn new(module: &'a AnalyzedModule, source: u32, constants: &'a Constants<'a>) -> Self {
        let mut native_nodes = HashMap::new();
        let mut native_symbols = HashMap::new();
        for binding in module.external_bindings() {
            if let Some(reference) = &binding.descriptor.reference
                && reference.module == "reel"
                && let Some(export) = reference.export.as_deref()
            {
                if let Some(node) = binding.node {
                    native_nodes.insert(node, export);
                }
                if let Some(symbol) = binding.symbol {
                    native_symbols.insert(symbol, export);
                }
            }
        }
        let mut context = Self {
            module,
            source,
            constants,
            positions: SourceMap::new(&module.hir().source),
            native_nodes,
            native_symbols,
            character_names: HashMap::new(),
        };
        for node in &module.hir().nodes {
            let Form::Binding {
                pattern,
                initializer: Some(initializer),
                mutable: false,
                ..
            } = node.form
            else {
                continue;
            };
            let Some(symbol) = module.facts()[pattern.0 as usize].symbol else {
                continue;
            };
            let Form::Call {
                callee, arguments, ..
            } = module.hir().node(context.ungroup(initializer)).form
            else {
                continue;
            };
            if context.native(callee) != Some("character") {
                continue;
            }
            if let Ok(arguments) = context.arguments(arguments, &["name", "id"])
                && let Some(name) = arguments[0].and_then(|value| context.string(value))
            {
                context.character_names.insert(symbol, name);
            }
        }
        context
    }
    fn ungroup(&self, mut node: NodeId) -> NodeId {
        while self.module.hir().node(node).kind == HirKind::Group {
            let Form::Single { child } = self.module.hir().node(node).form else {
                break;
            };
            node = child;
        }
        node
    }
    fn native(&self, node: NodeId) -> Option<&str> {
        let node = self.ungroup(node);
        self.native_nodes.get(&node).copied().or_else(|| {
            self.module.facts()[node.0 as usize]
                .symbol
                .and_then(|symbol| self.native_symbols.get(&symbol).copied())
        })
    }
    fn string(&self, node: NodeId) -> Option<&'a str> {
        let node = self.ungroup(node);
        let hir = self.module.hir();
        if hir.node(node).kind != HirKind::String || hir.children(node).next().is_some() {
            return None;
        }
        let span = hir.node(node).span;
        self.constants
            .get(&(self.source, span.start().into(), span.end().into()))
            .copied()
    }
    fn is_null(&self, node: NodeId) -> bool {
        matches!(self.module.hir().node(self.ungroup(node)).form, Form::Literal { token } if token.kind == SyntaxKind::Null)
    }
    fn parameter_names(&self, node: NodeId) -> String {
        let node = self.ungroup(node);
        let hir = self.module.hir();
        if hir.node(node).kind != HirKind::Object {
            return "parameters: dynamic table (names unavailable)".into();
        }
        let mut names = BTreeSet::new();
        let mut dynamic = false;
        for field in hir.children(node) {
            let name = match hir.node(field).form {
                Form::Field {
                    key: FieldKey::Name(name),
                    ..
                } => Some(hir.text(name.span)),
                Form::Field {
                    key: FieldKey::Expression(value),
                    ..
                } => self.string(value),
                _ => None,
            };
            if let Some(name) = name {
                names.insert(name);
            } else {
                dynamic = true;
            }
        }
        if names.is_empty() {
            return if dynamic {
                "parameters: dynamic table (names unavailable)"
            } else {
                "parameters: none (empty table)"
            }
            .into();
        }
        let mut description = format!(
            "parameters: {}",
            names
                .into_iter()
                .map(|name| format!("${}", one_line(name)))
                .collect::<Vec<_>>()
                .join(", ")
        );
        if dynamic {
            description.push_str("; additional names are dynamic");
        }
        description
    }
    fn arguments(
        &self,
        node: Option<NodeId>,
        names: &[&str],
    ) -> Result<Vec<Option<NodeId>>, &'static str> {
        let mut values = vec![None; names.len()];
        let mut positional = 0;
        for argument in node
            .into_iter()
            .flat_map(|node| self.module.hir().children(node))
        {
            let Form::Argument { name, value } = self.module.hir().node(argument).form else {
                return Err("spread arguments cannot be statically extracted");
            };
            if matches!(self.module.hir().node(value).form, Form::Spread { .. }) {
                return Err("spread arguments cannot be statically extracted");
            }
            let index = match name {
                Some(name) => names
                    .iter()
                    .position(|expected| *expected == self.module.hir().text(name.span)),
                None => {
                    let index = positional;
                    positional += 1;
                    Some(index)
                }
            };
            if let Some(index) = index.filter(|&index| index < values.len()) {
                values[index] = Some(value);
            }
        }
        Ok(values)
    }
    fn reference(&self, node: NodeId) -> String {
        let hir = self.module.hir();
        let position = self
            .positions
            .position(hir.node(node).span.start().into())
            .unwrap();
        let mut reference = format!(
            "{}:{}:{}",
            self.module.source_path(),
            position.line + 1,
            position.scalar_column + 1
        );
        let mut current = hir.node(node).parent;
        let mut scopes = Vec::new();
        let mut surrounding_call = None;
        while let Some(parent) = current {
            match hir.node(parent).form {
                Form::Function {
                    name: Some(name), ..
                }
                | Form::Class {
                    name: Some(name), ..
                } => scopes.push(hir.text(name.span)),
                Form::Call { callee, .. } if surrounding_call.is_none() => {
                    surrounding_call = Some(callee)
                }
                _ => {}
            }
            current = hir.node(parent).parent;
        }
        if !scopes.is_empty() {
            scopes.reverse();
            write!(reference, " | {}", scopes.join("::")).unwrap();
        }
        if let Some(callee) = surrounding_call {
            if self.native(callee) == Some("narrator") {
                reference.push_str(" | speaker: narrator");
            } else if let Some(name) = self.character_name(callee) {
                write!(reference, " | speaker: {}", one_line(name)).unwrap();
            } else {
                write!(
                    reference,
                    " | call: {}",
                    one_line(hir.text(hir.node(callee).span))
                )
                .unwrap();
            }
        }
        reference
    }
    fn character_name(&self, callee: NodeId) -> Option<&str> {
        let symbol = self.module.facts()[self.ungroup(callee).0 as usize].symbol?;
        self.character_names.get(&symbol).copied()
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn describe_failure(
    failure: &crate::Failure,
    sources: &[aside_compiler::semantic::SourceModule],
) -> String {
    failure
        .diagnostics
        .iter()
        .map(|reported| {
            let source = sources
                .iter()
                .find(|source| {
                    reported.source.as_deref().is_some_and(|path| {
                        path == source.canonical_path || Some(path) == source.source_path.as_deref()
                    })
                })
                .or_else(|| sources.first());
            if let Some(source) = source {
                let position = SourceMap::new(&source.source)
                    .position(reported.diagnostic.range.start().into())
                    .unwrap();
                format!(
                    "{}:{}:{}: {}",
                    source
                        .source_path
                        .as_deref()
                        .unwrap_or(&source.canonical_path),
                    position.line + 1,
                    position.scalar_column + 1,
                    reported.diagnostic.message
                )
            } else {
                reported.diagnostic.message.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
