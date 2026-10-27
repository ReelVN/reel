//! Immutable compressed execution words and a program-shared bounded page cache.
//! Frames retain logical PCs. Only a VM's current fetch window pins a hot page;
//! eviction never removes the reservation of a page still owned by a reader.

use super::{Error, ErrorKind};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

pub(crate) const PAGE_BYTES: usize = 32 * 1024;
pub(crate) const PAGE_WORDS: usize = PAGE_BYTES / 4;
pub(crate) const HOT_BYTES: usize = 8 * 1024 * 1024;
const HOT_PAGES: usize = HOT_BYTES / PAGE_BYTES;

/// Retained code ownership and the independent upper bound of the shared cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CodeCacheStats {
    /// Immutable compressed bytes, boundaries, directories and cache slots.
    pub cold_capacity_bytes: usize,
    /// Pages owned by the cache or a VM/iterator fetch window.
    pub resident_capacity_bytes: usize,
    /// Largest simultaneous owned page allocation since admission.
    pub peak_resident_capacity_bytes: usize,
    /// Page ownership limit, including page objects and Arc allocations.
    pub hot_limit_bytes: usize,
    /// One validation page and the bounded inflater's stack workspace.
    pub construction_scratch_bytes: usize,
    /// Cold storage, the full hot limit and bounded inflate workspace.
    pub capacity_ceiling_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CodeBlock {
    pub offset: u32,
    pub len: u32,
}

/// Immutable, independently compressed execution pages shared by all VMs using
/// a verified program. Runtime readers share a fixed eight-MiB hot-page budget.
#[derive(Debug)]
pub struct CodeStore {
    words: u32,
    packed: Box<[u8]>,
    blocks: Box<[CodeBlock]>,
    boundaries: Box<[u32]>,
    cache: Mutex<PageCache>,
    residency: Arc<Residency>,
}

#[derive(Debug, Default)]
struct Residency {
    current: AtomicUsize,
    peak: AtomicUsize,
}

#[derive(Debug)]
struct PageCache {
    pages: Vec<Option<Arc<CodePage>>>,
    slots: Box<[u32]>,
    cursor: usize,
}

#[derive(Debug)]
pub(crate) struct CodePage {
    index: u32,
    bytes: Box<[u8]>,
    residency: Arc<Residency>,
}
impl Drop for CodePage {
    fn drop(&mut self) {
        self.residency
            .current
            .fetch_sub(self.capacity_bytes(), Ordering::Relaxed);
    }
}
impl CodePage {
    fn capacity_bytes(&self) -> usize {
        self.bytes.len() + size_of::<Self>() + 2 * size_of::<usize>()
    }
    fn word(&self, index: usize) -> u32 {
        let start = index * 4;
        u32::from_le_bytes(
            self.bytes[start..start + 4]
                .try_into()
                .expect("admitted page word"),
        )
    }
}

fn allocation() -> Error {
    Error::new(ErrorKind::Allocation, "execution code page allocation")
}
fn encoding(detail: &'static str) -> Error {
    Error::new(ErrorKind::Encoding, detail)
}
fn bytes(length: usize) -> Result<Vec<u8>, Error> {
    let mut value = Vec::new();
    value.try_reserve_exact(length).map_err(|_| allocation())?;
    value.resize(length, 0);
    Ok(value)
}

