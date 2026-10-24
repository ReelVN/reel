//! Interpolation appends once into one rooted buffer, then seals without copying.
use super::*;
use std::fmt::Write;

/// Carries only the incomplete scalar between chunks; std owns UTF-8 rules.
#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Utf8 {
    tail: [u8; 4],
    len: u8,
}
impl Utf8 {
    pub(super) fn complete(self) -> bool {
        self.len == 0
    }

    pub(super) fn feed(
        &mut self,
        mut input: &[u8],
        mut visit: impl FnMut(&str),
    ) -> Result<(), FaultKind> {
        while self.len != 0 && !input.is_empty() {
            self.tail[self.len as usize] = input[0];
            self.len += 1;
            input = &input[1..];
            match std::str::from_utf8(&self.tail[..self.len as usize]) {
                Ok(text) => {
                    visit(text);
                    self.len = 0;
                }
                Err(error) if error.error_len().is_none() => {}
                Err(_) => return Err(FaultKind::Type),
            }
        }
        if self.len != 0 {
            return Ok(());
        }
        match std::str::from_utf8(input) {
            Ok(text) => visit(text),
            Err(error) if error.error_len().is_none() => {
                let (valid, tail) = input.split_at(error.valid_up_to());
                visit(std::str::from_utf8(valid).map_err(|_| FaultKind::Type)?);
                self.tail[..tail.len()].copy_from_slice(tail);
                self.len = tail.len() as u8;
            }
            Err(_) => return Err(FaultKind::Type),
        }
        Ok(())
    }
}

// Decimal Display of any f64 fits in 327 bytes, including sign and decimal point.
struct ScalarText {
    bytes: [u8; 327],
    len: usize,
}
impl Write for ScalarText {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let end = self.len + text.len();
        self.bytes
            .get_mut(self.len..end)
            .ok_or(std::fmt::Error)?
            .copy_from_slice(text.as_bytes());
        self.len = end;
        Ok(())
    }
}
impl ScalarText {
    fn new(value: Value, message: Option<&str>) -> Result<Self, FaultKind> {
        let mut text = Self {
            bytes: [0; 327],
            len: 0,
        };
        let result = if let Some(message) = message {
            text.write_str(message)
        } else {
            match value.tag() {
                ValueTag::Null => text.write_str("null"),
                ValueTag::Bool => write!(text, "{}", value.as_bool().unwrap()),
                ValueTag::Int => write!(text, "{}", value.as_int().unwrap()),
                ValueTag::Float => write!(text, "{}", value.as_float().unwrap()),
                _ => return Err(FaultKind::Type),
            }
        };
        result.map_err(|_| FaultKind::InvalidState)?;
        Ok(text)
    }
}

impl Vm {
    pub(super) fn text_finish_step(
        &mut self,
        s: &mut ScriptFrame,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let ScriptPhase::TextFinish {
            dest,
            input,
            offset,
            version,
            mut utf8,
        } = s.phase
        else {
            unreachable!()
        };
        let value = self.slots.read(at(s.regs, input.0))?;
        let info = self.heap.info(value)?;
        let count = (info.len - offset).min(self.text_scratch.data.capacity() as u32);
        if !meter.take(0, u64::from(count) * 2 + 1, 0) {
            return Ok(false);
        }
        if info.content_version != version || info.kind != ObjectKind::ByteBuffer {
            self.collection_fault(s, FaultKind::ConcurrentModification)?;
            return Ok(true);
        }
        if count == 0 {
            if !utf8.complete() {
                self.collection_fault(s, FaultKind::Type)?;
            } else {
                self.heap.seal_string(value)?;
                self.write(*s, dest, value)
                    .map_err(|_| VmError::Contract("text result register"))?;
                s.pc = s.instruction_end;
                s.phase = ScriptPhase::Body;
            }
            return Ok(true);
        }
        self.text_scratch.data.resize(count as usize, 0);
        self.heap
            .read_bytes(value, offset, &mut self.text_scratch.data)?;
        let valid = utf8.feed(&self.text_scratch.data, |_| {}).is_ok();
        self.text_scratch.data.clear();
        if !valid {
            self.collection_fault(s, FaultKind::Type)?;
        } else {
            s.phase = ScriptPhase::TextFinish {
                dest,
                input,
                offset: offset + count,
                version,
                utf8,
            };
        }
        Ok(true)
    }
    pub(super) fn text_push_step(
        &mut self,
        s: &mut ScriptFrame,
        builder: Reg,
        input: Reg,
        offset: u32,
        meter: &mut Meter,
    ) -> Result<bool, VmError> {
        let buffer = self.slots.read(at(s.regs, builder.0))?;
        let mut input_value = self.slots.read(at(s.regs, input.0))?;
        let kind = if input_value.tag() == ValueTag::Object {
            Some(self.heap.info(input_value)?.kind)
        } else {
            None
        };
        let mut message = None;
        let is_string = if kind == Some(ObjectKind::CoreError) {
            let error = self.heap.core_error_info(input_value)?;
            if self.is_text(error.details, ObjectKind::String) {
                input_value = error.details;
                true
            } else {
                message = Some(error.message);
                false
            }
        } else {
            kind == Some(ObjectKind::String)
        };
        let limit = self.text_scratch.data.capacity() as u32;
        let remaining = if is_string {
            self.heap.info(input_value)?.len - offset
        } else {
            327 - offset
        };
        let bound = remaining.min(limit);
        let old_len = u64::from(self.heap.info(buffer)?.len);
        let extra = Bytes::growth_bytes(old_len, bound.into());
        let work = u64::from(bound) * 2 + 1 + if is_string { 0 } else { 327 };
        if !meter.take(0, work, extra) {
            return Ok(false);
        }
        let scalar = if is_string {
            None
        } else {
            match ScalarText::new(input_value, message) {
                Ok(text) => Some(text),
                Err(kind) => {
                    self.collection_fault(s, kind)?;
                    return Ok(true);
                }
            }
        };
        let len = if let Some(scalar) = &scalar {
            scalar.len as u32
        } else {
            self.heap.info(input_value)?.len
        };
        let count = (len - offset).min(limit);
        if count == 0 {
            s.pc = s.instruction_end;
            s.phase = ScriptPhase::Body;
            return Ok(true);
        }
        self.text_scratch.data.resize(count as usize, 0);
        let result = if let Some(scalar) = &scalar {
            self.text_scratch
                .data
                .copy_from_slice(&scalar.bytes[offset as usize..(offset + count) as usize]);
            Ok(())
        } else {
            self.heap
                .read_bytes(input_value, offset, &mut self.text_scratch.data)
                .map(|_| ())
        }
        .and_then(|_| self.heap.bytes_append(buffer, &self.text_scratch.data));
        self.text_scratch.data.clear();
        match result {
            Ok(()) => {
                s.phase = ScriptPhase::TextPush {
                    builder,
                    input,
                    offset: offset + count,
                }
            }
            Err(error) => self.collection_fault(s, heap_fault(error))?,
        }
        Ok(true)
    }
}
