# Value 布局选型结果 · 2026-10-08

**集成负责人已决定工程默认：正式 VM 采用 explicit16，完整 i64/f64 内联，
堆使用代际句柄与增量追踪回收。** 本报告记录该工程选择；本 PR 仍仅为设计实验，
没有实现正式 VM 或追踪 GC，也没有修改引擎接口或规范文档。

保留的数据比较 explicit16 与两个采用 immutable wide-int 引用计数的 NaN 候选，
同时包含表示、所有权路径、统计更新和编译器生成代码的成本。三个基本 trace 中
约 1.4–2.1 倍的差距是这些 RC 候选的结果，不能归因于布局本身，也不能外推到
采用追踪 GC 的 NaN64。候选的存储收益和本夹具执行时间分别保留供审查。

## 新增：有界解释执行 trace

同一个运行时生成的 Instruction/常量数组、32 个动态寄存器、严格 bool 跳转，
相同的动态数值检查和共同宿主堆；逐样本核对全部寄存器/属性与控制流计数，再与
native oracle 核对。负载、所有权、存储口径和精确命令见 [TRACE.md](TRACE.md)。
每个样本 65,536 次更新，两次独立进程各 11 个样本；E 为 explicit16，
Box 为 nanbox8_box_rc，Slots 为 nanbox8_slots_rc。

| trace | 执行 ns/指令 E / Box / Slots | Box/E | Slots/E | 峰值请求 B E / Box / Slots |
|---|---:|---:|---:|---:|
| 剧情变量分支 | 2.130 / 3.162 / 2.978 | 1.484 | 1.398 | 752 / 440 / 440 |
| UI 属性计算 | 2.375 / 4.313 / 4.165 | 1.816 | 1.754 | 800 / 464 / 464 |
| 浮点动画 | 2.012 / 4.289 / 4.135 | 2.132 | 2.055 | 768 / 448 / 448 |
| 独立 wide 0% | 2.507 / 4.309 / 4.090 | 1.719 | 1.632 | 17,136 / 8,632 / 8,632 |
| 独立 wide 1% | 2.496 / 4.100 / 3.936 | 1.642 | 1.577 | 17,136 / 9,112 / 9,016 |
| 独立 wide 10% | 2.524 / 4.199 / 4.010 | 1.664 | 1.589 | 17,136 / 12,552 / 11,704 |
| 独立 wide 50% | 2.494 / 4.824 / 4.412 | 1.934 | 1.769 | 17,136 / 37,344 / 33,208 |
| 独立 wide 100% | 2.483 / 5.162 / 4.619 | 2.079 | 1.860 | 17,136 / 66,016 / 57,784 |

三个基本 trace **没有 wide 值和临时 Box 分配**，但 NaN 候选在复制和覆盖路径
仍需检查 WIDE 标签，以决定是否执行 retain/release；explicit16 的所有整数内联，
其 NoWide retain/release 路径可被编译器消除。因此该差距包含 RC 候选的所有权
判定成本，不是只测 16/8 字节布局，也不是追踪 GC 的 NaN64 对照。连续槽方案
保留相同的 WIDE 判定，不能据此排除所有权路径对结果的影响。旧数组浮点位模式
读取的 0.731 倍不代表浮点动画的解释执行速度；旧 50% wide 数组读取的
23.346 倍，在新 trace 中为 1.934 倍。

wide 引用由常量池/寄存器拥有，Copy/Load retain 共享不可变值，覆盖时 release，
最后一个引用消失时立即释放并重用槽位。宿主字段为共同 native 标量，读取 native
wide int 时重新编码；这是明确的边界成本，不冒充 VM class/table 字段存储。
wide 计时还包含候选的逻辑创建/Box 分配与释放计数、live/峰值/请求字节统计，
以及 retain/release 统计计数更新，除实际引用计数变化外另有测量记账成本。
explicit16 没有对应 wide 路径；这些成本并未单独消融或从耗时中扣除。无全局
allocator hook 不等于无统计开销，不能由本数据推算去掉统计后的纯布局或 GC 成本。
构造与释放时间也分列于 [trace 汇总](results/trace-combined-summary.md)。临时逻辑
创建与实际 Box 请求分开计数，两种候选的临时逻辑创建数相同：

| wide pool | 常量 wide 根 | 最多临时 live wide | execute 新 wide 值 | Box 临时请求 / 累计 B | Slots 临时 Box 请求 |
|---|---:|---:|---:|---:|---:|
| 0% | 0 | 0 | 0 | 0 / 0 | 0 |
| 1% | 10 | 2 | 1,280 | 1,280 / 10,240 | 0 |
| 10% | 102 | 4 | 13,120 | 13,120 / 104,960 | 0 |
| 50% | 512 | 5 | 65,984 | 65,984 / 527,872 | 0 |
| 100% | 1024 | 5 | 132,096 | 132,096 / 1,056,768 | 0 |

