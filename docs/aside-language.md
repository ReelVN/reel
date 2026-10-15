# Aside VM 与完整语言规范
设计版本 1.1 · 开发基线，接口实现状态见正文。

显式 tuple 与 union 契约用于 `is`、绑定、共享捕获、参数和返回值；tuple 要求数量及逐项类型一致，union 接受任一分支。检查使用加载时计入容量的显式工作栈，执行前按类型 DAG 的工作上界计费，检查过程不分配临时容器。失败赋值不发布新值。
CellContract 的 type_id 为 Option<TypeId>：语言绑定保存 Some(包内类型)，独立宿主 Cell 保存 None 并服从自身的 builtin 契约；不会把宿主 Cell 的占位索引误当成当前包的类型。

已链接的 native 函数可以作为普通函数值保存、传递、返回和可选调用，也可以交给宿主的 CallScript 回调入口。B01 FunctionKind 新增 Native(NativeId)，编码为 tag 3 后接 u32 注册索引；签名必须与注册一致，不携带脚本指令、寄存器、绑定、捕获或异常区域，源码映射可在 PC 0 保存取值位置。普通调用、宿主启动和 CallScript 都直接建立同一种 NativeFrame，完整参数只绑定一次；缺省闭包与可变参数共享原来的 Cell，不经过脚本转发帧。aside-host 为每个合同预置一个 Native FunctionDef，供宿主绑定方法；编译器复用已有定义，仅在缺失且实际取值时新建。VM 在模块常量根区持有稳定函数值，MakeClosure 读取该常量而不分配对象，也不要求 ALLOCATE 效果。不添加作者全局变量。实际脚本调用处优先作为故障来源；宿主直接启动该函数值时使用定义中的取值位置（若有）。

[文档目录](README.md) · [作者指南 中文](guide/zh-CN.md) · [Creator guide English](guide/en.md)

## 一 定位与完成标准

Aside 是以 Rust 实现的完整动态嵌入式语言，Reel 是首个宿主。它承担剧情、业务状态、自定义 UI、组件、回调、异步流程、扩展脚本与工具逻辑；语言、编译器和 VM 不依赖窗口、媒体或特定舞台。工作区使用 aside-vm、aside-compiler，源码扩展名 .aside。本文定义最终目标语义；开发里程碑只记录实现状态，保持全部目标能力。

Quirrel 4.41，提交 19f17df9b6ee43c91ef5c635fd45970ec76c5dbe，是行为参照和测试资产。固定其源码、构建参数、许可和原断言；Aside 不承诺源码、字节码或全部行为兼容。逐调用默认值、每实例字段初始化、显式 self、强绑定方法、严格 bool、检查整数算术及普通表自身槽的插入/覆盖，均是为作者可预测性选择的有意差异，分别验收。不能为提高兼容率删除断言，也不能因旧语义不同而撤销已经确定的 Aside 规则。

完整目标包含动态值、共享对象与 cell、tuple、集合、文本和可变缓冲、函数与类、模式、模块、异常、迭代器、generator、协作 thread、Future、异步迭代、任务组合、标准库、宿主扩展和作者工具。Rust 的借用/生命周期语法、trait、泛型单态化不是目标；这些机制并非完成上述作者操作的必要条件。脚本堆采用单线程协作执行，跨线程交换拥有数据和稳定身份。执行器先实现解释执行，表示与优化可以演进，任何布局都必须保持完整 int64/float64 语义。

完成条件同时覆盖语言、标准库、扩展契约、调试工具和 Reel 作者场景。只运行一段剧情、只解析全部语法或只通过旧语言测试，都不算完整 Aside。

实现状态（L04–L09 / V03）：源码解析、语义、IR、B01 编解码和 VM 执行已贯通。当前支持普通调用/缺省、初始化局部绑定与共享捕获 Cell、异常/finally、tuple/array/table 与集合展开、索引/成员及混合赋值、可选取值链、位置调用 spread/rest、Array/Tuple for 和尾随 children 收集。通用 Host 支持稳定身份、共享属性表、被追踪的私有内容及退休；语言不依赖 Reel 节点类型。微小预算、交错 GC、缺失/null、非法键和取消检查实际执行。索引位置在右值前保存；可选链只在显式可选段检查 null，括号结束传播；rest 每次创建独立外层 array。

class/super、类/元方法 callable、完整模式与快照等仍按未实现边界拒绝。静态编译、受支持子集执行和完整语言验收分别记录。具体接口、资源边界及检查命令见[编译接口](../crates/aside-compiler/LOWERING.md)与[VM](../crates/aside-vm/README.md)。Reel 宿主不为 choose 或对白另设两项/两行语义上限，实际容量由宿主资源配置决定。

宿主可通过 AnalysisOptions.hosts 提供外围名称环境，默认为空。作者模块声明、局部绑定和显式 use 导入优先于外围宿主名称；use 与别名仍遵循普通模块规则。Reel 的编译入口以此提供当前注册的 reel::prelude，而独立 Aside 不含这些名称。此配置不改写源码，语义分析、链接和运行诊断沿用作者原始位置；语言与 VM 不依赖 Reel。


## 二 语法与求值顺序
没有普通参数时，`component { ... }` 等价于 `component() { ... }`；具名函数、局部函数值、成员和索引取得的函数均可使用。if/while/for/match 等头部仍保留后面的控制流块，确需尾随调用时用圆括号包住整个调用。该简写沿用同一调用与 children lowering，不增加 VM 指令。
尾随 children 块调用作为独立语句时，可以像控制语句块一样省略分号。例如 `column { text("标题"); }` 后可直接书写下一个组件。显式分号同样有效，且不会取消子节点收集。独立 children 调用后的换行结束该语句，下一行的括号不会被解析成继续调用；取值上下文仍可正常链接后续操作。其他普通调用、绑定和赋值仍使用分号。


函数统一写 fn；async fn 创建任务，gen fn 创建同步生成器。绑定使用 let 与 let mut，闭包写 |x| expression 或 || { statements }，模块导入写 use。除上文的独立 children 调用外，普通语句必须以分号结束，不做换行自动插入；函数、类及作为语句的控制流块后不要求分号。块末无分号表达式为块值，否则为 null；return、break、continue、throw、yield 以分号结束。

```aside
use reel::{character, narrator, story};

fn add(a: int, b: int = 1) -> int { a + b }

fn opening() {
    let mio = character("Mio");
    mio("先别开灯。");
    narrator("雨水沿着玻璃滑落。");
}
story.register("chapter01.opening", opening, title: "窗上的光");
```

标识符、源码、注释和文本使用 UTF-8；标识符采用固定 Unicode 版本的 XID 规则并允许 _，名称按实际字符序列区分。注释为 // 与 /* ... */，块注释可嵌套。字符串使用双引号，支持反斜线、双引号、n/r/t/0 及 u{十六进制标量} 转义，非法标量报错。f"音量：{volume}" 为插值，{{ 与 }} 表示字面花括号。三双引号包围多行文本，保留换行且不自动缩进裁剪；前缀 r 禁用转义，f 启用插值，二者不混用。数组为 [a, b]，对象为 {name: "Mio", [key]: value}，tuple 为 (a, b)。列表允许末尾逗号。

{} 在要求表达式的位置为对象，在 fn、if、循环、try 等要求块的位置为块。|| {...} 是闭包块，返回对象写 || ({name: "Mio"})。match 分支结果位置的最外层裸 {...} 同样是块，末尾表达式提供分支值；对象结果写 _ => ({name: value})。闭包参数类型中的 union 使用括号，如 |x: (A|null)| body；参数列表最外层的 | 固定为结束符。存在语法歧义的独立对象表达式必须加括号；解析器不按运行结果猜测。if/while/for/match 头部及 class 的基类表达式最外层关闭尾随 children 调用，使其后的花括号属于控制流；确需尾随调用时用圆括号包住该表达式。对象/数组展开分别写 {...base, score: 1}、[...items, last]，均按源码顺序复制条目/元素，保留内部引用。对象展开只复制自有槽，不复制 delegate，后项覆盖前项。

if condition { ... } else { ... }、while condition { ... }、for pattern in values { ... } 是基本控制流；if 作为取值表达式必须有 else。loop { ... } 为无限循环。break/continue 默认指向最近循环，可通过 label: while ... 与 break label; / continue label; 指定外层循环。循环本身不产生隐式结果。for 每轮创建新绑定，闭包保留本轮值。a..b 不含终点，a..=b 含终点，均产生整数 Range；步长由 range(start, end, step:, inclusive:) 提供，零步长报错，方向与端点不符为空区间，推进溢出在越界前终止。

一般表达式按源码从左到右求值一次；短路、条件和模式有各自明确规则。赋值先计算并保存目标对象与键的值，再计算右值并写入；右值随后重绑原对象或键的名称，不改变已经准备的位置；复合赋值先计算左值位置一次，再读取、求右值、计算和写入。赋值链 a = b = value; 作为一条语句，先从左到右确定所有目标位置，末尾 value 求值一次，再按从内到外的顺序写入。整条链无表达式结果；算术、参数、let 初值等取值位置使用赋值会诊断。每个解构目标按自身的原子准备/提交规则写入，整条链沿用普通副作用规则。求值错误不会自动撤销已经发生的普通副作用；有事务语义的宿主接口另行声明。

当前源码执行支持 `a..b`、`a..=b` 的惰性整数区间，`for` 直接读取迭代结果，不为每轮构造临时 tuple。`range()` 的显式步长接口仍是待实现标准库。字符串插值按书写顺序求值，当前支持 null、bool、int、float 和 string；容器/实例自定义字符串转换另行实现。插值使用同一个分块缓冲区，按字节计费与暂停，完成 UTF-8 验证后直接封存；每轮不复制已构造的前缀。array/tuple/table 的 `len()`、array 的 `append(...values)`/`pop()`、table 的 `has_own(key)` 已接通；方法取值保留原接收者，表自己的同名字段优先，表中的普通闭包不注入接收者。空数组 pop 抛可捕获错误，append 按实参顺序追加并返回原数组。

## 三 值 绑定 相等与数值

值包括 null、bool、int64、float64、不可变 UTF-8 string、不可变 Bytes、tuple、array、table、class、instance、closure、generator、thread、Future、weakref 及注册宿主值。TextBuffer、ByteBuffer、Set、Range、Iterator 等是完整标准类型；它们是否占据独立 Value 标签不影响语言地位。

let 只禁止重绑名称，不冻结对象；let mut 允许重绑。参数默认不可重绑，mut 参数允许重绑。词法作用域允许内层遮蔽；未声明或尚未初始化的名称读取报错。可变捕获由所有闭包和原作用域共享 CellId；不可变捕获可按值保存，所指对象身份继续共享。array/table/instance 等身份对象的赋值和传参不隐式复制。

```aside
let state = {score: 0};
state.score = 1;
let mut count = 0;
let next = || { count += 1; count };
```

标量按值相等，array/table/instance/closure/Future 等身份对象按身份相等；tuple 按第五节递归规则相等。bool 不与数字相等。int/float 混合比较按精确数值关系，不能先把大 int 舍入成 float；+0.0、-0.0 与整数 0 相等。NaN 不自等，Infinity/NaN 是合法浮点值，具体 NaN 位型和对象内布局不是语言可观察身份。

条件、!、&&、|| 只接受 bool；逻辑运算短路并返回 bool。?? 仅在左值为 null 时计算右值。?.、?[...]、?(...) 只在接收者为 null 时短路，该段及其后同一可选链不求索引/实参；括号结束链传播。非 null 接收者的缺失字段仍报错，不能吞掉 metamethod 的异常。

int 加减乘、负号及整数除法溢出报错；MIN/-1 报错。int/int 的 / 向零截断；任一操作数为 float 则按 float64 运算。% 是与向零除法一致的 remainder，div_euclid/rem_euclid 提供欧几里得规则。数字除零含浮点 ±0 均报错，其余 float 运算允许产生非有限值；宿主属性可另行要求有限值。+ 只接受数值对或字符串对，字符串化使用 str()/插值。

位运算使用 64 位补码；移位量须为 0..63。>> 算术右移，左移按位截断，逻辑右移使用标准函数 unsigned_shr。位移不假装算术乘法。字面量支持十进制、0x、0o、0b 与分隔下划线；超出 int64 范围时报错，负最小值按整体常量处理。浮点比较、解析、舍入、格式化及常量折叠与运行时一致。优先级从高到低为：成员/索引/调用，前缀 await ! ~ + -，* / %，+ -，<< >>，&，^，|，< <= > >= 与 is，== !=，&&，||，??，.. / ..=，yield，赋值。二元运算左结合，?? 与赋值右结合，比较与区间不允许无括号连用。is 只测试类型，不转换值；赋值无可用于运算的结果。parser 与非法类型测试直接依据此表。

## 四 对象 集合与键

固定 array/tuple 的元素按源码从左到右各求值一次，再构造容器。固定 table 字面量的每个 key/value 同样按源码顺序各求值一次，全部表达式完成后才按有序字段描述建表；命名字段的 key 是对应字符串。不可散列键等建表错误发生在这一构造阶段，不提前跳过后续字段表达式，也不撤销此前已发生的普通副作用。重复键按字段顺序处理，后值覆盖，首次插入位置不变。编译器保留有序键值对，不因静态看似重复而删除表达式求值。

array 为可变有序序列，索引只接受非负 int，越界报错，不把 float 截断或把负数解释为倒数索引。标准操作包括 len、append、extend、insert、remove、pop、clear、slice、find、稳定 sort、浅 clone 与显式 deep_clone。普通 slice 创建新的外层序列、共享元素引用。

table 为按插入顺序迭代的动态关联对象；点访问等价于字符串键读取。读取先查自有槽，再沿无环 delegate 链查找，最后按第九节执行缺失钩子。普通 table 的 = 总在自身插入或覆盖槽，不修改 delegate；覆盖不改变原位置，删除再插入移到末尾。remove 删除自有槽；set_existing 要求自有槽存在，define 要求自有槽不存在。raw 操作绕过动态钩子。