impl CodeStore {
    pub(crate) fn planned_capacity_bytes(
        words: u32,
        packed_len: usize,
        block_count: usize,
    ) -> Result<usize, Error> {
        let hot = HOT_PAGES.min(block_count);
        let fixed = size_of::<Self>()
            + 2 * size_of::<usize>()
            + size_of::<Residency>()
            + 2 * size_of::<usize>();
        fixed
            .checked_add(packed_len)
            .and_then(|value| {
                value.checked_add(block_count.checked_mul(size_of::<CodeBlock>() + 4)?)
            })
            .and_then(|value| value.checked_add((words as usize).div_ceil(32).checked_mul(4)?))
            .and_then(|value| {
                value.checked_add(
                    hot.checked_mul(PAGE_BYTES + size_of::<CodePage>() + 4 * size_of::<usize>())?,
                )
            })
            .ok_or_else(|| Error::new(ErrorKind::Limit, "execution code allocation size"))
    }
    pub(crate) fn construction_scratch_bytes() -> usize {
        PAGE_BYTES + size_of::<miniz_oxide::inflate::core::DecompressorOxide>()
    }
    /// Admits independently compressed pages, retaining the same cold bytes as
    /// the wire representation. Validation uses one page-sized scratch buffer.
    pub(crate) fn from_blocks(
        words: u32,
        packed: Vec<u8>,
        blocks: Vec<CodeBlock>,
    ) -> Result<Arc<Self>, Error> {
        if blocks.len() != (words as usize).div_ceil(PAGE_WORDS) {
            return Err(encoding("execution code page count"));
        }
        let mut boundaries = Vec::new();
        boundaries
            .try_reserve_exact((words as usize).div_ceil(32))
            .map_err(|_| allocation())?;
        boundaries.resize((words as usize).div_ceil(32), 0u32);
        let mut scratch = bytes(if words == 0 { 0 } else { PAGE_BYTES })?;
        let mut offset = 0usize;
        let mut next = 0usize;
        for (page, block) in blocks.iter().enumerate() {
            if block.offset as usize != offset {
                return Err(encoding("execution code page offset"));
            }
            offset = offset
                .checked_add(block.len as usize)
                .ok_or_else(|| encoding("execution code page extent"))?;
            let source = packed
                .get(block.offset as usize..offset)
                .ok_or_else(|| encoding("execution code compressed extent"))?;
            let size = ((words as usize - page * PAGE_WORDS).min(PAGE_WORDS)) * 4;
            inflate(source, &mut scratch[..size])?;
            for (local, raw) in scratch[..size].as_chunks::<4>().0.iter().enumerate() {
                let pc = page * PAGE_WORDS + local;
                if pc == next {
                    boundaries[pc / 32] |= 1 << (pc % 32);
                    let head = u32::from_le_bytes(*raw);
                    next = pc
                        .checked_add(1 + ((head >> 8) & 3) as usize)
                        .ok_or_else(|| encoding("execution word extent"))?;
                }
            }
        }
        if offset != packed.len() || next != words as usize {
            return Err(encoding("execution code word extent"));
        }
        let mut pages = Vec::new();
        pages
            .try_reserve_exact(HOT_PAGES.min(blocks.len()))
            .map_err(|_| allocation())?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(blocks.len())
            .map_err(|_| allocation())?;
        slots.resize(blocks.len(), u32::MAX);
        Ok(Arc::new(Self {
            words,
            packed: packed.into_boxed_slice(),
            blocks: blocks.into_boxed_slice(),
            boundaries: boundaries.into_boxed_slice(),
            cache: Mutex::new(PageCache {
                pages,
                slots: slots.into_boxed_slice(),
                cursor: 0,
            }),
            residency: Arc::new(Residency::default()),
        }))
    }
    pub(crate) fn words_len(&self) -> u32 {
        self.words
    }
    pub(crate) fn words(self: &Arc<Self>, start: u32, len: u32) -> CodeWords {
        CodeWords {
            store: self.clone(),
            pc: start as usize,
            end: start as usize + len as usize,
            window: CodeWindow::default(),
        }
    }
    pub(crate) fn blocks(&self) -> &[CodeBlock] {
        &self.blocks
    }
    pub(crate) fn packed(&self) -> &[u8] {
        &self.packed
    }
    pub(crate) fn is_boundary(&self, pc: usize) -> bool {
        pc == self.words as usize
            || (pc < self.words as usize && self.boundaries[pc / 32] & (1 << (pc % 32)) != 0)
    }
    /// Actual retained storage, including pages still pinned by any reader.
    pub(crate) fn capacity_bytes(&self) -> usize {
        let stats = self.cache_stats();
        stats.cold_capacity_bytes + stats.resident_capacity_bytes
    }
    pub fn cache_stats(&self) -> CodeCacheStats {
        let pages = HOT_PAGES.min(self.blocks.len());
        let cold = size_of::<Self>()
            + 2 * size_of::<usize>()
            + self.packed.len()
            + self.blocks.len() * (size_of::<CodeBlock>() + 4)
            + self.boundaries.len() * 4
            + pages * size_of::<Option<Arc<CodePage>>>()
            + size_of::<Residency>()
            + 2 * size_of::<usize>();
        let hot = pages * (PAGE_BYTES + size_of::<CodePage>() + 2 * size_of::<usize>());
        let scratch = if self.words == 0 {
            0
        } else {
            Self::construction_scratch_bytes()
        };
        CodeCacheStats {
            cold_capacity_bytes: cold,
            resident_capacity_bytes: self.residency.current.load(Ordering::Relaxed),
            peak_resident_capacity_bytes: self.residency.peak.load(Ordering::Relaxed),
            hot_limit_bytes: hot,
            construction_scratch_bytes: scratch,
            capacity_ceiling_bytes: cold + hot + scratch,
        }
    }
    pub(crate) fn word(self: &Arc<Self>, pc: usize, window: &mut CodeWindow) -> Result<u32, Error> {
        self.word_budgeted(pc, window, &mut |_, _| true)?
            .ok_or_else(|| Error::new(ErrorKind::Limit, "execution code budget"))
    }
    pub(crate) fn word_budgeted(
        self: &Arc<Self>,
        pc: usize,
        window: &mut CodeWindow,
        admit: &mut impl FnMut(u64, u64) -> bool,
    ) -> Result<Option<u32>, Error> {
        if pc >= self.words as usize {
            return Err(Error::new(ErrorKind::Index, "execution code word"));
        }
        let page = pc / PAGE_WORDS;
        let matches = window
            .page
            .as_ref()
            .is_some_and(|current| current.index as usize == page)
            && window
                .store
                .as_ref()
                .is_some_and(|store| Arc::ptr_eq(store, self));
        if !matches {
            // Release our old pin before asking the bounded cache for a page.
            window.page = None;
            window.store = None;
            let Some(page) = self.page(page, admit)? else {
                return Ok(None);
            };
            window.page = Some(page);
            window.store = Some(self.clone());
        }
        Ok(Some(
            window
                .page
                .as_ref()
                .expect("loaded code page")
                .word(pc % PAGE_WORDS),
        ))
    }
    fn page(
        &self,
        index: usize,
        admit: &mut impl FnMut(u64, u64) -> bool,
    ) -> Result<Option<Arc<CodePage>>, Error> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| encoding("execution code cache lock"))?;
        if cache.slots[index] != u32::MAX
            && let Some(page) = &cache.pages[cache.slots[index] as usize]
        {
            return Ok(Some(page.clone()));
        }
        let block = self.blocks[index];
        let length = ((self.words as usize - index * PAGE_WORDS).min(PAGE_WORDS)) * 4;
        let allocation = length + size_of::<CodePage>() + 2 * size_of::<usize>();
        if !admit(
            (length + block.len as usize / 4 + 1) as u64,
            allocation as u64,
        ) {
            return Ok(None);
        }
        let limit = HOT_PAGES.min(self.blocks.len());
        let slot = if cache.pages.len() < limit {
            cache.pages.push(None);
            cache.pages.len() - 1
        } else {
            let mut available = None;
            for delta in 0..limit {
                let slot = (cache.cursor + delta) % limit;
                if cache.pages[slot]
                    .as_ref()
                    .is_none_or(|page| Arc::strong_count(page) == 1)
                {
                    available = Some(slot);
                    break;
                }
            }
            available.ok_or_else(|| {
                Error::new(ErrorKind::Limit, "execution code cache pages are pinned")
            })?
        };
        if let Some(previous) = cache.pages[slot].take() {
            cache.slots[previous.index as usize] = u32::MAX;
        }
        cache.cursor = (slot + 1) % limit;
        let mut output = bytes(length)?;
        inflate(
            &self.packed[block.offset as usize..block.offset as usize + block.len as usize],
            &mut output,
        )?;
        let page = CodePage {
            index: index as u32,
            bytes: output.into_boxed_slice(),
            residency: self.residency.clone(),
        };
        let current = self
            .residency
            .current
            .fetch_add(page.capacity_bytes(), Ordering::Relaxed)
            + page.capacity_bytes();
        self.residency.peak.fetch_max(current, Ordering::Relaxed);
        let page = Arc::new(page);
        cache.slots[index] = slot as u32;
        cache.pages[slot] = Some(page.clone());
        Ok(Some(page))
    }
}

