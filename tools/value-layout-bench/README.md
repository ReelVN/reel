# Value 布局设计实验

本目录是性能选型夹具，不是生产 VM 实现。依据 `docs/aside-language.md`、
`docs/engine-architecture.md` 与作者指南设计版本 1.0；不更改语言、引擎接口或
`docs/`。基线提交：`98f4be2344c1ac12815d19b6bc0ace39cdc77cc4`。
范围仅本目录，依赖只有 Rust 标准库；汇总脚本仅依赖 Python 标准库。
结果和建议见 [REPORT.md](REPORT.md)，原始采样保存在 [results/](results/)。
新增 [有界解释执行 trace](TRACE.md) 使用同一标量取指循环和动态类型检查，
覆盖剧情变量、UI 属性、浮点动画及独立 wide-int 压力；数组内核仅是组成成本。
trace 二进制为 trace-bench，数组二进制仍为 value-layout-bench。
下文说明原数组夹具；trace 的 wide 共享/释放策略与存储核算单列于 TRACE.md。

## 表示与边界

- `Explicit`：`repr(C, align(8))`，64-bit 显式 tag + 64-bit payload，16 字节。
  int64 全部内联，浮点保留原始位模式。
- `NanBox`：一个 u64，8 字节。`0x7ff9` 到 `0x7ffd` 五种高 16-bit 标签表示
  null、bool、small int、object handle、wide int handle。其余位模式为 f64；
  所有输入 NaN 归一为 `0x7ff8_0000_0000_0000`，保留非 NaN 的全部位，含负零。
  内联整数范围为 `[-2^47, 2^47-1]`；范围外每次构造分配独立 `Box<i64>`，
  通过代际句柄读取，不去重，不截断完整 int64。
- 句柄载荷低 24-bit 为 index，高 24-bit 为 generation，generation 从 1 开始。
  index `0..=0xffffff`；generation `1..=0xffffff`。代数耗尽释放后永久退役槽位，
  不回绕；测试直接设置临界代数来验证退役。容量耗尽用夹具断言表达，不是 VM
  的资源诊断协议。只验证句柄最大位边界，不真的分配 1600 万个对象。
- 两种表示使用同一 `Arena<Object>` 实现：索引界限与代数检查、同一种对象
  `Box`、同一种读取/释放路径。wide int 使用同一泛型 Arena 实现的另一个堆。
  slot 含 generation、free-list next 和 `Option<Box<T>>`；x64 为 16 字节，
  i686 为 12 字节，编译时断言检查。Value 和句柄编码不装入原生指针。
- `Batch` 唯一拥有每个分配，读取只复制 Value 位；未实现共享 Value 的 retain、
  引用计数、写屏障、循环回收、快照、算子、字节码或任务。这些须在设计审查后
  独立实现/验证。此处 NaN 归一化是候选设计条件，不是对语言规范的修改。

边界验证覆盖 bool/null、signed 48-bit 端点及邻值、int64 MIN/MAX、普通浮点、
最小 subnormal、最大 finite、无穷、正负 signaling/quiet NaN、与标签冲突的 NaN
位模式、正负零、句柄最大 index/generation、陈旧/重复释放/越界句柄、槽位重用和
退役。另以固定种子检查 100,000 组随机 int64/f64/object 位模式，直接核对字段；
不能将 checksum 或测试数量视为完整 VM 正确性证明。每次基准启动先验证边界。

## 复现

在本目录执行（`rust-toolchain.toml` 固定 1.90.0）：

```sh
cargo fmt --check
cargo test --release --offline
cargo clippy --all-targets --offline -- -D warnings
rustup target add i686-unknown-linux-gnu
cargo check --all-targets --target i686-unknown-linux-gnu --offline
cargo build --release --offline
taskset -c 0 ./target/release/value-layout-bench \
  --samples 11 --n 262144 --cache-n 4194304 --visits 16777216 \
  --seed 11400714819323198485 > results/run.csv 2> results/run.log
python3 summarize.py results/run.csv
```

`taskset` 是本次 Linux 环境选项；其他目标按实际工具调整并记录。
`--validate-only` 可仅运行启动边界检查。Rust crate 没有第三方依赖；初次安装
固定工具链/目标需要联网，后续 Cargo 命令可离线运行。

## 计时与成本口径

输入从运行时种子生成并 Fisher-Yates 打散，两种表示共用完全相同的输入。
wide 比例精确为 `floor(n * percent / 100)`，不是随机抽样比例。
所有 case 各做一次不计入统计的 warmup；每次重新构造 batch。每轮交替 E/N 顺序，
单线程顺序采样。release 为 opt-level 3、thin LTO、codegen-units 1，未指定
`target-cpu=native`。构造、读取、释放分开计时；CSV 保存每个样本原始纳秒值。

- 构造包括数组分配/编码、对象或 wide int 的各次 Box 分配和 slot 表增长。
- 读取先逐元素核对逻辑 digest，再计时至少 `--visits` 次 Value 访问；
  `ceil(visits/n)` 个完整 pass。这是预热后的读取，验证本身不计时。
  Integer 为 wrapping sum，Float 为浮点位模式 sum（不测浮点算术），Object 为
  generation-checked payload sum，mixed 为约等比例的五种标签完整分派。
  缓存 case 为大数组顺序访问/打散索引访问。共同 permutation 是 `Vec<u32>`。
- 释放包括扫描 Value、代数失效与逐个 Box deallocation、Value Vec 和所有 slot
  表释放。读取完成后 slot 表不缩容，释放中保持的表存储也不能忽略。
- 计时函数不内联；构造输入/产物以及每次读取 pass 的 slice/heap 都经
  `std::hint::black_box`，read 结果经 black_box 后与独立算出的预期结果核对。
  既不在每个读元素上加黑盒成本，也不允许编译器把重复 pass 化成一个常量结果。
  输入生成、逐元素核对、CSV 输出和 warmup 不计时；两种表示有相同逻辑内核。
- `array_bytes = capacity * size_of(Value)`；堆为实际观测的
  `slot.capacity * size_of(Slot)` 加 `live_boxes * size_of(T)`。CSV 分别记录 object
  和 wide int 的 slot 表/Box 请求字节，`total_bytes` 是满 batch 存活请求字节。
  每个 Box 8 字节为请求大小，不是 malloc 实际占用。分配请求数由唯一的 Box、
  Vec 首次分配、实际观察到的表容量增长推导；reallocation 单列。
  没有给计时路径插入全局分配器计数钩子。
- 该口径可从代码、容量和计数核对，**不是 RSS/allocator usable size 或峰值**：
  不含 malloc 元数据/取整/碎片、表 reallocation 瞬时重叠、栈上管理结构。
  共享输入/permutation 属于夹具；其字节单独写入 stderr log，不算作 Value 存储。
  它们仍会影响本次缓存工作集，无法据此推断真实 VM 的占比。

汇总报告保留所有样本，给出中位数和各阶段 min–max/median；不移除离群点。
受虚拟化、调频、allocator 重用和共享宿主影响的小差异必须谨慎解读。
基准没有项目 VM 性能预算；选型标准是逐项比较时间与存储，不能生成一个虚构
“完整 VM 加速”分数。32-bit 交叉编译只证明该目标的布局断言和类型检查，
不证明已在 32-bit 设备或 PS Vita 上运行。
