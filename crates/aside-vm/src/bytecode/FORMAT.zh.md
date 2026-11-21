# Aside 字节码 v1

[English](FORMAT.md) · 简体中文

编译器和 VM 共用此编码。格式变更后需重新构建产物。数据与标签见 [model.rs](model.rs) 和 [codec.rs](codec.rs)，指令操作数见 [spec.rs](spec.rs)，程序包验证见 [verify.rs](verify.rs)。创作者可见的语义见[语言规范](../../../../docs/aside-language.zh.md)。

## 文件头与记录

68 字节文件头包含 `ASIDBC01`（8 字节）、格式版本、语言版本、宿主 ABI（各 u32）、content_version（u64）、编码字节总数（u32）和 9 个区段的记录数（各 u32）。载荷以可选 `TextPoolDescriptor` 开始，先写一个 u8 存在标志，存在时再写 u64 字节数以及 32 字节 BLAKE3 摘要。

区段依次是符号、类型、常量、签名、原生函数、模块、函数、能力和源码。各区段再次记录 u32 数量，必须与文件头一致。整数用小端编码。向量和 UTF-8 字符串用 u32 长度。bool 和 Option 用 u8 标签 0 或 1。枚举使用明确的 u8 标签。字段紧密排列，无填充。各类 ID 使用独立的 u32 新类型。磁盘和运行时 PC 均按 u32 执行字寻址。

记录内的字段顺序沿用 model.rs，各记录前缀如下。

- Module 先写全局数量，再写路径、导入、导出、全局项和初始化入口。
- Function 先写捕获、绑定、描述符、代码、异常区域和源码映射的数量及 register_count，再写模块、名称、类别、签名、上下文以及对应六张表。代码数量是逻辑指令数。代码表条目是全局字起点和字长度（各 u32）。
- 共享代码位于模块之后、函数之前。先写字总数（u32）和块数（u32），再写连续的压缩 `(offset, length)` 对（各含两个 u32），最后写压缩字节数（u32）和压缩数据。每个 zlib 块独立解压，除末块外均为 32 KiB。各函数范围按声明顺序划分字流，指令可以跨块。
- 源码映射先写 u32 区间数，非空时再写 u32 压缩字节数和压缩数据。每 32 个区间为一组，将前序 PC、文件和起点从零重新计算。每个区间保存四个采用 ZigZag 变换及最短无符号 LEB128 编码的差值。PC、文件和起点分别减去前一值，终点减去本区间起点。每个差值支持两个 u32 相减的完整有符号范围。运行时保留压缩数据和稀疏 `(PC, byte offset)` 索引，查找时二分索引并至多解码 32 个区间。
- 源文件记录含路径 SymbolId（u32）、原始 UTF-8 字节数（u32）、压缩字节数（u32）和一条独立的 zlib 流。编码使用固定版本的压缩器，级别为 1。加载先准入完整的原始长度及解压工作区，然后分配，验证压缩流、准确长度和 UTF-8，再把源码保留为诊断使用的普通文本。

| 记录 | 标签 |
| --- | --- |
| Const | 0 Null, 1 Bool, 2 Int64, 3 Float64Bits, 4 Utf8, 5 Bytes, 6 Tuple, 7 Text(u64 offset,u32 length) |
| TypeDef | 0 Builtin，1 Tuple，2 Union，3 Function，4 Named，5 StringLiteral，6 Record，7 Array，8 带可选字段的 Record |
| FunctionKind | 0 Plain, 1 Async, 2 Generator, 3 Native(u32 NativeId) |
| Context | 0 Ordinary, 1 NoWait, 2 Derived, 3 Init |
| CaptureKind | 0 Value, 1 Cell |
| Descriptor | 0 Global, 1 Closure, 2 Call, 3 Sequence, 4 Table, 5 Class, 6 Super, 7 Property, 8 Pattern, 9 Spawn, 10 TableSources |
| BindingInitialization | 0 Explicit, 1 Parameter(u32) |
| ParameterDefault | 参数的 Option 内为 0 Constant(u32 ConstId) 或 1 Function(u32 FunctionId) |
| TableSources 条目 | 0 Pair([Reg;2]), 1 Spread(Reg) |

