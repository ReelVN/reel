# 有界解释执行 trace

补充数组内核不能回答的布局问题。只在本实验目录中生成和执行 14 种夹具指令，
没有前端、字节码包、Aside VM API、任务、类、模块、快照或生产 GC。
接口仍依据规范 1.0；这里的指令子集不改变语言语义。

## 三种代表性负载和独立压力维度

每个 case 的 Instruction 数组、常量与寄存器编号在运行时从同一个 seed 生成；
每种表示执行同一个 Program。32 个动态 Value 寄存器，每样本 65,536 次更新，
用 int 计数器、严格 bool 条件跳转结束；工作上限为 cycles × 64 + 128 条指令。
默认案例的实际取指/类型检查/复制/分支/调用次数写在 trace run logs。

- **story_branch**：从对象读整数剧情变量 score，复制后加 seed 决定的 1..7
  增量；超过 seed 决定的 1000..1999 阈值时减阈值并调用一次返回 bool 的
  宿主式函数。写回 score 和 bool 属性，再复制 bool 到寄存器。默认实际宿主
  调用 114 次；没有 wide-int 分配。
- **ui_properties**：读 progress，执行 progress × 0.97 + 0.013；
  达到 0.4 时减 0.4，然后复制并写回。计算 progress × width + offset 并
  写回 display 属性，width/offset 从 seed 取得；每 64 次调用一次 clamp01，
  写入第三个属性。默认宿主调用 1024 次；没有 wide-int 分配。
- **float_animation**：读 position、velocity；复制旧 position，执行
  position + velocity × (1/60)，以动态分支将结果绕回 [0,1]，写回 position；
  velocity 从 seed 生成，可能为正/负。每 64 次调用一次 clamp01 并写回
  观测属性。默认宿主调用 1024 次；没有 wide-int 分配。
- **wide_trace_000/001/010/050/100pct**：独立压力程序，三种基本负载不被
  wide 数字替换。遍历同一打散的 1024 个 int64 常量，复制、加 1、写入
  宿主属性、读回，再复制/比较/条件分支，每 64 次调用一次 identity。
  pool 中 wide 数分别为 0/10/102/512/1024（百分比向下取整），wide 数在
  ±2^50 附近；其余数字在 signed 48-bit 内。默认 65,536 次恰好完整遍历
  64 次。两种布局的完整 int64 运算结果一致。

数值指令对两种表示都检查动态类型；int Add/Sub/Mul/Rem 检查溢出和零除，
Add/Sub/Mul 允许 int/float 混合并转 float。Less 子集只用于同类型的 int/int
或 float/float，混合比较返回明确错误，未实现或宣称完整语言比较规则。
JumpIf 检查 bool，对象访问检查 Object，属性索引为夹具生成的 0..3。
宿主式调用使用相同、不可内联的 native helper，具有可核对的调用计数和
参数 digest；没有外部 IO 或连接器请求。

## 同一宿主堆、明确的所有权

宿主对象共用原数组夹具的 Arena：同一种索引/代数检查、Box 和 get/get_mut
实现。四个属性为 native Input 标量；**宿主属性没有换成各自布局的 Value**，
因此这比较的是适配器属性模型，不是 VM class/table 字段存储或对象 GC。
根对象由整个 trace State 外部持有，寄存器中的 Object 句柄在此范围借用，
两种表示都不做对象引用计数。x64 每个样本宿主存储都是 slot 表 64 B + Box
payload 64 B。get/set/call 的同一机器代码调用路径在汇编中核对。

wide-int 为不可变标量，常量池和每个寄存器槽各自拥有引用：

- 新的范围外算术结果分配独立 wide 值；不去重。Load/LoadIndexed/Copy
  **共享已有 wide 值并 retain**，复制不分配新的 Box。self-copy 先 retain
  再释放旧槽，避免提前释放唯一引用。
- 覆盖寄存器先取得新值的所有权，再 release 旧值。最后一个引用消失时
  Box 立即释放；槽位进入 free list 并增加 generation。24/24-bit 句柄不
  存原生指针，代数耗尽时永久退役。
