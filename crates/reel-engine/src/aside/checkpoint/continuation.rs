//! Detached business continuations; runtime roots and instruction PCs stay in the VM.
use crate::aside::data::TypedDocument;
use aside_vm::resume::{BindingKey, ContinuationDescriptor};
use serde::{Deserialize, Serialize};
use std::{io::Write, sync::Arc};

const MAGIC: &[u8; 8] = b"REELCON1";
const VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct Header {
    version: u32,
    continuation: ContinuationDescriptor,
    bindings: Vec<Vec<BindingKey>>,
}

/// Stable call-chain keys and one shared typed graph of live business values.
/// The graph's root is an array, ordered by frame and then by `bindings`.
/// Native operation and current-view reconstruction are coordinated by the host.
#[derive(Debug)]
pub struct ContinuationRecord {
    header: Header,
    values: Arc<TypedDocument>,
}
impl ContinuationRecord {
    /// Assemble values captured at one frozen execution boundary. Keeping all
    /// bindings in one graph preserves references shared by different activations.
    pub fn new(
        continuation: ContinuationDescriptor,
        bindings: Vec<Vec<BindingKey>>,
        values: Arc<TypedDocument>,
    ) -> Result<Self, &'static str> {
        if continuation.frames.is_empty() || continuation.frames.len() != bindings.len() {
            return Err("continuation and business binding frame counts differ");
        }
        Ok(Self {
            header: Header {
                version: VERSION,
                continuation,
                bindings,
            },
            values,
        })
    }

    /// Source identities to resolve against the newly loaded code package.
    pub fn continuation(&self) -> &ContinuationDescriptor {
        &self.header.continuation
    }

    /// Stable binding identities in flattened graph order, grouped by activation.
    pub fn bindings(&self) -> &[Vec<BindingKey>] {
        &self.header.bindings
    }

    /// Shared state graph, detached from its source heap and package.
    pub fn values(&self) -> &Arc<TypedDocument> {
        &self.values
    }

    /// Stream directly into a storage worker's section encoder.
    pub fn write_to(
        &self,
        mut writer: impl Write,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        writer.write_all(MAGIC)?;
        postcard::to_io(&self.header, &mut writer)?;
        self.values.write_to(writer)?;
        Ok(())
    }

    /// Decode one complete, bounded logical continuation record.
    pub fn decode(bytes: &[u8]) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let bytes = bytes
            .strip_prefix(MAGIC)
            .ok_or("continuation record magic")?;
        let (header, values): (Header, _) = postcard::take_from_bytes(bytes)?;
        if header.version != VERSION {
            return Err("continuation record version".into());
        }
        let values = Arc::new(TypedDocument::decode(values)?);
        Ok(Self::new(header.continuation, header.bindings, values)?)
    }
}

#[cfg(test)]
mod tests;