执行准入接受 Plain、Generator 和 Native。Async 标签保留。Generator 函数在调用时绑定实参与默认值，再返回惰性生成器。

元组常量及结构化 tuple、union、function 类型引用前面的记录，组成有限的有向无环图。命名类型保留可递归的名义身份。模块导入固定模块、导出项和版本。全局变量保留类型、可变性和可选的链接期函数。闭包按准确的 Value 或 Cell 布局捕获绑定或上层捕获。类的初始化器、方法及模式默认值保留捕获来源映射。

## 参数与原生合同

Signature 包含普通参数、可选 rest、结果类型、效果和可选 PropertyBagTail。Parameter 包含名称、类型、可变标志及可选 Constant 或 Function 默认值。常量通过共享的分片加载器物化。Function 默认入口只接收前序参数和定义处捕获，类别与上下文为 Plain 和 NoWait。原生函数的 Function 默认入口无脚本捕获。两种默认值都在调用处验证所产生的参数值。

参数寄存器位于前缀。Parameter(i) 绑定记录保留名称、类型、可变性和 Value(Reg(i)) 或 Cell(Reg(i)) 存储形式。省略记录表示 Value。绑定器按顺序创建并初始化参数 Cell。函数及其 Function 默认入口共享前缀存储，可变参数必须使用 Cell。内部默认值求值传递这些 Cell，普通调用会建立新绑定。原生存储布局由最长的 Function 默认值前缀推导，Constant 默认值不提供前缀。VerifiedProgram 保留推导出的稀疏 Cell 元数据，查询布局时无需分配。

CellNew 寻址 Explicit 类型的 BindingId。BindInit 只发布一次，BindWrite 和 CellWrite 检查共享 Cell 的初始化、类型和可变性。类型引用保留包版本和定义模块。未初始化、已初始化为 null 和 GC 释放各有独立状态。

NativeDef 声明规范符号、签名、ABI 和能力。原生 FunctionDef 复用该签名，上下文为 Ordinary，脚本表为空。MakeClosure 读取有根的稳定代码值。普通调用与宿主回调共用原生帧绑定。可选的 PC 0 源码条目标识宿主发起的调用，脚本调用仍保留各自的位置。

Rest 用新建的外层数组收集剩余位置实参。紧凑的名称条目是 -1、静态 SymbolId 整数或由 GC 追踪的不可变字符串。ArgsSpread 将数组和元组捕获为位置参数，将表中字符串键的自有条目捕获为具名参数。实参求值后在最终绑定中报告重复和顺序冲突。PropertyBagTail 携带目标参数、模式能力及写入与动画要求。运行时准入选择受支持的能力。

## 指令

执行字的 0–7 位为操作码，8–9 位为后续字数量，10 位为 B/C 打包标志，11–31 位为首操作数。首操作数的 21 位全为 1 时，下一个字提供完整 u32。缺省操作数为零。仅 B 非零时使用一个字。B/C 可以打包为一个含两个 u16 的字，也可占两个完整字。PC 操作数统一占一个完整字，以便先固定布局，再映射指令序号目标。执行前检查规范的指令头、未使用操作数的零值以及指令边界。

编译器的可编辑指令长 16 字节，PC 使用指令序号。验证时将分支、异常区和源码区间一次映射到字 PC，压缩共享字流，再释放这些向量。加载保留同样的压缩块，通过有界的页读取器验证，无需展开整个指令包。同一已验证程序的所有 VM 共享按需填充的 8 MiB 热页缓存。VM 的取指窗口无锁读取当前页，换页时复用有界缓存。帧和快照保留字 PC。分支、异常和恢复的 PC 均由边界位图准入。`to_unpacked` 是显式的、可能失败的编辑操作，可还原指令序号 PC。代码读取及迭代器会传播解压、分配和缓存错误。

存储统计分别记录不可变冷容量、当前与峰值热驻留、热页限额和构建临时内存。`PackageStats` 包含实际拥有的容量，执行准入预留完整代码容量上限。所有指令都能在 VM 安全点按预算让出。

表中暂停列描述语言层的暂停。W 为外部等待，Y 为生成器产出，T 为线程暂停。调用继承累计的上下文和效果限制。NoWait 内可进行同步调用。

