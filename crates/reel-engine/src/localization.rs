//! Platform-independent Fluent catalogs. IO and background execution belong to the host.
use fluent_bundle::{FluentArgs, FluentResource, concurrent::FluentBundle};
use fluent_syntax::ast::{Entry, Expression, InlineExpression, Pattern, PatternElement};
use std::{collections::HashMap, ops::Range, sync::Arc};
use unic_langid::LanguageIdentifier;

pub struct Catalog {
    source: String,
    locale: LanguageIdentifier,
    resource: Arc<FluentResource>,
    bundle: FluentBundle<Arc<FluentResource>>,
    static_messages: HashMap<Box<str>, StaticText>,
}

enum StaticText {
    Source(Range<usize>),
    Decoded(Box<str>),
}

/// Content identity shared by runtime calls and authoring tools.
pub fn message_id(source: &str) -> String {
    format!("text-{}", blake3::hash(source.as_bytes()).to_hex())
}

pub fn language_id(locale: &str) -> Result<LanguageIdentifier, String> {
    locale
        .parse()
        .map_err(|e| format!("invalid locale {locale:?}: {e}"))
}

impl Catalog {
    /// Parse a complete candidate before publishing it. Suitable for a resource worker.
    pub fn parse(locale: &str, source: &str, text: String) -> Result<Self, String> {
        let locale = language_id(locale)?;
        let resource = FluentResource::try_new(text).map_err(|(resource, errors)| {
            errors
                .iter()
                .map(|e| {
                    let before = &resource.source()[..e.pos.start];
                    let line = before.bytes().filter(|&b| b == b'\n').count() + 1;
                    let column = before.rsplit('\n').next().unwrap().chars().count() + 1;
                    format!("{source}:{line}:{column}: {e}")
                })
                .collect::<Vec<_>>()
                .join("\n")
        })?;
        let resource = Arc::new(resource);
        let mut bundle = FluentBundle::new_concurrent(vec![locale.clone()]);
        bundle
            .add_resource(Arc::clone(&resource))
            .map_err(|errors| format!("{source}: {errors:?}"))?;
        let static_messages = resource
            .entries()
            .filter_map(|entry| {
                let Entry::Message(message) = entry else {
                    return None;
                };
                let value = static_text(message.value.as_ref()?, &resource, &bundle)?;
                Some((message.id.name.into(), value))
            })
            .collect();
        Ok(Self {
            locale,
            resource,
            bundle,
            static_messages,
            source: source.into(),
        })
    }

    fn format(&self, id: &str, args: &FluentArgs<'_>) -> Option<Result<String, String>> {
        if let Some(value) = self.static_messages.get(id) {
            return Some(Ok(match value {
                StaticText::Source(range) => self.resource.source()[range.clone()].into(),
                StaticText::Decoded(text) => text.to_string(),
            }));
        }
        let pattern = self.bundle.get_message(id)?.value()?;
        let mut errors = Vec::new();
        let text = self.bundle.format_pattern(pattern, Some(args), &mut errors);
        Some(if errors.is_empty() {
            Ok(text.into_owned())
        } else {
            Err(format!("{} / {id}: {errors:?}", self.locale))
        })
    }
}

fn static_text(
    pattern: &Pattern<&str>,
    resource: &FluentResource,
    bundle: &FluentBundle<Arc<FluentResource>>,
) -> Option<StaticText> {
    let source_text = match pattern.elements.as_slice() {
        [PatternElement::TextElement { value }] => Some(*value),
        [
            PatternElement::Placeable {
                expression: Expression::Inline(InlineExpression::StringLiteral { value }),
            },
        ] if !value.contains('\\') => Some(*value),
        _ => None,
    };
    if let Some(value) = source_text {
        // Fluent's AST text borrows this immutable resource. String literals do not
        // acquire bidi isolation, so an unescaped literal needs no decoded copy.
        let start = value.as_ptr().addr() - resource.source().as_ptr().addr();
        return Some(StaticText::Source(start..start + value.len()));
    }
    if !pattern.elements.iter().all(|element| {
        matches!(
            element,
            PatternElement::TextElement { .. }
                | PatternElement::Placeable {
                    expression: Expression::Inline(InlineExpression::StringLiteral { .. }),
                }
        )
    }) {
        return None;
    }
    let mut errors = Vec::new();
    let text = bundle.format_pattern(pattern, None, &mut errors);
    // Preserve formatting failures at the calling tr rather than failing a load.
    errors
        .is_empty()
        .then(|| StaticText::Decoded(text.into_owned().into_boxed_str()))
}

/// A request is owned by the calling script task until its one terminal result.
pub trait CatalogLoader {
    fn load(&mut self, locale: &str, source: &str) -> Result<u64, String>;
    fn poll(&mut self, id: u64) -> Option<Result<Catalog, String>>;
    fn cancel(&mut self, id: u64);
}

pub struct Localization {
    locale: LanguageIdentifier,
    base_locale: LanguageIdentifier,
    catalogs: HashMap<LanguageIdentifier, Catalog>,
    templates: HashMap<(LanguageIdentifier, String), Catalog>,
}
impl Default for Localization {
    fn default() -> Self {
        let locale: LanguageIdentifier = "en".parse().unwrap();
        Self {
            base_locale: locale.clone(),
            locale,
            catalogs: HashMap::new(),
            templates: HashMap::new(),
        }
    }
}
impl Localization {
    pub(crate) fn sources(&self) -> impl Iterator<Item = (String, String)> + '_ {
        self.catalogs
            .values()
            .map(|c| (c.locale.to_string(), c.source.clone()))
    }
    pub fn locale(&self) -> &LanguageIdentifier {
        &self.locale
    }
    pub fn set_locale(&mut self, locale: LanguageIdentifier) -> bool {
        if locale == self.locale {
            return false;
        }
        self.base_locale = LanguageIdentifier::from_parts(locale.language, None, None, &[]);
        self.locale = locale;
        true
    }
    pub fn install(&mut self, catalog: Catalog) {
        self.catalogs.insert(catalog.locale.clone(), catalog);
    }
    pub fn format(
        &mut self,
        id: &str,
        fallback: &str,
        args: Option<&FluentArgs<'_>>,
    ) -> Result<String, String> {
        self.format_in(None, id, fallback, args)
    }

    /// Format in one language without changing the selected UI language.
    pub fn format_in(
        &mut self,
        locale: Option<&LanguageIdentifier>,
        id: &str,
        fallback: &str,
        args: Option<&FluentArgs<'_>>,
    ) -> Result<String, String> {
        let base =
            locale.map(|locale| LanguageIdentifier::from_parts(locale.language, None, None, &[]));
        let selected = locale.unwrap_or(&self.locale);
        let base = base.as_ref().unwrap_or(&self.base_locale);
        let empty = FluentArgs::new();
        let parameters = args.unwrap_or(&empty);
        for locale in [selected, base] {
            if let Some(result) = self
                .catalogs
                .get(locale)
                .and_then(|c| c.format(id, parameters))
            {
                return result;
            }
        }
        if args.is_none() {
            return Ok(fallback.into());
        }
        let key = (selected.clone(), fallback.into());
        if !self.templates.contains_key(&key) {
            let source = format!("fallback = {}", fallback.replace('\n', "\n    "));
            let catalog = Catalog::parse(&selected.to_string(), id, source)?;
            self.templates.insert(key.clone(), catalog);
        }
        self.templates[&key]
            .format("fallback", parameters)
            .unwrap_or_else(|| Err(format!("{id}: fallback has no message value")))
    }
}
