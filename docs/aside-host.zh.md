# 从 Rust 扩展 Aside

[English](aside-host.md) · [文档目录](README.zh.md) · [Aside 语言规范](aside-language.zh.md)

`aside-host` 为嵌入 Aside 提供原生声明、Rust 值转换和属性模式，依赖 Aside 编译器和 VM。它重新导出的 `native` 和 `Properties` 宏使用 [Moxy](https://github.com/aacebo/moxy)，编译期依赖固定为 0.5.4。

## 导出 Rust 函数

```rust
use aside_host::native;

#[native(symbol = "sample::add")]
fn add(left: i64, #[aside(default = 2)] right: i64) -> i64 {
    left + right
}

let binding = add::binding();
```

宏保留 Rust 函数，并生成同名模块，提供 `SYMBOL`、`DOCUMENTATION`、`binding()` 和 `invoke()`。绑定包含 `NativeContract`、直接函数指针及函数的英文 `///` 文档。编辑器可调用 `NativeBinding::into_documentation()` 一次取得注册签名与 Markdown。可恢复的绑定在声明旁使用 `native_documentation!`。文档可写可执行的 `aside` 代码块，`aside_compiler::documentation::examples` 会把源码及行号交给验证工具。宿主在编译和载入时建立注册表，之后按准入的 `NativeId` 分派到处理函数。`invoke` 直接读取 VM 已绑定的实参。

`NativeContract::external()` 提供源码分析元数据。`aside_host::lower()` 生成字节码原生链接和默认值的常量池引用。`aside_host::runtime_support()` 按程序包解析宿主合同，结果传给 `Vm::load`。宿主选择程序包可调用的原生函数。

编辑器用这些元数据生成签名和只读 Aside 声明，保留参数的注册名称与字面默认值。Rust 声明可用命名宿主类型或联合类型等源码类型细化动态运行时值，属性声明还提供读写类型。这些信息用于语义成员查找、补全、悬停及跳转定义。生成声明共享英文源码文档，反映实际宿主实现。

生成声明中的引用保留注册符号身份。只读视图的跳转定义与悬停使用和作者源码相同的声明。

模块声明保留规范身份和源码文档，因此全局服务名称和显式导入都跳到同一个生成模块。属性模式标明其提供给节点构造器的参数。悬停可概述这些可选属性，补全、签名帮助及生成声明仍保留完整的参数合同。

命名宿主类型在源码分析、字节码生成和 VM 载入之间共用一份可信注册。每项注册指定规范名称、版本和基类型。Rust 通过绑定到堆的类型句柄为宿主值设置注册身份；Aside 注解和 `is` 检查该身份及其声明的基类型。同一注册类型的各个值共享祖先描述。VM 快照保留宿主身份，恢复时核对注册指纹。

`NamedType` 描述现有的宿主值。它的名称可用在类型注解和 `is` 中，构造器仍是注册函数。可调用与可等待的类型也通过同一份声明提供实际调用签名和等待结果。普通成员访问保留对象接收者。

`ModuleResolver::has_closed_members` 标识分析阶段能获得完整成员合同的类型，对这类类型访问缺失成员会产生源码诊断。解析器对未知或可扩展类型采用开放成员查找。Reel 与编辑器按注册的命名类型回答此查询。

`NamedType::registration()` 从类型已绑定的成员声明生成共享原生方法，包括继承和覆盖的方法。`HostTypeSupport::methods` 按公开名称、规范原生符号和 ABI 版本标识各方法。载入时将这些条目解析到程序包独立准入的原生可调用定义，并检查必需的接收者参数。较小的程序包可以省略尚未链接的原生方法。每种类型和代码租约只为共享方法表计费一次，构造宿主对象时只保存自有字段和名义类型身份。

成员读取（包括动态字符串索引）先查宿主自有字段，再查共享方法表。自有字段沿用已有验证器和写入规则，只有共享方法时该成员只读。取出方法会得到 `BoundNative` 值，通过 GC 和快照保留原接收者。仍存活的绑定可以复用，直接调用方法也走相同的绑定路径。不可变宿主值同样支持方法。共享方法声明发生变化时，恢复所检查的注册指纹也随之改变。

声明可以按常量参数细化结果。源码分析在绑定位置和具名实参后应用匹配分支，省略的实参采用注册默认值。动态实参保留声明中的完整结果类型。原生准入在各自边界检查源码签名和运行时 ABI。

宿主将声明纳入分析环境后，Aside 可使用位置、具名和默认参数，也可通过函数值调用。

```aside
fn main() {
    let sum = add;
    sum(right: 8, left: 4) + add(3)
}
```

`Vm::host_provided_arguments` 按源码求值顺序借出显式提供的实参及其解析后的参数索引。省略的默认值和已绑定的接收者不在其中，显式的 null 仍保留。`with_host_provided_arguments` 在提供同一份借用视图的同时允许访问可变堆，配置处理函数可直接处理传入的属性，省去额外的实参表。原生帧在续接、GC 和恢复期间保留这些值。

已有 VM 绑定器只绑定一次实参，生成的包装函数读取绑定值并调用 Rust。完整的[宿主扩展测试](../crates/aside-host/tests/extension/main.rs)包含模块初始化、字节码编解码、准入和分派。

```sh
cargo test --locked -p aside-host --test extension
```

## 状态、值与错误

### 原生函数的可变参数

`NativeContract::rest` 是可选的参数名称。编译器和运行时都使用普通数组 rest 参数。`#[aside(rest)] values: Value` 必须是最后一个脚本参数，并且没有默认值。原生包装函数接收 VM 为本次调用创建的外层数组，没有剩余实参时收到一个空数组。位置实参展开、函数值和绑定接收者共用 VM 绑定器和 NativeFrame。rest 名称为收集到的位置实参保留，用作具名实参时会报错。打包实参计入 VM 执行和堆预算。

```rust
use aside_host::{native, aside_vm::Value};

#[native(symbol = "sample::collect")]
fn collect(#[aside(rest)] items: Value) -> Value {
    items
}
```

Rust 通过 `FromValue` 接收数组值，可用 `Value` 或接受数组的拥有型转换。需要读取元素时使用 `Heap::sequence_len/get`。`#[aside(context)]` 参数不计入脚本参数顺序，可以放在 rest 参数之后。

函数必须是同步、安全、无泛型的自由函数，参数模式必须是标识符。`#[aside(name = "script_name")]` 修改脚本侧参数名。默认值是字面整数、浮点数、布尔值、字符串或 `null`，存于字节码常量池。`abi` 默认为 1。

可选的 `#[aside(context)] context: &mut CallContext<'_, Host>` 提供可变宿主状态、VM 和当前调用凭证。这个参数只出现在 Rust 签名中。

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

访问外部状态的函数要显式声明效果和所需能力。默认效果 `ALLOCATE` 适用于转换值或计算值，并且无需修改宿主或等待的函数。同步函数直接返回值，由包装层向调用凭证提交成功结果。`Result<T, E>` 将错误传给宿主分派器，`E` 必须通过 `From` 接受转换、堆和 VM 错误。分派器可用源码位置处的语言故障拒绝待处理调用，也可报告宿主内部故障。Reel 使用现有的 `AdapterError` 映射和拒绝路径。

内置转换支持 `i64`、`f64`、`bool`、`String`、`Value` 和 `Option<T>`。`()` 返回 `null`，`&str` 可作为字符串返回。数值保留声明的整数或浮点类型。`Option<T>` 在生成的源码声明中表示为 `T | null`，运行时 ABI 接受 `Any`，将 `null` 映射到 `None`，其余值通过 `T` 验证。其他拥有型 Rust 类型可实现 `FromValue` 和 `IntoValue`。

`String` 将 UTF-8 线性复制到 Rust 拥有的内存，宿主负责该内存。字符串返回值分配在 VM 计费的堆内。`Value` 只复制 VM 身份，跨原生调用保留值时需要显式 VM 根。上下文只在本次调用中借用。原生代码手动触发 GC 时，仍需保留的新增临时值须有根。大型或异步操作使用手写的可恢复原生函数，沿用 `NativeContract`、`HostTicket` 和 NativeFrame 协议，显式管理调度和取消。

## 绑定接收者

`aside_host::lower()` 为每个合同预先生成一个原生可调用定义，源码把函数当作值读取时编译器会复用该定义。宿主初始化时从已准入程序解析一次 `FunctionId`。`Vm::bind_native(receiver, function)` 创建不可变的可调用值，追踪接收者并保留代码版本和模块身份。原生函数必须声明一个可以接受该值的必需首参。

首参已预先绑定。其余位置参数、具名参数和默认值由 VM 绑定器处理，再次显式提供接收者会报参数重复。可将这个值存入宿主属性、传给脚本、通过 `start_closure` 调用，或作为原生 `CallScript` 回调。所有路径均进入 NativeFrame，执行声明的效果和等待权限检查。Rust 跨 GC 保留方法时要给它设置根。

已有属性的宿主对象可用 `Vm::bind_callable(object, function)` 获得调用能力。该方法把对象自身绑定为必需首参。可信注册独立于脚本可见字段，实参准备、入口调用、回调、GC 和等待均沿用绑定方法的路径。

Reel 通过这种方式提供节点的 `add`、`remove` 和 `clear`。提取方法会保留对应节点，退休时会清除节点的私有树边。

## 声明宿主属性

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

无字段枚举确定属性身份。声明顺序决定 `Field::index()` 和 `Field::PROPERTIES`。每个描述符提供名称、内置类型、默认值、可写性和动画标记。多词变体推荐显式写 `name`，省略时使用全小写的变体名。省略类型时为 `Any`，省略默认值时为 `null`。

`Property::parameter()` 从同一份声明生成构造参数。`Field::validate_host` 是静态 `HostPropertyValidator::new("sample.gauge", 1, Field::validate_host)` 的回调。将 `Some(&VALIDATOR)` 传给 `Heap::alloc_host`。保存快照的宿主还会调用 `registry.register_validator(&VALIDATOR)` 注册验证器的稳定名称和版本。宿主验证器接收 `(&PropertyRead, receiver: Value, index: u32, value: Value)`，跨字段约束可在发布前检查完整的更新候选。`Field::validate` 检查单个字段。可用 `validate = function` 提供签名为 `fn(&Heap, Value) -> Result<(), HeapError>` 的完整属性验证器，省略时检查声明的内置类型。只读写入在验证前即被拒绝。构造器负责初值、私有的追踪边和宿主身份。脚本值不存 Rust 地址。可运行的宿主示例构造宿主对象，并检查脚本读取、写入成功、只读拒绝和自定义数值验证。

Reel 用此模式定义通用节点构造参数、属性写入和补间字段。投影使用带类型的字段身份和 `reel-ui::stage::NodeProps`。

### 可观察属性

`Heap::observe_host_properties(host, writable_count, readonly_count)` 在字段表中注册可观察的可写前缀和随后的只读输出。范围之外的方法仍是普通字段。`PropertyId` 标识宿主对象和字段索引。

构造器用 `property_argument` 解析引用，再用 `bind_host_property` 建立连接。脚本和控件通过 `write_host_property` 写入，服务通过 `publish_host_property` 更新权威只读值。两种路径都在发布前验证全部受影响的值。需要回写的控件值使用 `require_writable_property`。`retire_host` 解除订阅并冻结存活的目标。`table_store` 直接写入存储，可用于初始化。绑定属性通过上述发布 API 写入。

统一更新多项服务值时使用 `publish_host_properties`。它与配置批次共用准备、连接约束验证和提交流程，同时允许为只读服务字段提供具体值。绑定输入仍要求普通写入权限。发布前准备好全部存储，更新被拒绝时保留原值。

需要限制单次准备工作量时，`begin_host_publication()` 创建与 `begin_property_batch` 相同的有根 `PropertyBatch`。保留其值和原始输入，然后使用 `property_batch_required_work`、`step_property_batch` 和 `commit_property_batch`。准备可按预算让出，原值保持原样。发布需要报告的完整提交额度。服务写权限随重启和快照保留，替换绑定仍需普通写权限。

`PropertySchema::documentation(index)` 读取每个变体的英文 `///` 注释用于属性悬停。文档保存在静态元数据中，节点值和保存的会话包含声明的属性。

## 可运行的宿主验证

[宿主扩展示例](../crates/aside-host/tests/extension/main.rs)演示注册、值转换、属性及回调，也涵盖 GC 和字节码恢复。

```sh
cargo test --locked -p aside-host -p aside-host-macros
```
