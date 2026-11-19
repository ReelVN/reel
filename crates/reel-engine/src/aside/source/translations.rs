//! Inline translation extraction from bound HIR, shared with author tools.
use super::{CatalogProducts, TranslationMessage};
use aside_compiler::{
    TextRange,
    hir::{self as h, Form, HirKind as H},
    lowering::{CallExpansion, ExpandedArgument, ExpandedValue},
    strings::{StringLiteral, StringPart},
};
use aside_vm::bytecode as bc;
use std::collections::{BTreeMap, BTreeSet};

/// A host compile diagnostic attached to the author's original expression.
#[derive(Debug, Clone)]
pub struct TranslationDiagnostic {
    pub module: String,
    pub span: TextRange,
    pub message: String,
}
impl std::fmt::Display for TranslationDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{:?}: {}", self.module, self.span, self.message)
    }
}
impl std::error::Error for TranslationDiagnostic {}

/// Recoverable inline translation metadata for diagnostics, hovers and completion.
#[derive(Default, Debug)]
pub struct TranslationAnalysis {
    pub diagnostics: Vec<TranslationDiagnostic>,
    pub calls: Vec<TranslationSite>,
}
/// A resolved call retains regions even when a neighboring entry is incomplete.
#[derive(Debug)]
pub struct TranslationSite {
    pub span: TextRange,
    pub source_span: Option<TextRange>,
    pub source: Option<String>,
    pub table_span: Option<TextRange>,
    pub locales: Vec<String>,
    pub variants: Vec<TranslationVariant>,
    pub holes: Vec<TranslationHole>,
}
#[derive(Debug)]
pub struct TranslationVariant {
    pub locale: String,
    pub span: TextRange,
    pub text: Option<String>,
}
#[derive(Debug)]
pub struct TranslationHole {
    pub span: TextRange,
    pub expression: TextRange,
    pub source_slot: Option<usize>,
}

/// Analyze the same bound calls and template rules used by compilation, while
/// retaining useful source regions after recoverable editor errors. Previews
/// contain the original expression text and never evaluate author code.
pub fn analyze_translations(
    module: aside_compiler::editor::EditorModule<'_>,
) -> TranslationAnalysis {
    extract_views(&[module], None).2
}
fn translation_site(
    context: &Context<'_>,
    call: h::NodeId,
    arguments: Option<h::NodeId>,
) -> TranslationSite {
    let module = context.module;
    let node = module.hir().node(call);
    let mut site = TranslationSite {
        span: node.span,
        source_span: None,
        source: None,
        table_span: None,
        locales: Vec::new(),
        variants: Vec::new(),
        holes: Vec::new(),
    };
    let mut positional = 0;
    let mut source_holes = Vec::new();
    let mut table = None;
    for argument in arguments
        .into_iter()
        .flat_map(|args| module.hir().children(args))
    {
        let Form::Argument { name, value } = module.hir().node(argument).form else {
            continue;
        };
        let name = if let Some(name) = name {
            module.hir().name(name.name.unwrap())
        } else {
            let name = ["source", "translations", "params", "locale"]
                .get(positional)
                .copied()
                .unwrap_or("");
            positional += 1;
            name
        };
        match name {
            "source" => {
                site.source_span = Some(module.hir().node(value).span);
                if let Ok(template) = context.template(value) {
                    site.source = Some(preview(context, &template));
                    for part in &template.parts {
                        if let Part::Hole(node, key) = part {
                            let slot = source_holes.len();
                            source_holes.push(key.clone());
                            site.holes.push(hole(context, *node, Some(slot)));
                        }
                    }
                }
            }
            "translations" => {
                table = Some(context.ungroup(value));
                site.table_span = Some(module.hir().node(value).span);
            }
            _ => {}
        }
    }
    if let Some(table) = table {
        for field in module.hir().children(table) {
            let Form::Field { key, value } = module.hir().node(field).form else {
                continue;
            };
            let locale = match key {
                h::FieldKey::Name(token) => module.hir().name(token.name.unwrap()).to_owned(),
                h::FieldKey::Expression(node) => context
                    .template(node)
                    .map(|t| preview(context, &t))
                    .unwrap_or_default(),
            };
            let locale = crate::localization::language_id(&locale)
                .map(|id| id.to_string())
                .unwrap_or(locale);
            site.locales.push(locale.clone());
            let template = context.template(value).ok();
            let text = template.as_ref().map(|t| preview(context, t));
            if let Some(template) = template {
                let mut used: BTreeMap<&str, usize> = BTreeMap::new();
                for part in &template.parts {
                    if let Part::Hole(node, key) = part {
                        let count = used.entry(key).or_default();
                        let slots = source_holes
                            .iter()
                            .enumerate()
                            .filter_map(|(i, source)| (source == key).then_some(i))
                            .collect::<Vec<_>>();
                        let slot = if slots.len() == 1 {
                            slots.first().copied()
                        } else {
                            slots.get(*count).copied()
                        };
                        *count += 1;
                        site.holes.push(hole(context, *node, slot));
                    }
                }
            }
            site.variants.push(TranslationVariant {
                locale,
                span: module.hir().node(value).span,
                text,
            });
        }
    }
    site
}
fn preview(context: &Context<'_>, template: &Template) -> String {
    let mut text = String::new();
    for part in &template.parts {
        match part {
            Part::Text(value) => text.push_str(value),
            Part::Hole(node, _) => {
                text.push('{');
                text.push_str(
                    context
                        .module
                        .hir()
                        .text(context.module.hir().node(*node).span),
                );
                text.push('}');
            }
        }
    }
    text
}
fn hole(context: &Context<'_>, node: h::NodeId, source_slot: Option<usize>) -> TranslationHole {
    let expression = context.module.hir().node(node).span;
    let span = context
        .module
        .hir()
        .node(node)
        .parent
        .filter(|parent| context.module.hir().node(*parent).kind == H::Interpolation)
        .map_or(expression, |parent| context.module.hir().node(parent).span);
    TranslationHole {
        span,
        expression,
        source_slot,
    }
}

