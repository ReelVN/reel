//! The logical checkpoint record; runtime roots and instruction PCs stay in the VM.
use crate::aside::data::TypedDocument;
use crate::reading::checkpoint::SavedReading;
use aside_vm::resume::{BindingKey, ContinuationDescriptor, RootKey};
use serde::{Deserialize, Serialize};
use std::{io::Write, sync::Arc};

mod voices;

const MAGIC: &[u8; 8] = b"REELCHK1";
const VERSION: u32 = 1;

/// A live business value supplied by the saved graph or the current Root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CheckpointBinding {
    /// Persistent data in the shared graph's flattened binding order.
    Data(BindingKey),
    /// The object passed as `run_flow`'s current context, including direct aliases.
    CurrentContext(BindingKey),
    /// A direct object member named explicitly in the current Root's context table.
    CurrentContextMember {
        /// Stable business binding identity.
        key: BindingKey,
        /// String key supplied by the author in `run_flow(..., context: {...})`.
        member: String,
    },
}
impl CheckpointBinding {
    /// Stable source binding identity, independent of its current register.
    pub fn key(&self) -> &BindingKey {
        match self {
            Self::Data(key)
            | Self::CurrentContext(key)
            | Self::CurrentContextMember { key, .. } => key,
        }
    }
    pub(super) fn string_bytes(&self) -> usize {
        self.key().0.capacity()
            + match self {
                Self::CurrentContextMember { member, .. } => member.capacity(),
                _ => 0,
            }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Header {
    version: u32,
    root: RootKey,
    continuation: ContinuationDescriptor,
    bindings: Vec<Vec<CheckpointBinding>>,
    reading: Option<SavedReading>,
}

/// Stable Root and call-chain keys with one shared typed state graph.
/// The graph root is an array: Root State first, followed by live business
/// data values ordered by frame and then by `bindings`. Current-context bindings
/// consume no graph element and connect to the current Root's object on loading.
/// Native operation and current-view reconstruction are coordinated by the host.
#[derive(Debug, Clone)]
pub struct CheckpointRecord {
    header: Header,
    values: Arc<TypedDocument>,
}
impl CheckpointRecord {
    /// Assemble values captured at one frozen execution boundary. Keeping all
    /// bindings in one graph preserves references shared by different activations.
    pub fn new(
        root: RootKey,
        continuation: ContinuationDescriptor,
        bindings: Vec<Vec<CheckpointBinding>>,
        values: Arc<TypedDocument>,
    ) -> Result<Self, &'static str> {
        if continuation.frames.is_empty() || continuation.frames.len() != bindings.len() {
            return Err("continuation and business binding frame counts differ");
        }
        Ok(Self {
            header: Header {
                version: VERSION,
                root,
                continuation,
                bindings,
                reading: None,
            },
            values,
        })
    }

    /// Current bootstrap declaration selected by this saved game.
    pub fn root(&self) -> &RootKey {
        &self.header.root
    }

    /// Retained detached data and stable metadata, charged while the record is used.
    pub fn retained_bytes(&self) -> u64 {
        (size_of::<Self>()
            + self.header.root.0.capacity()
            + self.header.continuation.frames.capacity()
                * size_of::<aside_vm::resume::FrameDescriptor>()
            + self
                .header
                .continuation
                .frames
                .iter()
                .map(|f| {
                    f.flow.0.capacity()
                        + match &f.site {
                            aside_vm::resume::ResumeSite::Point(k) => k.0.capacity(),
                            aside_vm::resume::ResumeSite::Return(k) => k.0.capacity(),
                        }
                })
                .sum::<usize>()
            + self.header.bindings.capacity() * size_of::<Vec<CheckpointBinding>>()
            + self
                .header
                .bindings
                .iter()
                .map(|v| {
                    v.capacity() * size_of::<CheckpointBinding>()
                        + v.iter().map(CheckpointBinding::string_bytes).sum::<usize>()
                })
                .sum::<usize>()) as u64
            + self.values.retained_bytes()
            + self
                .header
                .reading
                .as_ref()
                .map_or(0, SavedReading::retained_bytes)
    }

    /// Source identities to resolve against the newly loaded code package.
    pub fn continuation(&self) -> &ContinuationDescriptor {
        &self.header.continuation
    }

    /// Stable binding identities and their saved-data or current-context source.
    pub fn bindings(&self) -> &[Vec<CheckpointBinding>] {
        &self.header.bindings
    }

    /// Shared state graph, detached from its source heap and package.
    pub fn values(&self) -> &Arc<TypedDocument> {
        &self.values
    }

    pub(super) fn with_reading(mut self, reading: SavedReading) -> Self {
        self.header.reading = Some(reading);
        self
    }

    pub(super) fn take_reading(&mut self) -> Option<SavedReading> {
        self.header.reading.take()
    }

    /// Reattach the live transcript prefix to a detached history checkpoint.
    pub fn history_jump(&self, source: &crate::Session, group: u64) -> Option<Self> {
        let history = source.view().history.prefix_through(group)?;
        let mut record = self.clone();
        record.header.reading.as_mut()?.attach_history(history);
        Some(record)
    }

    /// Stream directly into a storage worker's section encoder.
    pub fn write_to(
        &self,
        mut writer: impl Write,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        writer.write_all(MAGIC)?;
        postcard::to_io(&self.header, &mut writer)?;
        self.values.write_to(&mut writer)?;
        if let Some(reading) = &self.header.reading {
            voices::write(&reading.data.history, &mut writer)?;
        }
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
        let (values, tail) = TypedDocument::decode_prefix(values)?;
        let mut reading = header.reading;
        if !tail.is_empty() {
            let history = &mut reading.as_mut().ok_or("voice history without reading")?.data.history;
            voices::read(history, tail)?;
        }
        let mut record = Self::new(header.root, header.continuation, header.bindings, Arc::new(values))?;
        record.header.reading = reading;
        Ok(record)
    }
}

#[cfg(test)]
mod tests;
