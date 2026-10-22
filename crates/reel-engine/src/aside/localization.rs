use super::{AdapterError, HostTicket, SessionVm, observable, text};

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Translate literal source text with an adjacent static locale table.
        /// Interpolated source holes execute once in source order; translated
        /// holes refer to those values and never execute another expression.
        /// Use bracketed string keys for tags such as ["zh-CN"].
        /// The compiler extracts catalogs and supplies the internal lookup key.
        /// Literal templates can use explicit Fluent params; interpolated strings
        /// supply their own parameters and use normal Aside text conversion.
        /// Interpolated values use Fluent's bidirectional text isolation.
        /// An explicit locale selects its loaded catalog; otherwise use the current
        /// observable locale. Missing translations use the supplied source text.
        "reel::tr";
        /// Preload and retain a locale's indexed catalogs, including its available base locale.
        /// An explicit source loads one custom Fluent asset for that locale instead.
        /// Wait for complete parsing; failures or task cancellation preserve installed catalogs.
        "reel::localization::load";
        /// Load the indexed locale and its available base before selecting it atomically.
        /// Missing catalogs use source text. Failures or task cancellation preserve the
        /// current language; a newer selection supersedes an older pending call.
        /// Superseded calls finish with null and do not publish their selection.
        /// Retain only current/base automatic catalogs and explicitly preloaded catalogs.
        "reel::localization::set_locale";
        /// Read the current observable locale identifier.
        "reel::localization::locale";
    }
}
use crate::localization::{CatalogLoader, Localization, language_id};
use aside_vm::{
    RootKind, RootToken, Value, ValueTag,
    executor::{Completion, HostResponse, WaitId},
};
use fluent_bundle::{FluentArgs, FluentValue};
use std::{collections::BTreeMap, sync::Arc};

/// Shared, unparsed source compilation products retained across fresh sessions
/// and restoration. Source playback retains all authored locale text; release
/// assets instead use their seekable catalog loader and an empty handle.
#[derive(Clone, Default, Debug)]
pub struct SourceCatalogs(Arc<super::source::CatalogProducts>);
impl SourceCatalogs {
    pub(super) fn from_products(products: Arc<super::source::CatalogProducts>) -> Self {
        Self(products)
    }
    /// Combine source catalogs with the platform's indexed and explicit assets.
    /// Source locales take precedence; explicit asset requests use the fallback.
    pub fn loader(&self, fallback: Box<dyn CatalogLoader>) -> Box<dyn CatalogLoader> {
        if self.0.catalogs.is_empty() {
            fallback
        } else {
            self.with_fallback(Some(fallback))
        }
    }
    fn with_fallback(&self, fallback: Option<Box<dyn CatalogLoader>>) -> Box<dyn CatalogLoader> {
        Box::new(SourceLoader::new(self.clone(), fallback))
    }
}
const SOURCE_CATALOG: &str = "__reel/source-catalog";
enum SourceLoad {
    Inline { locale: String },
    External(u64),
}
struct SourceLoader {
    products: SourceCatalogs,
    fallback: Option<Box<dyn CatalogLoader>>,
    pending: BTreeMap<u64, SourceLoad>,
    next: u64,
}
impl SourceLoader {
    fn new(products: SourceCatalogs, fallback: Option<Box<dyn CatalogLoader>>) -> Self {
        Self {
            products,
            fallback,
            pending: BTreeMap::new(),
            next: 0,
        }
    }
}
impl AsMut<dyn CatalogLoader> for SourceLoader {
    fn as_mut(&mut self) -> &mut (dyn CatalogLoader + 'static) {
        self
    }
}
impl CatalogLoader for SourceLoader {
    fn resolve(&self, locale: &str) -> Option<&str> {
        if self.products.0.catalogs.contains_key(locale) {
            Some(SOURCE_CATALOG)
        } else {
            self.fallback.as_ref()?.resolve(locale)
        }
    }
    fn load(&mut self, locale: &str, source: &str) -> Result<u64, String> {
        let id = self
            .next
            .checked_add(1)
            .ok_or("catalog request identity exhausted")?;
        let request = if source == SOURCE_CATALOG && self.products.0.catalogs.contains_key(locale) {
            SourceLoad::Inline {
                locale: locale.into(),
            }
        } else {
            SourceLoad::External(
                self.fallback
                    .as_mut()
                    .ok_or("catalog asset loader is not installed")?
                    .load(locale, source)?,
            )
        };
        self.next = id;
        self.pending.insert(id, request);
        Ok(id)
    }
    fn poll(&mut self, id: u64) -> Option<Result<crate::localization::Catalog, String>> {
        match self.pending.get(&id)? {
            SourceLoad::Inline { locale } => {
                let result = crate::localization::Catalog::parse(
                    locale,
                    SOURCE_CATALOG,
                    self.products.0.catalogs[locale].clone(),
                );
                self.pending.remove(&id);
                Some(result)
            }
            SourceLoad::External(external) => {
                let result = self.fallback.as_mut()?.poll(*external)?;
                self.pending.remove(&id);
                Some(result)
            }
        }
    }
    fn cancel(&mut self, id: u64) {
        if let Some(SourceLoad::External(external)) = self.pending.remove(&id)
            && let Some(fallback) = self.fallback.as_mut()
        {
            fallback.cancel(external);
        }
    }
}