累计请求流量不等于存活存储。临时 wide 数没有随执行次数累积；结束后 live=0、
Box 分配/释放相等。表按 Vec 容量增长，50/100% 的常量池填满初始容量后，
第一批临时值触发一次表增长；这也是此夹具的策略，不是最终堆选型。

两轮 Box/E：剧情 1.532/1.464，UI 1.862/1.774，动画 2.183/2.088；
50% wide 为 1.939/1.929。100% wide 的 Slots/E 两轮为 1.674/2.022，有明显
跨进程波动。全部样本保留，story Slots 的 execute min–max/median 为
0.985–2.454；构造/释放若只有几十 ns，时钟和调度噪声占比很大，不据此排序。
完整原始 [trace-run-1.csv](results/trace-run-1.csv)、[trace-run-2.csv](results/trace-run-2.csv)
和 [trace-environment.txt](results/trace-environment.txt) 已保存。

[汇编摘录](results/trace-assembly-notes.txt) 核对三个 execute 实例：逐条 scalar
opcode/操作数加载、间接 opcode 分派、寄存器动态类型分派均保留；三者调用同一
native get/set/call 函数。E 的标签分派没有被跳过。序言中的 movupd 搬运 slice
描述符，数值处理中的 SSE 是标量 float 指令，没有把整个指令流变成数组求和。
所有表示使用相同 release 优化选项，没有单独禁用优化。

## 环境与复现

本环境：Linux x86_64 / KVM，报告 CPU 型号 AMD EPYC 9V45 96-Core Processor，
5 个可见 vCPU、cgroup CPU 配额 4 核、内存限额 16 GiB；计时进程固定 CPU 0。
`lscpu` 报告 L3 32 MiB。这不是 N100、PS Vita 或独占物理设备测量。
Rust 1.90.0 (`1159e78c4`, LLVM 20.1.8)，release opt-level 3、thin LTO、
codegen-units 1、无 `target-cpu=native`，标准 System allocator。
完整环境见 [environment.txt](results/environment.txt)。

在本目录执行：

```sh
cargo fmt --check
cargo test --release --offline
cargo clippy --all-targets --offline -- -D warnings
cargo check --all-targets --target i686-unknown-linux-gnu --offline
cargo build --release --offline
taskset -c 0 ./target/release/value-layout-bench --samples 11 --n 262144 \
  --cache-n 4194304 --visits 16777216 --seed 11400714819323198485 \
  > results/run-1.csv 2> results/run-1.log
# 在新的进程中重复上述计时命令，保存为 run-2.csv / run-2.log。
python3 summarize.py results/run-1.csv results/run-2.csv
```

这些验证命令均通过。固定工具链最初缺失，已在 workspace scratch 中安装；安装
组件的并发冲突通过顺序重装修复。初次安装目标需 `rustup target add
i686-unknown-linux-gnu`。i686 对所有 targets 类型检查和编译时断言通过：Value
16/8 字节、Handle 8 字节、数组 Arena Slot 12 字节；**没有运行 32 位程序，也没有 VitaSDK
构建或设备测试**。x64 实测对应为 16/8、8、16 字节。

## 数组微基准：组成成本

以下数组数据保留自提交 ac24702。整数扫描不做动态标签检查，E 被向量化；
这些数字仅描述该内核，不能作为解释器整体表示选择的依据。

E = explicit16，N = nanbox8；表中为两次进程运行、每次每个表示 11 次、合计
22 个样本的中位数。每种表示/case 各一次不计统计的 warmup，采样顺序交替。
单位 ns/Value；read 每样本访问 16,777,216 次，构造/释放各一次 batch。
常规 n=262,144，`int_4k` n=4,096，缓存 case n=4,194,304。
`read N/E` 小于 1 表示候选较快。浮点内核测位模式读取，不测浮点算术。

| case | 构造 E / N | 读取 E / N | 释放 E / N | read N/E |
|---|---:|---:|---:|---:|
| int_4k | 0.723 / 0.842 | 0.144 / 0.376 | 0.243 / 1.342 | 2.604 |
| int_scan | 0.677 / 0.847 | 0.155 / 0.375 | 0.228 / 1.327 | 2.413 |
| float_scan | 0.912 / 0.892 | 0.155 / 0.113 | 0.227 / 1.336 | 0.731 |
| mixed_dispatch | 10.831 / 9.231 | 6.627 / 7.054 | 3.700 / 4.380 | 1.064 |
| object_read | 15.807 / 14.697 | 0.677 / 0.685 | 10.653 / 11.048 | 1.012 |
| cache_seq | 3.893 / 2.406 | 0.367 / 0.382 | 0.644 / 1.294 | 1.039 |
| cache_random | 3.737 / 2.418 | 5.241 / 5.959 | 0.634 / 1.485 | 1.137 |
| wide 0% | 0.744 / 0.846 | 0.154 / 0.375 | 0.227 / 1.329 | 2.434 |
| wide 1% | 0.742 / 1.087 | 0.152 / 0.442 | 0.226 / 1.517 | 2.904 |
| wide 10% | 0.748 / 2.536 | 0.153 / 0.977 | 0.225 / 2.910 | 6.390 |
| wide 50% | 0.789 / 8.728 | 0.154 / 3.584 | 0.226 / 9.136 | 23.346 |
| wide 100% | 0.790 / 9.055 | 0.152 / 0.802 | 0.226 / 9.964 | 5.277 |

