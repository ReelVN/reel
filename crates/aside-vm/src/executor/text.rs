//! Interpolation appends once into one rooted buffer, then seals without copying.
use super::*;
use std::fmt::Write;

#[derive(Clone, Copy, Default)]
pub(super) struct Utf8 {
    remaining: u8,
    scalar: u32,
    minimum: u32,
}
impl Utf8 {
    fn push(&mut self, byte: u8) -> bool {
        if self.remaining == 0 {
            match byte {
                0..=0x7f => return true,
                0xc2..=0xdf => {
                    self.remaining = 1;
                    self.scalar = u32::from(byte & 0x1f);
                    self.minimum = 0x80;
                }
                0xe0..=0xef => {
                    self.remaining = 2;
                    self.scalar = u32::from(byte & 0xf);
                    self.minimum = 0x800;
                }
                0xf0..=0xf4 => {
                    self.remaining = 3;
                    self.scalar = u32::from(byte & 7);
                    self.minimum = 0x10000;
                }
                _ => return false,
            }
        } else {
            if byte & 0xc0 != 0x80 {
                return false;
            }
            self.scalar = (self.scalar << 6) | u32::from(byte & 0x3f);
            self.remaining -= 1;
            if self.remaining == 0
                && (self.scalar < self.minimum || char::from_u32(self.scalar).is_none())
            {
                return false;
            }
        }
        true
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
    fn new(value: Value) -> Result<Self, FaultKind> {
        let mut text = Self {
            bytes: [0; 327],
            len: 0,
        };
        let result = match value.tag() {
            ValueTag::Null => text.write_str("null"),
            ValueTag::Bool => write!(text, "{}", value.as_bool().unwrap()),
            ValueTag::Int => write!(text, "{}", value.as_int().unwrap()),
            ValueTag::Float => write!(text, "{}", value.as_float().unwrap()),
            _ => return Err(FaultKind::Type),
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
            if utf8.remaining != 0 {
                self.collection_fault(s, FaultKind::Type)?;
            } else {
                self.heap.seal_string(value)?;
                self.write(*s, dest, value)
                    .map_err(|_| VmError::Contract("text result register"))?;
                s.pc.0 += 1;
                s.phase = ScriptPhase::Body;
            }
            return Ok(true);
        }
        self.text_scratch.data.resize(count as usize, 0);
        self.heap
            .read_bytes(value, offset, &mut self.text_scratch.data)?;
        let valid = self.text_scratch.data.iter().all(|&byte| utf8.push(byte));
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
        let input_value = self.slots.read(at(s.regs, input.0))?;
        let is_string = self.is_text(input_value, ObjectKind::String);
        let limit = self.text_scratch.data.capacity() as u32;
        let remaining = if is_string {
            self.heap.info(input_value)?.len - offset
        } else {
            327 - offset
        };
        let bound = remaining.min(limit);
        let old_len = u64::from(self.heap.info(buffer)?.len);
        let extra = (old_len + u64::from(bound)).div_ceil(BYTE_CHUNK_LEN as u64)
            - old_len.div_ceil(BYTE_CHUNK_LEN as u64);
        let work = u64::from(bound) * 2 + 1 + if is_string { 0 } else { 327 };
        if !meter.take(0, work, extra * BYTE_CHUNK_BYTES) {
            return Ok(false);
        }
        let scalar = if is_string {
            None
        } else {
            match ScalarText::new(input_value) {
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
            s.pc.0 += 1;
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
