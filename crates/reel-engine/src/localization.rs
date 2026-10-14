//! Platform-independent Fluent catalogs. IO and background execution belong to the host.
use fluent_bundle::{FluentArgs, FluentResource, concurrent::FluentBundle};
use std::collections::HashMap;
use unic_langid::LanguageIdentifier;

pub struct Catalog {
    locale: LanguageIdentifier,
    bundle: FluentBundle<FluentResource>,
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
        let mut bundle = FluentBundle::new_concurrent(vec![locale.clone()]);
        bundle
            .add_resource(resource)
            .map_err(|errors| format!("{source}: {errors:?}"))?;
        Ok(Self { locale, bundle })
    }

    fn format(&self, id: &str, args: &FluentArgs<'_>) -> Option<Result<String, String>> {
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

/// A request is owned by the calling script task until its one terminal result.
pub trait CatalogLoader {
    fn load(&mut self, locale: &str, source: &str) -> Result<u64, String>;
    fn poll(&mut self, id: u64) -> Option<Result<Catalog, String>>;
    fn cancel(&mut self, id: u64);
}

pub struct Localization {
    locale: LanguageIdentifier,
    catalogs: HashMap<LanguageIdentifier, Catalog>,
    templates: HashMap<(LanguageIdentifier, String), Catalog>,
}
impl Default for Localization {
    fn default() -> Self {
        Self {
            locale: "en".parse().unwrap(),
            catalogs: HashMap::new(),
            templates: HashMap::new(),
        }
    }
}
impl Localization {
    pub fn locale(&self) -> &LanguageIdentifier {
        &self.locale
    }
    pub fn set_locale(&mut self, locale: LanguageIdentifier) -> bool {
        if locale == self.locale {
            return false;
        }
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
        let empty = FluentArgs::new();
        let parameters = args.unwrap_or(&empty);
        let base: LanguageIdentifier = self.locale.language.to_string().parse().unwrap();
        for locale in [&self.locale, &base] {
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
        let key = (self.locale.clone(), fallback.into());
        if !self.templates.contains_key(&key) {
            let source = format!("fallback = {}", fallback.replace('\n', "\n    "));
            let catalog = Catalog::parse(&self.locale.to_string(), id, source)?;
            self.templates.insert(key.clone(), catalog);
        }
        self.templates[&key]
            .format("fallback", parameters)
            .unwrap_or_else(|| Err(format!("{id}: fallback has no message value")))
    }
}