d 为写入 Reg，x/y/z 为读取 Reg，k 为 ConstId，s 为 SymbolId，b 为 BindingId，c 为 CaptureId，g 为 Global 描述符，q 为带类型的描述符，p 为 Pc。描述符访问器提供寄存器窗口和条件写入。范围的尾端可等于代码长度，指令目标必须是实际指令。空的寄存器窗口可从 register_count 开始。

| 十六进制 | 操作码 | 操作数或带类型的描述符 | 控制流 | 可抛错 | 暂停 |
| --- | --- | --- | --- | --- | --- |
| 0001 | LoadConst | d,k | 下一条 | 是 | - |
| 0002 | Move | d,x | 下一条 | 否 | - |
| 0003 | Clear | d | 下一条（设置未初始化位图） | 否 | - |
| 0004 | BindInit | b,x | 下一条（只初始化一次） | 是 | - |
| 0005 | BindRead | d,b | 下一条 | 是 | - |
| 0006 | BindWrite | b,x | 下一条（运行时检查可变性） | 是 | - |
| 0007 | CellNew | b | 下一条（按 BindingDef 合同分配未初始化 Cell） | 是 | - |
| 0008 | CellRead | d,x | 下一条 | 是 | - |
| 0009 | CellWrite | x,y | 下一条 | 是 | - |
| 000a | CaptureRead | d,c | 下一条（按声明返回 Value 或 Cell） | 是 | - |
| 000b | GlobalRead | d,q:Global(module,global) | 下一条 | 是 | - |
| 000c | GlobalInit | x,q:Global | 下一条 | 是 | - |
| 000d | GlobalWrite | x,q:Global | 下一条 | 是 | - |
| 000e | Clear2 | d,d | 下一条（把两个槽设为未初始化） | 否 | - |
| 000f | Clear3 | d,d,d | 下一条（把三个槽设为未初始化） | 否 | - |
| 0010–001e | Add,Sub,Mul,Div,Rem,BitAnd,BitOr,BitXor,Shl,Shr,Eq,Ne,Lt,Le,Gt | d,x,y（编码按列出顺序） | 下一条 | 是 | - |
| 001f | Ge | d,x,y | 下一条 | 是 | - |
| 0020–0023 | Neg,Pos,Not,BitNot | d,x | 下一条 | 是 | - |
| 0024 | Is | d,x,TypeId | 下一条 | 是 | - |
| 0025 | TextNew | d | 下一条 | 是 | - |
| 0026 | TextPush | x,y | 向构造器追加标量或字符串 | 是 | - |
| 0027 | TextFinish | d,x | 验证并封存 UTF-8 构造器 | 是 | - |
| 0028 | CheckType | x,TypeId | 下一条（不匹配时报 Type） | 是 | - |
| 0030 | Jump | p | 跳转 | 否 | - |
| 0031 | BranchBool | x,p,p | 按严格 bool 双向分支 | 是 | - |
| 0032 | BranchNull | x,p,p | 按 null 与否双向分支 | 否 | - |
| 0040 | MakeClosure | d,q:Closure(function,capture sources) | 下一条 | 是 | - |
| 0041 | Call | d,q:Call(target,window/packed args,names,optional positional contract) | 返回或恢复后继续到下一条 | 是 | W |
| 0042 | Return | x | 完成清理后返回 | 是 | - |
| 0043 | ArgsNew | d | 下一条 | 是 | - |
| 0044 | ArgsPush | x,y | 下一条 | 是 | - |
| 0045 | ArgsNamed | x,y,s | 下一条（按顺序保留重复条目，最终绑定时诊断） | 是 | - |
| 0046 | ArgsSpread | x,y | 下一条（此时捕获元素） | 是 | - |
| 0047 | ArgsNamedSpread | x,y | 下一条（此时捕获字符串键条目） | 是 | - |
| 0048 | CallNoWait | d,q:Call | 同步返回后继续到下一条，被调用者继承 NoWait | 是 | - |
| 0050 | MakeTuple | d,q:Sequence(window) | 下一条 | 是 | - |
| 0051 | MakeArray | d,q:Sequence | 下一条 | 是 | - |
| 0052 | MakeTable | d,q:Table/TableSources(ordered entries) | 下一条 | 是 | - |
| 0053 | IndexGet | d,x,y | 下一条 | 是 | - |
| 0054 | IndexSet | x,y,z | 下一条 | 是 | - |
| 0055 | MemberGet | d,x,s | 下一条（只绑定类方法，表中的闭包仍为普通函数） | 是 | - |
| 0056 | MemberSet | x,y,s | 下一条 | 是 | - |
| 0057 | TableSpread | x,y | 下一条（捕获自有槽） | 是 | - |
| 0058 | ArraySpread | x,y | 下一条（捕获元素） | 是 | - |
| 0059 | MakeClass | d,q:Class(nominal type,base,fields,methods,initializers) | 下一条 | 是 | - |
| 005a | SuperGet | d,x,q:Super(lexical base,symbol) | 下一条 | 是 | - |
| 005b | PropertyRef | d,x,q:Property(key,read/write) | 下一条 | 是 | - |
| 005c | ChildrenPush | x,y | 向数组追加可组合的宿主值，跳过 null | 是 | - |
| 005d | MakeRange | d,x,y | 升序整数区间，排除终点 | 是 | - |
| 005e | MakeRangeInclusive | d,x,y | 升序整数区间，包含终点 | 是 | - |
| 005f | ArrayPush | x,y | 向数组追加一个值 | 是 | - |
| 0060 | Match | d,x,q:Pattern(nodes,outputs) | 下一条（bool，成功后提交输出） | 是 | - |
| 0061 | PatternRequire | x | 下一条（为 false 时报告 PatternMismatch） | 是 | - |
| 0062 | IterInit | d,x | 下一条 | 是 | - |
| 0063 | IterNext | d,x | 下一条（内部元组含 done 和 value） | 是 | - |
| 0064 | IterClose | x | 下一条 | 是 | - |
| 0065 | AsyncIterInit | d,x | 下一条 | 是 | - |
| 0066 | AsyncIterNext | d,x | 下一条（Awaitable，随后显式 Await） | 是 | - |
| 0067 | AsyncIterClose | d,x | 下一条（Awaitable） | 是 | - |
| 0068 | IterAdvance | d,x,y | 将 done 写入 d，value 写入 y | 是 | - |
| 0069 | MatchFail | - | 抛出 MatchError | 是 | - |
| 006a | SuperRef | d（捕获当前方法的接收者和词法基类） | 下一条 | 是 | - |
| 006b | ChildrenSpread | x,y | 下一条（捕获可组合的子节点，跳过 null） | 是 | - |
| 006c | PatternField | k,x,d | 下一条（读取表 `x` 的自有键 `k`，将 `k` 替换为存在标志，将字段值或 null 写入 `d`。`d = k` 时只检查是否存在） | 是 | - |
| 006d | MakeTypedArray | d,q:Sequence,t（元素类型） | 下一条 | 是 | - |
| 0070 | Throw | x | 抛出并展开 | 是 | - |
| 0071 | Rethrow | — | 抛出（仅限 catch） | 是 | - |
| 0072 | Leave | p | 展开并清理后跳到目标 | 是 | - |
| 0073 | EndFinally | RegionId | 恢复已保存的展开 | 是 | - |
| 0080 | Yield | d,x | 产出 x。恢复时执行下一条，并令 d 为 null。仅限 Generator | 是 | Y |
| 0081 | Await | d,x | 下一次调度 | 是 | W |
| 0082 | Spawn | d,q:Spawn(call,owner Reg) | 下一条（任务 Future） | 是 | - |
| 0083 | Cancel | x,y | 下一条（协作式请求） | 是 | - |
| 0084 | ThreadNew | d,x | 下一条 | 是 | - |
| 0085 | ThreadResume | d,x,y | 下一条（同步驱动） | 是 | - |
| 0086 | ThreadSuspend | d,x | 恢复后继续到下一条 | 是 | T |
| 0087 | ThreadClose | x | 下一条 | 是 | - |

