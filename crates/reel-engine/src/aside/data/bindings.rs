//! Direct property connections use the same graph identities as their owners.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) enum Address {
    /// Index in the saved owner's stable State schema, mapped to its current field.
    State(u32),
    Native(String),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Property {
    pub address: Address,
    pub writable: bool,
}
impl Property {
    pub fn bytes(&self) -> usize {
        match &self.address {
            Address::Native(name) => name.capacity(),
            _ => 0,
        }
    }
}
impl Capture {
    pub(super) fn capture_binding(
        &mut self,
        heap: &Heap,
        context: &typed::Context<'_>,
    ) -> Result<(), AdapterError> {
        let index = self.bindings[self.binding_cursor];
        let source = &self.sources[index];
        let object = source.object;
        let value = source.value;
        let (owner, property) = self
            .typed
            .as_ref()
            .unwrap()
            .binding_source(heap, context, value)
            .map_err(|e| {
                context.error.replace(Some(e));
                AdapterError::Input("logical property capture failed")
            })?;
        let owner = self.atom(heap, owner, Some(context))?;
        let key = self.resource_name("source")?;
        let Object::Table(entries) = &mut self.document.objects[object as usize] else {
            unreachable!()
        };
        entries.push((key, owner));
        self.typed
            .as_mut()
            .unwrap()
            .record_property(object, property, &mut self.charge)
            .map_err(|e| {
                context.error.replace(Some(e));
                AdapterError::Input("logical property capture failed")
            })?;
        self.binding_cursor += 1;
        Ok(())
    }
}
