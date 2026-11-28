use super::{BytecodePackage, Const, Error, ErrorKind, TextPoolDescriptor};
use std::sync::Arc;

/// Resident UTF-8 text with shared immutable ownership of its byte allocation.
/// Construction takes the host's vector; binding and cloning retain its backing.
/// File IO, decompression and storage policy remain outside Aside.
#[derive(Clone, Debug)]
pub struct TextPool {
    bytes: Arc<Vec<u8>>,
    descriptor: TextPoolDescriptor,
}

impl TextPool {
    /// Takes ownership of the vector without copying or shrinking its backing.
    /// The package allocation ledger includes its full capacity and shared header.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, Error> {
        std::str::from_utf8(&bytes)
            .map_err(|_| Error::new(ErrorKind::Encoding, "text pool is not UTF-8"))?;
        let descriptor = TextPoolDescriptor {
            byte_len: bytes.len() as u64,
            digest: *blake3::hash(&bytes).as_bytes(),
        };
        Ok(Self {
            bytes: Arc::new(bytes),
            descriptor,
        })
    }

    pub fn descriptor(&self) -> TextPoolDescriptor {
        self.descriptor
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub(crate) fn capacity_bytes(&self) -> usize {
        self.bytes.capacity() + std::mem::size_of::<Vec<u8>>() + 2 * std::mem::size_of::<usize>()
    }

    pub(crate) fn text(&self, offset: u64, len: u32) -> Result<&[u8], Error> {
        let start = usize::try_from(offset)
            .map_err(|_| Error::new(ErrorKind::Index, "text pool offset exceeds address space"))?;
        let end = start
            .checked_add(len as usize)
            .ok_or_else(|| Error::new(ErrorKind::Index, "text pool range overflow"))?;
        let bytes = self
            .bytes
            .get(start..end)
            .ok_or_else(|| Error::new(ErrorKind::Index, "text constant outside its pool"))?;
        // The entire backing was already validated. Only the two UTF-8
        // boundaries need checking; do not rescan each referenced paragraph.
        if (start != self.bytes.len() && self.bytes[start] & 0xc0 == 0x80)
            || (end != self.bytes.len() && self.bytes[end] & 0xc0 == 0x80)
        {
            return Err(Error::new(
                ErrorKind::Encoding,
                "text constant splits a UTF-8 code point",
            ));
        }
        Ok(bytes)
    }
}

/// Moves all UTF-8 constants into one external backing. Symbols and debug
/// source records are unchanged; the authoring host selects its build profile.
pub fn externalize_text(
    package: &mut BytecodePackage<
        impl super::InstructionSequence,
        impl super::SourceMapSequence,
        impl super::DescriptorSequence,
    >,
) -> Result<TextPool, Error> {
    if package.text_pool.is_some()
        || package
            .constants
            .iter()
            .any(|c| matches!(c, Const::Text { .. }))
    {
        return Err(Error::new(
            ErrorKind::Encoding,
            "package already uses an external text pool",
        ));
    }
    let mut len = 0usize;
    for constant in &package.constants {
        if let Const::Utf8(text) = constant {
            u32::try_from(text.len())
                .map_err(|_| Error::new(ErrorKind::Limit, "text constant length"))?;
            len = len
                .checked_add(text.len())
                .ok_or_else(|| Error::new(ErrorKind::Limit, "text pool length"))?;
        }
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len)
        .map_err(|_| Error::new(ErrorKind::Allocation, "text pool reservation"))?;
    for constant in &package.constants {
        if let Const::Utf8(text) = constant {
            bytes.extend_from_slice(text.as_bytes());
        }
    }
    let pool = TextPool::from_bytes(bytes)?;
    let mut offset = 0;
    for constant in &mut package.constants {
        if let Const::Utf8(text) = constant {
            let len = text.len() as u32;
            *constant = Const::Text { offset, len };
            offset += u64::from(len);
        }
    }
    package.text_pool = Some(pool.descriptor());
    Ok(pool)
}