has_own 只检查自有槽；has_in_chain 只检查自有与 delegate 链的真实槽，不执行 _get；try_get 执行正常读取并返回 (found, value)。get(key, default: null) 只在缺失时返回缺省，存储的 null 是已找到值。keys/values/entries 明确只枚举自有槽，entries 产生 (key, value) tuple。Map 为 table 的标准库命名别名/构造入口，不建立第二套隐含不同的映射语义。

可散列键为 null、bool、int、除 NaN 外的 float、string、Bytes 及递归可散列 tuple。等值键必须同 hash，包括精确相等的 int/float 与正负零；可变对象及含它们的 tuple 不能作为普通键，错误指出元素路径。Set 使用同一键规则与插入顺序，提供 membership、add/remove、union/intersection/difference。IdentityMap/IdentitySet 另行按对象身份存键，适合对象索引；其键为强引用，弱键版本明确命名且不保证枚举稳定。弱集合及 weakref 的保活、失效和快照使用第十八节规则。

内部散列种子及桶布局不对脚本暴露；可持久数据摘要使用规定编码的 digest API。深复制保留新图内部的别名与环，遇不可克隆宿主值报引用路径，不能用 JSON 代替通用复制。deep_equal 是显式、受预算的结构比较库操作，不修改普通 ==。

array/table/Set 的默认迭代器记录结构版本；增删或改变位置使下一次推进报 ConcurrentModification，覆盖已有位置的值不改变结构版本，尚未访问位置可看到新值。稳定遍历使用显式副本。for 体等待时其他任务的修改同样适用，不能把单线程理解成没有交错。容器 schema、顺序、结构版本和游标进入快照。

## 五 tuple 的值语义

(a,b) 是二元 tuple，(a,) 为一元，() 为零元，(a) 为分组。空 tuple 与 null 不同；普通函数无返回值仍返回 null。tuple 数量和位置是值结构，元素表达式从左到右求值一次，原动态类型不被数值统一化。其槽不可重写、增删或通过解构反向修改。

```aside
fn measure(node) { (node.width, node.height) }
let (width, height) = measure(view);
let items = [];
let pair = (items, items);
pair[0].append("shared");
```

tuple 是浅层不可变值，例中两项保持同一个 array。赋值、传参及返回不深复制元素。实现可以内联或共享不可变 tuple 存储，但不暴露 tuple 身份；identity_eq 和 weakref 不接受 tuple。tuple 可经由可变对象间接参与环，GC 和快照必须保留这些引用。

同长度 tuple 按顺序用元素普通 == 比较，嵌套 tuple 递归；可变容器/实例元素仍按身份。不同长度不等，tuple 与 array 不等，() != null。包含 NaN 的 tuple 可不自等，不能只因内部存储句柄相同就直接返回 true。相等使用受预算的显式栈，不依赖无限 Rust 递归。

所有元素递归可散列时 tuple 才可作为普通键；长度、顺序与元素 hash 共同参与。可缓存不可变内容的内部 hash，但不得将其当跨进程稳定身份。包含 array、table、instance、closure、Future 或 NaN 时拒绝散列并给出路径。

t[i] 使用从零起的 int 索引，负数及越界报错；t.len() 返回固定长度；slice(range) 返回 tuple；遍历按位置顺序。所有读取和切片继续共享内部对象引用。

let (a,b) = value 只匹配 tuple 且默认精确数量；let [a,b] 只匹配 array。允许嵌套、_ 和一个尾部 ...rest；tuple rest 是 tuple，array rest 是 array。tuple 不用缺省值补不存在的位置，需可选值时显式返回 null，需可变长度时用 array。

多返回值始终是一个 tuple：return (a,b);、yield (a,b); 和 async 返回 tuple 均交付一个值。f(pair) 传一个参数，f(...pair) 才展开位置参数；不会隐式取首项或按表达式所在位置自动扩成多个结果。优化可用多寄存器传递，但动态调用、快照、异常和调试观察必须等价。

### 集合操作的共同约定

array/tuple 的 `slice(range)` 使用非负索引，保留原容器种类和元素引用；范围端点必须在容器边界内。array 提供 `insert(index, value)`、`remove(index)`、`resize(length, value: null)`、`reverse()`、`extend(items)`、`clear()`、`top()`、`swap(a, b)`。insert 允许在长度位置追加；remove 返回被移除的值，top 返回末项且空数组报越界；其余修改方法返回原数组。extend 接受 array/tuple，自身展开只捕获调用开始时的元素。table 提供 `keys()`、`values()`、`entries()`，按自有字段顺序返回新 array；entries 的元素为 `(key, value)` tuple；`clear()` 返回原表。

这些方法支持其声明的具名参数；绑定错误发生在全部实参求值后、任何修改前。批量编辑通过私有工作副本按元素推进，完成后一次提交；中途异常、取消或来源内容变化不发布半成品，内容变化报告 ConcurrentModification 并保留实际来源。工作副本与旧内容共享子对象，提交保持容器身份；旧存储交给增量 GC 回收，不在提交点线性析构。append/pop 保持已有逐项行为。

## 六 Unicode 文本和可变缓冲

Reel 的 `game.exit()` 是 native ABI 1 的宿主退出请求：接收后不再运行脚本，任务与资源按宿主关闭协议清理。Aside VM 不引入平台退出操作，B01/ASB 编码不变。

string 为不可变、有效 UTF-8 文本；相等按精确文本序列，不隐式规范化。API 明确区分 byte_len、scalar_count、grapheme_count、bytes()、scalars()、codepoints() 与 graphemes()：bytes 为字节，scalars 为单 Unicode 标量 string，codepoints 为整数，graphemes 为字素 string。普通 string 不提供含糊的 s[i]；使用显式视图或 slice_bytes/slice_scalars/slice_graphemes。

文本基础方法：`byte_len()` 返回 UTF-8 字节数；`scalar_count()` 返回 Unicode 标量数，组合字符与 ZWJ 序列按各自标量计数。`slice_bytes(range)` 和 `slice_scalars(range)` 返回不可变 string，使用非负、从零开始的范围，不截断越界输入；倒置范围或非法 UTF-8 字节边界报 OutOfBounds。`starts_with(prefix)`、`ends_with(suffix)` 精确比较文本，空前后缀总是匹配，不做规范化。`separator.join(items)` 接受 string 的 array/tuple，只在相邻项之间插入 separator；空输入返回空 string，非 string 项报 Type，不隐式格式化。这些方法支持声明的具名参数。字素方法仍是后续能力，不能将标量数冒充显示字符数。

to_ascii_lowercase()/to_ascii_uppercase() 返回新字符串，仅转换 ASCII 字母，保留其他 UTF-8 字节。实现复用 Rust 标准库字节切片转换与现有分片文本缓冲，按字节线性推进并计入工作/分配预算；空字符串也返回字符串。它们不执行 Unicode 或语言环境相关大小写折叠。B01/ASB 不变。

计数、切片和前后缀比较按字节分片；byte_len 为常数成本，scalar 切片单次前向扫描定位后复制。join 只向一个私有缓冲顺序追加，每个输出字节复制一次；空项也按元素计费。来源数组在执行片之间变化报告 ConcurrentModification；失败、取消不发布部分结果。UTF-8 解码与标量迭代复用 Rust 标准库，VM 仅管理分块游标、预算与根，不维护另一套 Unicode 解码规则。

所有范围从零开始，端点规则与 Range 一致；字节切片端点必须在 UTF-8 边界，否则报错。UTF-8 解码默认严格，替换非法输入需显式模式。Unicode 库版本属于包兼容元数据。normalize、大小写转换、trim、find/replace、split/join、parse/format 均定义输入单位、边界和错误；默认大小写转换不依赖进程 locale，本地化规则通过显式 locale 参数选择。字素不是字体布局 cluster，Reel reveal 由文本服务将规定揭示单位映射到稳定排版。

TextBuffer 提供 append、insert_bytes/remove_bytes/replace_bytes、对应字素操作、clear 与 to_string。文本编辑不能产生非法 UTF-8；buffer 是共享身份对象，to_string 产生不受后续编辑影响的不可变值。任何内容改动使其既有文本游标失效。缓冲自身不自动可观察，UI 更新经 model 写回或显式可观察包装。

Bytes 为不可变字节值，按内容相等与散列；ByteBuffer 为共享可变字节序列，支持 0..255 字节访问、范围编辑、显式端序数值读写与编解码。普通 slice/to_bytes 的结果不受后续缓冲编辑影响；需要共享视图时使用明确的 view 类型与版本检查。脚本和 native 都不得把借用裸指针跨重入、扩容或等待保留。快照保存有效内容和别名，不保存裸地址或未用容量。

大型编码、搜索、规范化、替换、hash、equality 与复制都按字节/元素计费；支持分块 continuation，或由明确输入上限保证不可分割调用有界。append 不能仅靠整串复制掩盖平方成本，基准包含长对白、组合字符、ZWJ emoji、二进制 NUL、非法编码和保存后继续编辑。

## 七 模式和分支

模式统一支持名称、_、字面值、tuple、array、对象字段、嵌套、尾部 rest、类型测试和 OR。类型测试写 pattern is Type，OR 写 left | right，is 结合更紧；类型测试中的 union 用括号明确，如 value is (A|null)。() 是零元 tuple 模式，单项圆括号是模式分组，含逗号是一元或多元 tuple 模式。同一分支名称不得重复，OR 的各侧必须绑定相同名称。对象模式默认只要求列出的自有字段；{name, score: points = 0, ...rest} 在 score 缺失时才求缺省，显式 null 不触发；rest 为其他自有字段的新 table。tuple/array 默认精确长度，通过 rest 明确接收剩余项，不静默丢弃或补 null。

```aside
let result = match event {
    {kind: "click", position: (x, y)} if x >= 0 => hit(x, y),
    {kind: "key", key: "Escape"} => close_panel(),
    _ => null,
};
```

match scrutinee 只求值一次，按源码顺序匹配，guard 要求 bool，命中后只执行一个分支。guard 不可外部等待；其显式副作用不会因不命中而回滚。动态输入未匹配抛 MatchError，有限已知类型可作穷尽诊断，默认鼓励 _ 分支。if let/while let 使用同一模式规则。

执行子集支持 match 的名称、通配符、字面值、分组、类型测试和 OR，以及 guard、分支取值和分支内控制流；结构解构及 if let/while let 仍待接通。分支绑定按每次进入创建，闭包保留当次值。匹配降低为共享比较、类型检查和控制流指令，字符串比较沿既有可恢复执行器运行；无分支命中由 MatchFail 指令产生 MatchError，源码和 ASB 行为相同。示例见 [match_values.aside](../examples/match_values.aside)。

结构匹配只读取真实自有数据槽或注册的纯结构字段，不执行任意 _get、外部操作或等待。匹配失败不是异常；let 解构失败为 PatternMismatch。模式先校验结构与必需项，再按模式书写顺序求缺省，完成后一次建立绑定；失败不创建部分名称。解构赋值只允许已有的可重绑词法名称，先准备所有结果再提交，不能通过任意 setter 假装原子更新。全部模式遍历和临时值受预算且可保存。

## 八 函数 调用与类

类执行使用 B01 `Class` 描述及共享调用栈；声明身份记录为模块、名称和内容版本。类布局在创建时建立，实例只保存自己的字段槽。构造、字段初始化和布局建立服从执行预算，字段值及绑定方法的接收者由同一 GC 追踪。类调用、方法和普通函数共用参数名、缺省、类型与效果检查。

块内 fn 声明先建立只读函数绑定，允许自递归和互相递归；捕获普通 let 的初始化状态仍受检查，不得把未初始化值视为 null。命名参数适用于脚本闭包和 native，使用同一签名描述与绑定器。位置参数在命名参数前；fn f(a,b=1,...rest) 支持每次调用新建外层 array 接收尾部位置参数，没有剩余项时也新建空 array。rest 注解约束整个 array 绑定，不隐含逐元素约束；确定与 array 不相容的注解给出诊断。rest 名称不能以具名实参供给，也不能被 PropertyBagTail 静默接收为同名属性；其他普通尾袋属性保持原合同。

callee、所有实参、展开和 children 按源码顺序各求值一次，然后绑定签名；重复名、未知名、多余或缺失参数报错。缺省在每次调用中按参数顺序求值，可引用前面参数，不可引用后面参数；不允许外部等待/yield。缺省的块表达式以块尾值产生参数结果；显式 return 的词法范围从普通函数体开始，缺省边界内的 return 给出 InvalidDefault 诊断，即使位于条件或短路分支也一样。缺省内定义的嵌套函数按自己的函数边界正常 return。break/continue 仅作用于缺省内部建立的循环，throw 沿参数准备规则传播。实际参数全部先求值，再运行缺省，children=[] 每次独立。

`f(...sequence)` 展开 array/tuple 为位置参数；`f(...options)` 展开 table 的自有条目为具名参数，键必须是 string，不读取 delegate 或调用用户代码。`f(...positional, ...named)` 直接支持函数值、脚本函数、宿主函数与绑定方法的完整转发，不另设 call 包装接口。浅捕获在展开处固定外层条目，内部对象引用仍共享；分片期间源被修改时报 ConcurrentModification。非法源、非字符串键、复制和额度错误发生在展开阶段；动态顺序错误、重复名、参数槽冲突、缺参和缺省处理发生在全部实际参数完成后的绑定阶段。具名参数之后不能出现位置参数，空展开不改变顺序；具名展开可以接在显式具名参数之后。覆盖样式先写 `{...defaults, ...style}`，同一次调用重复供给参数始终报错，不隐式覆盖。尾随 children 仍是最后一个具名参数。


