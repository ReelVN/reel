# Value 布局选型结果 · 2026-10-08

**建议：性能优先时，本轮保留显式标签 16 字节基线作为待实现方案；8 字节候选
保留为后续受限实验，不据此切换生产设计。** 候选减少数组存储，浮点读取和部分
构造内核有收益；整数解码、wide-int 装箱和释放有明确代价。没有通用赢家，也没有
测到完整 VM 加速。下一轮决策应由真实工作负载的标签分布、wide-int 比例、Value
寿命和解释执行 trace 决定。此 PR 只提供设计审查证据，不实现引擎、不更改规范 1.0。

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
16/8 字节、Handle 8 字节、Slot 12 字节；**没有运行 32 位程序，也没有 VitaSDK
构建或设备测试**。x64 实测对应为 16/8、8、16 字节。

## 时间结果

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
语言完整验收。两种表示尚无引用计数/循环回收、共享 wide-int、VM 分派、宿主调用
和快照成本；所有读取均在验证预热后进行。测试堆的 per-Box 分配、元数据和读写
路径是实验选择，未证明是最终最优堆设计。没有计分配器取整/碎片和增长瞬时峰值；
release 还扫描 Value 标签及释放表，不能将数组 deallocation 当完整 GC。
只有一个虚拟化 x64 环境和一组确定性输入；微基准 SIMD 收益不可直接乘到解释器。

在后续设计审查中确认真实 int64/float/对象分布、复制/所有权规则和目标设备预算，
再决定是否以解释执行 trace 验证候选。此结果没有授权或替代生产接口设计修改。
