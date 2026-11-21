//! Immutable text operations retain cursors, never a second whole-string copy.
use super::*;
use crate::executor::text::Utf8;
mod find;
mod split;
use find::FindWork;
use split::SplitWork;

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(in crate::executor) struct TextWork {
    call: BuiltinCall,
    mode: TextMode,
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
enum TextMode {
    Scan {
        offset: u32,
        utf8: Utf8,
        scalars: u32,
        bytes: u32,
        begin: u32,
        range: Option<(u32, u32)>,
    },
    Copy {
        offset: u32,
        end: u32,
    },
    Compare {
        offset: u32,
        base: u32,
        len: u32,
    },
    Join {
        index: u32,
        offset: u32,
        separator: bool,
        len: u32,
        version: u64,
    },
    Trim {
        offset: u32,
        utf8: Utf8,
        bytes: u32,
        begin: Option<u32>,
        end: u32,
    },
    Split(SplitWork),
    Find(FindWork),
}

impl Vm {
    pub(super) fn start_text_builtin(
        &mut self,
        s: &mut ScriptFrame,
        call: BuiltinCall,
    ) -> Result<(), FaultKind> {
        let receiver = self.value(s.env, 1).map_err(vm_fault)?;
        let len = self.heap.info(receiver).map_err(heap_fault)?.len;
        let argument = || self.builtin_argument(s, call, 0).map_err(vm_fault);
        let mode = match call.method {
            Builtin::Split | Builtin::Replace => {
                let separator = argument()?;
                if !self.is_text(separator, ObjectKind::String) {
                    return Err(FaultKind::Type);
                }
                if call.method == Builtin::Replace
                    && !self.is_text(
                        self.builtin_argument(s, call, 1).map_err(vm_fault)?,
                        ObjectKind::String,
                    )
                {
                    return Err(FaultKind::Type);
                }
                if s.effects.0 & Effects::ALLOCATE == 0 {
                    return Err(FaultKind::Effect);
                }
                let result = if call.method == Builtin::Split {
                    self.heap.alloc_array(&[]).map_err(heap_fault)?
                } else {
                    self.heap.alloc_byte_buffer(&[]).map_err(heap_fault)?
                };
                let prefix = self.heap.alloc_array(&[]).map_err(heap_fault)?;
                let roots = self
                    .heap
                    .alloc_array(&[result, prefix, Value::NULL])
                    .map_err(heap_fault)?;
                self.put(s.env, 2, roots).map_err(vm_fault)?;
                TextMode::Split(SplitWork::start(
                    self.heap.info(separator).map_err(heap_fault)?.len,
                ))
            }
            Builtin::Trim | Builtin::TrimStart | Builtin::TrimEnd => {
                if s.effects.0 & Effects::ALLOCATE == 0 {
                    return Err(FaultKind::Effect);
                }
                TextMode::Trim {
                    offset: 0,
                    utf8: Utf8::default(),
                    bytes: 0,
                    begin: None,
                    end: 0,
                }
            }
            Builtin::AsciiLowercase | Builtin::AsciiUppercase => TextMode::Copy {
                offset: 0,
                end: len,
            },
            Builtin::ScalarCount | Builtin::SliceScalars => TextMode::Scan {
                offset: 0,
                utf8: Utf8::default(),
                scalars: 0,
                bytes: 0,
                begin: 0,
                range: if matches!(call.method, Builtin::SliceScalars) {
                    Some(self.range_bounds(argument()?)?)
                } else {
                    None
                },
            },
            Builtin::SliceBytes => {
                let (begin, end) = self.range_bounds(argument()?)?;
                if end > len
                    || !self.string_boundary(receiver, begin)?
                    || !self.string_boundary(receiver, end)?
                {
                    return Err(FaultKind::OutOfBounds);
                }
                TextMode::Copy { offset: begin, end }
            }
            Builtin::StartsWith
            | Builtin::EndsWith
            | Builtin::StripPrefix
            | Builtin::StripSuffix => {
                let other = argument()?;
                if !self.is_text(other, ObjectKind::String) {
                    return Err(FaultKind::Type);
                }
                let other_len = self.heap.info(other).map_err(heap_fault)?.len;
                if other_len > len {
                    let result = match call.method {
                        Builtin::StripPrefix | Builtin::StripSuffix => Value::NULL,
                        _ => Value::bool(false),
                    };
                    return self.builtin_finish(s, call, Ok(result)).map_err(vm_fault);
                }
                TextMode::Compare {
                    offset: 0,
                    base: if matches!(call.method, Builtin::EndsWith | Builtin::StripSuffix) {
                        len - other_len
                    } else {
                        0
                    },
                    len: other_len,
                }
            }
            Builtin::Join => {
                let items = argument()?;
                let len = self.heap.sequence_len(items).map_err(heap_fault)?;
                let version = self.heap.info(items).map_err(heap_fault)?.content_version;
                TextMode::Join {
                    index: 0,
                    offset: 0,
                    separator: false,
                    len,
                    version,
                }
            }
            Builtin::StringContains | Builtin::StringFind => {
                let pattern = argument()?;
                if !self.is_text(pattern, ObjectKind::String) {
                    return Err(FaultKind::Type);
                }
                let pattern_len = self.heap.info(pattern).map_err(heap_fault)?.len;
                if pattern_len == 0 || pattern_len > len {
                    return self.finish_find(s, call, (pattern_len == 0).then_some(0));
                }
                TextMode::Find(FindWork::Start)
            }
            _ => unreachable!("text method dispatch"),
        };
        if matches!(
            call.method,
            Builtin::SliceBytes
                | Builtin::SliceScalars
                | Builtin::Join
                | Builtin::AsciiLowercase
                | Builtin::AsciiUppercase
        ) {
            if s.effects.0 & Effects::ALLOCATE == 0 {
                return Err(FaultKind::Effect);
            }
            let buffer = self.heap.alloc_byte_buffer(&[]).map_err(heap_fault)?;
            self.put(s.env, 2, buffer).map_err(vm_fault)?;
        }
        s.phase = ScriptPhase::TextBuiltin(TextWork { call, mode });
        Ok(())
    }

    fn string_boundary(&self, value: Value, index: u32) -> Result<bool, FaultKind> {
        // Valid strings can only be split before a leading byte or at the end.
        let segment = self.heap.byte_segment(value, index).map_err(heap_fault)?;
        Ok(segment.first().is_none_or(|b| b & 0xc0 != 0x80))
    }

    pub(in crate::executor) fn text_builtin_step(
        &mut self,
        s: &mut ScriptFrame,
        work: TextWork,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        match self.advance_text(s, work, meter) {
            Ok(progress) => Ok(progress),
            Err(kind) => {
                self.collection_fault(s, kind)?;
                Ok(true)
            }
        }
    }

    fn finish_text(&mut self, s: &mut ScriptFrame, call: BuiltinCall) -> Result<(), FaultKind> {
        let buffer = self.value(s.env, 2).map_err(vm_fault)?;
        self.heap.seal_string(buffer).map_err(heap_fault)?;
        self.builtin_finish(s, call, Ok(buffer)).map_err(vm_fault)
    }

    fn advance_text(
        &mut self,
        s: &mut ScriptFrame,
        mut work: TextWork,
        meter: &mut Meter,
    ) -> Result<bool, FaultKind> {
        let receiver = self.value(s.env, 1).map_err(vm_fault)?;
        work.mode = match work.mode {
            TextMode::Split(state) => return self.split_text_step(s, work, state, meter),
            TextMode::Find(state) => return self.find_text_step(s, work, state, meter),
            TextMode::Scan {
                offset,
                mut utf8,
                mut scalars,
                mut bytes,
                mut begin,
                range,
            } => {
                let segment = self
                    .heap
                    .byte_segment(receiver, offset)
                    .map_err(heap_fault)?;
                let count = segment.len() as u32;
                if !meter.take(0, u64::from(count) * 2 + 1, 0) {
                    return Ok(false);
                }
                utf8.feed(segment, |text| {
                    if let Some((start, end)) = range {
                        for ch in text.chars() {
                            if scalars == end {
                                break;
                            }
                            bytes += ch.len_utf8() as u32;
                            scalars += 1;
                            if scalars == start {
                                begin = bytes;
                            }
                        }
                    } else {
                        scalars += text.chars().count() as u32;
                    }
                })?;
                if let Some((_, end)) = range {
                    if scalars == end {
                        TextMode::Copy {
                            offset: begin,
                            end: bytes,
                        }
                    } else if count == 0 {
                        return Err(FaultKind::OutOfBounds);
                    } else {
                        TextMode::Scan {
                            offset: offset + count,
                            utf8,
                            scalars,
                            bytes,
                            begin,
                            range,
                        }
                    }
                } else if count == 0 {
                    self.builtin_finish(s, work.call, Ok(Value::int(i64::from(scalars))))
                        .map_err(vm_fault)?;
                    return Ok(true);
                } else {
                    TextMode::Scan {
                        offset: offset + count,
                        utf8,
                        scalars,
                        bytes,
                        begin,
                        range,
                    }
                }
            }
            TextMode::Copy { offset, end } => {
                let buffer = self.value(s.env, 2).map_err(vm_fault)?;
                let Some(count) =
                    self.copy_text_part(buffer, receiver, offset..end, work.call.method, meter)?
                else {
                    return Ok(false);
                };
                if count == 0 {
                    self.finish_text(s, work.call)?;
                    return Ok(true);
                }
                TextMode::Copy {
                    offset: offset + count,
                    end,
                }
            }
            TextMode::Compare { offset, base, len } => {
                let other = self.builtin_argument(s, work.call, 0).map_err(vm_fault)?;
                let left = self
                    .heap
                    .byte_segment(receiver, base + offset)
                    .map_err(heap_fault)?;
                let right = self.heap.byte_segment(other, offset).map_err(heap_fault)?;
                let count = left.len().min(right.len()).min((len - offset) as usize);
                if !meter.take(0, count as u64 * 2 + 1, 0) {
                    return Ok(false);
                }
                let equal = left[..count] == right[..count];
                let next = offset + count as u32;
                if equal && next < len {
                    TextMode::Compare {
                        offset: next,
                        base,
                        len,
                    }
                } else if matches!(
                    work.call.method,
                    Builtin::StripPrefix | Builtin::StripSuffix
                ) {
                    if !equal || len == 0 {
                        let result = if equal { receiver } else { Value::NULL };
                        self.builtin_finish(s, work.call, Ok(result))
                            .map_err(vm_fault)?;
                        return Ok(true);
                    }
                    if s.effects.0 & Effects::ALLOCATE == 0 {
                        return Err(FaultKind::Effect);
                    }
                    let (begin, end) = if work.call.method == Builtin::StripSuffix {
                        (0, base)
                    } else {
                        (len, self.heap.info(receiver).map_err(heap_fault)?.len)
                    };
                    let buffer = self.heap.alloc_byte_buffer(&[]).map_err(heap_fault)?;
                    self.put(s.env, 2, buffer).map_err(vm_fault)?;
                    TextMode::Copy { offset: begin, end }
                } else {
                    self.builtin_finish(s, work.call, Ok(Value::bool(equal)))
                        .map_err(vm_fault)?;
                    return Ok(true);
                }
            }
            TextMode::Join {
                mut index,
                offset,
                separator,
                len,
                version,
            } => {
                let items = self.builtin_argument(s, work.call, 0).map_err(vm_fault)?;
                if self.heap.info(items).map_err(heap_fault)?.content_version != version {
                    if !meter.take(0, 1, 0) {
                        return Ok(false);
                    }
                    return Err(FaultKind::ConcurrentModification);
                }
                if index == len {
                    if !meter.take(0, 1, 0) {
                        return Ok(false);
                    }
                    self.finish_text(s, work.call)?;
                    return Ok(true);
                }
                let source = if separator {
                    receiver
                } else {
                    self.heap.sequence_get(items, index).map_err(heap_fault)?
                };
                if !self.is_text(source, ObjectKind::String) {
                    if !meter.take(0, 1, 0) {
                        return Ok(false);
                    }
                    return Err(FaultKind::Type);
                }
                let end = self.heap.info(source).map_err(heap_fault)?.len;
                let buffer = self.value(s.env, 2).map_err(vm_fault)?;
                let Some(count) =
                    self.copy_text_part(buffer, source, offset..end, work.call.method, meter)?
                else {
                    return Ok(false);
                };
                if count == 0 {
                    if !separator {
                        index += 1;
                    }
                    TextMode::Join {
                        index,
                        offset: 0,
                        separator: !separator,
                        len,
                        version,
                    }
                } else {
                    TextMode::Join {
                        index,
                        offset: offset + count,
                        separator,
                        len,
                        version,
                    }
                }
            }
            TextMode::Trim {
                offset,
                mut utf8,
                mut bytes,
                mut begin,
                mut end,
            } => {
                let segment = self
                    .heap
                    .byte_segment(receiver, offset)
                    .map_err(heap_fault)?;
                let count = segment.len() as u32;
                if !meter.take(0, u64::from(count) * 2 + 1, 0) {
                    return Ok(false);
                }
                let start_only = matches!(work.call.method, Builtin::TrimStart);
                utf8.feed(segment, |text| {
                    for ch in text.chars() {
                        let position = bytes;
                        bytes += ch.len_utf8() as u32;
                        if !ch.is_whitespace() {
                            begin.get_or_insert(position);
                            end = bytes;
                            if start_only {
                                break;
                            }
                        }
                    }
                })?;
                if count == 0 || start_only && begin.is_some() {
                    let len = self.heap.info(receiver).map_err(heap_fault)?.len;
                    let begin = if matches!(work.call.method, Builtin::TrimEnd) {
                        0
                    } else {
                        begin.unwrap_or(len)
                    };
                    let end = if start_only || begin == len { len } else { end };
                    if begin == 0 && end == len {
                        self.builtin_finish(s, work.call, Ok(receiver))
                            .map_err(vm_fault)?;
                        return Ok(true);
                    }
                    let buffer = self.heap.alloc_byte_buffer(&[]).map_err(heap_fault)?;
                    self.put(s.env, 2, buffer).map_err(vm_fault)?;
                    TextMode::Copy { offset: begin, end }
                } else {
                    TextMode::Trim {
                        offset: offset + count,
                        utf8,
                        bytes,
                        begin,
                        end,
                    }
                }
            }
        };
        s.phase = ScriptPhase::TextBuiltin(work);
        Ok(true)
    }

    fn copy_text_part(
        &mut self,
        buffer: Value,
        source: Value,
        range: std::ops::Range<u32>,
        method: Builtin,
        meter: &mut Meter,
    ) -> Result<Option<u32>, FaultKind> {
        let count = (range.end - range.start).min(self.text_scratch.data.capacity() as u32);
        let old_len = u64::from(self.heap.info(buffer).map_err(heap_fault)?.len);
        let bytes = Bytes::growth_bytes(old_len, count.into());
        if !meter.take(0, u64::from(count) * 3 + 1, bytes) {
            return Ok(None);
        }
        self.text_scratch.data.resize(count as usize, 0);
        let result = self
            .heap
            .read_bytes(source, range.start, &mut self.text_scratch.data)
            .and_then(|_| {
                match method {
                    Builtin::AsciiLowercase => self.text_scratch.data.make_ascii_lowercase(),
                    Builtin::AsciiUppercase => self.text_scratch.data.make_ascii_uppercase(),
                    _ => {}
                }
                self.heap.bytes_append(buffer, &self.text_scratch.data)
            });
        self.text_scratch.data.clear();
        result.map_err(heap_fault)?;
        Ok(Some(count))
    }
}