class 使用单继承，命名声明写 class Child: Parent { ... }；匿名类表达式写 class { ... } 或 class: Parent { ... }。命名声明建立词法绑定，该绑定可直接作为类值使用；匿名类表达式创建并返回新的类对象。实例有声明字段，let/let mut 表示只读/可写；字段初始化每实例按基类到子类执行。未给初值的只读字段必须由构造过程赋值一次；结束时验证完整，实例状态为 Constructing、Ready 或 Failed，并记录已初始化字段。成功返回 Ready；异常结束标记 Failed。显式发布过的引用仍保持身份，但对 Failed 实例的普通操作报告 ConstructionFailed，诊断/status 查询保留可用；Constructing 中读取未初始化字段报告 UninitializedField。普通已经发生的副作用沿用语言规则，构造失败不伪装成外部事务回滚；快照保存构造状态、字段位图与执行帧。owner/Region 硬撤销或 invoke_sync 预算终止同样由 VM 清理把未完成实例标记 Failed；GC/快照只追踪已初始化槽。全部别名与既有绑定方法读取同一实例状态，方法调用也执行 Failed 检查。构造方法 fn init(self, ...) 显式首参 self；类调用创建实例并调用最近的有效 init，派生类未声明 init 时继承基类构造。声明派生 init 时只调用它，super.init(...) 显式初始化基类，禁止同一基类 init 重复执行。字段初始化不会因 super.init 重跑。

方法调用提供接收者一次；方法取值形成强保留接收者的绑定闭包，普通表中的闭包不自动注入 self，普通函数不继承调用者的隐式 this。super.method 固定从词法基类查找并保留同一个接收者。每次方法取值可以创建新闭包，不保证两次取值身份相等；事件注销依赖 Subscription token。static let/static let mut 与 static fn 属于类，static fn 无 self；只读 static 初始化一次。class 可作为值和局部声明，继承图不得成环，公开字段与方法反射不暴露内部布局。

## 九 delegate 与扩展操作

set_delegate/get_delegate 管理 table 的显式后备链，拒绝环，改变链或槽时更新查找版本。动态成员缓存必须包含布局/查找版本。普通 table 自身槽的插入/覆盖不因存在 delegate 而变为修改父表。

元方法查找使用 raw 规则，不递归触发自身钩子。VM 调用元方法时显式提供 receiver；表中作为钩子的普通闭包也须声明 self，类方法由绑定器注入一次。核心协议为：
- _get(self,key) 返回 (found: bool,value)，只用于真实槽查找失败；found=false 是缺失，真正异常照常传播。
- _set(self,key,value) 用于 class/native 显式声明的虚拟属性或代理写入口；普通 table 写自身槽不调用它。没有此入口的 instance 不能写未知字段。
- _call(self,...args) 是普通动态调用入口，继承当前 Task、owner、RegionId 与效果上下文；普通任务中可以挂起，受限上下文仍按原边界执行。命名参数和 children 依据它的实际签名绑定，receiver 注入一次，signature(callable) 隐去已绑定 self；_iter(self) 返回 Iterator；_tostring(self) 必须返回 string。
- _add/_sub/_mul/_div/_mod(self,right)、_neg(self) 扩展对应运算；仅在内建类型组合无定义时尝试左操作数钩子，未定义则报错，不再猜测右反射钩子。
- _cmp(self,right) 返回负/零/正 int，为 < <= > >= 提供比较；不改写 == 的身份/值规则，不成为普通键的可变 hash 来源。

除普通调用入口 _call 外，上述钩子、构造和属性访问不允许外部 Pending，但普通可恢复栈仍可 BudgetYield。递归钩子有调用深度与工作量上限。注册新的 host 操作应使用版本化描述，不靠随意新增同名魔法槽改变既有语法。显式 deep_equal、key extractor 等提供业务比较，无需覆盖全局相等规则。

## 十 模块 类型与反射

use reel::{stage,text}; 为分组导入，use game::ui::dialogue::dialogue_view; 为单项导入，use reel::prelude::*; 为公开名字通配导入。别名用 as，pub 控制导出，pub use 重导出；命名空间导入使用 use game::ui::dialogue as dialogue;，解析器依据模块/符号声明区分，若两者同名则要求显式分组选择符号。通配不能静默覆盖已有名字。

入口源码所在目录是源码根；工程目录默认启动 main.aside 的 main。game:: 后的模块段对应目录和 .aside 文件，名称须合法标识符。外部源码根可由工具的 --module 别名=目录 参数提供，同一实际文件只建立一个模块环境。main/game/reel/std 保留；reel 与 std 由宿主注册，不搜索同名源码目录。启动函数可以私有，这不使其余私有符号可被导入。use 在编译时解析，构建产物保存确定的模块集合。

当前普通函数与值模块已贯通多源码分析、B01 和 VM。文件提供方按真实导入发现源码，模块别名归一到固定声明身份；同一文件只产生一份环境。导入读取与重导出共享原 cell，B01 的模块绑定编号依次包含自有全局和导入，验证时解析并缓存实际 cell 索引。物理来源独立保存在 SourceFile.path，运行故障保留对应文件与表达式范围。

先链接模块强连通分量的函数/类型符号。SCC 凝聚图按依赖先于使用者的拓扑顺序初始化；同一 SCC 内及互不依赖的并列 SCC 用规范化模块路径稳定排序，再执行顶层表达式。链接不执行 class 字段初始化、extends 求值或任意表达式；未创建完成的 class 不可实例化。读取未初始化绑定报带实际链路的 ModuleInitializationCycle；可链接的函数互相引用不被 import 图的环一概禁止。模块初始化不得外部等待。

模块实例包含固定版本的导入绑定和私有全局。发布单元为不可变 ModuleSet 描述，所指环境中的可变业务值仍按对象语义变化。会话由宿主从确定代码集合创建独立模块环境。修改模块时构建它及全部反向依赖的新实例与导入，再原子发布；其余不可变内容可共享。旧闭包/帧保留旧代码与原绑定，不自动跳入新环境。

候选初始化仅可分配/写候选对象，并调用标为 InitSafe 的宿主操作；禁止修改旧会话、全局设置、外部资源。候选失败释放候选图，活会话不变。Reel story.register(id,entry,title:) 为普通 InitSafe 登记 API，id 为唯一稳定 string，entry 为模块级、零必需参数普通 fn；发布前检查冲突，普通调用仍保留原闭包版本，不以登记替换函数机制。

类型注解支持 any、基础类别、class/native 名称、T|null/union、固定 tuple (T,U)、函数签名。函数类型写 fn(T,U) -> R，箭头右结合，返回类型可为 union；函数类型本身参与 union 写 (fn(T) -> R)|null。() 为零元 tuple 类型，单类型圆括号作分组，含逗号为一元或多元 tuple 类型。注解不改变动态存储，在能确定时静态诊断，否则参数进入/返回及声明写入时验证。array/table 类别注解不谎称元素永久受保护；强约束容器使用显式 schema 容器构造，每次脚本/native 写入都验证，别名共享同一 schema。不能对普通共享容器检查一次就宣称永久 Array<int>。

type_of、is、signature、members、own_fields 提供一致反射。签名包含参数名、顺序、缺省是否存在、变参、返回契约和效果，不因反射而执行缺省表达式。native 与脚本类型遵循同一描述。反射不泄露原生地址、任意私有环境、恢复 PC 的写权限或绕过字段/schema 的写入口。

## 十一 children 与可观察绑定
当前实现增量贯通尾随 children、array/tuple 的 `for` 与可组合宿主对象。宿主通过通用的 composable 标记声明可收集对象；Aside 不依赖 Reel 类型或平台库。宿主对象有保持身份的公开字段表和私有载荷，成员读取/赋值访问同一字段表，私有载荷仅由宿主访问。公开字段参与 GC，私有载荷中的值同样可追踪。children 收集沿用本节 Node/null 规则，并保持 NoWait 边界；普通 helper 不继承调用者的收集器。for 的每轮词法绑定独立，array 结构变化遵循集合迭代规则。新增字节码行为直接进入当前 B01 实现与执行能力表，不引入旧实现兼容路径。


尾随 children 块是通用调用语法：立即计算名为 children 的 array 参数，不是隐式闭包、不按帧重跑。块内独立表达式语句产生 Node 时按顺序收集，分号不取消收集；块末 Node 只收集一次。null 忽略，其他值报错。let 初始化与赋值不收集；if/for/while/match/普通语句块继承词法收集位置，控制流整体不再次收集末值。嵌套尾随块建立独立收集器，被调用函数不继承调用者收集器。显式 children 与尾随块不能共存，事件回调必须命名传入。

```aside
fn panel(title, children = []) {
    column(gap: 12) {
        text(title);
        for child in children { child; }
    }
}
let view = panel("声音") {
    slider(value: bind(settings.music), min: 0, max: 1);
    text(value: derive(|| f"音量：{settings.music}"));
};
```

children/构造不得外部等待，BudgetYield 时临时构造帧可保存。Node 返回时未挂载，单父和事务挂载由宿主验证。

bind(model.volume) 是编译器识别的可观察属性位置表达式，生成 Read 或 ReadWrite PropertyRef；对象与键只在创建时求值一次，后续固定该位置。宿主注册稳定属性身份、读取、可选写入与版本。尚未纳入 model 的普通 table 字段、临时值和 a+b 不可 bind；Reel model({...}) 与 observable collection 提供业务可观察状态。需要选择另一个位置时重建引用。

宿主属性绑定的执行合同：`bind(node.x)` 与 `bind(node[key])` 固定接收者和键；引用保留该位置的读取及写入权限。节点构造参数或属性赋值接受引用时建立连接，之后写入普通值沿连接写穿。`unbind(bind(node.x))` 断开该位置的连接，返回并保留当前值。连接成环、不可观察位置、失效来源和只读写入均报错。变更先验证受影响属性的完整候选值，再一次提交；校验失败不改变任何值或连接。显式销毁来源时，现有连接冻结最后有效值并断开；已有引用再使用时报告来源已销毁。来源只通过显式退休销毁，普通 GC 保留仍被引用的来源。动画按最终来源取得写入权，重绑定或解绑使依赖旧连接的活动停止。


节点公开只读可观察 `hover`、`pressed`、`focused`、`disabled`。hover/focused 汇总节点及后代；pressed 汇总指针捕获，并包含按钮自身的键盘/手柄确认按住状态。disabled 反映继承祖先禁用后的有效值。隐藏、禁用、失焦及退休清除交互中的状态；状态在调度业务回调前发布，展示节点可以直接 bind，无需回调或逐帧脚本。`decorated: false` 关闭按钮默认焦点框、开关默认指标及其留白，保留交互和普通 children；作者用文字、图片、背景、变换及绑定组合外观。构造参数 decorated 默认 true，当前节点 native ABI 为 3，B01 不变。

`model(defaults)` 由 Reel 提供，复制当前支持的纯数据图，保留内部共享引用和环。返回的 table 仍支持普通索引、追加字段和集合方法，其字段可以 bind；嵌套 table 及后来赋入的数据对象保持同一观察身份。clear 删除字段时，旧连接冻结最后值，旧引用失效；重新增加同名字段需重新 bind。默认图不接受闭包、任务、节点等宿主资源；宿主通过 alloc_immutable_host 声明的不可变数据值（如 rich）按身份共享，其内部图的不可变性由宿主保证。复制按 VM 原生 continuation 预算推进，失败或取消不发布半成品。

derive(|| expression) 创建只读 DerivedBinding，记录实际可观察读取，版本变化后重算并替换条件依赖集合。禁止修改外部对象、可观察状态、启动活动或等待；native 必须 DerivedSafe，传递调用中继续检查。内部临时分配/写入允许，不能把临时引用逃逸成外部可变影响。普通捕获值保持通常共享语义，不因读取自动响应。环报错，替换/销毁解除订阅。移除最后一个属性消费者时，沿派生输入解除无消费者的订阅；重新绑定保留引用会恢复更新，共享消费者不受影响。输入宿主退休导致的冻结不可恢复。可写控件不可接受只读派生，通知/事务提交由宿主负责。
派生结果沿用只读属性引用，可通过 `binding.value` 读取并继续派生。依赖变化合并到同步脚本段结束后的提交点，在下一次舞台发布或业务任务运行前稳定；不在每帧扫描所有绑定。首次计算沿原调用栈执行，后续计算保留闭包的源码位置。失败保留上次有效投影并报告脚本故障。


Reel 的 `slider` / `toggle` 是普通宿主构造函数。可写 `value` 保存当前数值/布尔值；用户输入先写入，再以普通事件 `{target, value}` 调度 `on_change`，脚本赋值不触发回调。宿主属性校验可读取接收者其它字段，跨字段约束在写入前完成；错误沿原赋值源码位置报告。`bind` / `derive` 的语言设计不因控件引入而改变。

`view(build, ...布局属性)` 返回稳定的普通容器。build 是不可等待的组件工厂，首次调用及其实际读取的可观察数据变化后生成一个 Node 或 null；成功时替换容器的旧内容并取消旧节点拥有的任务。构造失败保留旧内容并报告源码错误。该接口适合条件面板、选项与对白页；只需要更新属性时使用 bind/derive，避免重建节点。build 可以调用普通组件函数，计算依赖在每次成功构造后替换；构造期间再次修改自己读取的来源报循环错误。

`list(items, key:, build:, ...布局属性)` 返回稳定的纵向容器。items 是不可等待的数组来源函数；key(item) 返回本次列表中唯一的值：标量保留类型，字符串按内容比较，其余对象按身份比较；对象键须复用原对象引用，null 也可作键。来源及 key 的实际可观察读取触发协调；build(row) 只在新 key 出现时调用，返回 Node 或 null。row.key/value/index 是只读可观察字段，value 为最新条目，index 从 0 开始。保留 key 的排序、数据替换不重建节点；读取最新记录的回调用 row.value，需要跟随记录替换的属性用 `derive(|| row.value.label)`。`bind(row.value.label)` 仍固定创建时的字段位置。build 中的普通读取不成为列表或外围 view 的依赖，动态行内内容使用 bind/derive/view。删除 key 退休其节点及所属任务/活动；重新加入同一 key 创建新行。重复 key、非法节点或构造失败不替换原树。list 的 children 由列表管理，作者通过来源数据增删重排，嵌套组件放在 build 内。列表使用现有 scroll 组合滚动，当前实例化全部条目；虚拟化另行实现。

