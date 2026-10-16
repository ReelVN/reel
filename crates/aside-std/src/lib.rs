//! Aside's reusable native library. Embedders register [`contracts`] for linking
//! and admission, install [`modules`] in their resolver, and dispatch calls through
//! one [`StandardLibrary`] per loaded program. [`prelude`] may be imported by default.
//! Script values and mutable random state belong to the VM heap, not this adapter.
#![forbid(unsafe_code)]

mod math;
mod random;
mod time;

use aside_host::aside_compiler::semantic::{ExternalSymbol, ModuleInterface};
use aside_host::aside_vm::{
    FaultCode, HeapError, NewFault,
    bytecode::{FunctionId, FunctionKind, VerifiedProgram},
    executor::{FaultKind, HostResponse, HostTicket, Vm, VmError},
};
use aside_host::{Error, NativeBinding, NativeContract, NativeHandler};

fn bindings() -> Vec<NativeBinding<StandardLibrary, NativeError>> {
    let mut bindings = vec![
        random::random::binding(),
        random::int::binding(),
        random::float::binding(),
        random::pick::binding(),
        time::now::binding(),
        time::parts::binding(),
        time::format::binding(),
    ];
    bindings.extend(math::bindings());
    bindings
}

/// The same trusted ABI declarations are used by the compiler and VM admission.
pub fn contracts() -> Vec<NativeContract> {
    bindings()
        .into_iter()
        .map(|binding| binding.contract)
        .collect()
}

/// Reconnect the library's heap-owned state when restoring an execution snapshot.
pub fn register_snapshots(
    registry: &mut aside_host::aside_vm::snapshot::SnapshotRegistry,
) -> Result<(), aside_host::aside_vm::snapshot::SnapshotError> {
    registry.register_validator("std.random", 1, random::readonly)
}

/// Public names only; bound method implementations are not free functions.
pub fn prelude() -> Vec<ExternalSymbol> {
    vec![random::random::binding().contract.external()]
}

pub fn modules() -> Vec<ModuleInterface> {
    let mut modules: Vec<_> = ["std", "std::prelude"]
        .into_iter()
        .map(|path| ModuleInterface {
            canonical_path: path.into(),
            version: "1".into(),
            exports: prelude(),
        })
        .collect();
    modules.push(ModuleInterface {
        canonical_path: "std::time".into(),
        version: "1".into(),
        exports: bindings()[4..7]
            .iter()
            .map(|binding| binding.contract.external())
            .collect(),
    });
    modules.push(ModuleInterface {
        canonical_path: "std::math".into(),
        version: "1".into(),
        exports: math::bindings()
            .into_iter()
            .map(|binding| binding.contract.external())
            .collect(),
    });
    modules
}

/// A program's resolved native handlers. This holds code identities only; it
/// does not retain random objects or require GC roots. Rebuild when loading a
/// different program, just as with the host's other linked native functions.
pub struct StandardLibrary {
    handlers: Vec<Option<NativeHandler<Self, NativeError>>>,
    random_methods: [Option<FunctionId>; 3],
}

impl StandardLibrary {
    pub fn new(program: &VerifiedProgram) -> Self {
        let package = program.package();
        let bindings = bindings();
        let handlers = package
            .natives
            .iter()
            .map(|native| {
                bindings
                    .iter()
                    .find(|binding| {
                        binding.contract.symbol == package.symbols[native.symbol.index()]
                    })
                    .map(|binding| binding.invoke)
            })
            .collect();
        let mut random_methods = [None; 3];
        for (index, function) in package.functions.iter().enumerate() {
            if let FunctionKind::Native(native) = function.kind {
                let symbol = &package.symbols[package.natives[native.index()].symbol.index()];
                for (slot, binding) in random_methods.iter_mut().zip(&bindings[1..4]) {
                    if *symbol == binding.contract.symbol {
                        *slot = Some(FunctionId(index as u32));
                    }
                }
            }
        }
        Self {
            handlers,
            random_methods,
        }
    }

    /// Returns false for another host's native without consuming its ticket.
    /// Invalid script input becomes a catchable fault at the native call site.
    pub fn dispatch(&mut self, vm: &mut Vm, ticket: HostTicket) -> Result<bool, Error> {
        let Some(handler) = self.handlers.get(ticket.native.index()).copied().flatten() else {
            return Ok(false);
        };
        let message = match handler(self, vm, ticket) {
            Ok(()) => return Ok(true),
            Err(NativeError::Input(message)) => std::borrow::Cow::Borrowed(message),
            Err(NativeError::Message(message)) => std::borrow::Cow::Owned(message),
            Err(NativeError::Host(Error::Heap(HeapError::TypeMismatch | HeapError::WrongKind))) => {
                std::borrow::Cow::Borrowed("invalid argument type")
            }
            Err(NativeError::Host(error)) => return Err(error),
        };
        let value = vm.heap_mut().alloc_string(&message)?;
        vm.respond(
            ticket,
            HostResponse::Error(NewFault {
                value,
                code: FaultCode::core(FaultKind::Type),
                cause: None,
            }),
        )?;
        Ok(true)
    }
}

#[derive(Debug)]
enum NativeError {
    Host(Error),
    Input(&'static str),
    Message(String),
}
impl From<Error> for NativeError {
    fn from(error: Error) -> Self {
        Self::Host(error)
    }
}
impl From<HeapError> for NativeError {
    fn from(error: HeapError) -> Self {
        Self::Host(error.into())
    }
}
impl From<VmError> for NativeError {
    fn from(error: VmError) -> Self {
        Self::Host(error.into())
    }
}