struct Context<'a> {
    module: aside_compiler::editor::EditorModule<'a>,
}
struct Template {
    parts: Vec<Part>,
}
enum Part {
    Text(String),
    Hole(h::NodeId, String),
}

impl Context<'_> {
    fn error(&self, node: h::NodeId, message: impl Into<String>) -> TranslationDiagnostic {
        TranslationDiagnostic {
            module: self.module.source_path().into(),
            span: self.module.hir().node(node).span,
            message: message.into(),
        }
    }
    fn ungroup(&self, mut node: h::NodeId) -> h::NodeId {
        while let (H::Group, Form::Single { child }) = (
            self.module.hir().node(node).kind,
            self.module.hir().node(node).form,
        ) {
            node = child;
        }
        node
    }
    fn is_tr(&self, node: h::NodeId) -> bool {
        self.module.canonical_reference(node).is_some_and(|r| {
            r.module == "reel" && r.version == "1" && r.export.as_deref() == Some("tr")
        })
    }
    fn template(&self, node: h::NodeId) -> Result<Template, TranslationDiagnostic> {
        let node = self.ungroup(node);
        let literal = StringLiteral::new(self.module.hir(), node).ok_or_else(|| self.error(node, "tr text must be a literal or interpolated string so every translation can be extracted"))?;
        let mut parts = Vec::new();
        for part in literal.parts() {
            parts.push(match part {
                StringPart::Text(text) => {
                    let mut bytes = Vec::with_capacity(text.text.len());
                    text.append_to(&mut bytes)
                        .map_err(|_| self.error(node, "invalid analyzed string escape"))?;
                    Part::Text(String::from_utf8(bytes).expect("decoded unicode"))
                }
                StringPart::Expression { node, .. } => Part::Hole(node, self.identity(node)),
            });
        }
        Ok(Template { parts })
    }
    // Bound names distinguish lexical scopes. Significant HIR tokens retain
    // operators and literal spelling; grouping and whitespace do not matter.
    fn identity(&self, node: h::NodeId) -> String {
        enum Visit {
            Node(h::NodeId),
            Token(h::HirToken),
            End,
        }
        let mut key = String::new();
        let mut pending = vec![Visit::Node(node)];
        while let Some(visit) = pending.pop() {
            let node = match visit {
                Visit::End => {
                    key.push(']');
                    continue;
                }
                Visit::Token(token) => {
                    let text = self.module.hir().text(token.span);
                    key.push_str(&format!("{:?}:{}:{text};", token.kind, text.len()));
                    continue;
                }
                Visit::Node(node) => node,
            };
            let node = self.ungroup(node);
            let hir = self.module.hir();
            if let Form::Name { .. } = hir.node(node).form {
                if let Some(reference) = self.module.canonical_reference(node) {
                    key.push_str(&format!("external:{reference:?};"));
                } else if let Some(symbol) = self.module.facts()[node.0 as usize].symbol {
                    key.push_str(&format!("binding:{};", symbol.0));
                } else {
                    key.push_str(&format!("unresolved:{};", hir.text(hir.node(node).span)));
                }
                continue;
            }
            key.push_str(&format!("{:?}[", hir.node(node).kind));
            pending.push(Visit::End);
            for element in hir.elements(node).iter().rev() {
                match *element {
                    h::HirElement::Node(child) => pending.push(Visit::Node(child)),
                    h::HirElement::Token(token) => pending.push(Visit::Token(token)),
                }
            }
        }
        key
    }
    fn validate_reference_uses(&self) -> Result<(), TranslationDiagnostic> {
        for (i, node) in self.module.hir().nodes.iter().enumerate() {
            let id = h::NodeId(i as u32);
            if !matches!(node.kind, H::Name | H::Member) || !self.is_tr(id) {
                continue;
            }
            let mut value = id;
            while let Some(parent) = self.module.hir().node(value).parent {
                if self.module.hir().node(parent).kind == H::Group {
                    value = parent;
                } else {
                    break;
                }
            }
            let Some(parent) = self.module.hir().node(value).parent else {
                continue;
            };
            match self.module.hir().node(parent).form {
                Form::Call {callee, ..} if callee == value => {}
                Form::Binding {initializer: Some(initializer), mutable: false, pattern, ..}
                    if initializer == value && self.module.hir().node(pattern).kind == H::NamePattern => {}
                _ => return Err(self.error(id, "tr requires a statically resolved call; call it directly or through an immutable let alias instead of passing it as a value")),
            }
        }
        Ok(())
    }
}

