# Extending Aside from Rust

`aside-host` provides native declarations, Rust value conversions and property schemas for embedding Aside. It depends on Aside's compiler and VM. The re-exported `native` and `Properties` macros use [Moxy](https://github.com/aacebo/moxy), pinned to 0.5.4 as a compile-time dependency.

## Export a Rust function

```rust
use aside_host::native;

#[native(symbol = "sample::add")]
fn add(left: i64, #[aside(default = 2)] right: i64) -> i64 {
    left + right
}

let binding = add::binding();
```

The macro preserves the Rust function and generates a same-named module with `SYMBOL`, `DOCUMENTATION`, `binding()` and `invoke()`. The binding contains a `NativeContract`, a direct function pointer and the function's English `///` documentation. Editor tooling can consume `NativeBinding::into_documentation()` to obtain the registered signature and Markdown together. Resumable bindings use `native_documentation!` beside their declaration. Write runnable `aside` fences in these comments; `aside_compiler::documentation::examples` supplies their source and line numbers to validation. The host builds the registry during compilation and loading, then dispatches each admitted `NativeId` to its handler. `invoke` reads the VM's bound arguments directly.

`NativeContract::external()` supplies source-analysis metadata. `aside_host::lower()` generates bytecode native linkage and constant-pool references for defaults. `aside_host::runtime_support()` resolves the host's contracts against the package; pass its result to `Vm::load`. The host selects which native calls a package may use.

The editor integration generates signatures and read-only Aside declarations from this metadata. Parameters retain their registered names and literal defaults. Rust declarations can refine a dynamic runtime value with a source type, such as a named host object or a union; property declarations also supply their read and write types. This information feeds semantic member lookup, completion, hover and definition navigation. The generated declarations share the English source documentation and remain views of the host implementation.

References inside generated declarations retain their registered symbol identities. Definition navigation and hover in these read-only views use the same declarations as author source.

Module declarations retain their canonical identity and source documentation, so global service names navigate to the same generated module as explicit imports. Property schemas identify the parameters they contribute to node constructors. Hover can summarize these optional properties while completion, signature help and generated declarations retain the complete parameter contract.

Named host types share one trusted registration between source analysis, bytecode lowering and VM loading. Each registration identifies the canonical name, version and base types. Rust assigns the registered identity to a host value through a heap-bound type handle; Aside annotations and `is` check that identity and its declared bases. Each registered type shares its ancestor descriptor across values. VM snapshots retain the host identity and verify the registration fingerprint on restore.

`NamedType` describes an existing host value. Its name is available in type annotations and `is`; constructors remain registered functions. Callable and awaitable types expose their actual call signature and awaited result through the same declaration. Ordinary member access retains the object's receiver.

`ModuleResolver::has_closed_members` identifies types whose complete member contract is available to analysis. A missing member on these types is a source diagnostic. Resolvers leave unknown or extensible types open; Reel and the editor use their registered named types for this query.

`NamedType::registration()` derives shared native methods from the type's bound member declarations, including inherited members and overrides. `HostTypeSupport::methods` identifies each method by its visible name, canonical native symbol and ABI version. Loading resolves these entries to the package's independently admitted native callable definitions and verifies their required receiver parameter. A smaller package may omit an unlinked native. The shared method table is charged once per type and code lease; constructing a host stores its own fields and nominal identity.

Member reads, including dynamic string indexing, search own host fields first and then the shared method table. Own fields retain their existing validators and write policy; a shared method without an own field is read-only. Extracting a method creates the ordinary `BoundNative` value, retaining the original receiver through GC and snapshots. A direct method call uses this same temporary binding. Methods work on immutable host values, and changing a shared method declaration changes the registration fingerprint checked during restoration.

A declaration may refine its result for a constant argument. Source analysis applies the matching case after positional and named argument binding; omitted arguments use the registered default. Dynamic arguments retain the declaration's complete result type. Native admission verifies the source signature and runtime ABI at their respective boundaries.

Once the host includes this declaration in its analysis environment, Aside can use positional, named and default arguments, including through a function value:

```aside
fn main() {
    let sum = add;
    sum(right: 8, left: 4) + add(3)
}
```