`game.dialogue()` 与 `game.choice()` 返回当前会话的稳定只读可观察服务对象。前者提供累积 `lines`、`current_line_ids`、当前组第一行 `speaker_name/text/reveal` 和 `active`；每行提供 `id/speaker_name/text/reveal`。`reveal` 是完整 UTF-8 文本的可见字节终点，`text(reveal: ...)` 只改变绘制范围，保留完整文本排版。`dialogue.clear()` 清除显示页，保留会话历史；当前仍在显示的行组继续拥有原来的推进权限。后者提供 `options` 与 `select(id)`。服务列表和行由引擎拥有，脚本不可修改；过期/重复 select 返回 false，成功返回 true，choose 返回原业务 key。对白和选择不会自动创建界面；作者显式把读取这些服务的组件挂到舞台。game.configure 只配置呈现参数，不含默认界面开关，其 native ABI 为 3。桌面播放器默认用首次画布尺寸初始化窗口；未设置画布时为 1280×720，显式 Rust 窗口尺寸优先。这是宿主呈现策略，不改变 Aside 的执行或等待语义。未消费的对白推进输入仍由会话处理。

`game.history()` 返回会话稳定的只读可观察历史数组；历史组的 `lines` 和行字段同样只读。`game.reading()` 提供只读可观察 `auto/paused/delay`，可以 bind/derive 到任意作者控件。`game.set_auto(enabled, delay: null)` 接受 bool，省略 delay 保留当前值，显式 delay 为有限非负秒数；`game.pause_reading(paused)` 接受 bool。这些变更通过既有宿主属性通知发布，不引入语言特殊语法；赋值只读服务字段或修改历史集合会报错。

`game.set_skip(enabled, unread: false)` 的参数均为 bool；`game.reading().skipping/skip_unread` 与 `game.dialogue().read` 为只读可观察属性。快进与自动阅读互斥，模式切换复用既有属性通知，不引入 VM 跳转或特殊语法。

Reel 的 `rich(markup)` 返回普通不可变宿主值，只读 `.plain` 是去掉标记后的正文；rich_escape(text) 为显式富文本插值转义左方括号。它们不改变 Aside 字符串、插值或类型系统。text.value、narrator、角色调用与 character.line 接受 string 或此宿主值，bind/derive 保留通常语义；game.dialogue 和 game.history 的 text 字段保留提交值的样式。`text(..., on_link: callback)` 交付只读 `{target, id}` 事件，回调归文字节点所有。富文本也可携带句内速度、停顿与自动续句标记，由共享对白播放器解释，不增加 VM 指令。标记、排版和输入语义见架构规范与创作者指南。rich/rich_escape ABI 1，narrator/character.speak/character.line ABI 2；增加 text_align/line_height 后节点构造 ABI 为 input 6、scroll/text_input/list/view 2、其余 5，tween ABI 2。B01 编码不变。

图片的 GPU 驻留与上传复用由渲染后端管理；相同资源身份的图片在已提交后可跨帧共享，脚本不需要预热缓存或重复加载。该实现不改变 image 的语言接口或 native ABI。

## 十二 Task Future 与取消

Reel 节点构造把 children 候选归一化为显式提供的完整组件根；已属于这些根的后代不再重复挂载。Aside 尾随 children 块仍按原规则收集表达式，普通 add 返回值不变，显式 children 数组使用同一节点合同；这不是语言对某个方法名的特判。

text_input 的显式高度内文字、选区、光标和 IME 位置共同垂直居中；text 的显式宽度参与自动高度测量。富文本事件链接悬停/聚焦下划线仅重绘。均沿既有宿主属性与输入合同，不新增 Aside 语法或 ABI。

Reel 的 `checkbox(label: null, value: false, on_change: null, ...)` 和 `radio(option, label: null, value: null, on_change: null, ...)` 是普通宿主节点，新增 native ABI 1。checkbox.value 为可写布尔值；radio.value 保存业务值，激活写入 option。共享一个可写绑定即组成单选组，值比较与 list key 相同。脚本赋值静默，用户变更先提交再执行节点所属回调 `{target,value}`；option 更新使旧呈现的选项激活失效。children 与 decorated 仍使用共同舞台，不新增语言语法或 B01/ASB 编码。

Reel 的 `clock(interval: 1, owner: null)` 是普通宿主函数，返回带只读可观察 ticks/elapsed 的活动。复用 pause/resume/stop/await，所有者规则与 run 相同；长步长合并周期、暂停不计时，finish 拒绝无终点活动。新增 native ABI 1，无新语法或 B01/ASB 变化。

独立 `aside-std` 提供 `std` 与 `std::prelude` 模块，宿主通过 Aside host SDK 注册同一份接口与实现；Reel 默认导入该 prelude。首批 `random(seed: null)` 及其 int/float/pick 绑定方法使用普通 native 调用和共享身份，随机状态在 VM 堆中保存。构造有宿主效果，抽取有写入效果，不能用于模块初始化或纯派生表达式；在主函数或事件中创建并保存随机流。没有新增语言语法、VM 指令或 B01/ASB 编码，Aside 不依赖 Reel。

Reel `scroll` 的 scroll_x/scroll_y 是普通可写可绑定数值属性，四个 viewport_width/viewport_height/content_width/content_height 为宿主发布的只读可观察尺寸；content_align 接受 start/center/end。Reel `input` 的 on_pointer 接收只读 `{target,x,y,phase}` 主指针拖动事件，沿用节点回调任务和取消。新增参数后 scroll native ABI 为 3、input 为 7；无新语言语法，B01/ASB 不变。

普通 fn 在当前 Task 的可恢复调用栈执行，可 await；Reel 入口和 UI 回调由带明确 OwnerId 的任务运行。say 等可暂停 native 能挂起普通 helper 链，不要求每层改写 async。没有任务上下文或位于受限边界时，动态尝试外部等待报错。

async fn 调用立即返回任务 Future，函数体在后续调度片开始；创建带 ParentTaskId 的新任务，继承当前明确 owner。普通调用返回不取消任务，丢弃 Future 不等于取消。spawn(owner:,task:) 指定其他合法所有者，无主后台任务不能被暗中创建。ParentTaskId 供因果追踪，不等于自动强制结构化寿命。

Reel 的 sequence/parallel 通过普通位置 rest 参数接收动作，repeat(action, times: null) 通过普通默认参数指定次数；它们返回描述，由 run 创建实现 Awaitable 协议的活动。组合与计时属于 Reel 宿主库，独立 Aside 不增加专用语法或平台依赖。

await 接受 Awaitable，不接受普通标量。Future、Reel Activity 和 SaveRequest/LoadRequest/DeleteRequest 本身实现此协议，直接写 await activity 或 await request，不引入额外完成属性。即使已经完成也在下一调度步骤继续；完成一次后按登记顺序交付所有等待者。Fault 在 await 处建立消费者故障：当前 origin/stack 标出 await 调用链，AwaitedFault.source 保留生产者完整故障及原失败位置；活动 Stopped 是普通终态，Cancelled 抛取消错误。Awaitable 注册等待、解除订阅、结果状态与恢复描述必须显式定义，不能只用类型名称宣称可等待。

Future.resolve(Future) 采用 adoption，async 返回 Future 同样继承结果；检测直接和间接 adoption 环。需要把 Future 当普通值返回时包在 tuple/对象中。任务 Future 仅由任务返回/失败/终止结算，手工 Future 才能 resolve/reject；重复结算报 AlreadySettled。未观察失败在已给等待者/处理器一个正常调度机会后产生诊断，后续观察更新记录而不重报。结算与通知均不在生产者调用栈内同步重入用户回调。

宿主 Awaitable 实现合同：VM 将可信宿主对象绑定到当前已准入包中的单参数 native（参数为对象本身，类型 any，无缺省或 rest；效果包含 WAIT）。绑定不是脚本可修改的字段。Await 指令检查动态 NoWait/效果边界，然后进入已有 NativeFrame，首次 HostCall 即调度边界；Ready 也不在响应调用内执行脚本。Pending、Continue、CallScript、代际 WaitId、根和取消全部复用普通宿主调用。转发生产者故障时，每次 await 建立独立消费者 envelope，保留原值/错误码，以 AwaitedFault 记录源故障与 activation；catch 原因同时保留。宿主按登记顺序维护订阅，以 wait_is_pending 查询取消/完成后的有效性，解除消费者订阅不能停止共享生产者；任务/owner 撤销后旧完成不得交付。当前宿主包的 native 身份和版本属于恢复描述；VM 快照、async fn、Future/adoption、generator 仍未实现，本合同不将它们宣称已交付。

task.request_cancel(reason) 为合作请求，在下一个合法安全点注入一次 Cancelled；脚本可捕获并收尾，Future 按实际最终结果结算。owner.revoke 为不可阻止的硬终止，停止其任务，不运行这些脚本 finally；宿主幂等释放拥有的订阅、资源和活动。取消不自动撤销已经写入的共享状态。必要资源清理依赖 Owner/ResourceToken，不能仅靠可能被跳过的 finally。异步 close 必须在 owner 仍有效时显式 await。

Reel 的 `completion(owner: null)` 是此协议的具体宿主接口：resolve(value)、reject(error)、cancel() 第一次结算返回 true，重复返回 false；await 返回原值或抛出拒绝值/Cancelled。它是归会话或舞台节点所有的一次性 UI 结果，不是上文尚未实现的语言 Future，不执行 adoption，也不引入 Aside 对舞台或平台的依赖。等待者被取消只解除其订阅；节点退休取消未结算结果，已结算值可重复等待。

TaskGroup 拥有自己创建的子任务。all 返回输入顺序的结果 array；首次失败请求取消其余任务并等其终止，再抛含主/收尾失败的 GroupError。race 交付最先结算项的 (index,result) 或失败，同一调度步骤依完成队列顺序决定，按组策略取消其余自有任务并完成收尾。owner 硬撤销使整组立即失效。wait_any 对外部共享 Future 只解除本次订阅，不能取消其生产者。组、取消请求、待结果和收尾状态均进入快照。

async 正文返回尚未结算的 Future 时进入 Adopting，任务及其结果仍受原 owner 管理。被采用者结算后才结算采用者并终止任务；owner 撤销解除本次 adoption 订阅，取消采用者自己的任务 Future，保留共享被采用者的生产者。取消请求与 adoption 订阅状态进入快照。

## 十三 迭代 generator 与 thread

Iterator.next() 返回 {done: bool,value: any}；done=false 是元素，首次 done=true 可携带最终 return 值，其后稳定返回 {done:true,value:null}。for 不把最终 return 当元素。iter(collection) 创建新游标，由 for 在退出时 close；iter(existing_iterator) 创建借用包装，结束只关闭包装，原游标继续保留推进位置。own_iter(iterator) 显式转移关闭责任。close 幂等，组合迭代器传播其拥有游标的关闭。

gen fn 调用创建生成器，不立即执行正文；next 推进，yield value 只挂起本生成器；yield 同时可作低优先级表达式，例如 let incoming = yield outgoing;，其结果由 send 提供，普通 next 等价发送 null。yield 的操作数包含普通二元与区间表达式，结合力高于赋值、低于区间；yield; 交付 null。yield a = b 按无效赋值目标诊断，let incoming = yield outgoing + 1 则先形成被交付值。重入 next/close 报 ReentrantIterator。send(value) 为前一个 yield 表达式提供值，第一次驱动只接受 null；异常使生成器进入 Failed 并向驱动者传播，之后 next 报 GeneratorFailed，close 可幂等释放。主动 close 在挂起点展开同步 finally，禁止继续 yield；owner 强制撤销按任务规则处理。同步生成器及其 helper 不可外部 Pending，但可 BudgetYield，这是同步迭代协议的边界。

AsyncIterator.next() 返回 Awaitable，其结果为同一 step；由 class、async fn 和标准适配器实现完整异步流。for await pattern in source 逐次等待 next，只允许一个 in-flight next，第二次报 ConcurrentNext。提前退出解除当前等待并按所有权执行 close_async；错误保留主失败与收尾失败。手动推进可保留半消费流。同步/异步 map/filter/enumerate/zip/take/collect 均声明惰性、耗尽、关闭、异常及回调效果；需要等待的回调使用异步适配器。

Thread(fn) 是由 call/resume(value) 手动驱动的独立脚本栈，suspend(value) 与驱动者双向交换。状态为 Created、Running、Suspended、Returned、Failed、Closed；重入驱动报错，首次只用 call，返回和异常只交付一次，终态再驱动报 ThreadFinished。close 展开同步 finally 后终止。手动 thread 不接受外部 Pending；需要外部等待的流程使用普通 Task 或 AsyncIterator，不通过手动 Thread 的同步驱动入口，也不保留驱动者 Rust 栈。其 frame、异常、交换值、owner 关联与终态均可保存。

## 十四 异常 退出与资源

try {...} catch error {...} finally {...} 至少含 catch/finally 一个。throw 可以抛脚本值，运行时以 ErrorEnvelope 保存原值、稳定 code、源位置和栈；标准库推荐 Error 实例，catch 能读取原值。throw; 只在 catch 中重新抛出当前 envelope，保留原始栈；throw value; 建立本次抛出位置与 cause。

正常结束、return、break/continue、异常展开和合作取消穿过 try 时先执行 finally。finally 不得 await、yield、return 或跳出其自身边界；可按预算让出。该词法限制覆盖条件与短路分支；内嵌函数按自己的函数边界分析，实际调用时继承调用者约束。普通 helper 调用可以同步返回，真正进入外部等待时由有效 NoWait 检查处理。finally 再抛出时替代原结果并保留原结果/异常为 cause。异常值、待 return 值及展开位置都是可恢复帧状态。

