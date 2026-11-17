//! Streaming literal splitting over the VM's chunked UTF-8 storage.
use super::*;

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(super) enum SplitWork {
    Prefix {
        index: u32,
        matched: u32,
    },
    Scan {
        offset: u32,
        matched: u32,
        begin: u32,
    },
    Empty {
        offset: u32,
        leading: bool,
    },
    Emit {
        offset: u32,
        end: u32,
        resume: Resume,
    },
    Insert {
        offset: u32,
        resume: Resume,
    },
}

#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(super) enum Resume {
    Scan(u32),
    Empty(u32),
    Done,
}

impl Resume {
    fn next(self) -> Option<SplitWork> {
        Some(match self {
            Self::Scan(offset) => SplitWork::Scan {
                offset,
                matched: 0,
                begin: offset,
            },
            Self::Empty(offset) => SplitWork::Empty {
                offset,
                leading: false,
            },
            Self::Done => return None,
        })
    }
}

impl SplitWork {
    pub(super) fn start(separator_len: u32) -> Self {
        match separator_len {
            0 => Self::Empty {
                offset: 0,
                leading: true,
            },
            1 => Self::Scan {
                offset: 0,
                matched: 0,
                begin: 0,
            },
            _ => Self::Prefix {
                index: 0,
                matched: 0,
            },
        }
    }
}

// All three roots live in the existing text-work slot. No raw Value is kept in
// the serialized cursor, and failed/cancelled work uses normal frame cleanup.
const RESULT: u32 = 0;
const PREFIX: u32 = 1;
const FRAGMENT: u32 = 2;

