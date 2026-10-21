//! Read-only script projections over the existing shared fault graph.
use super::*;
use crate::payload::{BYTE_CHUNK_BYTES, Bytes, Values};

fn code_name(kind: FaultKind) -> &'static str {
    match kind {
        FaultKind::Thrown => "Thrown",
        FaultKind::Type => "Type",
        FaultKind::Uninitialized => "Uninitialized",
        FaultKind::AlreadyInitialized => "AlreadyInitialized",
        FaultKind::ReadOnly => "ReadOnly",
        FaultKind::DuplicateArgument => "DuplicateArgument",
        FaultKind::MissingArgument => "MissingArgument",
        FaultKind::UnknownArgument => "UnknownArgument",
        FaultKind::Overflow => "Overflow",
        FaultKind::DivisionByZero => "DivisionByZero",
        FaultKind::Effect => "Effect",
        FaultKind::Resource => "Resource",
        FaultKind::InvalidState => "InvalidState",
        FaultKind::Cancelled => "Cancelled",
        FaultKind::OutOfBounds => "OutOfBounds",
        FaultKind::MissingKey => "MissingKey",
        FaultKind::UnhashableKey => "UnhashableKey",
        FaultKind::ConcurrentModification => "ConcurrentModification",
        FaultKind::ModuleInitializationCycle => "ModuleInitializationCycle",
        FaultKind::ArgumentOrder => "ArgumentOrder",
        FaultKind::MatchError => "MatchError",
        FaultKind::UninitializedField => "UninitializedField",
        FaultKind::ConstructionFailed => "ConstructionFailed",
        FaultKind::PatternMismatch => "PatternMismatch",
        FaultKind::NotAwaitable => "NotAwaitable",
    }
}
fn string_cost(len: usize) -> (u64, u64) {
    (
        1 + len as u64,
        Bytes::chunks_for_len(len as u64) * BYTE_CHUNK_BYTES,
    )
}
fn add(a: (u64, u64), b: (u64, u64)) -> (u64, u64) {
    (a.0 + b.0, a.1 + b.1)
}
fn decimal_cost(n: u64) -> (u64, u64) {
    string_cost(if n == 0 { 1 } else { n.ilog10() as usize + 1 })
}
fn record_cost(names: &[&str]) -> (u64, u64) {
    names.iter().fold(
        (
            2 + names.len() as u64,
            Values::allocation_bytes(names.len() as u64 * 2),
        ),
        |cost, name| add(cost, string_cost(name.len())),
    )
}
fn location_cost(location: Location) -> (u64, u64) {
    let cost = add(
        record_cost(&["function", "content_version", "pc", "source"]),
        decimal_cost(location.code.version),
    );
    if location.source.is_some() {
        add(cost, record_cost(&["file", "start", "end"]))
    } else {
        cost
    }
}

impl Heap {
    /// Admission uses the same payload chunk formulas as the allocator. Object
    /// slots remain governed by Heap's existing object-slot accounting.
    pub(crate) fn fault_member_cost(
        &self,
        object: Value,
        name: &str,
    ) -> Result<(u64, u64), HeapError> {
        let payload = self.fault_payload(object)?;
        Ok(match (payload.data, name) {
            (
                FaultData::Envelope {
                    code,
                    complete: true,
                    ..
                }
                | FaultData::Error(code),
                "code",
            ) => string_cost(code_name(code.kind).len()),
            (
                FaultData::Envelope {
                    origin,
                    complete: true,
                    ..
                },
                "origin",
            ) => location_cost(origin),
            (FaultData::Envelope { complete: true, .. }, "stack" | "causes") => {
                (1, super::FAULT_NODE_BYTES)
            }
            (FaultData::Error(code), "message") => string_cost(code.kind.message().len()),
            (FaultData::Stack(entry), "origin") => location_cost(entry.location),
            (FaultData::Stack(entry), "frame_id") => decimal_cost(entry.frame.0),
            (FaultData::Stack(entry), "module_version") => decimal_cost(entry.module_version),
            (FaultData::Stack(entry), "source_version") => decimal_cost(entry.source_version),
            (FaultData::Stack(entry), "kind") => string_cost(match entry.kind {
                StackFrameKind::Script => 6,
                StackFrameKind::Native { .. } => 6,
            }),
            (
                FaultData::Stack(StackEntry {
                    kind: StackFrameKind::Native { continuation, .. },
                    ..
                }),
                "continuation_version",
            ) => decimal_cost(continuation.version),
            (
                FaultData::Stack(StackEntry {
                    kind: StackFrameKind::Native { continuation, .. },
                    ..
                }),
                "continuation_stage",
            ) => decimal_cost(continuation.stage),
            (FaultData::Cause { event, .. }, "event") => decimal_cost(event),
            (FaultData::Cause { location, .. }, "origin") => location_cost(location),
            (FaultData::Cause { kind, .. }, "kind") => string_cost(match kind {
                CauseKind::CaughtFault => 11,
                CauseKind::ExplicitCause => 13,
                CauseKind::AwaitedFault { .. } => 12,
                CauseKind::ReplacedExit(_) => 12,
            }),
            (
                FaultData::Cause {
                    kind: CauseKind::AwaitedFault { activation },
                    ..
                },
                "activation",
            ) => decimal_cost(activation),
            (
                FaultData::Cause {
                    kind: CauseKind::ReplacedExit(exit),
                    ..
                },
                "exit",
            ) => {
                let cost = add(
                    record_cost(&[
                        "kind",
                        "target",
                        "code",
                        "close_kind",
                        "logical_id",
                        "activation",
                    ]),
                    string_cost(match exit {
                        ReplacedExit::Throw(_) => 5,
                        ReplacedExit::Return => 6,
                        ReplacedExit::Leave { .. } => 5,
                        ReplacedExit::Close { .. } => 5,
                    }),
                );
                match exit {
                    ReplacedExit::Leave { code, .. } => add(
                        cost,
                        location_cost(Location {
                            code,
                            pc: Pc(0),
                            source: None,
                        }),
                    ),
                    ReplacedExit::Close {
                        kind,
                        logical_id,
                        activation,
                    } => add(
                        add(
                            add(
                                cost,
                                string_cost(match kind {
                                    CloseKind::Generator => 9,
                                    CloseKind::Thread => 6,
                                }),
                            ),
                            decimal_cost(logical_id),
                        ),
                        decimal_cost(activation),
                    ),
                    _ => cost,
                }
            }
            _ => (1, 0),
        })
    }

