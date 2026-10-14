# Extending Aside from Rust

`aside-host` provides native declarations, Rust value conversions and property schemas for embedding Aside. It depends on Aside's compiler and VM, independently of Reel and platform libraries. The re-exported `native` and `Properties` macros use [Moxy](https://github.com/aacebo/moxy), pinned to 0.5.4 as a compile-time dependency. No parser sources are copied into the workspace.

## Export a Rust function

```rust
use aside_host::native;

#[native(symbol = "sample::add")]
fn add(left: i64, #[aside(default = 2)] right: i64) -> i64 {
    left + right
}

let binding = add::binding();
```

The original Rust function remains callable. Its body keeps the compiler's original tokens and delimiter spans; only the declaration is parsed to generate the interface. Its same-named module provides `SYMBOL`, `binding()` and `invoke()`. The binding contains a `NativeContract` and a direct function pointer. A host builds these declarations during compilation/loading, then dispatches each admitted `NativeId` to its handler. Calling `invoke` does not rebuild the registry, look up parameter names or allocate a parameter vector.

`NativeContract::external()` supplies source-analysis metadata. `aside_host::lower()` generates B01 native linkage and constant default functions. `aside_host::runtime_support()` resolves the host's independently supplied contracts against the package; pass its result to `Vm::load`. A package cannot supply its own authority to call native code. B01/ASB v1 and native ABI v1 are unchanged.

Once the host includes this declaration in its analysis environment, Aside can use positional, named and default arguments, including through a function value:

```aside
fn main() {
    let sum = add;
    sum(right: 8, left: 4) + add(3)
}
```

The existing VM binder performs argument binding once. The generated wrapper reads the bound values and calls Rust. The complete runnable host, including module initialization, bytecode encoding/decoding, admission and dispatch, is [extension.rs](../crates/aside-host/tests/extension.rs):

```sh
cargo test --locked -p aside-host --test extension
```

## State, values and errors

### Variadic native declarations

Scope: aside-host declarations, aside-host-macros, their end-to-end checks, Reel's
contract initializers and the shared specifications/guides. B01/ASB v1 and native
ABI v1 stay unchanged; there are no new dependencies. `NativeContract::rest` is
an optional parameter name. The compiler and runtime declare it as the ordinary
array rest parameter. `#[aside(rest)] values: Value` must be the last script
parameter and cannot have a default. The native wrapper receives the VM's fresh
outer rest array, including an empty array when no remaining arguments are given.
It does not copy the elements into a Rust vector. Positional spread, function
values, bound receivers, GC, admission and source diagnostics reuse the existing
binder and NativeFrame. The rest name cannot be supplied as a named argument.

This supports ordinary variadic library calls without teaching the compiler
about Reel. Validation covers source to B01,
loading, empty and spread calls, defaults, bound methods, interleaved collection
and mismatched runtime declarations. Work and allocations for packing arguments
remain governed by VM slice/heap budgets; wrappers add no collection or copy.

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

Functions are synchronous, safe, monomorphic free functions. Parameters must have identifier patterns. `#[aside(name = "script_name")]` changes the script parameter name. Defaults are literal integers, floats, booleans, strings or `null`; they are ordinary B01 defaults, not a second Rust argument binder. The `abi` option defaults to 1.

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

Built-in conversions cover `i64`, `f64`, `bool`, `String`, `Value` and `Option<T>`; `()` returns `null`, and `&str` can be returned as a string. Numbers retain their declared integer/float type. `Option<T>` declares `Any`, maps `null` to `None` and validates non-null values through `T`. Implement `FromValue` and `IntoValue` for additional owned Rust types.

`String` copies UTF-8 into owned Rust memory in linear time; this memory belongs to the host. String results allocate in the VM's accounted heap. `Value` copies only a VM identity; keeping it after the native call requires an explicit VM root. The context is borrowed for one call. Native code must root any new temporary values it retains across a manually requested collection. Large or asynchronous operations use handwritten resumable natives with the same `NativeContract`, `HostTicket` and NativeFrame protocol, so scheduling and cancellation remain explicit.

## Bind a receiver

