use super::{AdapterError, HostTicket, SessionVm, observable, text};
use crate::localization::{CatalogLoader, Localization, language_id, message_id};
use aside_vm::{
    RootKind, RootToken, Value, ValueTag,
    executor::{Completion, HostResponse, WaitId},
};
use fluent_bundle::{FluentArgs, FluentValue};

struct Pending {
    wait: Option<WaitId>,
    id: u64,
    locale: String,
    source: String,
    result: Option<Result<crate::localization::Catalog, String>>,
}
#[derive(Default)]
pub(super) struct Translations {
    service: Localization,
    loader: Option<Box<dyn CatalogLoader>>,
    pending: Vec<Pending>,
    revision: Option<RootToken>,
    selected: bool,
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
                    },
                )
            })
            .chain(self.service.sources().map(|(locale, source)| {
                Record::new(Part::Catalog, &(None::<WaitId>, locale, source))
            }))
            .chain(
                self.pending
                    .iter()
                    .map(|p| Record::new(Part::Catalog, &(p.wait, &p.locale, &p.source))),
            ),
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
            self.service.set_locale(
                language_id(&saved.locale).map_err(aside_vm::snapshot::SnapshotError::Invalid)?,
            );
        } else {
            let (wait, locale, source): (Option<WaitId>, String, String) = record.read()?;
            let wait = wait.map(|w| map.wait(w)).transpose()?;
            let id = self
                .loader
                .as_mut()
                .ok_or(AdapterError::Input("restoring catalogs requires a loader"))?
                .load(&locale, &source)
                .map_err(aside_vm::snapshot::SnapshotError::Invalid)?;
            self.pending.push(Pending {
                wait,
                id,
                locale,
                source,
                result: None,
            });
        }
        Ok(())
    }
    pub(super) fn close(&mut self) {
        if let Some(loader) = &mut self.loader {
            for pending in self.pending.drain(..) {
                loader.cancel(pending.id);
            }
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct SavedTranslations {
    locale: String,
    revision: Option<RootToken>,
    selected: bool,
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
        self.translations.loader = Some(loader);
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
            let Pending { wait, id, .. } = self.translations.pending[index];
            let loader = self.translations.loader.as_mut().unwrap();
            if wait.is_some_and(|wait| !self.vm.wait_is_pending(wait)) {
                loader.cancel(id);
                self.translations.pending.swap_remove(index);
                continue;
            }
            if self.translations.pending[index].result.is_none() {
                self.translations.pending[index].result = loader.poll(id);
            }
            let Some(result) = &self.translations.pending[index].result else {
                if wait.is_none() {
                    break;
                } // Install saved catalogs before pending replacements.
                index += 1;
                continue;
            };
            if wait.is_none()
                && let Err(error) = result
            {
                return Err(aside_vm::snapshot::SnapshotError::Invalid(format!(
                    "restoring translation catalog: {error}"
                ))
                .into());
            }
            let result = match result {
                Ok(_) => {
                    self.translation_changed()?;
                    Completion::Ready(Value::NULL)
                }
                Err(message) => Completion::Error(self.translation_fault(message.clone())?),
            };
            if let Some(wait) = wait {
                self.vm.complete(wait, result)?;
            }
            let pending = if wait.is_none() {
                self.translations.pending.remove(index)
            } else {
                self.translations.pending.swap_remove(index)
            };
            if let Some(Ok(catalog)) = pending.result {
                self.translations.service.install(catalog);
            }
            woke = true;
        }
        Ok(woke)
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
                let source = text::read_string(self.vm.heap(), args[1], self.read_limit)?;
                let request = language_id(&locale).and_then(|locale| {
                    self.translations
                        .loader
                        .as_mut()
                        .ok_or_else(|| "catalog loader is not installed".to_string())?
                        .load(&locale.to_string(), &source)
                });
                match request {
                    Ok(id) => {
                        let wait = ticket.wait_id();
                        if let Err(error) = self.vm.respond(ticket, HostResponse::Pending(wait)) {
                            self.translations.loader.as_mut().unwrap().cancel(id);
                            return Err(error.into());
                        }
                        self.translations.pending.push(Pending {
                            wait: Some(wait),
                            id,
                            locale,
                            source,
                            result: None,
                        });
                        self.issued = None;
                        return Ok(());
                    }
                    Err(message) => Err(message),
                }
            }
            "reel::localization::set_locale" => {
                let locale = text::read_string(self.vm.heap(), args[0], self.read_limit)?;
                match language_id(&locale) {
                    Ok(locale) => {
                        if self.translations.service.locale() != &locale {
                            self.translation_changed()?;
                        }
                        self.translations.service.set_locale(locale);
                        self.translations.selected = true;
                        Ok(Value::NULL)
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
                let id = if args[1].tag() == ValueTag::Null {
                    message_id(&fallback)
                } else {
                    text::read_string(heap, args[1], self.read_limit)?
                };
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
                match self.translations.service.format(
                    &id,
                    &fallback,
                    has_parameters.then_some(&parameters),
                ) {
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
