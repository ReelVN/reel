use super::code_residency::{CodeStore, CodeWindow};
use super::{Error, ErrorKind, Instruction, OperandKind, instruction_spec};
use std::sync::Arc;

/// PCs address entries in editable vectors and words in runtime code.
/// Code reads propagate storage failures; an iterator always yields one result
/// per declared instruction, including when a read fails.
pub trait InstructionSequence {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn end_pc(&self) -> usize {
        self.len()
    }
    fn is_boundary(&self, pc: usize) -> bool {
        pc <= self.end_pc()
    }
    fn instruction(&self, pc: usize) -> Result<Option<Instruction>, Error>;
    fn next_pc(&self, pc: usize) -> Result<Option<usize>, Error> {
        Ok((pc < self.end_pc()).then_some(pc + 1))
    }
    fn iter(&self) -> impl ExactSizeIterator<Item = Result<Instruction, Error>>;
    fn indexed_iter(&self) -> impl ExactSizeIterator<Item = Result<(usize, Instruction), Error>> {
        self.iter()
            .enumerate()
            .map(|(pc, value)| value.map(|value| (pc, value)))
    }
    /// Decode each instruction and its PC range in one sequential pass.
    fn ranged_iter(
        &self,
    ) -> impl ExactSizeIterator<Item = Result<(std::ops::Range<usize>, Instruction), Error>> {
        self.iter()
            .enumerate()
            .map(|(pc, value)| value.map(|value| (pc..pc + 1, value)))
    }
    fn previous_pc(&self, pc: usize) -> Option<usize> {
        (pc > 0 && pc <= self.end_pc()).then(|| pc - 1)
    }
    fn editable(&self) -> Option<&[Instruction]> {
        None
    }
    fn runtime_words(&self) -> Option<impl ExactSizeIterator<Item = Result<u32, Error>>> {
        None::<std::iter::Empty<Result<u32, Error>>>
    }
    fn runtime_store(&self) -> Option<&Arc<CodeStore>> {
        None
    }
    fn runtime_start(&self) -> u32 {
        0
    }
    /// Retained allocation capacity, excluding the inline storage handle.
    fn capacity_bytes(&self) -> usize;
}
impl InstructionSequence for Vec<Instruction> {
    fn len(&self) -> usize {
        Vec::len(self)
    }
    fn instruction(&self, pc: usize) -> Result<Option<Instruction>, Error> {
        Ok(self.get(pc).copied())
    }
    fn iter(&self) -> impl ExactSizeIterator<Item = Result<Instruction, Error>> {
        self.as_slice().iter().copied().map(Ok)
    }
    fn editable(&self) -> Option<&[Instruction]> {
        Some(self)
    }
    fn capacity_bytes(&self) -> usize {
        self.capacity() * size_of::<Instruction>()
    }
}
const FIRST_WIDE: u32 = (1 << 21) - 1;
const PACKED_BC: u32 = 1 << 10;

/// Runtime instructions addressed by word PC. Verified functions reference one
/// program-shared compressed store and its boundary bitmap. Conversion can hold
/// temporary owned words before publishing the common store.
#[derive(Clone, Debug)]
enum CodeData {
    Owned(Box<[u32]>),
    Shared {
        store: Arc<CodeStore>,
        start: u32,
        count: u32,
    },
}
#[derive(Clone, Debug)]
pub struct CompactCode {
    data: CodeData,
    words: u32,
}

/// Inspect executable instructions already in memory. Successive reads share
/// one cache window. A missing page ends the query without allocating or
/// evicting execution pages on behalf of an optional consumer.
#[derive(Default)]
pub struct ResidentCodeReader {
    window: CodeWindow,
}

