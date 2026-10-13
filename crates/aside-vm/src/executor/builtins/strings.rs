//! Immutable text operations retain cursors, never a second whole-string copy.
use super::*;
use crate::executor::text::Utf8;

#[derive(Clone, Copy)]
pub(in crate::executor) struct TextWork {
    call: BuiltinCall,
    mode: TextMode,
}

#[derive(Clone, Copy)]
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
            Builtin::StartsWith | Builtin::EndsWith => {
                let other = argument()?;
                if !self.is_text(other, ObjectKind::String) {
                    return Err(FaultKind::Type);
                }
                let other_len = self.heap.info(other).map_err(heap_fault)?.len;
                if other_len > len {
                    return self
                        .builtin_finish(s, call, Ok(Value::bool(false)))
                        .map_err(vm_fault);
                }
                TextMode::Compare {
                    offset: 0,
                    base: if matches!(call.method, Builtin::EndsWith) {
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
            _ => unreachable!("text method dispatch"),
        };
        if matches!(
            call.method,
            Builtin::SliceBytes | Builtin::SliceScalars | Builtin::Join
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
                let Some(count) = self.copy_text_part(buffer, receiver, offset, end, meter)? else {
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
                if !equal || offset + count as u32 == len {
                    self.builtin_finish(s, work.call, Ok(Value::bool(equal)))
                        .map_err(vm_fault)?;
                    return Ok(true);
                }
                TextMode::Compare {
                    offset: offset + count as u32,
                    base,
                    len,
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
                let Some(count) = self.copy_text_part(buffer, source, offset, end, meter)? else {
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
        };
        s.phase = ScriptPhase::TextBuiltin(work);
        Ok(true)
    }

    fn copy_text_part(
        &mut self,
        buffer: Value,
        source: Value,
        offset: u32,
        end: u32,
        meter: &mut Meter,
    ) -> Result<Option<u32>, FaultKind> {
        let count = (end - offset).min(self.text_scratch.data.capacity() as u32);
        let old_len = u64::from(self.heap.info(buffer).map_err(heap_fault)?.len);
        let chunks = (old_len + u64::from(count)).div_ceil(BYTE_CHUNK_LEN as u64)
            - old_len.div_ceil(BYTE_CHUNK_LEN as u64);
        if !meter.take(0, u64::from(count) * 2 + 1, chunks * BYTE_CHUNK_BYTES) {
            return Ok(None);
        }
        self.text_scratch.data.resize(count as usize, 0);
        let result = self
            .heap
            .read_bytes(source, offset, &mut self.text_scratch.data)
            .and_then(|_| self.heap.bytes_append(buffer, &self.text_scratch.data));
        self.text_scratch.data.clear();
        result.map_err(heap_fault)?;
        Ok(Some(count))
    }
}