fn inflate(source: &[u8], output: &mut [u8]) -> Result<(), Error> {
    use miniz_oxide::inflate::{
        TINFLStatus,
        core::{DecompressorOxide, decompress, inflate_flags},
    };
    let flags = inflate_flags::TINFL_FLAG_PARSE_ZLIB_HEADER
        | inflate_flags::TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
    let (status, consumed, written) =
        decompress(&mut DecompressorOxide::new(), source, output, 0, flags);
    if status != TINFLStatus::Done || consumed != source.len() || written != output.len() {
        return Err(encoding("invalid compressed execution code"));
    }
    Ok(())
}

/// One current page per VM or sequential reader. The hot hit only reads this
/// immutable page; it does not acquire a mutex or borrow an interior cell.
#[derive(Default, Debug)]
pub(crate) struct CodeWindow {
    store: Option<Arc<CodeStore>>,
    page: Option<Arc<CodePage>>,
}
impl CodeWindow {
    /// One identity/page check admits the maximum four-word instruction window.
    /// The final three words of a page use the fallible cross-page path.
    #[inline]
    pub(crate) fn hot_quad(&self, store: &Arc<CodeStore>, pc: usize) -> Option<&[u8; 16]> {
        let page = self.page.as_ref()?;
        if page.index as usize != pc / PAGE_WORDS || !Arc::ptr_eq(self.store.as_ref()?, store) {
            return None;
        }
        let start = (pc % PAGE_WORDS) * 4;
        page.bytes.get(start..start + 16)?.try_into().ok()
    }
}