`Vm::host_provided_arguments` borrows the explicitly supplied arguments in source evaluation order, paired with their resolved parameter indices. Omitted defaults and a bound receiver are absent; an explicit null remains present. `with_host_provided_arguments` supplies the same borrowed view alongside mutable heap access, so configuration handlers can process supplied properties without constructing another argument table. The native frame retains these values across continuation, GC and restoration.

The existing VM binder performs argument binding once. The generated wrapper reads the bound values and calls Rust. The complete runnable host, including module initialization, bytecode encoding/decoding, admission and dispatch, is [extension.rs](../crates/aside-host/tests/extension.rs):

```sh
cargo test --locked -p aside-host --test extension
```

## State, values and errors

### Variadic native declarations

`NativeContract::rest` is an optional parameter name. The compiler and runtime declare it as the ordinary
array rest parameter. `#[aside(rest)] values: Value` must be the last script
parameter and cannot have a default. The native wrapper receives the VM's fresh
outer rest array, including an empty array when no remaining arguments are given.
Positional spread, function values and bound receivers use the VM binder and
NativeFrame. The rest name is reserved for the collected positional arguments;
supplying it as a named argument reports an error. Packing arguments uses the VM's
execution and heap budgets.

```rust
use aside_host::{native, aside_vm::Value};

#[native(symbol = "sample::collect")]
fn collect(#[aside(rest)] items: Value) -> Value {
    items
}
```

Rust receives an array value through `FromValue`; use `Value` or an owned
conversion that accepts an array. Read it through `Heap::sequence_len/get` when
the operation needs its elements. The `#[aside(context)]` parameter does not
count toward the script parameter order and may follow the rest parameter.
The [extension host](../crates/aside-host/tests/extension.rs) executes empty,
spread and function-value calls and checks fresh outer-array ownership. The
[bound native host](../crates/aside-host/tests/bound_native.rs) exercises rest
with defaults, a bound receiver, host entry/callback calls and incremental GC.

Functions are synchronous, safe, monomorphic free functions. Parameters must have identifier patterns. `#[aside(name = "script_name")]` changes the script parameter name. Defaults are literal integers, floats, booleans, strings or `null`, stored in the bytecode constant pool. The `abi` option defaults to 1.

An optional `#[aside(context)] context: &mut CallContext<'_, Host>` parameter supplies mutable host state, the VM and the current ticket. It is omitted from the script signature. For example:

```rust
use aside_host::{native, CallContext, Error};
use aside_host::aside_vm::bytecode::Effects;

struct Host { calls: usize }

#[native(symbol = "sample::decorate", effects = Effects::ALL,
         capabilities = ["sample.text"])]
fn decorate(
    #[aside(context)] context: &mut CallContext<'_, Host>,
    value: String,
    #[aside(default = "!")] suffix: String,
) -> Result<String, Error> {
    context.host.calls += 1;
    Ok(format!("{value}{suffix}"))
}
```

Declare the function's effects and required capabilities explicitly when it touches external state. The default effect is `ALLOCATE`; it suits a value conversion/computation without host mutation or waiting. A synchronous wrapper must not respond to its ticket itself: the wrapper sends the successful return value. `Result<T, E>` propagates errors to the host dispatcher; `E` must accept the conversion, heap and VM errors via `From`. The dispatcher decides whether to reject the pending call with a language fault at its source location or report an internal host failure. Reel uses its existing `AdapterError` mapping and rejection path.

Built-in conversions cover `i64`, `f64`, `bool`, `String`, `Value` and `Option<T>`; `()` returns `null`, and `&str` can be returned as a string. Numbers retain their declared integer/float type. `Option<T>` exposes `T | null` in generated source declarations; its runtime ABI accepts `Any`, maps `null` to `None` and validates non-null values through `T`. Implement `FromValue` and `IntoValue` for additional owned Rust types.

`String` copies UTF-8 into owned Rust memory in linear time; this memory belongs to the host. String results allocate in the VM's accounted heap. `Value` copies only a VM identity; keeping it after the native call requires an explicit VM root. The context is borrowed for one call. Native code must root any new temporary values it retains across a manually requested collection. Large or asynchronous operations use handwritten resumable natives with the same `NativeContract`, `HostTicket` and NativeFrame protocol, so scheduling and cancellation remain explicit.

## Bind a receiver

`aside_host::lower()` seeds one native callable definition per contract; the compiler reuses it when source reads the function as a value. Resolve its `FunctionId` from the admitted program once during host setup. `Vm::bind_native(receiver, function)` creates an immutable callable that traces the receiver and retains the code version and module identity. The native must declare a required first parameter accepting that value.