pub(super) fn describe(api: &mut aside_host::source::Api) {
    use aside_compiler::semantic::TypeHint as T;
    use aside_host::source::nullable;
    api.refine(
        "reel::tr",
        T::String,
        &[
            ("source", T::String),
            ("translations", nullable(T::Table)),
            ("params", nullable(T::Table)),
            ("locale", nullable(T::String)),
        ],
    );
    api.refine(
        "reel::localization::load",
        T::Null,
        &[("locale", T::String), ("source", nullable(T::String))],
    );
    api.refine(
        "reel::localization::set_locale",
        T::Null,
        &[("locale", T::String)],
    );
    api.refine("reel::localization::locale", T::String, &[]);
}

struct Candidate {
    id: u64,
    locale: String,
    source: String,
    result: Option<crate::localization::Catalog>,
}
#[derive(serde::Serialize, serde::Deserialize)]
enum Operation {
    Select { locale: String, generation: u64 },
    Load { locales: Vec<String> },
    Restore { pinned: bool },
}
type SavedCatalogs = (
    Option<WaitId>,
    Operation,
    Vec<(String, String)>,
    Vec<String>,
);
struct Pending {
    wait: Option<WaitId>,
    operation: Operation,
    catalogs: Vec<Candidate>,
    retained: Vec<String>,
}
impl Pending {
    fn cancel(&self, loader: &mut dyn CatalogLoader) {
        for candidate in &self.catalogs {
            loader.cancel(candidate.id);
        }
    }
    fn poll(&mut self, loader: &mut dyn CatalogLoader) -> Result<bool, String> {
        let mut ready = true;
        for candidate in &mut self.catalogs {
            if candidate.result.is_none() {
                match loader.poll(candidate.id) {
                    Some(Ok(catalog)) => candidate.result = Some(catalog),
                    Some(Err(error)) => return Err(error),
                    None => ready = false,
                }
            }
        }
        Ok(ready)
    }
}
#[derive(Default)]
pub(super) struct Translations {
    service: Localization,
    loader: Option<SourceLoader>,
    source: SourceCatalogs,
    pending: Vec<Pending>,
    revision: Option<RootToken>,
    selected: bool,
    generation: u64,
}
impl Translations {
    pub(super) fn snapshot_records(&self) -> super::snapshot::RecordStream<'_> {
        use super::snapshot::{Part, Record};
        Box::new(
            std::iter::once_with(|| {
                Record::new(
                    Part::Translations,
                    &SavedTranslations {
                        locale: self.service.locale().to_string(),
                        revision: self.revision,
                        selected: self.selected,
                        generation: self.generation,
                    },
                )
            })
            .chain(self.service.sources().map(|(locale, source, pinned)| {
                Record::new(
                    Part::Catalog,
                    &(
                        None::<WaitId>,
                        Operation::Restore { pinned },
                        vec![(locale, source)],
                        Vec::<String>::new(),
                    ),
                )
            }))
            .chain(self.pending.iter().map(|p| {
                let sources: Vec<_> = p
                    .catalogs
                    .iter()
                    .map(|candidate| (&candidate.locale, &candidate.source))
                    .collect();
                Record::new(Part::Catalog, &(p.wait, &p.operation, sources, &p.retained))
            })),
        )
    }
    pub(super) fn restore_record(
        &mut self,
        record: &super::snapshot::Record,
        map: &aside_vm::snapshot::SnapshotMap,
    ) -> Result<(), AdapterError> {
        if matches!(record.part, super::snapshot::Part::Translations) {
            let saved: SavedTranslations = record.read()?;
            self.revision = saved.revision.map(|r| map.root(r)).transpose()?;
            self.selected = saved.selected;
            self.generation = saved.generation;
            self.service.set_locale(
                language_id(&saved.locale).map_err(aside_vm::snapshot::SnapshotError::Invalid)?,
            );
        } else {
            let (wait, operation, sources, retained): SavedCatalogs = record.read()?;
            let wait = wait.map(|w| map.wait(w)).transpose()?;
            let catalogs = self
                .start_catalogs(sources)
                .map_err(aside_vm::snapshot::SnapshotError::Invalid)?;
            // Installed records are restored first. Keep this ownership even while
            // their asynchronous loads are still waiting to publish the old catalogs.
            self.service.retain_for_load(&retained);
            self.pending.push(Pending {
                wait,
                operation,
                catalogs,
                retained,
            });
        }
        Ok(())
    }
    pub(super) fn close(&mut self) {
        if let Some(loader) = &mut self.loader {
            for pending in self.pending.drain(..) {
                pending.cancel(loader.as_mut());
                self.service.release_load(&pending.retained);
            }
        }
    }
    fn start_catalogs(&mut self, sources: Vec<(String, String)>) -> Result<Vec<Candidate>, String> {
        let mut catalogs: Vec<Candidate> = Vec::with_capacity(sources.len());
        if sources.is_empty() {
            return Ok(catalogs);
        }
        let loader = self
            .loader
            .as_mut()
            .ok_or("catalog loader is not installed")?;
        for (locale, source) in sources {
            match loader.load(&locale, &source) {
                Ok(id) => catalogs.push(Candidate {
                    id,
                    locale,
                    source,
                    result: None,
                }),
                Err(error) => {
                    for candidate in catalogs {
                        loader.cancel(candidate.id);
                    }
                    return Err(error);
                }
            }
        }
        Ok(catalogs)
    }
    fn indexed_sources(&self, locales: &[String]) -> Vec<(String, String)> {
        locales
            .iter()
            .filter(|locale| !self.service.contains(&language_id(locale).unwrap()))
            .filter_map(|locale| {
                self.loader
                    .as_ref()?
                    .resolve(locale)
                    .map(|source| (locale.clone(), source.to_owned()))
            })
            .collect()
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct SavedTranslations {
    locale: String,
    revision: Option<RootToken>,
    selected: bool,
    generation: u64,
}
impl Drop for Translations {
    fn drop(&mut self) {
        self.close();
    }
}

impl SessionVm {
    /// Select resource IO before executing a load; pending tasks retain their backend.
    pub fn set_catalog_loader(
        &mut self,
        loader: Box<dyn CatalogLoader>,
    ) -> Result<(), AdapterError> {
        if self.revoked || !self.translations.pending.is_empty() {
            return Err(AdapterError::Input(
                "cannot replace catalog loader with pending loads or a retired session",
            ));
        }
        self.translations.loader = Some(SourceLoader::new(
            self.translations.source.clone(),
            Some(loader),
        ));
        Ok(())
    }
    /// Retain the same source index when constructing a fresh or restored VM.
    pub fn source_catalogs(&self) -> SourceCatalogs {
        self.translations.source.clone()
    }
    /// Select source compilation products before any locale loads are pending.
    /// Subsequent asset-loader replacement preserves this source index.
    pub fn set_source_catalogs(&mut self, source: SourceCatalogs) -> Result<(), AdapterError> {
        if self.revoked || !self.translations.pending.is_empty() {
            return Err(AdapterError::Input(
                "cannot replace source catalogs with pending loads or a retired session",
            ));
        }
        self.translations.source = source;
        if let Some(loader) = &mut self.translations.loader {
            loader.products = self.translations.source.clone();
        } else {
            self.translations.loader =
                Some(SourceLoader::new(self.translations.source.clone(), None));
        }
        Ok(())
    }
    pub fn localization_pending(&self) -> bool {
        !self.translations.pending.is_empty()
    }
    pub fn locale(&self) -> Option<String> {
        self.translations
            .selected
            .then(|| self.translations.service.locale().to_string())
    }
    fn translation_changed(&mut self) -> Result<(), AdapterError> {
        if let Some(root) = self.translations.revision {
            let heap = self.vm.heap_mut();
            let host = heap.root_value(root)?;
            let previous = heap
                .host_property(aside_vm::PropertyId { host, field: 0 })?
                .as_bool()
                .unwrap();
            observable::publish(heap, host, 0, Value::bool(!previous))?;
        }
        Ok(())
    }
    fn observe_translation(&mut self) -> Result<(), AdapterError> {
        let heap = self.vm.heap_mut();
        let host = if let Some(root) = self.translations.revision {
            heap.root_value(root)?
        } else {
            let host = observable::record(heap, &[("revision", Value::bool(false))], &[])?;
            self.translations.revision =
                Some(heap.add_root(self.root_owner, RootKind::Host, host)?);
            host
        };
        heap.observe_read(host, Some(0))?;
        Ok(())
    }
    fn translation_fault(&mut self, message: String) -> Result<aside_vm::NewFault, AdapterError> {
        Ok(aside_vm::NewFault {
            value: text::alloc_string(self.vm.heap_mut(), &message, self.read_limit)?,
            code: aside_vm::FaultCode::core(super::FaultKind::InvalidState),
            cause: None,
        })
    }
    pub fn maintain_localization(&mut self) -> Result<bool, AdapterError> {
        let mut woke = false;
        let mut index = 0;
        while index < self.translations.pending.len() {
            let wait = self.translations.pending[index].wait;
            let loader = self.translations.loader.as_mut().unwrap();
            if wait.is_some_and(|wait| !self.vm.wait_is_pending(wait)) {
                self.translations.pending[index].cancel(loader.as_mut());
                let pending = self.translations.pending.swap_remove(index);
                if self.translations.service.release_load(&pending.retained) {
                    self.translation_changed()?;
                    woke = true;
                }
                continue;
            }
            match self.translations.pending[index].poll(loader.as_mut()) {
                Ok(false) => {
                    if wait.is_none() {
                        break; // Restore installed catalogs before pending replacements.
                    }
                    index += 1;
                    continue;
                }
                Err(message) => {
                    if wait.is_none() {
                        return Err(aside_vm::snapshot::SnapshotError::Invalid(format!(
                            "restoring translation catalog: {message}"
                        ))
                        .into());
                    }
                    let fault = self.translation_fault(message)?;
                    self.vm.complete(wait.unwrap(), Completion::Error(fault))?;
                    let pending = self.translations.pending.swap_remove(index);
                    pending.cancel(self.translations.loader.as_mut().unwrap().as_mut());
                    if self.translations.service.release_load(&pending.retained) {
                        self.translation_changed()?;
                    }
                }
                Ok(true) => {
                    let changed = self.translation_will_change(
                        &self.translations.pending[index].operation,
                        &self.translations.pending[index].catalogs,
                    );
                    if changed {
                        self.translation_changed()?;
                    }
                    if let Some(wait) = wait {
                        self.vm.complete(wait, Completion::Ready(Value::NULL))?;
                    }
                    let pending = self.translations.pending.remove(index);
                    self.commit_translation(pending.operation, pending.catalogs);
                    self.translations.service.release_load(&pending.retained);
                }
            }
            woke = true;
        }
        Ok(woke)
    }
    fn translation_will_change(&self, operation: &Operation, catalogs: &[Candidate]) -> bool {
        if let Operation::Select { generation, .. } = &operation
            && *generation != self.translations.generation
        {
            return false;
        }
        if !catalogs.is_empty() {
            return true;
        }
        matches!(operation, Operation::Select { locale, .. }
            if language_id(locale).unwrap() != *self.translations.service.locale())
    }
    fn commit_translation(&mut self, operation: Operation, catalogs: Vec<Candidate>) {
        if let Operation::Select { generation, .. } = &operation
            && *generation != self.translations.generation
        {
            return;
        }
        let pinned = match &operation {
            Operation::Select { .. } => false,
            Operation::Load { .. } => true,
            Operation::Restore { pinned } => *pinned,
        };
        for candidate in catalogs {
            self.translations.service.install_owned(
                candidate.result.expect("completed catalog candidate"),
                pinned,
            );
        }
        match operation {
            Operation::Select { locale, .. } => {
                self.translations
                    .service
                    .set_locale(language_id(&locale).unwrap());
                self.translations.selected = true;
            }
            Operation::Load { locales } => {
                for locale in locales {
                    self.translations
                        .service
                        .pin(&language_id(&locale).unwrap());
                }
            }
            Operation::Restore { .. } => {}
        }
    }
    fn supersede_locale_selection(&mut self) -> Result<u64, AdapterError> {
        self.translations.generation = self
            .translations
            .generation
            .checked_add(1)
            .ok_or(AdapterError::Input("locale selection identity exhausted"))?;
        let mut index = 0;
        while index < self.translations.pending.len() {
            if !matches!(
                self.translations.pending[index].operation,
                Operation::Select { .. }
            ) {
                index += 1;
                continue;
            }
            if let Some(wait) = self.translations.pending[index].wait
                && self.vm.wait_is_pending(wait)
            {
                // A superseded UI callback finishes normally without publishing its selection.
                self.vm.complete(wait, Completion::Ready(Value::NULL))?;
            }
            let pending = self.translations.pending.swap_remove(index);
            pending.cancel(self.translations.loader.as_mut().unwrap().as_mut());
            if self.translations.service.release_load(&pending.retained) {
                self.translation_changed()?;
            }
        }
        Ok(self.translations.generation)
    }
    fn begin_translation(
        &mut self,
        ticket: HostTicket,
        operation: Operation,
        sources: Vec<(String, String)>,
    ) -> Result<(), AdapterError> {
        let catalogs = match self.translations.start_catalogs(sources) {
            Ok(catalogs) => catalogs,
            Err(message) => {
                let fault = self.translation_fault(message)?;
                self.vm.respond(ticket, HostResponse::Error(fault))?;
                self.issued = None;
                return Ok(());
            }
        };
        if catalogs.is_empty() {
            if self.translation_will_change(&operation, &catalogs) {
                self.translation_changed()?;
            }
            self.vm.respond(ticket, HostResponse::Ready(Value::NULL))?;
            self.commit_translation(operation, catalogs);
        } else {
            let retained = match &operation {
                Operation::Load { locales } => locales
                    .iter()
                    .filter(|locale| {
                        !catalogs
                            .iter()
                            .any(|candidate| &candidate.locale == *locale)
                    })
                    .filter(|locale| {
                        self.translations
                            .service
                            .automatic(&language_id(locale).unwrap())
                    })
                    .cloned()
                    .collect(),
                _ => Vec::new(),
            };
            let wait = ticket.wait_id();
            if let Err(error) = self.vm.respond(ticket, HostResponse::Pending(wait)) {
                for candidate in catalogs {
                    self.translations
                        .loader
                        .as_mut()
                        .unwrap()
                        .cancel(candidate.id);
                }
                return Err(error.into());
            }
            self.translations.service.retain_for_load(&retained);
            self.translations.pending.push(Pending {
                wait: Some(wait),
                operation,
                catalogs,
                retained,
            });
        }
        self.issued = None;
        Ok(())
    }
    pub(super) fn dispatch_localization(
        &mut self,
        ticket: HostTicket,
        symbol: &str,
    ) -> Result<(), AdapterError> {
        let args = self.vm.host_arguments(ticket)?.to_vec();
        let result = match symbol {
            "reel::localization::load" => {
                let locale = text::read_string(self.vm.heap(), args[0], self.read_limit)?;
                match language_id(&locale) {
                    Ok(locale) => {
                        let (locales, sources) = if args[1].tag() == ValueTag::Null {
                            let locales = locale_chain(&locale);
                            let sources = self.translations.indexed_sources(&locales);
                            (locales, sources)
                        } else {
                            let source =
                                text::read_string(self.vm.heap(), args[1], self.read_limit)?;
                            let locale = locale.to_string();
                            (vec![locale.clone()], vec![(locale, source)])
                        };
                        return self.begin_translation(
                            ticket,
                            Operation::Load { locales },
                            sources,
                        );
                    }
                    Err(message) => Err(message),
                }
            }
            "reel::localization::set_locale" => {
                let locale = text::read_string(self.vm.heap(), args[0], self.read_limit)?;
                match language_id(&locale) {
                    Ok(locale) => {
                        let generation = self.supersede_locale_selection()?;
                        let sources = self.translations.indexed_sources(&locale_chain(&locale));
                        return self.begin_translation(
                            ticket,
                            Operation::Select {
                                locale: locale.to_string(),
                                generation,
                            },
                            sources,
                        );
                    }
                    Err(message) => Err(message),
                }
            }
            "reel::localization::locale" => {
                self.observe_translation()?;
                Ok(self
                    .vm
                    .heap_mut()
                    .alloc_string(&self.translations.service.locale().to_string())?)
            }
            "reel::tr" => {
                self.observe_translation()?;
                let heap = self.vm.heap_mut();
                let fallback = text::read_string(heap, args[0], self.read_limit)?;
                // The source compile extension replaces the static locale table
                // with its generated key. A raw dynamic native invocation has no
                // catalog extraction contract and must not discard its table.
                let id = text::read_string(heap, args[1], self.read_limit)?;
                let mut parameters = FluentArgs::new();
                let has_parameters = args[2].tag() != ValueTag::Null;
                if has_parameters {
                    heap.observe_read(args[2], None)?;
                    for index in 0..heap.table_len(args[2])? {
                        let (key, value) = heap.table_entry(args[2], index)?;
                        let name = text::read_string(heap, key, self.read_limit)?;
                        let value = if let Some(value) = value.as_int() {
                            FluentValue::from(value)
                        } else if let Some(value) = value.as_float() {
                            FluentValue::from(value)
                        } else {
                            FluentValue::from(text::read_string(heap, value, self.read_limit)?)
                        };
                        parameters.set(name, value);
                    }
                }
                let locale = if args[3].tag() == ValueTag::Null {
                    Ok(None)
                } else {
                    let name = text::read_string(heap, args[3], self.read_limit)?;
                    language_id(&name).map(Some)
                };
                match locale.and_then(|locale| {
                    self.translations.service.format_in(
                        locale.as_ref(),
                        &id,
                        &fallback,
                        has_parameters.then_some(&parameters),
                    )
                }) {
                    Ok(value) => Ok(text::alloc_string(
                        self.vm.heap_mut(),
                        &value,
                        self.read_limit,
                    )?),
                    Err(message) => Err(message),
                }
            }
            _ => unreachable!(),
        };
        let response = match result {
            Ok(value) => HostResponse::Ready(value),
            Err(message) => HostResponse::Error(self.translation_fault(message)?),
        };
        self.vm.respond(ticket, response)?;
        self.issued = None;
        Ok(())
    }
}

fn locale_chain(locale: &unic_langid::LanguageIdentifier) -> Vec<String> {
    let base = unic_langid::LanguageIdentifier::from_parts(locale.language, None, None, &[]);
    let mut locales = vec![locale.to_string()];
    if base != *locale {
        locales.push(base.to_string());
    }
    locales
}