    fn fault_record(&mut self, fields: &[(&str, Value)]) -> Result<Value, HeapError> {
        let table = self.alloc_table()?;
        for (index, (name, value)) in fields.iter().enumerate() {
            let key = self.alloc_string(name)?;
            self.table_store(table, index as u32, key, *value)?;
        }
        self.alloc_immutable_host(table, &[])
    }
    fn fault_location(&mut self, location: Location) -> Result<Value, HeapError> {
        let source = if let Some(span) = location.source {
            self.fault_record(&[
                ("file", Value::int(i64::from(span.file.0))),
                ("start", Value::int(i64::from(span.start))),
                ("end", Value::int(i64::from(span.end))),
            ])?
        } else {
            Value::NULL
        };
        let version = self.alloc_string(&location.code.version.to_string())?;
        self.fault_record(&[
            ("function", Value::int(i64::from(location.code.function.0))),
            ("content_version", version),
            ("pc", Value::int(i64::from(location.pc.0))),
            ("source", source),
        ])
    }
    pub(crate) fn fault_sequence_head(&self, value: Value) -> Result<Value, HeapError> {
        let payload = self.fault_payload(value)?;
        match payload.data {
            FaultData::StackView => Ok(self.fault_payload(payload.edges[0])?.edges[1]),
            FaultData::CausesView => Ok(self.fault_payload(payload.edges[0])?.edges[2]),
            FaultData::Stack(_) | FaultData::Cause { .. } => Ok(value),
            _ => Err(HeapError::WrongKind),
        }
    }
    pub(crate) fn fault_sequence_next(&self, value: Value) -> Result<Value, HeapError> {
        let payload = self.fault_payload(value)?;
        match payload.data {
            FaultData::Stack(_) | FaultData::Cause { .. } => Ok(payload.edges[0]),
            _ => Err(HeapError::WrongKind),
        }
    }
    pub(crate) fn fault_member(
        &mut self,
        object: Value,
        name: &str,
    ) -> Result<Option<Value>, HeapError> {
        let payload = self.fault_payload(object)?;
        let data = payload.data;
        let edges = payload.edges;
        let value = match (data, name) {
            (FaultData::Envelope { complete: true, .. }, "value") => edges[0],
            (
                FaultData::Envelope {
                    code,
                    complete: true,
                    ..
                }
                | FaultData::Error(code),
                "code",
            ) => self.alloc_string(code_name(code.kind))?,
            (
                FaultData::Envelope {
                    origin,
                    complete: true,
                    ..
                },
                "origin",
            ) => self.fault_location(origin)?,
            (FaultData::Envelope { complete: true, .. }, "stack" | "causes") => {
                let code = self.fault_payload(object)?.code.clone();
                let stack = name == "stack";
                self.alloc_fault_node(
                    if stack {
                        ObjectKind::FaultStack
                    } else {
                        ObjectKind::FaultCause
                    },
                    if stack {
                        FaultData::StackView
                    } else {
                        FaultData::CausesView
                    },
                    [object, Value::NULL, Value::NULL],
                    code,
                )?
            }
            (FaultData::Error(code), "message") => self.alloc_string(code.kind.message())?,
            (FaultData::Error(_), "details") => edges[0],
            (FaultData::Stack(entry), "origin") => self.fault_location(entry.location)?,
            (FaultData::Stack(entry), "frame_id") => {
                self.alloc_string(&entry.frame.0.to_string())?
            }
            (FaultData::Stack(entry), "module_id") => Value::int(i64::from(entry.module.0)),
            (FaultData::Stack(entry), "module_version") => {
                self.alloc_string(&entry.module_version.to_string())?
            }
            (FaultData::Stack(entry), "source_version") => {
                self.alloc_string(&entry.source_version.to_string())?
            }
            (FaultData::Stack(entry), "kind") => self.alloc_string(match entry.kind {
                StackFrameKind::Script => "script",
                StackFrameKind::Native { .. } => "native",
            })?,
            (FaultData::Stack(entry), "native_id") => match entry.kind {
                StackFrameKind::Native { native, .. } => Value::int(i64::from(native.0)),
                _ => Value::NULL,
            },
            (FaultData::Stack(entry), "abi") => match entry.kind {
                StackFrameKind::Native { abi_version, .. } => Value::int(i64::from(abi_version)),
                _ => Value::NULL,
            },
            (FaultData::Stack(entry), "continuation_version") => match entry.kind {
                StackFrameKind::Native { continuation, .. } => {
                    self.alloc_string(&continuation.version.to_string())?
                }
                _ => Value::NULL,
            },
            (FaultData::Stack(entry), "continuation_stage") => match entry.kind {
                StackFrameKind::Native { continuation, .. } => {
                    self.alloc_string(&continuation.stage.to_string())?
                }
                _ => Value::NULL,
            },
            (FaultData::Stack(_) | FaultData::Cause { .. }, "next") => edges[0],
            (FaultData::Cause { event, .. }, "event") => self.alloc_string(&event.to_string())?,
            (FaultData::Cause { location, .. }, "origin") => self.fault_location(location)?,
            (FaultData::Cause { kind, .. }, "kind") => self.alloc_string(match kind {
                CauseKind::CaughtFault => "CaughtFault",
                CauseKind::ExplicitCause => "ExplicitCause",
                CauseKind::AwaitedFault { .. } => "AwaitedFault",
                CauseKind::ReplacedExit(_) => "ReplacedExit",
            })?,
            (
                FaultData::Cause {
                    kind: CauseKind::AwaitedFault { activation },
                    ..
                },
                "activation",
            ) => self.alloc_string(&activation.to_string())?,
            (FaultData::Cause { .. }, "activation") => Value::NULL,
            (FaultData::Cause { kind, .. }, "fault" | "value") => {
                let linked = matches!(
                    kind,
                    CauseKind::CaughtFault
                        | CauseKind::ExplicitCause
                        | CauseKind::AwaitedFault { .. }
                        | CauseKind::ReplacedExit(ReplacedExit::Throw(_))
                );
                if name == "fault" {
                    if linked { edges[1] } else { Value::NULL }
                } else if linked {
                    self.fault_value(self.fault_id(edges[1])?)?
                } else {
                    edges[1]
                }
            }
            (
                FaultData::Cause {
                    kind: CauseKind::ReplacedExit(exit),
                    ..
                },
                "exit",
            ) => {
                let kind = self.alloc_string(match exit {
                    ReplacedExit::Throw(_) => "throw",
                    ReplacedExit::Return => "return",
                    ReplacedExit::Leave { .. } => "leave",
                    ReplacedExit::Close { .. } => "close",
                })?;
                let target = if let ReplacedExit::Leave { target, .. } = exit {
                    Value::int(i64::from(target.0))
                } else {
                    Value::NULL
                };
                let code = if let ReplacedExit::Leave { code, .. } = exit {
                    self.fault_location(Location {
                        code,
                        pc: Pc(0),
                        source: None,
                    })?
                } else {
                    Value::NULL
                };
                let (close, logical_id, activation) = if let ReplacedExit::Close {
                    kind,
                    logical_id,
                    activation,
                } = exit
                {
                    (
                        self.alloc_string(match kind {
                            CloseKind::Generator => "generator",
                            CloseKind::Thread => "thread",
                        })?,
                        self.alloc_string(&logical_id.to_string())?,
                        self.alloc_string(&activation.to_string())?,
                    )
                } else {
                    (Value::NULL, Value::NULL, Value::NULL)
                };
                self.fault_record(&[
                    ("kind", kind),
                    ("target", target),
                    ("code", code),
                    ("close_kind", close),
                    ("logical_id", logical_id),
                    ("activation", activation),
                ])?
            }
            (FaultData::Cause { .. }, "exit") => Value::NULL,
            (FaultData::StackView | FaultData::CausesView, "first") => {
                self.fault_sequence_head(object)?
            }
            _ => return Ok(None),
        };
        Ok(Some(value))
    }
}