impl ResidentCodeReader {
    /// Return the instruction and its following word PC, when resident.
    pub fn read(
        &mut self,
        code: &CompactCode,
        pc: usize,
    ) -> Result<Option<(Instruction, usize)>, Error> {
        if pc >= code.end_pc() || !code.is_boundary(pc) {
            return Ok(None);
        }
        code.fetch_budgeted(pc, &mut self.window, &mut |_, _| false)
    }
}
impl Default for CompactCode {
    fn default() -> Self {
        Self {
            data: CodeData::Owned(Box::new([])),
            words: 0,
        }
    }
}
impl PartialEq for CompactCode {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.words().eq(other.words())
    }
}
impl Eq for CompactCode {}
impl CompactCode {
    pub(crate) fn from_store(
        store: Arc<CodeStore>,
        start: u32,
        words: u32,
        count: u32,
    ) -> Result<Self, Error> {
        let end = start
            .checked_add(words)
            .ok_or_else(|| Error::new(ErrorKind::Index, "execution function range"))?;
        if end > store.words_len()
            || !store.is_boundary(start as usize)
            || !store.is_boundary(end as usize)
        {
            return Err(Error::new(
                ErrorKind::Index,
                "execution function boundaries",
            ));
        }
        Ok(Self {
            data: CodeData::Shared {
                store,
                start,
                count,
            },
            words,
        })
    }
    pub(crate) fn store(&self) -> Option<&Arc<CodeStore>> {
        match &self.data {
            CodeData::Shared { store, .. } => Some(store),
            CodeData::Owned(_) => None,
        }
    }
    pub(crate) fn start(&self) -> u32 {
        match &self.data {
            CodeData::Shared { start, .. } => *start,
            CodeData::Owned(_) => 0,
        }
    }
    pub(crate) fn shape(i: &Instruction) -> Result<(u32, bool, bool, usize), Error> {
        let spec = instruction_spec(i.opcode)
            .ok_or_else(|| Error::new(ErrorKind::Opcode, "compact opcode"))?;
        let [a, b, c] = i.operands;
        let wide = a >= FIRST_WIDE || spec.operands[0] == OperandKind::Pc;
        let pc_pair = spec.operands[1..].contains(&OperandKind::Pc);
        let packed = !pc_pair && c != 0 && b <= u16::MAX.into() && c <= u16::MAX.into();
        let pair = if pc_pair {
            2
        } else if packed {
            1
        } else if c != 0 {
            2
        } else {
            usize::from(b != 0)
        };
        let tail = usize::from(wide) + pair;
        let head = u32::from(i.opcode)
            | (tail as u32) << 8
            | (if packed { PACKED_BC } else { 0 })
            | (if wide { FIRST_WIDE } else { a }) << 11;
        Ok((head, wide, packed, tail))
    }
    fn allocation_words(words: usize) -> usize {
        if words == 0 {
            0
        } else {
            words + 1 + words.div_ceil(32)
        }
    }
    pub(crate) fn planned_bytes(code: &[Instruction]) -> Result<usize, Error> {
        let words = code.iter().try_fold(0usize, |sum, i| {
            sum.checked_add(1 + Self::shape(i)?.3)
                .ok_or_else(|| Error::new(ErrorKind::Limit, "compact word count"))
        })?;
        Self::allocation_words(words)
            .checked_mul(4)
            .ok_or_else(|| Error::new(ErrorKind::Limit, "compact allocation"))
    }
    /// Fix PC operand widths before mapping so rewritten targets cannot change layout.
    pub(crate) fn from_ordinal(code: &[Instruction]) -> Result<(Self, Vec<u32>), Error> {
        let mut pcs = Vec::new();
        pcs.try_reserve_exact(code.len() + 1)
            .map_err(|_| Error::new(ErrorKind::Allocation, "PC mapping"))?;
        let mut words = 0usize;
        for i in code {
            pcs.push(u32::try_from(words).map_err(|_| Error::new(ErrorKind::Limit, "word PC"))?);
            words += 1 + Self::shape(i)?.3;
        }
        pcs.push(u32::try_from(words).map_err(|_| Error::new(ErrorKind::Limit, "word PC"))?);
        let mut data = Vec::new();
        data.try_reserve_exact(Self::allocation_words(words))
            .map_err(|_| Error::new(ErrorKind::Allocation, "compact code"))?;
        for i in code {
            let mut i = *i;
            for (kind, value) in instruction_spec(i.opcode)
                .expect("checked opcode")
                .operands
                .iter()
                .zip(&mut i.operands)
            {
                if *kind == OperandKind::Pc {
                    *value = *pcs
                        .get(*value as usize)
                        .ok_or_else(|| Error::new(ErrorKind::Index, "ordinal PC"))?;
                }
            }
            let (head, wide, packed, tail) = Self::shape(&i)?;
            data.push(head);
            if wide {
                data.push(i.operands[0]);
            }
            if packed {
                data.push(i.operands[1] | i.operands[2] << 16);
            } else if tail - usize::from(wide) == 2 {
                data.extend_from_slice(&i.operands[1..]);
            } else if tail - usize::from(wide) == 1 {
                data.push(i.operands[1]);
            }
        }
        debug_assert_eq!(data.len(), words);
        Self::finish(data, code.len(), &pcs[..code.len()]).map(|code| (code, pcs))
    }
    fn finish(mut data: Vec<u32>, count: usize, pcs: &[u32]) -> Result<Self, Error> {
        let words = data.len();
        if words == 0 {
            return Ok(Self::default());
        }
        data.resize(Self::allocation_words(words), 0);
        data[words] =
            u32::try_from(count).map_err(|_| Error::new(ErrorKind::Limit, "instruction count"))?;
        for &pc in pcs {
            data[words + 1 + pc as usize / 32] |= 1 << (pc % 32);
        }
        Ok(Self {
            data: CodeData::Owned(data.into_boxed_slice()),
            words: u32::try_from(words).map_err(|_| Error::new(ErrorKind::Limit, "word count"))?,
        })
    }
    /// Validate wire headers without constructing an expanded instruction vector.
    #[cfg(test)]
    pub(crate) fn scan_words(
        words: usize,
        mut get: impl FnMut(usize) -> u32,
    ) -> Result<usize, Error> {
        Self::scan_words_checked(words, &mut get, |_| Ok(()))
    }
    #[cfg(test)]
    pub(crate) fn scan_words_checked(
        words: usize,
        mut get: impl FnMut(usize) -> u32,
        mut check: impl FnMut(&Instruction) -> Result<(), Error>,
    ) -> Result<usize, Error> {
        Self::try_scan_words_checked(words, |pc| Ok(get(pc)), &mut check)
    }
    pub(crate) fn try_scan_words_checked(
        words: usize,
        mut get: impl FnMut(usize) -> Result<u32, Error>,
        mut check: impl FnMut(&Instruction) -> Result<(), Error>,
    ) -> Result<usize, Error> {
        let mut pc = 0;
        let mut count = 0;
        while pc < words {
            let (i, next) = Self::parse_at(pc, words, &mut get)?;
            let (head, _, _, tail) = Self::shape(&i)?;
            if head != get(pc)? || next != pc + 1 + tail {
                return Err(Error::new(
                    ErrorKind::Encoding,
                    "noncanonical compact instruction",
                ));
            }
            for (kind, value) in instruction_spec(i.opcode)
                .expect("checked opcode")
                .operands
                .iter()
                .zip(i.operands)
            {
                if *kind == OperandKind::Unused && value != 0 {
                    return Err(Error::new(ErrorKind::Encoding, "unused compact operand"));
                }
            }
            check(&i)?;
            pc = next;
            count += 1;
        }
        Ok(count)
    }
    fn parse_at(
        pc: usize,
        words: usize,
        get: &mut impl FnMut(usize) -> Result<u32, Error>,
    ) -> Result<(Instruction, usize), Error> {
        let head = get(pc)?;
        let tail = ((head >> 8) & 3) as usize;
        let end = pc
            .checked_add(1 + tail)
            .ok_or_else(|| Error::new(ErrorKind::Limit, "instruction word extent"))?;
        if end > words {
            return Err(Error::new(ErrorKind::Truncated, "compact instruction tail"));
        }
        let mut values = [head >> 11, 0, 0];
        let mut cursor = pc + 1;
        if values[0] == FIRST_WIDE {
            if cursor == end {
                return Err(Error::new(ErrorKind::Encoding, "wide first operand"));
            }
            values[0] = get(cursor)?;
            cursor += 1;
        }
        if head & PACKED_BC != 0 {
            if cursor + 1 != end {
                return Err(Error::new(ErrorKind::Encoding, "packed operand tail"));
            }
            let pair = get(cursor)?;
            values[1] = pair & 0xffff;
            values[2] = pair >> 16;
        } else {
            if end - cursor > 2 {
                return Err(Error::new(ErrorKind::Encoding, "compact operand tail"));
            }
            if cursor < end {
                values[1] = get(cursor)?;
            }
            if cursor + 1 < end {
                values[2] = get(cursor + 1)?;
            }
        }
        Ok((
            Instruction {
                opcode: (head & 0xff) as u16,
                flags: 0,
                operands: values,
            },
            end,
        ))
    }
    /// Fetch at an admitted runtime PC. No boundary-table load is needed here.
    #[inline]
    pub(crate) fn fetch_cached(
        &self,
        pc: usize,
        window: &mut CodeWindow,
    ) -> Result<(Instruction, usize), Error> {
        if matches!(self.data, CodeData::Shared { .. }) {
            return self
                .fetch_budgeted(pc, window, &mut |_, _| true)?
                .ok_or_else(|| Error::new(ErrorKind::Limit, "execution code budget"));
        }
        let mut read = |pc| -> Result<u32, Error> {
            match &self.data {
                CodeData::Owned(data) => data
                    .get(pc)
                    .copied()
                    .ok_or_else(|| Error::new(ErrorKind::Index, "execution word")),
                CodeData::Shared { store, start, .. } => store.word(*start as usize + pc, window),
            }
        };
        let head = read(pc)?;
        let mut cursor = pc + 1;
        let end = cursor + ((head >> 8) & 3) as usize;
        let mut values = [head >> 11, 0, 0];
        if values[0] == FIRST_WIDE {
            values[0] = read(cursor)?;
            cursor += 1;
        }
        if head & PACKED_BC != 0 {
            let pair = read(cursor)?;
            values[1] = pair & 0xffff;
            values[2] = pair >> 16;
        } else {
            if cursor < end {
                values[1] = read(cursor)?;
            }
            if cursor + 1 < end {
                values[2] = read(cursor + 1)?;
            }
        }
        Ok((
            Instruction {
                opcode: (head & 0xff) as u16,
                flags: 0,
                operands: values,
            },
            end,
        ))
    }
    pub(crate) fn fetch(&self, pc: usize) -> Result<(Instruction, usize), Error> {
        self.fetch_cached(pc, &mut CodeWindow::default())
    }
    #[inline]
    pub(crate) fn fetch_budgeted(
        &self,
        pc: usize,
        window: &mut CodeWindow,
        admit: &mut impl FnMut(u64, u64) -> bool,
    ) -> Result<Option<(Instruction, usize)>, Error> {
        let CodeData::Shared { store, start, .. } = &self.data else {
            return self.fetch_cached(pc, window).map(Some);
        };
        let global = *start as usize + pc;
        if let Some(words) = window.hot_quad(store, global) {
            return Ok(Some(Self::decode_quad(words, pc)));
        }
        macro_rules! read {
            ($pc:expr) => {
                match store.word_budgeted(*start as usize + $pc, window, admit)? {
                    Some(word) => word,
                    None => return Ok(None),
                }
            };
        }
        let head = read!(pc);
        if let Some(words) = window.hot_quad(store, global) {
            return Ok(Some(Self::decode_quad(words, pc)));
        }
        let mut cursor = pc + 1;
        let end = cursor + ((head >> 8) & 3) as usize;
        let mut values = [head >> 11, 0, 0];
        if values[0] == FIRST_WIDE {
            values[0] = read!(cursor);
            cursor += 1;
        }
        if head & PACKED_BC != 0 {
            let pair = read!(cursor);
            values[1] = pair & 0xffff;
            values[2] = pair >> 16;
        } else {
            if cursor < end {
                values[1] = read!(cursor);
            }
            if cursor + 1 < end {
                values[2] = read!(cursor + 1);
            }
        }
        Ok(Some((
            Instruction {
                opcode: (head & 0xff) as u16,
                flags: 0,
                operands: values,
            },
            end,
        )))
    }
    #[cfg(any(test, feature = "vm-validation-oracle"))]
    pub(crate) fn resident_range(
        &self,
        window: &CodeWindow,
        start_pc: usize,
        end_pc: usize,
    ) -> bool {
        if start_pc >= end_pc || end_pc > self.end_pc() {
            return false;
        }
        match &self.data {
            CodeData::Owned(_) => true,
            CodeData::Shared { store, start, .. } => {
                window.contains(store, *start as usize + start_pc, *start as usize + end_pc)
            }
        }
    }
    #[inline]
    fn decode_quad(raw: &[u8; 16], pc: usize) -> (Instruction, usize) {
        let head = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        let first = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]);
        let second = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]);
        let third = u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]);
        let tail = ((head >> 8) & 3) as usize;
        let wide = head >> 11 == FIRST_WIDE;
        let (a, pair, last) = if wide {
            (first, second, third)
        } else {
            (head >> 11, first, second)
        };
        let (b, c) = if head & PACKED_BC != 0 {
            (pair & 0xffff, pair >> 16)
        } else {
            (
                if tail > usize::from(wide) { pair } else { 0 },
                if tail > usize::from(wide) + 1 {
                    last
                } else {
                    0
                },
            )
        };
        (
            Instruction {
                opcode: (head & 0xff) as u16,
                flags: 0,
                operands: [a, b, c],
            },
            pc + 1 + tail,
        )
    }
    #[inline]
    pub fn len(&self) -> usize {
        match &self.data {
            CodeData::Owned(data) => {
                if self.words == 0 {
                    0
                } else {
                    data[self.words as usize] as usize
                }
            }
            CodeData::Shared { count, .. } => *count as usize,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.words == 0
    }
    #[inline]
    pub fn end_pc(&self) -> usize {
        self.words as usize
    }
    pub fn is_boundary(&self, pc: usize) -> bool {
        pc == self.end_pc()
            || (pc < self.end_pc()
                && match &self.data {
                    CodeData::Owned(data) => {
                        data[self.end_pc() + 1 + pc / 32] & (1 << (pc % 32)) != 0
                    }
                    CodeData::Shared { store, start, .. } => {
                        store.is_boundary(*start as usize + pc)
                    }
                })
    }
    pub fn instruction(&self, pc: usize) -> Result<Option<Instruction>, Error> {
        if pc >= self.end_pc() || !self.is_boundary(pc) {
            return Ok(None);
        }
        self.fetch(pc).map(|value| Some(value.0))
    }
    pub fn previous_pc(&self, pc: usize) -> Option<usize> {
        if pc == 0 || pc > self.end_pc() {
            return None;
        }
        let mut p = pc - 1;
        while !self.is_boundary(p) {
            p -= 1;
        }
        Some(p)
    }
    pub fn next_pc(&self, pc: usize) -> Result<Option<usize>, Error> {
        if pc >= self.end_pc() || !self.is_boundary(pc) {
            return Ok(None);
        }
        self.fetch(pc).map(|value| Some(value.1))
    }
    pub fn iter(&self) -> CompactIter<'_> {
        CompactIter {
            code: self,
            front: 0,
            back: self.end_pc(),
            remaining: self.len(),
            window: CodeWindow::default(),
        }
    }
    pub fn indexed_iter(
        &self,
    ) -> impl ExactSizeIterator<Item = Result<(usize, Instruction), Error>> {
        CompactIndexed(self.iter())
    }
    pub fn words(&self) -> impl ExactSizeIterator<Item = Result<u32, Error>> {
        enum Words<'a> {
            Owned(std::iter::Copied<std::slice::Iter<'a, u32>>),
            Shared(super::code_residency::CodeWords),
        }
        impl Iterator for Words<'_> {
            type Item = Result<u32, Error>;
            fn next(&mut self) -> Option<Self::Item> {
                match self {
                    Self::Owned(values) => values.next().map(Ok),
                    Self::Shared(values) => values.next(),
                }
            }
            fn size_hint(&self) -> (usize, Option<usize>) {
                match self {
                    Self::Owned(values) => values.size_hint(),
                    Self::Shared(values) => values.size_hint(),
                }
            }
        }
        impl ExactSizeIterator for Words<'_> {}
        match &self.data {
            CodeData::Owned(data) => Words::Owned(data[..self.end_pc()].iter().copied()),
            CodeData::Shared { store, start, .. } => Words::Shared(store.words(*start, self.words)),
        }
    }
    pub fn capacity_bytes(&self) -> usize {
        match &self.data {
            CodeData::Owned(data) => data.len() * 4,
            CodeData::Shared { .. } => 0,
        }
    }
}
impl InstructionSequence for CompactCode {
    fn len(&self) -> usize {
        self.len()
    }
    fn end_pc(&self) -> usize {
        self.end_pc()
    }
    fn is_boundary(&self, pc: usize) -> bool {
        self.is_boundary(pc)
    }
    fn instruction(&self, pc: usize) -> Result<Option<Instruction>, Error> {
        self.instruction(pc)
    }
    fn next_pc(&self, pc: usize) -> Result<Option<usize>, Error> {
        self.next_pc(pc)
    }
    fn iter(&self) -> impl ExactSizeIterator<Item = Result<Instruction, Error>> {
        self.iter()
    }
    fn indexed_iter(&self) -> impl ExactSizeIterator<Item = Result<(usize, Instruction), Error>> {
        self.indexed_iter()
    }
    fn ranged_iter(
        &self,
    ) -> impl ExactSizeIterator<Item = Result<(std::ops::Range<usize>, Instruction), Error>> {
        let mut cursor = self.iter();
        (0..self.len()).map(move |_| {
            let start = cursor.front;
            cursor
                .next()
                .expect("declared instruction count")
                .map(|value| (start..cursor.front, value))
        })
    }
    fn previous_pc(&self, pc: usize) -> Option<usize> {
        self.previous_pc(pc)
    }
    fn runtime_words(&self) -> Option<impl ExactSizeIterator<Item = Result<u32, Error>>> {
        Some(self.words())
    }
    fn runtime_store(&self) -> Option<&Arc<CodeStore>> {
        self.store()
    }
    fn runtime_start(&self) -> u32 {
        self.start()
    }
    fn capacity_bytes(&self) -> usize {
        self.capacity_bytes()
    }
}
/// Sequential execution words, with bounded reverse boundary lookup for tools.
pub struct CompactIter<'a> {
    code: &'a CompactCode,
    front: usize,
    back: usize,
    remaining: usize,
    window: CodeWindow,
}
impl Iterator for CompactIter<'_> {
    type Item = Result<Instruction, Error>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let result = self.code.fetch_cached(self.front, &mut self.window);
        self.remaining -= 1;
        Some(result.map(|(i, next)| {
            self.front = next;
            i
        }))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl ExactSizeIterator for CompactIter<'_> {}