资源 close/dispose 必须幂等且可检查终态。文件/游标/订阅/native 资源由显式 token 与 owner 管理，GC 不执行用户脚本 finalizer；宿主低层释放同样有预算/队列。脚本清理无法保证硬终止时执行，底层资源释放必须独立保证。临时业务恢复若必须覆盖硬终止，应通过宿主事务/作用域恢复记录实现。原生崩溃和内部一致性破坏不伪装成可恢复脚本异常。

不可外部等待的边界包括同步宿主重入、缺省参数、模块初始化、构造、非 _call 元方法、派生求值、finally、children 和同步 iterator/thread 协议。静态可知时提前诊断，动态调用继续检查；“不可等待”不等于“不可按预算暂停”，invoke_sync 的特殊硬预算规则见下节。

## 十五 VM 与可恢复宿主调用

VM 使用显式调用帧、共享代码/常量、驻留符号、可复用参数区及可恢复中间状态。默认 Value 采用 16 字节显式标签与 64 位载荷，int64/float64 完整内联，堆对象通过完整代际句柄访问。Value 为可复制内部值，复制时由当前执行帧或显式宿主根负责保活。表示隐藏在 VM 私有存储接口后，字节码、快照和 C ABI 使用各自规定的编码。32/64 位目标分别检查大小、对齐、算术及句柄往返。密集数值数据使用连续类型缓冲，避免每个元素支付动态 Value 标签成本。

run_slice 接收指令、分配和时钟检查额度，返回 Returned、BudgetYield、HostCall、Waiting、Fault 或 ResourceBlocked。ResourceBlocked 表示建立完整故障记录所需的硬容量不足，其保留、恢复与取消规则见第二十三节。BudgetYield 不改变语言结果；全部临时根和继续位置都在 VM 状态中。HostCall 携带稳定函数符号、签名版本、TaskId、OwnerId、源位置和借用实参视图。宿主响应 HostCallResult 为 Ready(value)、Pending(wait_descriptor)、CallScript{callee,args,continuation}、Continue{continuation} 、Error(NewFault) 或 ForwardFault(fault_id)。Error 创建本次故障；ForwardFault 传播已保留的完整故障，具体契约见第二十三节。

Pending 必须先退出 native 调用栈，再保存操作描述、等待身份、返回槽和后继 PC。完成结果只恢复操作的后继阶段，不能重跑已经生效的调用。native 异步算法用分块状态机或后台拥有数据任务，不把 Rust Future/栈地址当持久续体。

CallScript 允许 native 算法调用可暂停脚本：宿主退出 Rust 栈，将版本化 continuation 作为显式 native frame 保存，VM 在同一 Task、同一 owner 和效果上下文压入 callback frame。脚本返回或抛出后，恢复该 continuation 并交付 ScriptExit::Returned/Fault；continuation 可继续返回 Ready/Pending/CallScript/Continue/Error/ForwardFault。callee 的模块版本、实参、算法阶段、临时对象与返回目的均可追踪和快照，原 native 入口不被重复执行。callback 的 Pending 由整条任务栈自然挂起，不需要新建无关任务。

Continue 保存版本化 native 算法状态，在同一 Task 下一预算片继续执行，无 WaitId、无外部等待，也不重新执行入口。它覆盖编码、hash、复制等无需脚本回调的分块算法，在允许 BudgetYield 的受限上下文同样适用。native frame 的临时根和游标进入快照。

CallScript 不提升权限或取消已有受限上下文；sort 的同步 comparator 仍不能等待，受限 derive 仍不能写外部状态。需要普通可暂停 callback 的宿主操作在签名中明确声明。native frame 的取消/释放由宿主 adapter 幂等完成；脚本 fault 未被 continuation 处理则按原调用链传播。旧 continuation 版本在所有帧/快照引用退出前必须可解析。

允许 native continuation 声明 RegionId 取消屏障和附加 owner 条件，不改变 Task 的主 owner。Reel scope(owner: node, body: callback) 通过它执行：节点先结束时只撤销该 region，取消其等待并丢弃到屏障为止的 callback 帧，内层脚本 catch/finally 不可阻止撤销；native adapter 幂等清理本区域资源后，在屏障外的原调用位置交付 ScopeCancelled，仍存活 Task 可继续。整个 Task 主 owner 撤销则按硬终止规则结束全部区域。区域层次、取消条件、临时资源及屏障 frame 一同保存；Task 主 owner 保持不变；执行上下文的默认资源 owner 为最内层活动 scope，否则为主 owner。新 async 子任务和未指定 owner 的活动使用该默认资源 owner，显式 owner 表示明确转交生存范围。无新增语言关键字。

invoke_sync 仅用于不可保留 native 栈的有界同步重入。它继承最外层同步调用的剩余指令/分配额度与截止时间，嵌套只可收紧并累计扣减，最终返回 Returned、Error 或明确的同步控制失败；完整调用在本次同步边界内结束。预算耗尽终止本次嵌套调用，向 native 返回 NonResumableBudgetExceeded，脚本 catch/finally 不可截获或延长此退出；已经发生的副作用不自动回滚。所有调用此路径的 native 必须能处理失败并释放借用。可变长度或可暂停回调使用 CallScript，不能循环 invoke_sync 绕过累计预算。

## 十六 宿主 ABI 能力与标准库

宿主可将一个已准入 native callable 的首参数绑定为接收者，产生普通可调用值。绑定值保留代码版本、模块和接收者身份，受既有 GC 跟踪；保存方法后调用仍使用原接收者。调用直接进入 NativeFrame，首参数预绑定，余下位置/具名/默认参数使用同一个 VM binder，显式重复指定首参数仍报重复绑定。脚本调用、宿主 start_closure 和 CallScript 回调使用相同协议，不建立合成脚本调用栈；诊断定位真实调用者。绑定不改变效果或等待许可，不保存 Rust 对象地址。

Reel 作者事件通过宿主已有 start_closure 创建节点拥有的普通任务，不引入 Aside 事件语法或 UI 依赖。回调接受一个事件对象并共享闭包捕获；可以等待普通宿主 Awaitable。Reel 在任务等待或结束后切换任务；指令预算和同步 native 往返只让出宿主执行，不在同步脚本段内切换其他任务。每个任务保留自己的等待和源码故障，节点退休使用既有硬取消与分片释放。Reel 的 SessionVm::step 返回 SessionStep，将实际 VM 结果、已处理的回调完成 Progress 和无可运行任务 Idle 区分；不会伪造 VM WaitId 表示 UI 空闲。

native 注册表包含稳定符号/版本、参数与命名调用描述、类型/返回值、允许效果、能力要求、Owner 规则、错误码和恢复 adapter。

可信链接包使用本次编译请求的 content_version；宿主 ABI 版本独立保持注册版本。一个调用同时匹配导入符号和名称使用节点时，各链接仍分别核对分析身份与签名；全部指向同一 NativeId 才视为同一注册，指向不同 NativeId 时给出 LinkContract，不能按匹配顺序选择。

Reel 节点的 rotation、scale_x/scale_y 和 anchor_x/anchor_y 是普通宿主数值属性，构造和赋值使用同一注册签名及纯验证器，非法值在作者赋值位置失败且不修改旧值。其角度、锚点及矩阵组合属于 Reel 的舞台合同，Aside 不新增变换语法或平台依赖；源码与 ASB 使用同一节点接口。

Reel 的固定节点数值属性通过普通命名参数表达 tween；宿主根据共同节点 schema 生成签名并验证数值。属性写入追踪是通用 Heap 能力：宿主可为既有字段启用逐字段写入版本；成功的脚本赋值递增对应版本，包含同值写入，失败赋值不递增。追踪数组由 GC 管理并计入堆预算，宿主动画内部写入不冒充脚本赋值。Aside 不依赖 Reel 或平台。

以下动态 schema 尾袋仍为设计合同，尚未进入执行子集。签名可显式声明 PropertyBagTail：先绑定普通已声明参数，再将余下具名参数按指定目标参数的实际 PropertySchema 验证。描述包含目标参数、允许属性集合/能力、值类型、读写与可动画要求；未知名、只读属性、类型错误和与保留参数冲突均报错。tween(target, x: 80, duration: 0.5) 中 duration 是普通参数，x 按 target 的可动画属性 schema 检查；s.override(node, visible: false) 按 node 的可覆盖属性 schema 检查。普通无此元数据的函数仍拒绝未知名。call(fn,positional:,named:) 使用同一绑定器，脚本包装器通过显式 properties/named 表接收再转发动态属性；普通 fn 只接收自己声明的参数，不会自动捕获任意未知具名项。全程使用同一绑定器。工具能从签名和目标类型生成属性补全。宿主类型声明身份/值语义、字段读写/版本、结构模式可见字段、可观察属性、克隆与迭代行为。脚本与 native 使用同一参数错误和类型诊断，不允许只有内建函数支持命名参数。

借用 Value/字节视图只能在当前 native 调用中使用；保留对象须申请代际 root handle，跨线程只传拥有数据、ID 或真正不可变缓冲。Rust panic 不跨 FFI；native 非法实现视为扩展故障，不声称能恢复任意内存错误。能力检查在操作前完成，InitSafe/DerivedSafe 限制传递到 callback，注册标签不豁免效果检查。

每种可达资源声明 Serializable、Rebuildable、InterruptedOnRestore 或 Unsupported，并提供版本化状态/恢复/取消契约。Unsupported 引发带引用路径的保存失败，不能静默丢弃。设备、窗口和 GPU 句柄按描述重建；native 符号按版本重连，不能仅按相同名字替换旧 continuation。

完整标准库按层提供：
- core：值/类型/错误、转换、assert、函数转发和反射。
- math、text、collections、iter：数值、Unicode/格式化、全部集合与同步/异步适配器。
- buffers、encoding、data：TextBuffer/Bytes/ByteBuffer、严格 UTF-8/端序编解码、结构化数据读写；JSON 明确不保存任意对象图。
- task：Future/Awaitable、取消、任务组、同步/异步迭代桥接及资源 token。
- host：能力查询、日志、时钟、RNG、资源/文件/网络等授权模块。Reel 使用可保存逻辑时钟与 seeded RNG；壁钟/系统熵显式调用，回滚不假装重演外部世界。

正则、压缩、网络、文件系统、动态编译等部署模块由能力清单说明依赖；缺失时加载/调用给出明确能力错误，标准核心不暗中依赖 OS。可选 compiler 模块产生同样可验证的模块包，动态源码与函数仍绑定代码版本。模块可选性是宿主部署能力，不是删除基础语言设施的借口。

可信宿主可用 `Vm::bind_callable(object, function)` 为 host 对象登记已准入 native 的调用入口。首个必需参数绑定对象自身；普通参数、默认值、具名/展开、效果权限和暂停继续走同一 native 调用准备，不递归执行脚本。登记不在公开字段中，脚本不能覆盖；对象字段和载荷继续按常规 GC 追踪。此协议与宿主 Awaitable 独立，不等同于尚未实现的用户类 `_call`。

## 十七 预算 回收与分块算法

指令之外，还统计容器元素、比较/hash 工作、复制字节、回收边和分配字节。每 256 个工作单位检查让出条件为起始配置，实际目标由尾延迟测量调整。native 原语必须有界；长字符串、Unicode、tuple 相等/hash、模式、排序、数据解析与复制均使用可恢复 continuation 或严格输入界限。只统计 CALL 指令不能证明有界。

字符串总长度不受单次复制额度限制。字节码常量按存储分块装入，字符串加法使用保留在执行帧中的构造状态，按工作与分配额度逐块复制；预算耗尽只让出，不把长文字变成资源错误。结果完成后一次发布为不可变 string，中间缓冲保持为根，异常或取消沿现有帧清理回收。每个块和临时缓冲仍计入真实堆预算；宿主单次读写的分块大小不成为文字总长上限。

分块操作必须规定中间状态可见性。sort 使用工作副本，记录来源的完整内容版本；comparator 或其他任务在准备期间修改来源任何位置，提交时报 ConcurrentModification 并保留当前来源，不覆盖并发写入。比较器须提供稳定一致顺序；运行时可诊断检测到的违约，但不会以任意脚本比较器为由破坏容器或内存安全。成功后一次提交排序，失败不写回副本。BudgetYield 时其他任务可运行，算法副本、游标、版本和 callback 状态进入根和快照；绝不重新执行已发生 callback 来恢复。对需要原子结果的其他操作采用同样准备/提交模式，不能因一个字节码操作就宣称其所有内部步骤不可见。

对象存储直接复用固定版本 slotmap 1.1.1，使用独立 HeapKey 类型与完整 64 位不透明键。句柄只在所属堆内有效，宿主长期保留的对象登记 root handle，并携带 HeapId/SessionEpoch。快照使用独立对象编号，恢复时重新分配句柄。普通传递遵循 KeyData 的完整 u64 往返契约。

脚本对象图采用单线程、非移动的增量三色标记清扫。Value 复制和堆内强边由追踪管理，省去逐次引用计数。slotmap 提供存储、槽位复用和安全访问；Aside 实现适配对象种类的根扫描、边遍历游标、写屏障、弱引用处理与清扫状态机。gc-arena 用作成熟算法参照，其 Gc 字段接口与整数句柄图不同。

外部根包括活任务 frame、活动会话、当前发布 ModuleSet 环境、待交付结果、native continuation 与宿主 root handle；分块算法的工作副本及 callback 状态同样属于根。对象字段、cell、集合元素和闭包捕获为堆内强边。旧模块缓存为弱索引；分配表及 GC 工作队列只作管理记录。不可变代码或资源可独立使用 Rust Rc/Arc，其计数与脚本对象图的可达性分开。

ModuleSet 的常量、符号、native 值和全局根在包加载时按实际数量预留，不套用单次执行窗口的长度额度。VmLimits.slots 声明执行工作集，模块根容量在加载时加入同一根池；执行帧和 continuation 仍使用操作窗口额度。两类根共用 GC 扫描和堆总内存计账，模块大小不挤占配置的执行工作集。