## 控制流与源码位置

每条源码映射记录都定义一个起点，其覆盖范围持续到下一条记录的起点或函数末尾。首条记录之前的 PC 没有源码位置。相邻的相同范围可以合并。可编辑函数保存普通区间向量，验证时转换 PC 并压缩一次。载入与执行沿用压缩数据，显式调用 `to_unpacked` 恢复向量及指令序号 PC。多种表示的源码路径、字节范围和调试诊断位置一致。

异常区域含保护区、catch 和 finally 的半开范围。各范围彼此包含或分离，同一嵌套区域的各块有共同的词法父级。普通分支或自然落空不能跨越清理边界。Leave 和 Return 由内到外运行 finally，故障沿同一异常区树选择 catch 或 finally。catch 内的故障跳过当前 catch，finally 内的故障也跳过同一个 finally。替代故障保留原因链。finally 内的调用继承 NoWait，显式暂停、return 和跳出均会被拒绝。

展开游标记录已消费的 RegionId，只沿原始 PC 的严格祖先继续。EndFinally 要求存在对应的活动展开记录。如果 Leave 目标仍位于一层包含它的源码块内，则保留该层。`exception_handler` 和 `cleanup_handler` 查询已验证的元数据，执行器持有有根续接状态并负责工作计费。

Await 通过可信的单参数原生适配器分派 Host 对象，保留调用方的结果槽和源码位置。已完成结果和待处理等待使用相同的调度与带代际检查的取消协议。