impl DoubleEndedIterator for CompactIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.back -= 1;
        while !self.code.is_boundary(self.back) {
            self.back -= 1;
        }
        self.remaining -= 1;
        Some(
            self.code
                .fetch_cached(self.back, &mut self.window)
                .map(|value| value.0),
        )
    }
}
struct CompactIndexed<'a>(CompactIter<'a>);
impl Iterator for CompactIndexed<'_> {
    type Item = Result<(usize, Instruction), Error>;
    fn next(&mut self) -> Option<Self::Item> {
        let pc = self.0.front;
        self.0.next().map(|i| i.map(|i| (pc, i)))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}
impl ExactSizeIterator for CompactIndexed<'_> {}
impl<'a> IntoIterator for &'a CompactCode {
    type Item = Result<Instruction, Error>;
    type IntoIter = CompactIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Rebase every metadata PC with the same map used for branch operands.
pub(crate) fn map_metadata_pcs(
    regions: &mut [super::ExceptionRegion],
    source: &mut [super::SourceMapEntry],
    mut map: impl FnMut(super::Pc) -> Result<super::Pc, Error>,
) -> Result<(), Error> {
    fn range(
        range: &mut super::PcRange,
        map: &mut impl FnMut(super::Pc) -> Result<super::Pc, Error>,
    ) -> Result<(), Error> {
        range.start = map(range.start)?;
        range.end = map(range.end)?;
        Ok(())
    }
    for region in regions {
        range(&mut region.protected, &mut map)?;
        if let Some(catch) = &mut region.catch {
            range(&mut catch.range, &mut map)?;
        }
        if let Some(finally) = &mut region.finally {
            range(finally, &mut map)?;
        }
    }
    for entry in source {
        entry.pc = map(entry.pc)?;
    }
    Ok(())
}

impl super::RuntimePackage {
    pub(crate) fn to_unpacked(&self) -> Result<super::BytecodePackage, Error> {
        use super::*;
        let mut functions = Vec::new();
        functions
            .try_reserve_exact(self.functions.len())
            .map_err(|_| Error::new(ErrorKind::Allocation, "editable functions"))?;
        for f in &self.functions {
            let mut pcs: Vec<u32> = Vec::new();
            pcs.try_reserve_exact(f.code.len() + 1)
                .map_err(|_| Error::new(ErrorKind::Allocation, "editable PC map"))?;
            for item in f.code.indexed_iter() {
                pcs.push(item?.0 as u32);
            }
            pcs.push(f.code.end_pc() as u32);
            let map = |pc: Pc| -> Result<Pc, Error> {
                pcs.binary_search(&pc.0)
                    .map(|index| Pc(index as u32))
                    .map_err(|_| Error::new(ErrorKind::Index, "word PC editing boundary"))
            };
            let mut code = Vec::new();
            code.try_reserve_exact(f.code.len())
                .map_err(|_| Error::new(ErrorKind::Allocation, "editable instructions"))?;
            for i in f.code.iter() {
                let mut i = i?;
                for (kind, value) in instruction_spec(i.opcode)
                    .ok_or_else(|| Error::new(ErrorKind::Opcode, "execution opcode"))?
                    .operands
                    .iter()
                    .zip(&mut i.operands)
                {
                    if *kind == OperandKind::Pc {
                        *value = map(Pc(*value))?.0;
                    }
                }
                code.push(i);
            }
            let mut regions = f.regions.clone();
            let mut source_map = Vec::new();
            source_map
                .try_reserve_exact(f.source_map.len())
                .map_err(|_| Error::new(ErrorKind::Allocation, "editable source runs"))?;
            source_map.extend(f.source_map.iter());
            map_metadata_pcs(&mut regions, &mut source_map, map)?;
            functions.push(FunctionDef {
                module: f.module,
                name: f.name,
                kind: f.kind,
                signature: f.signature,
                context: f.context,
                captures: f.captures.clone(),
                register_count: f.register_count,
                bindings: f.bindings.clone(),
                descriptors: f.descriptors.to_vec(),
                code,
                regions,
                source_map,
            });
        }
        Ok(BytecodePackage {
            format_version: self.format_version,
            language_version: self.language_version,
            host_abi_version: self.host_abi_version,
            content_version: self.content_version,
            text_pool: self.text_pool,
            symbols: self.symbols.clone(),
            types: self.types.clone(),
            constants: self.constants.clone(),
            signatures: self.signatures.clone(),
            natives: self.natives.clone(),
            modules: self.modules.clone(),
            functions,
            capabilities: self.capabilities.clone(),
            sources: self.sources.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::Opcode;
    #[test]
    fn words_preserve_zero_pairs_wide_values_and_word_pc_boundaries() {
        let values = [
            0,
            1,
            u16::MAX as u32,
            1 << 16,
            FIRST_WIDE - 1,
            FIRST_WIDE,
            u32::MAX,
        ];
        let mut code = Vec::new();
        for a in values {
            for b in values {
                for c in values {
                    code.push(Instruction::new(Opcode::Add, a, b, c));
                }
            }
        }
        let (compact, pcs) = CompactCode::from_ordinal(&code).unwrap();
        assert_eq!(
            compact.capacity_bytes(),
            CompactCode::planned_bytes(&code).unwrap()
        );
        assert_eq!(compact.iter().collect::<Result<Vec<_>, _>>().unwrap(), code);
        let mut builder = super::super::code_residency::CodeStoreBuilder::new();
        builder.append(compact.words()).unwrap();
        let store = builder.finish().unwrap();
        let shared =
            CompactCode::from_store(store, 0, compact.end_pc() as u32, code.len() as u32).unwrap();
        let mut window = CodeWindow::default();
        for (i, &pc) in pcs[..code.len()].iter().enumerate() {
            assert_eq!(
                shared.fetch_cached(pc as usize, &mut window).unwrap().0,
                code[i]
            );
        }
        for (i, &pc) in pcs[..code.len()].iter().enumerate().rev() {
            assert_eq!(compact.instruction(pc as usize).unwrap(), Some(code[i]));
            assert_eq!(
                compact.next_pc(pc as usize).unwrap(),
                Some(pcs[i + 1] as usize)
            );
        }
        for pc in 0..compact.end_pc() {
            assert_eq!(
                compact.is_boundary(pc),
                pcs.binary_search(&(pc as u32)).is_ok()
            );
        }
        assert_eq!(compact.instruction(compact.end_pc()).unwrap(), None);
        assert_eq!(compact.instruction(usize::MAX).unwrap(), None);
        let words = compact.words().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(
            CompactCode::scan_words(compact.end_pc(), |pc| words[pc]).unwrap(),
            code.len()
        );
    }
    #[test]
    fn pc_targets_have_stable_width_and_no_runtime_index() {
        assert_eq!(size_of::<CompactCode>(), 32);
        let raw = vec![
            Instruction::new(Opcode::BranchBool, 1, 1, 2),
            Instruction::new(Opcode::Jump, 2, 0, 0),
            Instruction::new(Opcode::Return, 0, 0, 0),
        ];
        let (code, pcs) = CompactCode::from_ordinal(&raw).unwrap();
        assert_eq!(pcs, vec![0, 3, 5, 6]);
        assert_eq!(code.instruction(0).unwrap().unwrap().operands, [1, 3, 5]);
        assert_eq!(code.instruction(3).unwrap().unwrap().operands, [5, 0, 0]);
        assert_eq!(code.capacity_bytes(), (6 + 1 + 1) * 4);
        assert_eq!(code.instruction(1).unwrap(), None);
    }
}
