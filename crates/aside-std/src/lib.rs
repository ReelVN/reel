//! Aside's reusable native library. Embedders use [`api`] for source declarations,
//! register [`contracts`] and [`types`] for linking and admission, and install
//! [`modules`] in their resolver. Calls dispatch through one [`StandardLibrary`]
//! per loaded program. [`prelude`] may be imported by default.
//! Script values and created random-stream state belong to the VM heap. The
//! host environment supplies process inputs for new streams and civil time.
#![forbid(unsafe_code)]

mod core;
mod curve;
pub use curve::sample_curve;
mod math;
mod random;
mod source;
mod time;

use aside_host::aside_compiler::semantic::{ExternalSymbol, ModuleInterface};
use aside_host::aside_vm::{
    FaultCode, HeapError, NewFault,
    bytecode::VerifiedProgram,
    executor::{FaultKind, HostResponse, HostTicket, Vm, VmError},
};
use aside_host::{Error, NativeBinding, NativeContract, NativeHandler};
use std::rc::Rc;

/// Host services used when creating unseeded random streams and reading civil time.
/// Implementations may keep interior state. A shared environment continues across
/// new sessions; VM snapshots already preserve each created random stream.
pub trait HostEnvironment {
    /// Choose the seed of a newly created independent random stream.
    fn random_seed(&self) -> u64;
    /// Return UTC Unix time in whole seconds, independently of story scheduling.
    fn unix_seconds(&self) -> i64;
}

/// Ordinary process entropy and system civil time for production embeddings.
#[derive(Default)]
pub struct SystemEnvironment;

impl HostEnvironment for SystemEnvironment {
    fn random_seed(&self) -> u64 {
        fastrand::Rng::new().get_seed()
    }
    fn unix_seconds(&self) -> i64 {
        jiff::Timestamp::now().as_second()
    }
}

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
    bindings.push(curve::binding());
    bindings.push(curve::sample::binding());
    bindings.push(core::error_info::binding());
    bindings
}

/// The same trusted ABI declarations are used by the compiler and VM admission.
pub fn contracts() -> Vec<NativeContract> {
    bindings()
        .into_iter()
        .map(|binding| binding.contract)
        .collect()
}

/// Query the registered library signatures and their English source documentation.
/// Editor clients match the canonical contract symbol after resolving an import.
pub fn documentation() -> Vec<aside_host::NativeDocumentation> {
    bindings()
        .into_iter()
        .map(NativeBinding::into_documentation)
        .collect()
}

/// Precise standard declarations, nominal types and modules for compiler and editor hosts.
pub fn api() -> &'static aside_host::source::Api {
    source::api()
}

/// Merge standard type declarations into an embedding host's existing native schema.
pub fn describe(target: &mut aside_host::source::Api) {
    target.types.extend(api().types.iter().cloned());
    for declaration in &api().functions {
        let symbol = aside_host::source::canonical(&declaration.symbol);
        if let Some(existing) = target
            .functions
            .iter_mut()
            .find(|existing| aside_host::source::canonical(&existing.symbol) == symbol)
        {
            *existing = declaration.clone();
        } else {
            target.functions.push(declaration.clone());
        }
    }
}

/// Runtime registrations derived from the same standard nominal declarations.
pub fn types() -> &'static [aside_host::HostTypeSupport] {
    static TYPES: std::sync::OnceLock<Vec<aside_host::HostTypeSupport>> =
        std::sync::OnceLock::new();
    TYPES.get_or_init(|| api().registrations())
}

/// Reconnect the library's heap-owned state when restoring an execution snapshot.
pub fn register_snapshots(
    registry: &mut aside_host::aside_vm::snapshot::SnapshotRegistry,
) -> Result<(), aside_host::aside_vm::snapshot::SnapshotError> {
    registry.register_validator("std.random", 1, random::readonly)
}

/// Public names only; bound method implementations are not free functions.
pub fn prelude() -> Vec<ExternalSymbol> {
    api().globals.clone()
}

/// Public standard module exports, retaining the same precise native and type identities.
pub fn modules() -> Vec<ModuleInterface> {
    api().modules.clone()
}

/// Resolved native handlers and host services for one program. Script random
/// streams belong to the VM heap. Rebuild the handlers when loading a different
/// program, sharing the host environment when its lifetime spans sessions.
pub struct StandardLibrary {
    handlers: Vec<Option<NativeHandler<Self, NativeError>>>,
    curve_constructor: Option<aside_host::aside_vm::bytecode::NativeId>,
    environment: Rc<dyn HostEnvironment>,
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
        let curve_constructor = package
            .natives
            .iter()
            .position(|native| package.symbols[native.symbol.index()] == "std::math::curve")
            .map(|index| aside_host::aside_vm::bytecode::NativeId(index as u32));
        Self {
            handlers,
            curve_constructor,
            environment: Rc::new(SystemEnvironment),
        }
    }

    /// Replace process services before executing the loaded program.
    pub fn set_host_environment(&mut self, environment: Rc<dyn HostEnvironment>) {
        self.environment = environment;
    }

    /// Share these host services with a new session or restored runtime.
    pub fn host_environment(&self) -> Rc<dyn HostEnvironment> {
        self.environment.clone()
    }

    /// Returns the consumed work, or None for another host's native.
    /// Invalid script input becomes a catchable fault at the native call site.
    pub fn dispatch(
        &mut self,
        vm: &mut Vm,
        ticket: HostTicket,
        work: u32,
    ) -> Result<Option<u32>, Error> {
        let Some(handler) = self.handlers.get(ticket.native.index()).copied().flatten() else {
            return Ok(None);
        };
        let result = if self.curve_constructor == Some(ticket.native) {
            curve::construct(self, vm, ticket, work)
        } else {
            handler(self, vm, ticket).map(|()| 1)
        };
        let mut kind = FaultKind::Type;
        let message = match result {
            Ok(work) => return Ok(Some(work)),
            Err(NativeError::ConcurrentModification) => {
                kind = FaultKind::ConcurrentModification;
                std::borrow::Cow::Borrowed("curve keys changed during construction")
            }
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
                code: FaultCode::core(kind),
                cause: None,
            }),
        )?;
        Ok(Some(1))
    }
}

#[derive(Debug)]
enum NativeError {
    ConcurrentModification,
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
