//! Literal lookup keeps only cursors and, for chunked text, KMP prefix data.
use super::*;

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(super) enum FindWork {
    Start,
    Prefix { index: u32, matched: u32 },
    Scan { offset: u32, matched: u32 },
    Count { offset: u32, end: u32, scalars: u32 },
}

impl Vm {
    pub(super) fn finish_find(
        &mut self,
        s: &mut ScriptFrame,
        call: BuiltinCall,
        index: Option<u32>,
    ) -> Result<(), FaultKind> {
        let result = if call.method == Builtin::StringContains {
            Value::bool(index.is_some())
        } else {
            index.map_or(Value::NULL, |index| Value::int(i64::from(index)))
        };
        self.builtin_finish(s, call, Ok(result)).map_err(vm_fault)
    }

    pub(super) fn find_text_step(
        &mut self,
        s: &mut ScriptFrame,
        mut work: TextWork,
        state: FindWork,
        meter: &mut Meter,
    ) -> Result<bool, FaultKind> {
        let source = self.value(s.env, 1).map_err(vm_fault)?;
        let len = self.heap.info(source).map_err(heap_fault)?.len;
        let pattern = self.builtin_argument(s, work.call, 0).map_err(vm_fault)?;
        let pattern_len = self.heap.info(pattern).map_err(heap_fault)?.len;
        let next = match state {
            FindWork::Start => {
                let source_bytes = self.heap.byte_segment(source, 0).map_err(heap_fault)?;
                let pattern_bytes = self.heap.byte_segment(pattern, 0).map_err(heap_fault)?;
                if source_bytes.len() == len as usize && pattern_bytes.len() == pattern_len as usize
                {
                    let cost = u64::from(len) * 3 + u64::from(pattern_len) * 2 + 1;
                    if !meter.take(0, cost, 0) {
                        return Ok(false);
                    }
                    let source = std::str::from_utf8(source_bytes).map_err(|_| FaultKind::Type)?;
                    let pattern =
                        std::str::from_utf8(pattern_bytes).map_err(|_| FaultKind::Type)?;
                    let index = source.find(pattern).map(|start| {
                        if work.call.method == Builtin::StringFind {
                            source[..start].chars().count() as u32
                        } else {
                            0
                        }
                    });
                    self.finish_find(s, work.call, index)?;
                    return Ok(true);
                }
                if !meter.take(0, 1, 0) {
                    return Ok(false);
                }
                if pattern_len == 1 {
                    FindWork::Scan {
                        offset: 0,
                        matched: 0,
                    }
                } else {
                    if s.effects.0 & Effects::ALLOCATE == 0 {
                        return Err(FaultKind::Effect);
                    }
                    // env2 roots the prefix table across execution slices.
                    let prefix = self.heap.alloc_array(&[]).map_err(heap_fault)?;
                    self.put(s.env, 2, prefix).map_err(vm_fault)?;
                    FindWork::Prefix {
                        index: 0,
                        matched: 0,
                    }
                }
            }
            FindWork::Prefix { index, matched } => {
                let prefix = self.value(s.env, 2).map_err(vm_fault)?;
                let Some((index, matched)) =
                    self.literal_prefix_step(pattern, prefix, index, matched, meter)?
                else {
                    return Ok(false);
                };
                if index == pattern_len {
                    FindWork::Scan {
                        offset: 0,
                        matched: 0,
                    }
                } else {
                    FindWork::Prefix { index, matched }
                }
            }
            FindWork::Scan { offset, matched } => {
                if offset == len {
                    if !meter.take(0, 1, 0) {
                        return Ok(false);
                    }
                    self.finish_find(s, work.call, None)?;
                    return Ok(true);
                }
                let (offset, matched) = if pattern_len == 1 {
                    let segment = self.heap.byte_segment(source, offset).map_err(heap_fault)?;
                    if !meter.take(0, segment.len() as u64 + 1, 0) {
                        return Ok(false);
                    }
                    let byte = self.heap.byte_segment(pattern, 0).map_err(heap_fault)?[0];
                    segment
                        .iter()
                        .position(|&candidate| candidate == byte)
                        .map_or((offset + segment.len() as u32, 0), |position| {
                            (offset + position as u32 + 1, 1)
                        })
                } else {
                    if !meter.take(0, 4, 0) {
                        return Ok(false);
                    }
                    self.literal_scan_step(
                        source,
                        pattern,
                        || self.value(s.env, 2).map_err(vm_fault),
                        offset,
                        matched,
                    )?
                };
                if matched != pattern_len {
                    FindWork::Scan { offset, matched }
                } else {
                    let start = offset - pattern_len;
                    if work.call.method == Builtin::StringContains || start == 0 {
                        self.finish_find(s, work.call, Some(0))?;
                        return Ok(true);
                    }
                    self.clear(s.env, 2).map_err(vm_fault)?;
                    FindWork::Count {
                        offset: 0,
                        end: start,
                        scalars: 0,
                    }
                }
            }
            FindWork::Count {
                offset,
                end,
                scalars,
            } => {
                let segment = self.heap.byte_segment(source, offset).map_err(heap_fault)?;
                let count = segment.len().min((end - offset) as usize);
                if !meter.take(0, count as u64 + 1, 0) {
                    return Ok(false);
                }
                // Every non-continuation byte starts a scalar in a valid string.
                let scalars = scalars
                    + segment[..count]
                        .iter()
                        .filter(|&&byte| byte & 0xc0 != 0x80)
                        .count() as u32;
                let offset = offset + count as u32;
                if offset == end {
                    self.finish_find(s, work.call, Some(scalars))?;
                    return Ok(true);
                }
                FindWork::Count {
                    offset,
                    end,
                    scalars,
                }
            }
        };
        work.mode = TextMode::Find(next);
        s.phase = ScriptPhase::TextBuiltin(work);
        Ok(true)
    }
}