pub(crate) struct CodeWords {
    store: Arc<CodeStore>,
    pc: usize,
    end: usize,
    window: CodeWindow,
}
impl Iterator for CodeWords {
    type Item = Result<u32, Error>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.pc == self.end {
            return None;
        }
        let word = self.store.word(self.pc, &mut self.window);
        self.pc += 1;
        Some(word)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.end - self.pc;
        (len, Some(len))
    }
}
impl ExactSizeIterator for CodeWords {}

#[derive(Default)]
pub(crate) struct CodeStoreBuilder {
    pending: Vec<u8>,
    packed: Vec<u8>,
    blocks: Vec<CodeBlock>,
    words: u32,
}
impl CodeStoreBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }
    pub(crate) fn append(
        &mut self,
        words: impl IntoIterator<Item = Result<u32, Error>>,
    ) -> Result<(), Error> {
        if self.pending.capacity() < PAGE_BYTES {
            self.pending
                .try_reserve_exact(PAGE_BYTES - self.pending.len())
                .map_err(|_| allocation())?;
        }
        for word in words {
            let word = word?;
            self.words = self
                .words
                .checked_add(1)
                .ok_or_else(|| Error::new(ErrorKind::Limit, "execution code word count"))?;
            self.pending.extend_from_slice(&word.to_le_bytes());
            if self.pending.len() == PAGE_BYTES {
                self.flush()?;
            }
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<(), Error> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let packed = miniz_oxide::deflate::compress_to_vec_zlib(&self.pending, 6);
        let offset = u32::try_from(self.packed.len())
            .map_err(|_| Error::new(ErrorKind::Limit, "compressed code offset"))?;
        let len = u32::try_from(packed.len())
            .map_err(|_| Error::new(ErrorKind::Limit, "compressed code page"))?;
        self.packed
            .try_reserve_exact(packed.len())
            .map_err(|_| allocation())?;
        self.blocks.try_reserve(1).map_err(|_| allocation())?;
        self.blocks.push(CodeBlock { offset, len });
        self.packed.extend(packed);
        self.pending.clear();
        Ok(())
    }
    pub(crate) fn words_len(&self) -> u32 {
        self.words
    }
    pub(crate) fn capacity_bytes(&self) -> usize {
        self.pending.capacity()
            + self.packed.capacity()
            + self.blocks.capacity() * size_of::<CodeBlock>()
            + size_of::<Self>()
            + if self.words == 0 {
                0
            } else {
                size_of::<miniz_oxide::deflate::core::CompressorOxide>() + PAGE_BYTES * 2
            }
    }
    pub(crate) fn finish(mut self) -> Result<Arc<CodeStore>, Error> {
        self.flush()?;
        CodeStore::from_blocks(self.words, self.packed, self.blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::{CompactCode, Instruction, Opcode};

    fn store(pages: usize) -> Arc<CodeStore> {
        let mut builder = CodeStoreBuilder::new();
        builder
            .append(std::iter::repeat_n(
                Ok(Opcode::Return as u32),
                pages * PAGE_WORDS,
            ))
            .unwrap();
        builder.finish().unwrap()
    }

    #[test]
    fn miss_is_prepaid_and_shared_hits_keep_the_same_page_ownership() {
        let code = store(2);
        assert_eq!(code.cache_stats().resident_capacity_bytes, 0);
        let mut first = CodeWindow::default();
        assert_eq!(
            code.word_budgeted(0, &mut first, &mut |_, _| false)
                .unwrap(),
            None
        );
        assert_eq!(code.cache_stats().resident_capacity_bytes, 0);
        let mut cost = (0, 0);
        assert_eq!(
            code.word_budgeted(0, &mut first, &mut |work, allocation| {
                cost = (work, allocation);
                true
            })
            .unwrap(),
            Some(Opcode::Return as u32)
        );
        assert!(cost.0 >= PAGE_BYTES as u64);
        assert!(cost.1 >= PAGE_BYTES as u64);
        let resident = code.cache_stats().resident_capacity_bytes;
        let mut second = CodeWindow::default();
        assert_eq!(
            code.word_budgeted(1, &mut second, &mut |_, _| panic!(
                "shared hot hit is covered by instruction work"
            ))
            .unwrap(),
            Some(Opcode::Return as u32)
        );
        assert_eq!(code.cache_stats().resident_capacity_bytes, resident);
        code.word_budgeted(2, &mut first, &mut |_, _| {
            panic!("hot window bypasses cache admission")
        })
        .unwrap();
    }

    #[test]
    fn pinned_windows_are_bounded_and_fallible_exports_preserve_errors() {
        let code = store(HOT_PAGES + 1);
        let mut windows = Vec::new();
        for index in 0..HOT_PAGES {
            let mut window = CodeWindow::default();
            code.word(index * PAGE_WORDS, &mut window).unwrap();
            windows.push(window);
        }
        let stats = code.cache_stats();
        assert_eq!(stats.resident_capacity_bytes, stats.hot_limit_bytes);
        let mut extra = CodeWindow::default();
        assert_eq!(
            code.word(HOT_PAGES * PAGE_WORDS, &mut extra)
                .unwrap_err()
                .kind,
            ErrorKind::Limit
        );
        let mut words = code.words((HOT_PAGES * PAGE_WORDS) as u32, 2);
        assert_eq!(words.len(), 2);
        assert_eq!(words.next().unwrap().unwrap_err().kind, ErrorKind::Limit);
        assert_eq!(words.len(), 1);
        assert_eq!(words.next().unwrap().unwrap_err().kind, ErrorKind::Limit);
        assert_eq!(words.len(), 0);
        drop(windows.remove(0));
        code.word(HOT_PAGES * PAGE_WORDS, &mut extra).unwrap();
        assert!(code.cache_stats().peak_resident_capacity_bytes <= stats.hot_limit_bytes);
    }

    #[test]
    fn a_compact_instruction_can_cross_the_compressed_page_boundary() {
        let mut raw = vec![Instruction::new(Opcode::Return, 0, 0, 0); PAGE_WORDS - 1];
        raw.push(Instruction::new(Opcode::Add, 1, 70000, 80000));
        raw.push(Instruction::new(Opcode::Return, 0, 0, 0));
        let (owned, pcs) = CompactCode::from_ordinal(&raw).unwrap();
        let mut builder = CodeStoreBuilder::new();
        builder.append(owned.words()).unwrap();
        let store = builder.finish().unwrap();
        let shared =
            CompactCode::from_store(store, 0, owned.end_pc() as u32, raw.len() as u32).unwrap();
        let pc = pcs[PAGE_WORDS - 1] as usize;
        assert_eq!(pc, PAGE_WORDS - 1);
        assert_eq!(
            shared
                .fetch_cached(pc, &mut CodeWindow::default())
                .unwrap()
                .0,
            raw[PAGE_WORDS - 1]
        );
        assert_eq!(shared.iter().collect::<Result<Vec<_>, _>>().unwrap(), raw);
    }

    #[test]
    fn corrupt_or_trailing_compressed_data_is_rejected_before_publication() {
        let code = store(1);
        let mut packed = code.packed.to_vec();
        let blocks = code.blocks.to_vec();
        packed[3] ^= 0x80;
        assert_eq!(
            CodeStore::from_blocks(code.words, packed, blocks)
                .unwrap_err()
                .kind,
            ErrorKind::Encoding
        );
        let mut packed = code.packed.to_vec();
        packed.push(0);
        let mut blocks = code.blocks.to_vec();
        blocks[0].len += 1;
        assert_eq!(
            CodeStore::from_blocks(code.words, packed, blocks)
                .unwrap_err()
                .kind,
            ErrorKind::Encoding
        );
    }

    #[test]
    fn poisoned_cache_is_a_fallible_read_and_keeps_iterator_length_honest() {
        let code = store(1);
        let thread = code.clone();
        let _ = std::thread::spawn(move || {
            let _guard = thread.cache.lock().unwrap();
            panic!("poison test");
        })
        .join();
        let mut words = code.words(0, 2);
        assert_eq!(words.next().unwrap().unwrap_err().kind, ErrorKind::Encoding);
        assert_eq!(words.len(), 1);
    }
}