## 字面量与集合捕获

发行版使用偏移和长度把 UTF-8 常量外置到文本池，执行前绑定并验证描述符摘要、UTF-8 和字符边界。字符串共享内联或池内存储。宿主绑定完整的不可变文本池。弱缓存复用仍存活的常量对象。快照保留池内文本的常量 ID，动态字符串则保存实际字节。元组常量增量物化。

`TextPool::from_bytes` 取得 `Vec<u8>` 的所有权，无需复制其字节分配。文本池克隆后共享同一个不可变 `Arc<Vec<u8>>`。分配账本包含向量完整容量、向量头和共享所有权头，包括宿主传入的未使用容量。

ArraySpread 和 TableSpread 在书写位置捕获来源，并跨暂停检查内容版本。发布前目标保持为私有构造器。数组展开到自身时只复制一次初始前缀，表展开到自身时保留原内容。MakeTable 按顺序读 Pair 或 Spread 条目，后一次覆盖保留首次插入位置。目标为空时直接复制唯一条目。合并到非空目标时，按原目标前缀进行 O(n*m) 次比较，键相等检查也按预算拆分。

TextPush 分片追加标量或字符串并计费。TextFinish 验证并封存 UTF-8，同时保留根并检测并发修改。区间使用 int64 端点、步长 +1，倒置区间为空，到 MAX 时安全结束。IterAdvance 直接写入 done 和 value，IterNext 返回包含两者的显式元组。

生成器结束时只交付一次最终返回值，此时 done=true，后续读取返回 (true, null)。产出的 null 对应 done=false。IterInit 保留生成器身份。IterClose 沿常规 finally 展开关闭生成器，尚未开始的生成器直接关闭。逃出正文的故障先关闭生成器，再传播原故障信封。生成器执行继承 NoWait 边界，内部工作仍可按预算暂停并恢复。

数组迭代允许覆盖元素，结构改变会被拒绝。ChildrenPush 收集可组合的 Host 值并忽略 null。ChildrenSpread 分片捕获数组或元组的每个元素，应用同一规则。CallNoWait 将等待限制传递给普通辅助函数。

## 验证与执行准入

解码在物化前扫描计数和形状，采用带检查的算术、可失败的容量预留，以及整包的字节、记录、寄存器和工作限制。验证检查引用、有向无环图、存储、区域和源码映射。共享扫描与比较在遍历前准入工作量和临时空间。默认工作额度覆盖完整扫描及物化，宿主也可显式配置。

构造 `VerifiedProgram` 须先验证。执行还使用独立可信的 RuntimeSupport 检查全部操作码、原生签名、ABI 和能力，包括不可达指令。格式工具可以检查当前运行时尚未实现但可表示的操作。宿主将自行注册的类型和名称解析到包内表，再授权能力。编解码与验证的输入保留在 [fixtures.rs](fixtures.rs) 和 [testdata](testdata)。