- 宿主 native 属性保存完整标量，SetField 将 Value 解码成 Input；
  GetField/Host 返回 native int64 时重新编码，范围外数字创建新 wide 值。
  这是一项明确的边界成本，没有把宿主属性复制假装成 VM 堆内共享。
- 结束时逐个 release 寄存器和常量，再释放 Value 数组、宿主对象和 slot
  表；验证所有 wide 值 live=0，Box 分配/释放数相等。

两种候选均是 8 字节编码，区别只在 wide 存储：

- **nanbox8_box_rc**：带 generation/next/refs 的 slot，载荷为
  Option<Box<i64>>。x64 slot 24 B，另加每个 live Box 的 8 B 请求。
- **nanbox8_slots_rc**：相同字段和检查、相同引用语义，i64 直接存于连续
  slot。x64 slot 也是 24 B，没有 per-value malloc；free slot 被及时重用。
  Vec 增长仍可能分配/重分配，记录真实容量。
- **explicit16**：int64 内联，wide 所有权操作没有堆引用可操作。

这不模拟生产 GC。不可变 wide 引用在覆盖时真正释放，不能通过只追加堆来
隐藏回收成本；同时没有对象环、异步释放、预算队列或 GC 尾延迟。本次压力
程序常量池始终持有 wide 根，执行中最多另外 2..5 个临时 live wide 值；
临时创建的累计数量与峰值 live 数必须分别解释。

## 测量与核对

同样的 Rust release 优化配置；不对任何表示单独关闭 SIMD/内联/LTO。
execute 是同一个泛型标量 pc 取指循环，读取运行时 Instruction 和寄存器，
两种表示执行同样的 kind 分类和类型检查。black_box 只在程序/state 边界及
结果处、代码 slice 入口使用，没有每条指令黑盒。

输入生成和 native oracle 不计时；每个 case/表示一次 excluded warmup。
三种表示按样本轮转首位，顺序运行，分开计 State 构造、整个 execute 和
teardown。每个计时结果与基线的**所有寄存器、所有属性、指令/检查/复制/
分支计数及宿主副作用**完全核对，并与独立 native 状态转移 oracle 核对。
不是只检查 checksum。错误类型、溢出、预算、wide 共享/self-copy/释放、
NaN/负零/int64 边界和槽退役也有直接行为检查。

CSV 分列 Value 数组、共同宿主表/Box、wide 表和 live Box 的请求存储，
以及 initial/end/peak requested 总量；容量为实际 Vec capacity。
temporary_wide_allocations 是新 wide 标量的逻辑创建数；
temporary_box_allocations 是 execute 中真实 per-value Box 请求数，
temporary_box_requested_bytes 为这部分累计 8 B 请求流量，**不是存活内存**。
连续槽方案的该列为 0，但逻辑创建/引用计数/槽位重用仍计时。
表分配、重分配以及执行期表重分配单列，retain/release 计数也保留。

峰值请求存储是在分配/释放点记录的表容量加 live Box payload，再加固定
Value/宿主存储，不含 allocator 取整、碎片、realloc 瞬时重叠和栈管理结构；
不是 RSS 或生产堆峰值。常量/指令数组是共用夹具，容量字节另写入 logs。
本地统计更新属于夹具成本，无全局 allocator hook。

运行两次：

    cargo build --release --offline --bin trace-bench
    taskset -c 0 ./target/release/trace-bench --samples 11 --cycles 65536 \
      --seed 11400714819323198485 > results/trace-run-1.csv 2> results/trace-run-1.log
    # 新进程重复，保存为 trace-run-2.csv / trace-run-2.log
    python3 summarize-trace.py results/trace-run-1.csv results/trace-run-2.csv

结果见 REPORT.md。32 个寄存器的工作集很小；这没有模拟大寄存器帧、多故事
任务、缓存压力下的解释器或正式 UI/动画调度。仅 x64 本环境执行；i686 仅编译
检查，PS Vita 未测。数组微基准继续作为组成成本，不能替代这些 trace，也不能
将任一组倍率外推为完整引擎或目标设备加速。
