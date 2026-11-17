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
    /// Resolve a canonical locale to its indexed asset key without reading it.
    /// Missing locales use source-text fallback and do not create a request.
    fn resolve(&self, locale: &str) -> Option<&str>;
    fn load(&mut self, locale: &str, source: &str) -> Result<u64, String>;
    fn poll(&mut self, id: u64) -> Option<Result<Catalog, String>>;
    fn cancel(&mut self, id: u64);
}

pub struct Localization {
    locale: LanguageIdentifier,
    base_locale: LanguageIdentifier,
    catalogs: HashMap<LanguageIdentifier, InstalledCatalog>,
    templates: HashMap<LanguageIdentifier, HashMap<String, Catalog>>,
    retained: HashMap<LanguageIdentifier, usize>,
}
struct InstalledCatalog {
    catalog: Catalog,
    pinned: bool,
}
impl Default for Localization {
    fn default() -> Self {
        let locale: LanguageIdentifier = "en".parse().unwrap();
        Self {
            base_locale: locale.clone(),
            locale,
            catalogs: HashMap::new(),
            templates: HashMap::new(),
            retained: HashMap::new(),
        }
    }
}
impl Localization {
    pub(crate) fn sources(&self) -> impl Iterator<Item = (String, String, bool)> + '_ {
        self.catalogs.values().map(|entry| {
            (
                entry.catalog.locale.to_string(),
                entry.catalog.source.clone(),
                entry.pinned,
            )
        })
    }
    pub fn locale(&self) -> &LanguageIdentifier {
        &self.locale
    }
    /// Select a prepared locale and release inactive automatic catalogs and templates.
    /// Explicit installations and catalogs retained by pending preloads stay available.
    pub fn set_locale(&mut self, locale: LanguageIdentifier) -> bool {
        let changed = locale != self.locale;
        self.base_locale = LanguageIdentifier::from_parts(locale.language, None, None, &[]);
        self.locale = locale;
        self.prune();
        changed
    }
    fn prune(&mut self) -> bool {
        let previous = (self.catalogs.len(), self.templates.len());
        self.catalogs.retain(|locale, entry| {
            entry.pinned
                || locale == &self.locale
                || locale == &self.base_locale
                || self.retained.contains_key(locale)
        });
        self.templates.retain(|locale, _| {
            locale == &self.locale
                || locale == &self.base_locale
                || self.catalogs.get(locale).is_some_and(|entry| entry.pinned)
                || self.retained.contains_key(locale)
        });
        previous != (self.catalogs.len(), self.templates.len())
    }
    /// Install an explicitly loaded catalog and retain it across locale changes.
    pub fn install(&mut self, catalog: Catalog) {
        self.install_owned(catalog, true);
    }
    pub(crate) fn install_owned(&mut self, catalog: Catalog, pinned: bool) {
        if !pinned
            && self
                .catalogs
                .get(&catalog.locale)
                .is_some_and(|entry| entry.pinned)
        {
            return;
        }
        self.catalogs
            .insert(catalog.locale.clone(), InstalledCatalog { catalog, pinned });
    }
    pub(crate) fn contains(&self, locale: &LanguageIdentifier) -> bool {
        self.catalogs.contains_key(locale)
    }
    pub(crate) fn automatic(&self, locale: &LanguageIdentifier) -> bool {
        self.catalogs.get(locale).is_some_and(|entry| !entry.pinned)
    }
    pub(crate) fn retain_for_load(&mut self, locales: &[String]) {
        for locale in locales {
            *self
                .retained
                .entry(language_id(locale).unwrap())
                .or_default() += 1;
        }
    }
    pub(crate) fn release_load(&mut self, locales: &[String]) -> bool {
        if locales.is_empty() {
            return false;
        }
        for locale in locales {
            let locale = language_id(locale).unwrap();
            let uses = self
                .retained
                .get_mut(&locale)
                .expect("pending catalog retention");
            *uses -= 1;
            if *uses == 0 {
                self.retained.remove(&locale);
            }
        }
        self.prune()
    }
    pub(crate) fn pin(&mut self, locale: &LanguageIdentifier) {
        if let Some(entry) = self.catalogs.get_mut(locale) {
            entry.pinned = true;
        }
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
        let selected = locale.unwrap_or(&self.locale);
        let empty = FluentArgs::new();
        let parameters = args.unwrap_or(&empty);
        let lookup = |locale| {
            self.catalogs
                .get(locale)
                .and_then(|entry| entry.catalog.format(id, parameters))
        };
        if let Some(result) = lookup(selected) {
            return result;
        }
        let base =
            locale.map(|locale| LanguageIdentifier::from_parts(locale.language, None, None, &[]));
        let base = base.as_ref().unwrap_or(&self.base_locale);
        if base != selected
            && let Some(result) = lookup(base)
        {
            return result;
        }
        if args.is_none() {
            return Ok(fallback.into());
        }
        let catalog = if let Some(catalog) = self
            .templates
            .get(selected)
            .and_then(|templates| templates.get(fallback))
        {
            catalog
        } else {
            let source = format!("fallback = {}", fallback.replace('\n', "\n    "));
            let catalog = Catalog::parse(&selected.to_string(), id, source)?;
            self.templates
                .entry(selected.clone())
                .or_default()
                .entry(fallback.into())
                .or_insert(catalog)
        };
        catalog
            .format("fallback", parameters)
            .unwrap_or_else(|| Err(format!("{id}: fallback has no message value")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_reclaims_automatic_catalogs_and_their_fallback_templates() {
        let mut service = Localization::default();
        service.install_owned(
            Catalog::parse("en", "en.ftl", "hello = Hello\n".into()).unwrap(),
            false,
        );
        service.install(Catalog::parse("de", "de.ftl", "hello = Hallo\n".into()).unwrap());
        service.set_locale(language_id("en").unwrap());
        let args = FluentArgs::new();
        for locale in ["en", "de"] {
            service
                .format_in(
                    Some(&language_id(locale).unwrap()),
                    "missing",
                    "Literal",
                    Some(&args),
                )
                .unwrap();
        }
        assert_eq!(service.templates.len(), 2);
        service.install_owned(
            Catalog::parse("fr", "fr.ftl", "hello = Bonjour\n".into()).unwrap(),
            false,
        );
        service.set_locale(language_id("fr").unwrap());
        assert_eq!(service.catalogs.len(), 2);
        assert!(!service.contains(&language_id("en").unwrap()));
        assert!(service.contains(&language_id("de").unwrap()));
        assert_eq!(service.templates.len(), 1);
        assert_eq!(
            *service.templates.keys().next().unwrap(),
            language_id("de").unwrap()
        );
    }
}