默认 HeapLimits 不设固定字节总额（max_bytes 为 u64::MAX）；宿主需要隔离预算时可显式设置。内存用量和峰值照常记录，真实分配失败仍报告错误。执行切片的工作和分配预算只决定何时让出，不是剧情内容容量上限。

对象、宿主根与根区间描述符按需求几何增长，默认数量额度为代际句柄的可表示上限；配置的最大数量不作为启动预分配量。对象表增长时一并准备 GC、释放及响应式队列容量，写屏障和回收步骤不分配；根区间增长同步扩展 FrameSlots 映射，保持既有 token 和扫描游标。增长的新旧存储重叠计入峰值，失败不发布对象或根，已申请的可复用容量继续计账。宿主可显式收紧数量和字节额度。

GC 步在 VM 安全点执行。标记期间采用强边插入屏障：每次强边写入和根发布，在新值可见前把目标纳入标记；新对象的初始强边及 weakref 升级使用同一入口。根扫描有持久游标，已扫描根的更新也经过屏障。大型容器保存边游标；插入、删除或重排若移动尚未扫描的边，须修正游标或补充标记这些边。全部根、灰队列与屏障工作完成后，在同一安全点进入弱引用判定和清扫。自开始弱引用判定直到本轮清扫结束，weakref 仅可升级本轮已确认存活或本轮新分配的对象；这一阶段切换是统一的存活判定截止点，代际有效本身不足以允许升级。新分配对象标为本轮存活，并纳入下一轮扫描。

根、边、弱引用和清扫均按独立工作额度推进。清扫使用有界分配键队列，每轮记录待处理数量，分片检查并重新排入存活对象；本轮新增对象留给后续轮次，队列记录与对象身份对应，避免重复释放。对象先在逻辑上失效，再分片释放 payload；大型容器拆分释放，缓冲分配/释放记录最大不可分割耗时。标记队列、游标、待释放字节及退休槽位全部计入堆预算。标记入队容量不足时先维护或返回资源错误，保持屏障完整性。设备资源按 Owner 显式撤销，GC 仅处理底层拥有数据的释放；回收时刻与业务确定性分开。

slotmap 的完整版本仍有回绕边界。封装删除时，在完整代数耗尽的槽位保留 Retired 记录，访问拒绝该状态，批量清理保留退休槽位。固定版本的满幅退休阈值为占用版本 0xffffffff，即同一槽位最多 2^31 次生命周期。版本字段识别属于依赖适配代码，升级时核对源码与专项用例；普通句柄保持完整往返。退休槽位计入容量而非活对象数量。验收以缩小代数范围强制覆盖旧句柄、退休、跨堆及恢复后失效，并在每个 GC 步之间修改根与大型容器，验证弱升级和分片清扫。

## 十八 快照 恢复与版本

VM 保存完整可恢复执行状态和对象图，宿主协调请求、舞台/资源屏障及原子磁盘提交。snapshot_begin 在安全点固定根和逻辑状态，snapshot_step 按对象/边/字节预算复制到独立区；业务写入暂停，后台结果排队，长字符串/缓冲/容器也分块。绑定派生与写回在捕获中暂停，非持久原生控件反馈可继续。

快照包括代码/模块/native ABI 版本，全部 Task/generator/thread 的 PC、接收者、返回槽、异常处理/展开、寄存器和中间值，native continuation、CellId、模块环境、对象共享/环/弱边，Future adoption/结算/等待队列、任务组/取消/异步 iterator、待交付结果，tuple 内部引用、缓冲有效内容、集合顺序/结构版本/游标和 schema。绑定保存属性身份与派生闭包，不保存临时通知队列。真正不可变代码/文本/资源可共享，cell、活动属性和可变缓冲必须独立记录。

弱引用只在目标经强路径已进入快照时保存目标关系，否则恢复为已失效；不能因序列化弱边而把目标变强。IdentityMap 保留强键关系与恢复后的对象身份，不按旧原生地址重建。

缺少 native adapter、代码版本或 ABI 时报告精确对象路径/版本并使保存或恢复失败。restore 在隔离堆先分配身份，再连接引用、等待、订阅和 native 描述，完整验证后一次公开；失败保留当前会话。禁止以新代码 PC 猜测旧恢复点。SessionEpoch 在新游戏/加载/回滚更新，TaskId、WaitId 与 native 完成消息匹配 epoch 后至多交付一次，旧结果丢弃。

捕获、恢复提交和热重载提交互斥。发行存档使用匹配游戏包代码；开发存档嵌入无法由固定包取得的全部被引用版本。旧代码/模块/native/源码映射版本在最后强版本引用离开后释放；持有者包括帧、闭包、快照、故障栈/原因节点、FaultInfo/FaultRoot 和进行中的 FaultBuild。跨版本迁移是显式流程，不默许替换执行中函数。

进程服务请求按 adapter 处理：仅当前正在持久提交的 SaveRequest，在该独立存档副本中记成功，活会话仍等待真实写盘，内存回滚快照不做此归一化。已结算请求恢复既有终态。其他未结算外部写请求只在精确进程/请求身份及完成记录匹配时重接，否则以 OperationInterrupted 结算；始终不盲目重放外部写入。

## 十九 编译包 构建与工具

Rust 扩展接入合同见 [Aside host SDK](aside-host.md)。共享宿主声明是编译签名、B01 默认参数与运行时准入的单一来源；绑定宏只生成声明及 Rust 调用包装，复用语言的参数绑定、NativeFrame、等待和 GC 协议。Aside 不依赖 Reel。

Rust 扩展现可声明可变参数：`#[aside(rest)]` 对应普通 array rest，接收剩余位置实参，支持位置展开。每次调用有独立的外层数组，元素按正常值语义共享；没有剩余参数时仍得到新空数组。rest 必须是最后一个脚本参数，不带默认值，也不能按 rest 名称传入。固定参数的具名/默认值、绑定接收者、函数值、宿主入口和 CallScript 共用原有绑定器；包装不复制参数到 Rust 集合。NativeContract.rest 在源码链接与运行时准入使用同一名称及 Array 类型，包声明不一致则拒绝加载；B01/ASB v1 与 native ABI v1 不变。

编译器输出版本化模块包：语言/包/宿主 ABI 版本、导入导出、签名、指令、常量、源码映射、能力与元数据。磁盘数字宽度、端序和编码固定，加载内存布局可不同。执行前验证解码、索引/寄存器/跳转、声明额度、native 符号及能力；恶意包不可导致越界、无界解码或巨量预分配。

作者源码没有独立的 8 MiB 读取门槛，B01 的源码、字符串常量与符号也没有 1 MiB 单项门槛。源码位置与各字符串长度仍须可由既有 u32 表示，编译和 B01/ASB 编解码继续执行各自的整体内存、字节与工作预算。B01 默认工作预算由整体字节预算的两倍推导，覆盖完整包的预扫描与物化两遍读取；两遍工作累计计费，调用者仍可显式配置预算。SourceFile.text 与其他字符串统一沿既有长度前缀解码，格式不变；源码路径及内容在 check/build/dev/run 中使用相同规则保留。

源码的平坦运算与后缀访问链按顺序处理，不把 CST 的左结合树高作为额外的 128 项语义限制。投影、分析和 lowering 使用显式链工作表，保留从左到右求值、短路、接收者一次求值、children 收集和源码位置。真正递归的括号、块与子表达式仍使用既有编译资源预算，默认深度为 128，宿主显式配置的预算不再被隐式压低到 128；该深度衡量编译器嵌套语法，不等于画面节点层数。lowering 分开节点调度与具体操作，错误以 Box<Error> 返回，保留完整源码位置与预算统计，仅失败时分配错误载体。不能让无关分支的巨大局部栈帧使正常嵌套组件在默认主线程栈上崩溃。不可变语法树由独立的 aside-syntax crate 持有，最后引用通过原子操作认领后迭代释放子树，长链不依赖宿主调用栈析构。

Cargo feature source 决定是否包含 aside-compiler；关闭时仍执行完整字节码语言和核心标准库，依赖树不得保留编译器、watcher 或 Quirrel oracle。hot-reload 与 source 独立，精简播放器可接收外部编译包。运行参数只选择已编入能力，不假装实现二进制裁剪。缓存按源码、依赖版本、语言和编译选项失效；动态成员缓存含布局版本，命名调用缓存含函数/签名版本。

工具目标包括 check/build/run、formatter、LSP 名称/类型/签名/命名参数和跨模块导航重命名、测试宿主/断言、字节码检查器、任务/generator 调试与 profiler。native 声明、文档、编辑器提示和 ABI 校验由同一注册元数据生成。调试器显示同步栈、异步因果链、owner、等待来源和版本；watch 默认纯读，不通过检查表达式启动活动或改变游戏。

诊断含稳定 code、严重性、模块版本、主/相关源码范围、调用栈及可选修复。保留 UTF-8 字节范围与展示行列，中文不能错位；动态调用标出签名/实参，await/native continuation 标出等待点与原失败点。提供人读和结构化输出，不向作者泄露内部地址或 Rust panic。覆盖名称/类型/参数/模式、只读写、受限等待/效果、无效句柄、资源额度、代码/恢复缺失及坏包错误。

## 二十 操作与验收覆盖

1. 包装组件：命名/展开/动态转发/children 统一绑定；验证求值一次、默认数组独立、重复参数错误、构造暂停后无重复 Node。
2. 回调控制器：cell 和强绑定方法保留接收者/模块；验证每轮捕获、订阅 token、引用环回收、owner 销毁后过期完成失效。
3. 结构结果：tuple 的零/一/多元、精确类型、浅别名、NaN、数值等值键、hash 拒绝路径、rest 与单值多返回；每种边界保存后继续一致。
4. 游戏集合：顺序、missing/null、schema、修改中迭代、IdentityMap；验证 keyed UI、重复读档、共享模型与环关系。
5. 文本和缓冲：Unicode 单位、非法编码、字素与字节边界、可变别名、冻结输出、超大编辑和预算；保存后继续编辑不改过去快照。
6. 模式分支：scrutinee 一次、guard 短路、OR 名称一致、缺省只在缺失时运行、失败无部分绑定、受预算深结构。
7. 并行流程：Future FIFO/adoption 环、组失败/取消/收尾、wait_any 不取消共享生产者、异步 iterator 单 next、generator close/finally 与 thread 重入。
8. native 回调：CallScript 在同 Task 挂起/返回/抛错/再次调用，保存算法中间帧；invoke_sync 累计预算不被嵌套重置，不重放副作用。
9. 保存和恢复：所有值、异常展开、活动等待、缓冲/游标/native continuation、旧 ModuleSet 与恢复错误路径；外部写只执行一次，旧 epoch 完成不生效。
10. 发行与工具：坏包、缺 ABI/能力、无 source 包、源映射、跨模块导航、任务调试；旧代码直到最后引用后才释放。

每项同时记录行为、时间、分配、峰值内存和最慢不可分割工作。负载覆盖属性/闭包/命名调用、深 tuple、字符串/缓冲、对象创建、宿主往返、长分支、连续 await、native CallScript、深链释放与循环回收。Value 优化在同一语义套件及 32/64 位目标上比较；默认实现和选型依据见下一节。

Quirrel fixture 保留来源 commit/hash、原断言、构建参数和分类：preserved、syntax_converted、harness_adapted、deliberate_delta、compiler_specific、blocked。转换使用语法/绑定信息及双向源映射，不能全库 regex 改写；未覆盖项不能算通过。Aside 独有语义建立原生正反例和性能/快照用例。实施顺序可以分为内核、通用语言、标准库工具、Reel 集成和规模验证，但本规范全部目标完成前不得把最小闭环称为语言完成。

## 二十一 Value 选型与性能证据

默认采用显式标签，依据是完整 int64 内联、无需窄化对象身份、简单的数值访问，以及当前有界解释执行实验的结果。8 字节 NaN-boxing 在通用 Value 数组容量上更省空间；完整引擎需要同时衡量运行时间、存储、回收和平台成本。