The first parameter is already bound. Remaining positional, named and default arguments use the VM binder; explicitly supplying the receiver again reports a duplicate argument. The value can be stored in a host property, passed to script, invoked through `start_closure`, or used by a native `CallScript` callback. These paths enter a NativeFrame that enforces the declared effects and waiting permissions. Root the method when retaining it across collection from Rust.

For a host object with properties that should also be callable, use `Vm::bind_callable(object, function)`. It binds the object itself as the required first parameter. The trusted registration is separate from script-visible fields; argument preparation, entry calls, callbacks, GC and waiting use the same paths as bound methods.

The runnable [bound_native.rs](../crates/aside-host/tests/bound_native.rs) host verifies these call paths with interleaved GC and bytecode encoding/decoding:

```sh
cargo test --locked -p aside-host --test bound_native
```

Reel exposes `add`, `remove` and `clear` on nodes through this mechanism. Extracting a method retains that specific node; retirement clears its private tree edges.

## Describe host properties

```rust
use aside_host::{Properties, PropertySchema};

#[derive(Properties)]
enum Field {
    #[aside(ty = Float, default = 0.0, animatable)]
    X,
    #[aside(ty = Bool, default = true)]
    Visible,
    #[aside(name = "id", ty = String, default = "stable", readonly)]
    Identity,
}
```

The fieldless enum defines property identity. Declaration order determines `Field::index()` and `Field::PROPERTIES`. Each descriptor supplies its name, builtin type, default, writability and animation flag. Explicit `name` values are recommended for multiword variants; the implicit name is the lowercase variant name. The implicit type is `Any`, and the implicit default is `null`.

`Property::parameter()` builds a constructor parameter from the same declaration. `Field::validate_host` can be passed directly to `Heap::alloc_host` as its write validator. The host validator receives `(&PropertyRead, receiver: Value, index: u32, value: Value)`, so cross-field constraints inspect the complete proposed update before publication; `Field::validate` checks an individual field. An optional `validate = function` supplies a complete property validator with signature `fn(&Heap, Value) -> Result<(), HeapError>`; without one, the declared builtin type is checked. Readonly writes are rejected before validation. Constructors still own initial values, private traced edges and host identity. Rust addresses are never embedded in script values. The runnable host example constructs a host object and checks script reads, accepted writes, readonly rejection and custom numeric validation.

Reel uses this schema for common node constructor parameters, property writes and tween fields. Projection uses typed field identities and `reel-ui::stage::NodeProps`.

### Observable properties

`Heap::observe_host_properties(host, writable_count, readonly_count)` registers a writable prefix followed by read-only observable outputs in the field table. Methods outside that range remain ordinary fields. `PropertyId` identifies a host and field index.

Constructors resolve a reference with `property_argument`, then retain the connection with `bind_host_property`. Script and control writes use `write_host_property`; authoritative read-only service updates use `publish_host_property`. Both validate all affected values before publishing. Use `require_writable_property` for control values that must write back. `retire_host` disconnects subscriptions and freezes surviving targets. `table_store` writes storage directly and is suitable for initialization; bound properties require the publishing APIs above.

For a joint authoritative service update, use `publish_host_properties`. It shares the configuration batch's preparation, connected-constraint validation and commit, while permitting concrete values for read-only service fields. Binding inputs retain their ordinary write permissions. All storage is prepared before publication; a rejected update preserves the original values.

For bounded preparation, `begin_host_publication()` creates the same rooted `PropertyBatch` used by `begin_property_batch`. Retain its value and the original inputs, then use `property_batch_required_work`, `step_property_batch` and `commit_property_batch`. Preparation yields with the previous values intact; publication requires the reported complete commit allowance. The service-write permission survives restarts and snapshots, while replacement bindings still require ordinary write access.

`PropertySchema::documentation(index)` reads each variant's English `///` comment for property hovers. Documentation stays in static metadata; node values and saved sessions contain the declared properties.

## Runnable host checks

The [extension host](../crates/aside-host/tests/extension.rs) and [bound native host](../crates/aside-host/tests/bound_native.rs) demonstrate registration, value conversion, properties and callbacks.

```sh
cargo test --locked -p aside-host -p aside-host-macros
```