/// Extract all canonical `tr` calls and produce their ordinary runtime argument
/// plans. No source rewriting, file IO, native execution or VM allocation occurs.
/// Editors reuse this validation and the message source spans.
pub fn extract_translations(
    modules: &[h::AnalyzedModule],
) -> Result<(CatalogProducts, Vec<Vec<CallExpansion>>), TranslationDiagnostic> {
    let selected = select_branches(modules)?;
    extract_selected(modules, &selected)
}

pub(super) fn select_branches(
    modules: &[h::AnalyzedModule],
) -> Result<Vec<aside_compiler::lowering::BranchSelection>, TranslationDiagnostic> {
    modules
        .iter()
        .map(|module| {
            aside_compiler::lowering::select_branches(module, Default::default()).map_err(|error| {
                TranslationDiagnostic {
                    module: module.source_path().into(),
                    span: error.span,
                    message: error.detail,
                }
            })
        })
        .collect()
}

pub(super) fn extract_selected(
    modules: &[h::AnalyzedModule],
    selected: &[aside_compiler::lowering::BranchSelection],
) -> Result<(CatalogProducts, Vec<Vec<CallExpansion>>), TranslationDiagnostic> {
    let views = modules
        .iter()
        .map(aside_compiler::editor::EditorModule::from)
        .collect::<Vec<_>>();
    let (products, expansions, analysis) = extract_views(&views, Some(selected));
    if let Some(error) = analysis.diagnostics.into_iter().next() {
        return Err(error);
    }
    Ok((products, expansions))
}
fn extract_views(
    modules: &[aside_compiler::editor::EditorModule<'_>],
    selected: Option<&[aside_compiler::lowering::BranchSelection]>,
) -> (
    CatalogProducts,
    Vec<Vec<CallExpansion>>,
    TranslationAnalysis,
) {
    let mut analysis = TranslationAnalysis::default();
    let mut products = CatalogProducts::default();
    let mut catalog_messages: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut expansions = Vec::new();
    for (module_index, module) in modules.iter().enumerate() {
        let context = Context { module: *module };
        if let Err(error) = context.validate_reference_uses() {
            analysis.diagnostics.push(error);
        }
        let mut plans = Vec::new();
        for (i, node) in module.hir().nodes.iter().enumerate() {
            let Form::Call {
                callee,
                arguments,
                children,
                ..
            } = node.form
            else {
                continue;
            };
            if !context.is_tr(callee) {
                continue;
            }
            let call = h::NodeId(i as u32);
            let emit = selected.is_some_and(|selection| selection[module_index].contains(call));
            if selected.is_none() {
                analysis
                    .calls
                    .push(translation_site(&context, call, arguments));
            }
            let prepared = (|| -> Result<Option<CallExpansion>, TranslationDiagnostic> {
                if children.is_some() {
                    return Err(context.error(call, "tr does not accept a children block"));
                }
                let mut args = Vec::new();
                let mut supplied = BTreeSet::new();
                let mut positional = 0;
                for arg in arguments
                    .into_iter()
                    .flat_map(|args| module.hir().children(args))
                {
                    let Form::Argument { name, value } = module.hir().node(arg).form else {
                        return Err(context.error(
                            arg,
                            "tr requires explicit arguments so its translations can be extracted",
                        ));
                    };
                    let name = if let Some(name) = name {
                        module.hir().name(name.name.unwrap())
                    } else {
                        let name = ["source", "translations", "params", "locale"]
                            .get(positional)
                            .copied()
                            .ok_or_else(|| {
                                context.error(
                                    arg,
                                    "tr accepts source, translations, params and locale",
                                )
                            })?;
                        positional += 1;
                        name
                    };
                    if !["source", "translations", "params", "locale"].contains(&name)
                        || !supplied.insert(name)
                    {
                        return Err(context.error(arg, "unknown or duplicate tr argument"));
                    }
                    args.push((name, value));
                }
                let source = args
                    .iter()
                    .find(|(name, _)| *name == "source")
                    .map(|(_, node)| *node)
                    .ok_or_else(|| context.error(call, "tr requires source text"))?;
                let source_template = context.template(source)?;
                let holes = source_template
                    .parts
                    .iter()
                    .filter_map(|part| {
                        if let Part::Hole(node, key) = part {
                            Some((*node, key.clone()))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                let params = args
                    .iter()
                    .find(|(name, _)| *name == "params")
                    .map(|(_, node)| *node);
                let explicit_params = params.is_some_and(|node| !matches!(module.hir().node(context.ungroup(node)).form, Form::Literal {token} if token.kind == aside_compiler::SyntaxKind::Null));
                if !holes.is_empty() && explicit_params {
                    return Err(context.error(params.unwrap(), "an interpolated tr source already supplies its parameters; use its expressions directly or use literal text with explicit params"));
                }
                let source_text = if emit {
                    render_source(&source_template, !holes.is_empty())
                } else {
                    String::new()
                };
                let mut translated = BTreeMap::new();
                if let Some((_, translations)) =
                    args.iter().find(|(name, _)| *name == "translations")
                {
                    let translations = context.ungroup(*translations);
                    if !matches!(module.hir().node(translations).form, Form::Literal {token} if token.kind == aside_compiler::SyntaxKind::Null)
                    {
                        if module.hir().node(translations).kind != H::Object {
                            return Err(context.error(
                                translations,
                                "tr translations must be a static locale table",
                            ));
                        }
                        for field in module.hir().children(translations) {
                            let Form::Field { key, value } = module.hir().node(field).form else {
                                return Err(context.error(
                                    field,
                                    "tr locale tables require explicit locale entries",
                                ));
                            };
                            let locale = match key {
                                h::FieldKey::Name(name) => {
                                    module.hir().name(name.name.unwrap()).to_owned()
                                }
                                h::FieldKey::Expression(node) => {
                                    let template = context.template(node)?;
                                    if template
                                        .parts
                                        .iter()
                                        .any(|part| matches!(part, Part::Hole(..)))
                                    {
                                        return Err(context.error(
                                            node,
                                            "a locale key must be a literal language tag",
                                        ));
                                    }
                                    render_source(&template, false)
                                }
                            };
                            let locale = crate::localization::language_id(&locale)
                                .map_err(|message| context.error(field, message))?
                                .to_string();
                            if translated.contains_key(&locale) {
                                return Err(context.error(
                                    field,
                                    "duplicate canonical locale in tr translations",
                                ));
                            }
                            let template = context.template(value)?;
                            let pattern = render_translation(
                                &context,
                                &template,
                                &holes,
                                explicit_params,
                                emit,
                            )?;
                            translated.insert(locale, pattern);
                        }
                    }
                }
                if !emit {
                    return Ok(None);
                }
                // Length framing makes the content package unambiguous across
                // modules, locales, source escapes and parameterized templates.
                let mut identity = blake3::Hasher::new();
                for text in std::iter::once(source_text.as_str()).chain(
                    translated
                        .iter()
                        .flat_map(|(locale, text)| [locale.as_str(), text.as_str()]),
                ) {
                    identity.update(&(text.len() as u64).to_le_bytes());
                    identity.update(text.as_bytes());
                }
                identity.update(&[u8::from(!holes.is_empty() || explicit_params)]);
                let key = format!("text-{}", identity.finalize().to_hex());
                for (locale, pattern) in translated {
                    catalog_messages
                        .entry(locale)
                        .or_default()
                        .insert(key.clone(), pattern);
                }
                products.messages.push(TranslationMessage {
                    key: key.clone(),
                    source: source_text.clone(),
                    module: module.source_path().into(),
                    span: node.span,
                });
                let mut runtime = Vec::new();
                for (name, node) in args {
                    let value = match name {
                        "source" => {
                            // The source argument's holes execute here, before any
                            // later argument (including locale), regardless of locale.
                            if !holes.is_empty() {
                                runtime.push(ExpandedArgument {
                                    name: "params".into(),
                                    source: node,
                                    value: ExpandedValue::TextRecord(
                                        holes
                                            .iter()
                                            .enumerate()
                                            .map(|(i, (node, _))| (format!("p{i}"), *node))
                                            .collect(),
                                    ),
                                });
                            }
                            ExpandedValue::Constant(bc::Const::Utf8(source_text.clone()))
                        }
                        "translations" => ExpandedValue::Constant(bc::Const::Utf8(key.clone())),
                        "params" if !holes.is_empty() => continue,
                        _ => ExpandedValue::Expression(node),
                    };
                    runtime.push(ExpandedArgument {
                        name: name.into(),
                        source: node,
                        value,
                    });
                }
                if !supplied.contains("translations") {
                    runtime.push(ExpandedArgument {
                        name: "translations".into(),
                        source,
                        value: ExpandedValue::Constant(bc::Const::Utf8(key)),
                    });
                }
                Ok(Some(CallExpansion {
                    call,
                    reference: module.canonical_reference(callee).unwrap().clone(),
                    arguments: runtime,
                }))
            })();
            match prepared {
                Ok(Some(plan)) => plans.push(plan),
                Ok(None) => {}
                Err(error) => analysis.diagnostics.push(error),
            }
        }
        expansions.push(plans);
    }
    for (locale, messages) in catalog_messages {
        let mut text = String::new();
        for (key, pattern) in messages {
            text.push_str(&key);
            text.push_str(" = ");
            text.push_str(&pattern);
            text.push('\n');
        }
        products.catalogs.insert(locale, text);
    }
    (products, expansions, analysis)
}

fn fluent_text(text: &str, out: &mut String) {
    out.push_str("{ \"");
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\u000A"),
            '\r' => out.push_str("\\u000D"),
            '\t' => out.push_str("\\u0009"),
            '\0' => out.push_str("\\u0000"),
            ch => out.push(ch),
        }
    }
    out.push_str("\" }");
}
fn render_source(template: &Template, interpolate: bool) -> String {
    let mut result = String::new();
    let mut slot = 0;
    for part in &template.parts {
        match part {
            Part::Text(text) if interpolate => fluent_text(text, &mut result),
            Part::Text(text) => result.push_str(text),
            Part::Hole(..) => {
                result.push_str(&format!("{{ $p{slot} }}"));
                slot += 1;
            }
        }
    }
    result
}
fn render_translation(
    context: &Context<'_>,
    template: &Template,
    source: &[(h::NodeId, String)],
    explicit_params: bool,
    emit: bool,
) -> Result<String, TranslationDiagnostic> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for part in &template.parts {
        if let Part::Hole(_, key) = part {
            *counts.entry(key).or_default() += 1;
        }
    }
    let mut used: BTreeMap<&str, usize> = BTreeMap::new();
    let mut result = String::new();
    for part in &template.parts {
        match part {
            Part::Text(text) if emit && explicit_params => result.push_str(text),
            Part::Text(text) if emit => fluent_text(text, &mut result),
            Part::Text(_) => {}
            Part::Hole(node, key) => {
                let matches = source
                    .iter()
                    .enumerate()
                    .filter_map(|(i, (_, source))| (source == key).then_some(i))
                    .collect::<Vec<_>>();
                if matches.is_empty() {
                    return Err(context.error(*node, "translation interpolation must reference an expression from the source string"));
                }
                let count = used.entry(key).or_default();
                let slot = if matches.len() == 1 {
                    matches[0]
                } else {
                    if counts[key.as_str()] != matches.len() {
                        return Err(context.error(*node, "repeated source expression is ambiguous in this translation; assign each intended value to an ordinary let binding"));
                    }
                    matches[*count]
                };
                *count += 1;
                if emit {
                    result.push_str(&format!("{{ $p{slot} }}"));
                }
            }
        }
    }
    if emit && result.is_empty() {
        fluent_text("", &mut result);
    }
    Ok(result)
}