2026-10-08 的实验保存在[固定实验提交](https://github.com/jhq223/reel/tree/147c4cf788078c72b4ca5206270fce9dc29e12df/tools/value-layout-bench)，原审查版本为 91e87ad。三种表示执行同一动态指令流、动态类型检查和宿主调用；两轮各 11 个样本。连续槽 NaN 候选相对显式标签的执行时间，在剧情分支、UI 属性计算、浮点动画中分别约为 1.40、1.75、2.05 倍。该组负载没有范围外整数分配。原始数据、环境、汇编检查和复现命令见实验报告。

这个比较包含实现策略差异：NaN 候选为范围外整数维护引用计数，即使值未装箱，复制和覆盖仍检查 WIDE 标签；显式标签可消去这些检查。宿主属性使用公共原生标量，比较包含编码往返，未覆盖 NaN 布局在 VM 对象字段中的密度收益。正式堆采用追踪回收，因此上述倍率仅属于这组实验实现。小寄存器工作集和虚拟化 x64 测量也没有给出完整 VM、低端设备或 Vita 的性能结果。

该决策使实现拥有明确起点。后续布局优化保留相同语言语义、快照格式和作者 API，以正式 VM 的剧情、UI、动画、对象图和密集数据负载复测。优化验收同时报告延迟分位数、内存峰值、分配与回收成本。

## 二十二 字节码与执行器的实现合同
公共格式位于 aside-vm::bytecode，只包含程序数据、验证和反汇编；aside-compiler 单向依赖它，格式模块不引用 Rowan、parser 或 Reel。下列合同承载完整语言目标，各实现任务分别记录已运行的指令与行为。

程序与验证
BytecodePackage 保存格式/语言/宿主 ABI 版本、内容版本、符号、常量、模块、函数、能力和源码映射。ModuleDef 包含规范化路径、固定导入/导出、全局绑定与初始化入口；FunctionDef 包含 Plain/Async/Generator/Native 类别、统一 Signature、捕获布局、寄存器数、代码和异常/清理区域。Signature 保存参数名称/顺序/类型/可变性、缺省表达式入口、rest、返回契约和效果；缺省仍在每次调用时运行，并继承被调用闭包的定义处词法捕获和 ModuleInstance；前序参数作为额外输入。捕获分析包含缺省表达式。
每次调用按参数顺序建立本次独立绑定，前序参数在后续缺省表达式开始前完成初始化。缺省表达式对前序参数的读写、其中产生的闭包与函数正文共享同一个词法绑定。需要共享可变身份的参数在这一步建立 Cell，正文入口复用它；捕获分析和 lowering 同时考虑缺省中的读写与闭包。已经初始化且身份无关的只读前缀可按值传递。全部调用实参仍先按源码顺序求值，再开始参数绑定。

缺省函数由参数描述中的 default 入口选择。编译器为表达式/块尾结果生成的 Return 结束该缺省入口；源码中的显式 return 继续受缺省词法边界约束。只有 VM 内部的缺省前缀调用路径可以传递前序参数 Cell 身份；普通 CallTarget/Arguments 始终传递作者值并建立新的调用绑定。同一个缺省 FunctionDef 作为普通函数调用时也遵守普通规则。验证器允许 Parameter(i) 使用 Value(Reg(i)) 或 Cell(Reg(i))，并逐对核对被调用函数与其缺省入口的前缀名称、类型、可变性、存储布局及捕获布局。内部前缀路径验证真实 Cell 的初始化、内容和契约，Cell 句柄保持内部表示。缺省入口与正文共享的是本次调用绑定，定义处捕获仍来自被调用闭包的词法环境。
脚本和 native 统一要求：出现在任何后续缺省前缀中的可变参数使用 Cell。NativeDef 复用现有 Signature；验证器按最后一个缺省入口的前缀推导规范布局，核对其余缺省入口的一致性。只读前缀可为 Value 或 Cell，所有相关入口采用同一选择；完全不属于缺省前缀的参数保持 Value。VerifiedProgram 以共享签名的紧凑只读布局保存结果，构建/核对先计工作量，持有容量进入包账本。每次调用读取这份结果，避免重新扫描全部 thunk。native 缺省入口保持无脚本词法捕获的现有约束，前序参数的共享绑定通过内部前缀路径传递。

native 使用同一个顺序准备规则。只有全部缺省与类型检查成功后才公开 native Entry。共享 Cell 环境与宿主实参数组分开：ticket 对外暴露已经绑定的作者值，隐藏 Cell 句柄留在内部；CallScript 等后续阶段使相关 Cell 变化时，发布恢复 ticket 前重新投影连续实参视图。原始 native 入口只调用一次。无缺省的 native 保持 Value 快路径，避免为此功能分配额外 Cell 或环境窗口。

参数准备的 Cell 建立、类型检查、前缀转交、缺省结果写回与正文进入均有明确阶段；先发布目标根再清来源，预算暂停与 GC 保持身份。准备失败从原调用点传播，函数正文的 handler 此时尚未进入。逐阶段操作按工作与分配额度计费。
Const 使用完整 int64、float64 位编码、UTF-8、Bytes 与规定的不可变结构。磁盘格式使用小端整数和 u32 索引；PC 表示指令序号。版本 1 每条指令为 16 字节：u16 opcode、u16 flags、三个 u32 操作数，未声明 flags 为零；复杂操作引用有类型的描述表。常量/符号/函数/模块/寄存器/PC 分别使用不同索引类型。编码器逐字段写入，运行时 Value、HeapId、root token 和 Rust 内存布局不进入包。
执行器只接收字段私有的 VerifiedProgram。解码/验证先核对长度与总量限制再分配，检查所有索引、窗口、跳转、捕获、签名/缺省入口、异常与清理区域、控制流及 native 版本/能力。未知 opcode 或缺少所需能力给出定位错误；动态类型、初始化状态、只读写和效果仍在运行时检查。包加载和不可变代码另有容量账本。
指令规格表统一规定操作数、读写寄存器、分支、可抛错/可暂停与描述表引用，并由 encoder、decoder、verifier、disassembler 和 dispatcher 共同引用。指令族覆盖值/绑定、cell/全局、数值与严格 bool 控制流、闭包/调用/返回、集合/类/成员、模式/迭代、异常/finally、yield/await 与任务；children 和属性引用按既定语义降为通用操作及描述。编译器和执行器使用同一份规格，禁止各自猜测编号和操作数含义。

连续帧与可扫描根区
热寄存器保存在 VM 拥有的连续 FrameSlots 窗口，按 base+Reg 访问；它们不进入 BTreeMap 集合，也不按每个寄存器分配对象或 RootToken。帧使用可复用且受容量约束的窗口；语言未初始化状态用独立 bitmap 表示，null 保持普通值含义。
Heap 为每个固定长度根区登记带 HeapId/SessionEpoch 和代际 RangeKey 的 RootRangeToken，只保存 owner/kind/长度和游标。每次 GC 步临时借用 VM 的 RootRanges 查询切片，Heap 不保存地址或长期借用。寄存器、实参、native 临时值、异常和交付结果均有根区。长期宿主保留继续使用单值根。
根区先建立全 null 的有效槽位，登记与 provider 映射在对 GC 可见前一起发布，随后逐项经插入屏障发布真实值。结束根区时先注销，再释放窗口。每次覆盖、移动、参数复制和结果交付均先屏障再写入；目标根先发布，来源根后清除。根扫描以描述符槽位、generation 和元素偏移推进，空洞/描述符/单个Value分别计工作量；槽位复用时重置旧游标。物理搬迁仅在安全点更新映射，保持逻辑身份、长度与顺序；短期借用结束后才能搬迁或重入。
仍有效根区缺失 provider 或长度不符是运行时一致性错误，不能按空根跳过。存在外部根区时，原有 gc_step 入口也须接入同一 provider，或明确拒绝缺失 provider 的调用。单值根、全部根区和灰队列处理完才进入统一弱引用截止点。窗口容量、bitmap、帧/续体元数据、描述符、空闲池、退休槽及搬迁双份驻留都计入总字节额度；实际释放后才退账，保留的池容量持续计费。相关接口保持 VM 内部封装，避免绕过屏障直接写槽。

恢复、调用与结果
一个会话的 Vm 共用 Heap、FrameSlots 和模块实例集，并持有有界的 Task 集合。每个 Task 保存自己的执行栈、异常展开、等待、交付、取消和分步操作状态。调度器在同一 Vm 内选择 Task 执行一片；某个 Task 外部等待时，其他 Task 仍能访问相同全局绑定、对象和闭包 cell。宿主 ticket、等待和交付身份包含 TaskId，结束或撤销任务只释放其自身状态。独立会话由独立 Vm 承载，所有任务元数据共用会话预算。
函数声明的 Context 给出该函数的限制；有效上下文累计调用链上的 NoWait、Init、Derived 条件，并收窄允许的 effects。任一调用层具有的限制持续生效，返回后恢复调用者上下文。宿主 ticket 携带完整有效约束，CallScript 和普通调用沿用同一规则。
ScriptFrame 保存 FrameId、固定 CodeRef/ModuleInstanceId、PC、窗口、闭包/接收者、参数绑定阶段、返回目标、效果、owner/region、异常展开与当前操作阶段；NativeFrame 保存版本化续体与其强引用槽。源码错误节点和未解决诊断不进入可执行 lowering。
函数声明的限制与实际调用链的有效限制分别表示。现有公开分析入口生成的声明上下文可由 Ordinary/NoWait/Init/Derived 表达；同时处于 Init 与 Derived 的执行限制由 VM 的累计标志保留，不能因某个声明标签而丢失。未来分析入口若产生新的合法声明组合，先扩展共享格式和所有消费者再开放该入口。
AnalyzedModule 保留分析入口上下文、各函数的种类/声明效果，以及稳定的符号和模块身份。lowering 从这些明确事实生成 FunctionDef Context 与签名；调用链的有效限制由运行时继续累计。导入与命名空间成员解析保留声明所属模块和导出身份，重导出另记其解析来源。宿主链接输入来自同一可信 native 注册元数据，提供稳定符号/版本、ABI、能力、效果和默认实现；语义层保存引用身份，包构建按链接输入核对并生成 NativeDef。缺少或冲突的链接信息给出定位诊断。
run_slice 接收指令、内部工作、分配和时钟额度，返回 Returned、BudgetYield、HostCall、Waiting、Fault 或 ResourceBlocked。ResourceBlocked 表示建立完整故障记录所需的硬容量不足，其保留、恢复与取消规则见第二十三节。长比较、展开、匹配等按明确阶段计费。普通函数、async 和 generator 共用调用绑定规则，由函数种类决定执行/创建任务/创建生成器。
HostCall 在暴露响应 ticket 前已登记状态和参数根。ticket 含调用序号、native 符号/签名版本、任务/owner 和源位置，响应消费一次；参数只通过短生命周期借用访问。Returned/Fault 的数据继续留在 VM 登记的交付根或拥有型宿主根中，直到转移到另一已登记根或拥有型根句柄，或显式放弃结果；领取裸 Value 本身保持原交付根，避免失去保活。
Ready 先发布结果再提交返回槽并进入后继；Pending 保存等待描述和后继阶段；CallScript 先保留 NativeFrame 再在同任务压入 callback；Continue 保存算法阶段在下一片继续；Error 从原调用点展开。发出、等待、提交、完成是可保存的不同阶段，恢复不得重跑已生效 native 入口。快照使用逻辑 ID 和代码版本，恢复时重新分配对象与根区，不序列化运行时 token。
捕获 cell 共用现有 Cell 对象种类。对象保存初始化状态、固定可变性和带代码/模块版本的类型契约。CellNew(binding) 根据绑定描述创建未初始化对象，并将句柄写入指定寄存器；BindInit 在类型检查与屏障后完成一次初始化。CellRead/BindRead 校验初始化，CellWrite/BindWrite 校验已初始化、可变性和类型。普通字段写入也服从这份契约。Cell 模式捕获句柄时保留共享状态，读取发生在函数真正执行时；前向只读捕获也可使用 Cell，确定已初始化的只读捕获可按值保存。null 是合法的已初始化内容。状态与契约随 GC、闭包和快照保留。
模块根声明与初始化入口中的词法局部量采用不同绑定身份。仅真正 Module 上下文的根声明进入 global 表；块、条件、循环和 catch 的局部量属于初始化入口，不能用 owner 为空推导 global。静态 BindingId 标识绑定位置，每次作用域进入用 CellNew 更新该位置的 Cell 句柄：同次进入的闭包共享它，旧闭包保留此前进入的 Cell。入口中的全部 CellNew/Clear 先于任何命名闭包构造；while 条件的 continue 回边经过对应入口，catch 绑定只由其专用入口初始化一次。缺省入口捕获定义处环境，不重新创建定义环境的局部 Cell。

外部 enclosing 词法绑定保留原身份和逐层捕获关系，不得视作当前入口新声明或普通 host 符号。若编译调用未提供实际外部 Value/Cell 链接，lowering 给出定位的 MissingLink，不能以新建未初始化槽伪装接通；既有可信直接 native 链接另循其合同。

绑定描述保存类型、可变性和初始化约束，共享 cell 写入遵守同一契约。Match 的输出仅在匹配成功时初始化。MemberGet 对类方法形成绑定接收者，表中的普通闭包保持普通函数。
调用展开的参数容器为当前调用私有。array 的分片浅捕获从开始至完成检查完整内容版本，包括元素覆盖、增删和重排；变化则以 ConcurrentModification 失败且不调用目标。tuple 外层不可变。捕获完成后，源容器后续修改不使已捕获外层序列失效；内部对象修改仍通过共享引用可见。游标、版本、部分捕获和待释放存储均计入容量与根，失败沿既有异常清理释放；不得把 BudgetYield 当作外部等待或丢失中间状态。

集合展开采用相同的分片浅捕获规则。`[...source]` 接受 array/tuple，`{...source}` 接受 table；null 或其他类型在展开位置报 Type。每个展开在其源码位置捕获完成，再执行后项表达式。表的显式键和值仍全部先求值，再按书写顺序构造并验证键；非法展开源则立即失败。后项覆盖已有键时保留键首次出现的顺序。构造结果在整条表达式完成后才发布，异常或取消释放未完成的容器。B01 使用 ArrayPush/ArraySpread 和有序 TableSources 描述符；TableSpread 复制已验证自有槽，空目标直接线性复制，合并到非空目标仅比较目标原有条目，不重复扫描本次已追加的源条目。每片工作及实际分配计入既有预算，不另设集合长度上限。

赋值位置与实参保持源码求值顺序。全部实际参数完成求值后才按签名绑定；有序参数容器保留重复名称，最终绑定阶段报告冲突。无展开普通调用使用连续参数窗口；展开在其源码位置捕获元素，随后参数的副作用不能反向改变先前展开。可变捕获使用共享 cell，每轮循环的绑定独立；不会保存可移动窗口的地址作为 upvalue。

异常区域与清理
保护区、catch 和 finally 的普通控制流边均由验证器核对。Jump、Branch 与自然落空进入 handler 的包会被拒绝；正常离开保护区或 catch 使用 Leave，return/throw 由统一展开器执行清理。catch 内抛出的新异常交给外层处理器，finally 内的新异常继续向外展开；每个 EndFinally 与当前活动展开记录匹配。清理中的普通调用继承禁止等待上下文。共享格式的坏包用例覆盖显式跳转、自然落空和异常边。

贯通验收
共享格式、验证器和手写包先固定；lowering 与 dispatcher 随后并行使用它们。源码验收必须真实经过解析、名称/捕获分析、寄存器 IR、字节码验证和执行：普通 helper 内的 native Ready/Pending 均得到同一结果；指令预算设为 1 时仍保持结果和副作用次数。native 入口只调用一次，重复/旧 epoch 完成被拒绝；暂停参数只由帧保活，期间完成 GC 后仍有效，任务与结果释放后可回收。
CallScript/Continue 验收多次恢复、callback fault 和受限上下文；帧压入/弹出、已扫描寄存器改写、根区复用与搬迁在每个 GC 步之间验证。Quirrel 转换断言接入相同源码链路，语法通过和执行通过分列。快照 round-trip、完整标准库、类/模块、生成器和异步语义各自按既定规范验收；单条源码执行成功只记录为执行链路贯通。

## 二十三 完整故障记录与生命周期

本节规定实现目标。执行器的实现和独立验证进度在对应任务中记录。

### 作者可见的行为

catch 绑定原抛值。字符串、整数、null 和对象都保持原有值语义；对象继续指向原对象。throw; 在当前函数词法上最内层活动 catch 中重抛同一故障，保留身份、错误码、原位置和原始调用栈。throw value; 创建新的故障，即使 value 与先前抛值相同，也有新的位置和调用栈。

core.error_info() 返回当前实际执行调用链上最近的活动 catch 的 FaultInfo；没有活动 catch 时返回 null。普通 helper 和同 Task 的 native callback 沿用调用链，新的 async Task 使用自己的上下文。throw; 的词法规则保持不变，helper 需要显式 throw value 或由宿主转发。catch 的活动期从处理器入口开始，到实际离开其范围结束；进入同一区域的 finally 后，该 catch 已退出。挂起的 generator、其他 Task 和已结束 catch 均不参与查询。

FaultInfo 是同一堆中的可追踪只读视图，保存它即可保留完整故障，普通变量释放后随对象图回收。视图公开 value、code、origin、stack 和带种类的 causes；value 所引用的作者对象仍遵守自身可变性。查询依赖当前执行上下文，声明为诊断读取效果，派生纯计算中给出效果诊断。运行时产生的语言错误使用 core.Error 值，含固定 code、message 和只读 details；作者可以抛任意值。Envelope 的稳定 code 来自版本化注册表，作者表中的同名字段保持普通数据。

新故障默认关联同一实际调用链中最近活动 catch 的完整故障。native 显式提供 cause 时以它替代这条隐式 catch 关系；finally 替代退出产生的关系另行保留。跨 Task 的原因通过明确的任务/等待传播记录连接。

### 同一堆中的故障图

ErrorEnvelope、分块调用栈与原因节点使用现有 Heap 的私有对象种类。FaultId 封装完整 HeapHandle；通用字段写入与对象构造入口维护这些内部种类的封装。复制 ID 本身沿用普通句柄的保活规则，访问时核对堆、epoch、代际和对象种类。一个已登记的故障根能够保住原值、调用栈、原因及对应代码版本。

建立完成后，故障身份、原值、code、origin 和原始 stack 固定。原因关系由 VM 追加，已有节点保持原内容；FaultInfo 显示当前已追加的关系。关系具有明确种类：
- CaughtFault：建立新故障时关联的活动 catch。
- ExplicitCause：native 或显式包装关联的原故障。
- AwaitedFault：本次 await 交付关联的生产者完整故障与稳定等待 activation。
- ReplacedExit：finally 真正替代的退出，保存替代位置及 Throw(FaultId)、Return(Value)、Leave(CodeRef, target) 或 Close(kind, logical_id, activation)。Close 区分 generator/thread。合作取消按既有规则注入可捕获的 Cancelled，因此其退出保留完整 Throw(FaultId)；owner/region 的硬撤销遵循既有硬终止规则。

同一故障可以同时保留 catch 原因与被替代的 Return。原有原因始终保留。每次展开事件具有稳定身份，按预算暂停后继续只追加一次。重抛和显式转发可以形成共享引用或回边；诊断游标记录已访问身份并输出引用关系，GC、快照和诊断均以显式游标遍历。原始栈只捕获一次。失败的 await 为每个实际等待者建立独立 envelope：value/code 来自生产者故障，origin/stack 来自消费者的 await 位置和调用链，AwaitedFault 指向生产者完整故障。生产者记录保持原状，因此多个等待者拥有各自的传播链。Future adoption 传递故障身份，实际 await 交付时才包装。稳定等待 activation、交付/FaultBuild 阶段与已建立的包装身份一起保存；生产者故障保留到完整包装根发布，分片或恢复后只建立一次；之后再次 await 使用新的 activation。若 await 位于活动 catch 内，其 catch 原因也按通用规则保留。包装使用同一 FaultBuild 和容量规则。

异常路由使用当前抛出/重抛/调用处的 PC 和已消费区域；原始故障位置用于诊断。finally 内被其内部 catch 处理的错误保持外层 pending exit。错误真正逃出 finally 时，先把完整旧退出发布到原因图，再清除旧展开根。

### native、交付与 catch 根

宿主响应明确区分 Error(NewFault { value, code, cause }) 与 ForwardFault(FaultId)。前者建立新故障；后者传播当前 native 持有的完整故障，从原调用位置展开。原值的位比较仅用于值语义，故障转发由显式身份决定。

NativeFrame 持有有界故障槽，与 continuation values 一起记账、扫描和保存。CallbackFault 先进入这些槽，再释放 callback 帧。Continue 和 Pending 默认保留故障槽；后续任何阶段都可读取它们。再次 CallScript 前，将仍需保留的故障移入明确的续体槽，或显式标记已处理。保留策略使用 Preserve 或 Replace(ids)，空集合表示已处理。Ready 在发布结果后释放剩余故障；Error/ForwardFault 先发布目标故障根，再释放来源。取消按同一分步清理规则释放全部槽。

delivery_value 读取原值时保留原交付。delivery_fault 返回完整视图。take_fault_delivery 将整个 envelope 转移到寿命独立于该 Task 的宿主根，成功后才释放交付和 Task 槽。返回的 FaultRoot 表示拥有的保留关系，必须显式释放。只复制原值的接口使用 copy_delivery_value_to_root，原故障交付保持到显式 discard。统一领取接口也可以返回带 Returned/Fault 标签的拥有型结果。

每个 catch 保留记录包含 FrameId、RegionId 与本次 activation。控制转移先发布重抛、cause 和 pending exit 所需的目标根，再删除实际退出的 catch 记录，最后提交 PC 或帧释放。仍包围目标位置的外层 catch 继续保留。普通 helper 调用维持父帧的活动 catch。作者已复制到变量或闭包的原值由正常对象图保活。容量按同时活动的 catch/展开记录计数，顺序执行独立 catch 可复用空间。

### 捕栈、预算与硬容量

FaultBuild 是 Task 的显式可恢复阶段。它先保留原值、原故障与 pending exit，再分块捕获版本化栈；对应帧在记录完成前继续保留。其他 Task 可以运行。完成后才向 catch、native 或交付公开完整故障。每条栈记录包含固定代码/模块/源码映射版本、函数、PC 和帧类别；native 边界包含 ABI、续体版本/阶段和原调用位置。记录持有真实的代码版本寿命，局部变量仍由原执行状态负责保活。

栈块、原因节点、游标、代码版本、native 槽、退休空间和释放队列均计入相应容量账本。根和边先经过屏障再发布，来源随后清除。捕栈、原因追加、扫描及释放均按工作额度分步。预算不足返回 BudgetYield 和所需工作量，恢复沿用阶段，已经生效的 throw/native 动作只执行一次。

普通资源错误在能够建立完整记录时成为可捕获的 core.Error。若建立故障本身遇到硬容量限制，返回 ResourceBlocked { task, resource, current, required } 给宿主；这项通知使用 Task 预留的控制状态，保留已有根、帧和构建游标。宿主可以执行回收/释放、在许可范围提高额度后恢复同一阶段，或取消该 Task。required 给出下一步推进所需的已知最低额度。故障尚未完整时维持阻塞状态，所有已产生副作用保持原状态。真实进程级分配失败仍服从平台分配失败策略。

invoke_sync 的累计硬预算规则保持原定义。该路径遇到 ResourceBlocked 时终止本次同步重入，向宿主报告资源失败，并按有界释放流程清理状态；它返回拥有型成功/故障或明确的控制失败，保留已发生的副作用。常规可恢复任务使用 run_slice。

### 保存与验收

快照保存完整故障图及共享/回边、code 注册表版本、固定代码/源码映射/native 版本、活动 catch、pending exit 与路由 PC、消费区域和 finally 阶段、FaultBuild 游标、native 故障槽和未领取交付。每次追加的事件身份一并保存。恢复先建立新对象身份再连接图；旧 HeapId、FaultId 和根 token 继续失效。缺失所需版本时给出精确失败位置。

验收覆盖：相同原值的新 throw 与显式转发；CallbackFault 经 Continue、Pending 和再次 CallScript 后传播；领取完整故障后结束 Task 并进行 GC；多层 finally 替代原因；退出内层 catch 后重抛外层；顺序 catch 次数远超同时活动容量；逐片捕栈、硬容量阻塞及恢复；快照恢复期间原因只记录一次。这些用例的源码检查、作者自测和独立运行分别记录。

## 参考来源
- Value 实验：https://github.com/jhq223/reel/pull/2
- 实验说明：https://github.com/jhq223/reel/blob/147c4cf788078c72b4ca5206270fce9dc29e12df/tools/value-layout-bench/TRACE.md
- 实验结果：https://github.com/jhq223/reel/blob/147c4cf788078c72b4ca5206270fce9dc29e12df/tools/value-layout-bench/REPORT.md
- slotmap 键契约：https://docs.rs/slotmap/1.1.1/slotmap/struct.KeyData.html
- slotmap 实现：https://docs.rs/slotmap/1.1.1/src/slotmap/basic.rs.html
- 增量回收参考：https://github.com/kyren/gc-arena


执行器整合状态（PR #49）：已接入 PR #23 的 V03 执行器及 PR #54 的 array/table、Index/Member、tuple 常量、调用展开和脚本 rest；Windows 原生 debug/release 已通过实际执行、GC、异常、取消及源码贯通检查。未支持的 delegate/hooks、property-bag、ArgsNamedSpread 和模式仍定位拒绝。owner 完整撤销由宿主协调，Vm::cancel(owner) 仅取消任务；Idle 不能代表独立宿主根已释放。单根与非任务 RootRange、任务状态及外部资源都完成有界清理后才能退休，细节见架构的 owner 撤销合同。

实现边界补充（S01 / PR #50）：Rust 会话宿主和真实窗口的对白、选择、取消及退休账本不执行 Aside。它们为后续宿主适配提供模型和视图，不能证明可信 native 已注册、等待恢复/根移交已接通，或本规范的完整会话与保存恢复已完成。

集合执行源码进展（PR #54）：分片构造先保留中间根再发布；table 同键覆盖保留首次插入顺序，比较使用既有精确数值规则，嵌套不可散列键的错误 details 为零起始 tuple 元素路径。FaultKind 追加 OutOfBounds=14、MissingKey=15、UnhashableKey=16、ConcurrentModification=17，原编号和 B01 v1 不变。ArgsSpread 保留浅捕获与捕获期间完整内容版本检查，packed 目的状态也需验证并原子预检双项追加；rest 每次创建独立外层 array，包括空值和宿主启动。运行、GC、异常和取消合同已通过 Windows 原生 debug/release 验证。table 为预算内线性查找，反复构造可能为平方复杂度；同步 tuple 常量受单项额度限制，不能据此宣称性能达标或完整语言已接通。


### 宿主属性的同步验证
宿主对象可以携带 HostPropertyValidator 纯函数，接收只读 Heap、接收者 Value、已定位的固定属性索引和新 Value，在现有属性赋值提交前返回结果。验证不进入脚本、不等待、不改堆；失败走原赋值点的语言错误路径，try/catch 可捕获且旧值保持。alloc_host 与 alloc_composable_host 显式传入可选验证器，元数据归对象所有，不建立额外能力注册。私有字段更新使用 set_host_payload 并经过统一 GC 写屏障；退休清除属性表和所有私有强边。宿主对象退休同时断开依赖它的派生计算，冻结最后成功值并清除排队重算；直接访问退休后的宿主对象返回明确的 RetiredHost，并在属性访问或宿主调用原位变为可捕获的 InvalidState；它与损坏、跨堆等无效句柄错误保持区分。

源码定位使用首尾有效语法 token，不把前导空白或注释作为错误位置；实际源码文件路径与逻辑模块身份分别保存。窗口显示源码路径、行列以及脚本抛出的字符串或标量值。

## Reel 音频宿主绑定

Reel 的外围名称环境包含 `audio` 命名空间，独立 Aside 不注册音频名称。`audio.play(source, looping:false, volume:1, owner:null, group:"effects", pan:0, fade_in:0)` 与 `audio.volume(activity,value,duration:0)` 使用 ABI v2；新增 `audio.group_volume(group,value,duration:0)`、`audio.pan(activity,value,duration:0)`、`audio.stop(activity,fade:0)` 使用 ABI v1。ASB 格式仍为 B01，旧音频调用产物需要重新编译。

分组为 music/voice/effects/ui，group_volume 另接受 master 控制总音量。音量为非负有限线性增益，pan 为 [-1,1]（左到右），时间为非负有限秒。每声、组、总音量相乘。过渡复用 Kira：音量在分贝域插值，声像线性插值，静音端点为 -60 dB；新命令从当前值替换未完成过渡。加载中的音量/声像变更设置首次播放值，fade_in 从实际播放开始。组设置也影响随后播放的声音。

播放活动通过标准 native awaitable 协议接入 await；pause/resume/stop 仍由 Reel 按活动类型分派。audio.stop 的淡出使用音频消费时钟，真正停止后才返回 Stopped；尚未开始或已暂停的声音立即停止。音量/声像控制返回原活动，group_volume 返回 null。暂停保留播放位置，音量/声像包络继续按混音时钟推进；输出挂起冻结全部包络。淡出停止期间再暂停会立即完成停止。owner 取消与关闭立即终止。读取与设备错误通过同一 host completion 传播到原始 await 位置。活动是只读宿主值，终态保存在 VM 堆内，活跃生产者保留宿主根；取消消费者仅撤销等待，owner 退休才取消生产者。
