//! Diagnostics for the currently executing script task.
use crate::{NativeError, StandardLibrary};
use aside_host::aside_vm::{Value, bytecode::Effects};
use aside_host::{CallContext, native};

/// Return the innermost active catch's read-only fault view, or null.
///
/// Helpers and native callbacks share their task's catch context. Keeping the
/// returned value retains the existing fault graph; this call does not copy it.
/// Derived computations cannot inspect execution diagnostics.
/// The view exposes value, code, origin, stack and causes. Code is a stable
/// name such as "Thrown" or "Cancelled". Stack and causes are repeatable `for`
/// sources over shared nodes, including empty chains. Stack nodes expose origin,
/// frame_id, module_id, module_version, source_version, kind, native_id, abi,
/// continuation_version, continuation_stage and next. Cause nodes expose kind,
/// event, origin, fault, value, activation, exit and next. A missing link is null.
/// Origin contains function, content_version, pc and nullable source with a
/// file index and start/end UTF-8 byte offsets. Full-width identities and versions use
/// decimal strings; function/module/file indexes and offsets use integers.
/// Metadata is projected on access; retaining views preserves the shared graph
/// through garbage collection and snapshots. Authors may mutate their original
/// thrown value, while fault fields and projected metadata are read-only.
///
/// ```aside
/// use std::core;
/// fn main() {
///     try { throw "example"; }
///     catch error {
///         let info = core.error_info();
///         for frame in info.stack { frame.origin.pc; }
///         info.code
///     }
/// }
/// ```
#[native(symbol = "std::core::error_info", effects = Effects(Effects::HOST))]
pub(super) fn error_info(
    #[aside(context)] context: &mut CallContext<'_, StandardLibrary>,
) -> Result<Value, NativeError> {
    Ok(context
        .vm
        .error_info(context.ticket.task)?
        .map_or(Value::NULL, |info| info.as_value()))
}