impl Vm {
    pub(super) fn split_text_step(
        &mut self,
        s: &mut ScriptFrame,
        mut work: TextWork,
        state: SplitWork,
        meter: &mut Meter,
    ) -> Result<bool, FaultKind> {
        let source = self.value(s.env, 1).map_err(vm_fault)?;
        let roots = self.value(s.env, 2).map_err(vm_fault)?;
        let separator = self.builtin_argument(s, work.call, 0).map_err(vm_fault)?;
        let replace = work.call.method == Builtin::Replace;
        let len = self.heap.info(source).map_err(heap_fault)?.len;
        let separator_len = self.heap.info(separator).map_err(heap_fault)?.len;
        let next = match state {
            SplitWork::Prefix { index, mut matched } => {
                let prefix = self.heap.get_field(roots, PREFIX).map_err(heap_fault)?;
                // KMP's prefix table prevents repeated partial matches from
                // turning long, repetitive inputs into quadratic work.
                let extra = Values::growth_bytes(index.into(), 1);
                if !meter.take(0, 4, extra) {
                    return Ok(false);
                }
                if index != 0 {
                    let byte = self
                        .heap
                        .byte_segment(separator, index)
                        .map_err(heap_fault)?[0];
                    let candidate = self
                        .heap
                        .byte_segment(separator, matched)
                        .map_err(heap_fault)?[0];
                    if byte == candidate {
                        matched += 1;
                    } else if matched != 0 {
                        let matched = self
                            .heap
                            .sequence_get(prefix, matched - 1)
                            .map_err(heap_fault)?
                            .as_int()
                            .unwrap() as u32;
                        work.mode = TextMode::Split(SplitWork::Prefix { index, matched });
                        s.phase = ScriptPhase::TextBuiltin(work);
                        return Ok(true);
                    }
                }
                self.heap
                    .array_push(prefix, Value::int(i64::from(matched)))
                    .map_err(heap_fault)?;
                if index + 1 == separator_len {
                    SplitWork::Scan {
                        offset: 0,
                        matched: 0,
                        begin: 0,
                    }
                } else {
                    SplitWork::Prefix {
                        index: index + 1,
                        matched,
                    }
                }
            }
            SplitWork::Scan {
                mut offset,
                mut matched,
                begin,
            } => {
                if !meter.take(0, 4, 0) {
                    return Ok(false);
                }
                if offset == len {
                    if !replace {
                        self.split_fragment(roots)?;
                    }
                    SplitWork::Emit {
                        offset: begin,
                        end: len,
                        resume: Resume::Done,
                    }
                } else {
                    let byte = self.heap.byte_segment(source, offset).map_err(heap_fault)?[0];
                    let candidate = self
                        .heap
                        .byte_segment(separator, matched)
                        .map_err(heap_fault)?[0];
                    if byte == candidate {
                        matched += 1;
                        offset += 1;
                    } else if matched != 0 {
                        let prefix = self.heap.get_field(roots, PREFIX).map_err(heap_fault)?;
                        matched = self
                            .heap
                            .sequence_get(prefix, matched - 1)
                            .map_err(heap_fault)?
                            .as_int()
                            .unwrap() as u32;
                    } else {
                        offset += 1;
                    }
                    if matched == separator_len {
                        if !replace {
                            self.split_fragment(roots)?;
                        }
                        SplitWork::Emit {
                            offset: begin,
                            end: offset - separator_len,
                            resume: Resume::Scan(offset),
                        }
                    } else {
                        SplitWork::Scan {
                            offset,
                            matched,
                            begin,
                        }
                    }
                }
            }
            SplitWork::Empty { offset, leading } => {
                if !meter.take(0, 4, 0) {
                    return Ok(false);
                }
                let (end, resume) = if leading {
                    (offset, Resume::Empty(offset))
                } else if offset == len {
                    (offset, Resume::Done)
                } else {
                    let byte = self.heap.byte_segment(source, offset).map_err(heap_fault)?[0];
                    let width = if byte < 0x80 {
                        1
                    } else if byte < 0xe0 {
                        2
                    } else if byte < 0xf0 {
                        3
                    } else {
                        4
                    };
                    (offset + width, Resume::Empty(offset + width))
                };
                if !replace {
                    self.split_fragment(roots)?;
                }
                SplitWork::Emit {
                    offset,
                    end,
                    resume,
                }
            }
            SplitWork::Emit {
                offset,
                end,
                resume,
            } => {
                let result = self.heap.get_field(roots, RESULT).map_err(heap_fault)?;
                let fragment = if replace {
                    result
                } else {
                    self.heap.get_field(roots, FRAGMENT).map_err(heap_fault)?
                };
                if offset != end {
                    let Some(count) = self.copy_text_part(
                        fragment,
                        source,
                        offset..end,
                        work.call.method,
                        meter,
                    )?
                    else {
                        return Ok(false);
                    };
                    work.mode = TextMode::Split(SplitWork::Emit {
                        offset: offset + count,
                        end,
                        resume,
                    });
                    s.phase = ScriptPhase::TextBuiltin(work);
                    return Ok(true);
                }
                if replace {
                    if !meter.take(0, 1, 0) {
                        return Ok(false);
                    }
                    if resume.next().is_none() {
                        self.heap.seal_string(result).map_err(heap_fault)?;
                        self.builtin_finish(s, work.call, Ok(result))
                            .map_err(vm_fault)?;
                        return Ok(true);
                    }
                    work.mode = TextMode::Split(SplitWork::Insert { offset: 0, resume });
                    s.phase = ScriptPhase::TextBuiltin(work);
                    return Ok(true);
                }
                let extra =
                    Values::growth_bytes(self.heap.info(result).map_err(heap_fault)?.len.into(), 1);
                if !meter.take(0, 3, extra) {
                    return Ok(false);
                }
                self.heap.seal_string(fragment).map_err(heap_fault)?;
                self.heap.array_push(result, fragment).map_err(heap_fault)?;
                self.heap
                    .set_field(roots, FRAGMENT, Value::NULL)
                    .map_err(heap_fault)?;
                let Some(next) = resume.next() else {
                    self.builtin_finish(s, work.call, Ok(result))
                        .map_err(vm_fault)?;
                    return Ok(true);
                };
                next
            }
            SplitWork::Insert { offset, resume } => {
                let replacement = self.builtin_argument(s, work.call, 1).map_err(vm_fault)?;
                let end = self.heap.info(replacement).map_err(heap_fault)?.len;
                if offset == end {
                    if !meter.take(0, 1, 0) {
                        return Ok(false);
                    }
                    resume
                        .next()
                        .expect("replacement inserted between fragments")
                } else {
                    let result = self.heap.get_field(roots, RESULT).map_err(heap_fault)?;
                    let Some(count) = self.copy_text_part(
                        result,
                        replacement,
                        offset..end,
                        Builtin::Replace,
                        meter,
                    )?
                    else {
                        return Ok(false);
                    };
                    SplitWork::Insert {
                        offset: offset + count,
                        resume,
                    }
                }
            }
        };
        work.mode = TextMode::Split(next);
        s.phase = ScriptPhase::TextBuiltin(work);
        Ok(true)
    }

    fn split_fragment(&mut self, roots: Value) -> Result<(), FaultKind> {
        let fragment = self.heap.alloc_byte_buffer(&[]).map_err(heap_fault)?;
        self.heap
            .set_field(roots, FRAGMENT, fragment)
            .map_err(heap_fault)
    }
}