`aside_host::lower()` seeds one native callable definition per contract; the compiler reuses it when source reads the function as a value. Resolve its `FunctionId` from the admitted program once during host setup. `Vm::bind_native(receiver, function)` creates an immutable callable that traces the receiver and retains the code version and module identity. The native must declare a required first parameter accepting that value.

The first parameter is already bound. Remaining positional, named and default arguments use the ordinary VM binder; explicitly supplying the receiver again reports a duplicate argument. The value can be stored in a host property, passed to script, invoked through `start_closure`, or used by a native `CallScript` callback. These paths enter the same NativeFrame without a generated script wrapper. Effects, waiting permissions and source diagnostics are unchanged. As with other VM values, root the method when retaining it across collection from Rust.

For a host object with properties that should also be callable, use `Vm::bind_callable(object, function)`. It binds the object itself as the required first parameter without allocating a wrapper for each call. The trusted registration is separate from script-visible fields; argument preparation, entry calls, callbacks, GC and waiting use the same paths as bound methods. It does not implement user-defined class metamethods.

The runnable [bound_native.rs](../crates/aside-host/tests/bound_native.rs) host verifies these call paths with interleaved GC and B01 encoding/decoding:

```sh
cargo test --locked -p aside-host --test bound_native
```

Reel exposes `add`, `remove` and `clear` on nodes through this mechanism. Extracting a method retains that specific node; retirement clears its private tree edges, so a saved method does not retain a retired tree.

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

The fieldless enum defines property identity without copying application data. Declaration order determines `Field::index()` and `Field::PROPERTIES`. Each descriptor supplies its name, builtin type, default, writability and animation flag. Explicit `name` values are recommended for multiword variants; the implicit name is the lowercase variant name. The implicit type is `Any`, and the implicit default is `null`.

`Property::parameter()` builds a constructor parameter from the same declaration. `Field::validate_host` can be passed directly to `Heap::alloc_host` as its write validator. The host validator receives `(&Heap, receiver: Value, index: u32, value: Value)`, so cross-field constraints can inspect current fields before mutation; `Field::validate` checks an individual field. An optional `validate = function` supplies a complete property validator with signature `fn(&Heap, Value) -> Result<(), HeapError>`; without one, the declared builtin type is checked. Readonly writes are rejected before validation. Constructors still own initial values, private traced edges and host identity. Rust addresses are never embedded in script values. The runnable host example constructs a host object and checks script reads, accepted writes, readonly rejection and custom numeric validation.

Reel uses this schema for all common node constructor parameters, property writes and tween fields. Projection uses typed field identities and the existing `reel-ui::stage::NodeProps`. Its pause/resume/stop/finish functions use `native`; dialogue, choice decoding and activity waits retain their resumable implementations. There is one compiler/VM binding protocol and one stage data model.

## Validation and cost

The migration covers `aside-host`, `aside-host-macros`, public access to the VM's borrowed byte chunks and builtin type matching, Reel's registration/node/activity adapters and the corresponding specifications and both creator guides. It adds no author-facing property or activity-count limit. Scalar and `Value` wrappers convert directly; property metadata is static. Registry/default construction happens at compilation/loading, outside animation updates.

```sh
cargo test --locked -p aside-host -p aside-host-macros
cargo test --locked --workspace --features reel-player/window,reel-cli/window,reel-render/wgpu
cargo clippy --locked --workspace --all-targets --features reel-player/window,reel-cli/window,reel-render/wgpu -- -D warnings
```

The standalone contract checks the generated Rust call, named/default parameters, UTF-8, mutable context, property writes and mismatched declaration rejection. Existing Reel contracts cover source/ASB loading, node identity, activity control, terminal results and cancellation. These checks do not establish physical-device rendering or timing results.

Host properties can opt into stable bindings with `Heap::observe_host_properties(host, writable_count, readonly_count)`. This registers a writable prefix followed by read-only observable outputs in the field table; methods outside it remain ordinary fields. `PropertyId` identifies a host and field index. Constructors resolve a reference with `property_argument`, then retain the connection with `bind_host_property`. Script/control writes use `write_host_property`; authoritative read-only service updates use `publish_host_property`. Both validate all affected values before publishing. Use `require_writable_property` for control values that must write back. `retire_host` disconnects subscriptions and freezes surviving targets. Direct `table_store` remains a storage primitive for initialization and does not notify property bindings.