两轮整数扫描 N/E 分别 2.404/2.420，浮点 0.726/0.734，50% wide 23.324/23.278，
主结论一致。所有样本保留：float read 的 min–max/median 为 E 0.983–1.901、
N 0.988–2.073；`int_4k` 的 E 构造最大达到中位数 4.385 倍。完整各阶段范围见
[combined-summary.md](results/combined-summary.md)，逐样本纳秒、checksum、字节和
分配请求见 [run-1.csv](results/run-1.csv)、[run-2.csv](results/run-2.csv)。
1–6% 的对象/混合/顺序缓存读取差异不构成广泛性能承诺。

防折叠和核对方法见 [README.md](README.md)：运行时输入、不可内联计时函数、
每 pass 的 black_box、独立 checksum 和计时外逐元素验证。
[生成代码摘录](results/assembly-notes.txt) 确认循环与内存读取保留；基线整数扫描
产生 `paddq` SIMD，候选保留 wide tag 分支、signed 48-bit 扩展和句柄解引用。
这说明整数内核的差异不能归结为数组字节数。50% wide 比 100% wide 更慢，
分支可预测性可能是原因之一；未测硬件事件，不能确认具体微架构归因。

## 请求存储与分配

完整 batch 存活时的请求字节，**不是 malloc 实际占用、RSS 或峰值**。
Value 数组用实际 capacity × size；Box payload 和 slot 表分列，slot 表也包括
尚未利用的容量。两种对象堆实现/检查/Box 完全相同；每个 wide int 独立装箱。
wide case 的 E 数组/总量始终 4,194,304 B，分配请求 1 次、重分配 0 次；
N 数组始终 2,097,152 B：

| wide 比例 | 装箱数 | N slot 表 B | N Box 请求 B | N 总量 B | N 分配 / 重分配请求 |
|---|---:|---:|---:|---:|---:|
| 0% | 0 | 0 | 0 | 2,097,152 | 1 / 0 |
| 1% | 2,621 | 65,536 | 20,968 | 2,183,656 | 2,623 / 10 |
| 10% | 26,214 | 524,288 | 209,712 | 2,831,152 | 26,216 / 13 |
| 50% | 131,072 | 2,097,152 | 1,048,576 | 5,242,880 | 131,074 / 15 |
| 100% | 262,144 | 4,194,304 | 2,097,152 | 8,388,608 | 262,146 / 16 |

因此 50% wide 时 N 为 5 MiB、E 为 4 MiB；100% 时 N 为 E 两倍。
在 table capacity 等于 live count 的理想请求模型下，N 的
`8n + 16*capacity_wide + 8*wide_count` 在约 1/3 wide 时追平 `16n`；
表容量余量和未计的 allocator 开销会使边界提前，不把此模型当实测峰值。
对象 case 总量 E/N 为 10/8 MiB，混合 case 为 5,663,200/3,566,048 B；
缓存 case 数组为 64/32 MiB。共享输入和随机索引另计于 run logs（缓存随机 case
80 MiB），仍影响缓存工作集；数组减半没有在这些整数缓存内核中带来读取加速。

## 限制与下一步

边界往返、随机位模式、陈旧句柄与退役验证支持这里的编码行为，不代表算子或
语言完整验收。新增 trace 仅有 immutable wide 的引用计数、有限指令分派和本地
宿主式操作，没有真实对象 GC、完整 VM、调度或快照；32 个寄存器的工作集很小，
没有覆盖缓存压力下的解释执行。测试堆的分配、元数据和读写路径是实验选择，
未证明是最终最优堆设计。没有计分配器取整/碎片和增长瞬时峰值；
release 还扫描 Value 标签及释放表，不能将数组 deallocation 当完整 GC。
只有一个虚拟化 x64 环境和一组确定性输入；微基准 SIMD 收益不可直接乘到解释器。

本报告把有界 trace 与组成内核分别交给集成负责人作为布局取舍证据；不替代目标
设备和正式引擎验收，不授权或实施生产接口设计修改。
