# 仓库开发工具

[English](README.md)

从仓库根目录执行这些命令，使用工作区指定的工具链和锁文件。

| `cargo run --locked -p reel-dev --` 后的命令 | 用途 |
| --- | --- |
| `check` | 运行源自 Quirrel 的断言和虚拟机执行检查 |
| `deps` | 检查共享 UI 类型和平台依赖边界 |
| `audio-regression [--release]` | 检查音频库回归 |
| `audio-probe` | 测量 PCM 分配和后台释放开销 |
| `root-negative` | 通过变异测试验证 GC 根对象统计，需要 Bash |
| `compiler-probe frontend/semantic/lowering [MODE]` | 测量编译器的工作量和分配开销，需要 GNU time |
| `measure BUILD_JSON contracts/r01_measurements [TEST_ARGS]` | 从 Cargo JSON 输出中读取构建信息并测量 UI，需要 GNU time |
| `debug PROJECT OUTPUT` | 从源码运行项目并记录手动复现的问题 |
| `perf prepare/run/collect/report/verify/runtime --help` | 通过发行播放器构建、测量和验证普通游戏 |
| `frame-cost vita/emulator/host INPUT OUTPUT [LIMITS_JSON]` | 从性能日志计算分段提交间隔和各级阶段耗时的百分位数 |
| `render-cost vita/emulator/host INPUT OUTPUT` | 分析固定帧重绘、渲染通道和目标事件、完成观测及追踪完整性 |

先构建编译器探针。例如运行以下命令。

```sh
cargo build --locked -p aside-compiler --release --example lowering_probe
cargo run --locked -p reel-dev -- compiler-probe lowering
```

编译器探针命令目前按 Unix 路径查找可执行文件。测量 UI 时，用
`--ignored` 或 `--include-ignored` 选择标记为忽略的用例。日志和测量结果
写入 `target/reel-artifacts/`。

`frame-cost` 读取最新的完整版本 1 帧报告。如果有更新但未写完的报告，
命令会报错。可选的 JSON 阈值表使用微秒作为单位，例如
`{"session_us":6000,"vm_slice_us":3000}`。开头的不完整区间和包含多次提交
的循环会分别标记。报告记录声明的运行环境、丢失的记录和样本数量。
GPU 实际执行时间和显示节奏需要单独测量。嵌套阶段的百分位数单独报告。

桌面采样在 `frames.tsv` 中记录数量有限的 `desktop_wake` 和
`desktop_completion` 行。它们的时间戳和 `frame_sample` 使用同一个经过时间
时钟。唤醒行记录所选原因、计划截止时间、Winit 报告的截止时间和实际回调时间。
因定时器到点而唤醒时，还会记录定时器晚了多久。完成行记录播放器何时从后端
取出帧和资源事件，以及轮询延迟。把这些行与提交间隔对照，可以查看获取交换链
图像的时机、原先计划和观测到的完成情况。`frame-cost` 和 `perf report`
在 `desktop_pacing` 下汇总各唤醒原因的次数、定时器迟到百分位数、完成事件
数量和被挤出缓冲区的事件数量。

性能采集由发行播放器的 `performance` 功能提供。每次运行使用一个新目录，
包含请求、导出的游戏、日志和报告。`prepare` 构建当前导出器，并导出游戏的
着色器和资源。桌面运行还会构建常规的 `reel-game` 程序。

```sh
cargo run --locked -p reel-dev -- perf prepare path/to/game target/reel-artifacts/host-run
cargo run --locked -p reel-dev -- perf run target/reel-artifacts/host-run
```

工作负载通过 `game.exit()` 退出，也可以用正常方式关闭交互式测试。
`--vm-timing` 开启虚拟机耗时采样，虚拟机累计工作量默认记录。报告保留
采样开销、不完整区间和可用能力。播放器平时按常规路径运行。性能采集需要构建时
启用 `performance`，启动时提供采样请求。

`summary.json` 保留各播放段的总用时和提交次数，即使该段的逐帧样本已被淘汰。
分段速率计入空闲时间。用区间样本查看帧耗时百分位数。

### 验证发行版本

执行 `perf prepare` 和 `perf run` 后，按项目配置的标准评估测量结果。
检查策略中列出的片段需要使用不同的场景标题。

```sh
reel-dev perf run path/to/run --policy path/to/release-policy.json
reel-dev perf verify path/to/run --policy path/to/release-policy.json
```

可以单独用 `verify` 按更新后的标准评估已有结果。使用 `run --policy`
时，目标平台、测量环境和常规播放模式都必须在播放器启动前符合策略。
验证会确认运行状态和帧报告属于同一份准备好的请求。

```json
{
  "version": 1,
  "target": "vita",
  "environment": "vita",
  "scenes": [
    {
      "label": "History scroll",
      "min_intervals": 120,
      "max_p95_submit_us": 16667,
      "max_submit_us": 33334
    }
  ]
}
```

环境值在准备时选定，可用值为 `host`、`vita` 或 `emulator`。场景
计时使用完整的提交间隔，每个场景的第一次呈现不参与统计。场景缺少间隔时，
验证会失败。可选的 `max_vm_bytes` 和 `max_gpu_bytes` 对照该场景内
观测到的内存记账。即使导出或播放失败，也可运行验证，得到记录失败阶段和
可用日志的 `verification.json`。检查失败时返回非零退出状态。

在配置好 VitaSDK 的 Linux shell 中构建 Vita 开发运行时。

```sh
cargo run --locked -p reel-dev -- perf runtime --output target/reel-artifacts/vita-runtime
```

按 Vita 或 Vita3K 的常规方法安装生成的 `reel-vita.vpk`。这个性能测量
应用的 Title ID 为 `REELVITAP`。安装一次即可，后续运行会更新它的运行时
文件。更多 cargo-vita 参数可加在 `runtime` 命令的 `--` 后。

```sh
cargo run --locked -p reel-dev -- perf prepare path/to/game target/reel-artifacts/vita-run --target vita
cargo run --locked -p reel-dev -- perf run target/reel-artifacts/vita-run --runtime target/reel-artifacts/vita-runtime --address DEVICE_IP
```

用 `--vdb PATH` 指定 vdb。连接中断后，用 `perf collect RUN --address DEVICE_IP`
取回报告。`perf report RUN` 会重新分析本地结果。Vita 读取一次
`ux0:/data/reel/performance.json`，将其保留为 `performance.active.json`，
并把报告写入请求指定的输出目录。

测量 Vita3K 时，准备命令使用 `--target vita --environment emulator`，
运行命令使用 `--emulator PATH_TO_VITA3K --storage CONFIGURED_STORAGE_ROOT --runtime RUNTIME`。
存储根目录包含 `ux0`，并且要与 Vita3K 配置一致。开始前先退出 Vita3K。
每次运行保留选定的基础配置，并绑定这个存储目录。默认读取 Vita3K 程序旁的
`config.yml`，可用 `--emulator-config` 指定其他文件。模拟器结果单独标记。
中断后用 `perf collect RUN --storage ROOT` 取回结果。

报告包含 `summary.json` 和控制台摘要，列出最慢的采样片段、较长的提交区间、
主线程阶段、固定帧开销和采样缺口。提交速率根据完整区间累计用时计算，
包括卡顿时间。固定帧准备单独计时，串行模式下的通道用时会标明来源。

准备时加上 `--frame "Exact game title" --accepted-frame 3 --repetitions 3`，
可保留一个已接受的帧并重复渲染。`--cache replay` 清掉效果结果，保留资源池
和渲染目标池。`--cache reuse` 会保留效果结果。`--serial` 在各渲染通道
后等待完成，便于归因。`--save-frame` 在测量后把尺寸和绘制所需的全部数据
写到 `frame.postcard`，供同一引擎构建离线分析。目前可在 Vita/GXM 适配器
上使用该模式。重复渲染结束后，播放器退出并写出 `frozen.tsv`，普通播放
样本写入 `frames.tsv`。`status.json` 记录最终状态、请求和后端能力。

比较固定帧时，绘制命令、资源标识和渲染设置应一致。串行模式会改变合批。
裁剪矩形用于估计可能的工作量。SDK 调用耗时包含内部等待，完成观测也包含排队
和轮询延迟。GPU 硬件时间戳与实际片元、纹理读取次数需要独立的硬件数据。报告
检查事件是否成对、是否完成，以及是否有追踪事件被丢弃。

行为检查位于对应 crate 的 Rust 测试中。CLI 的项目场景测试覆盖源码与 ASB、
存档恢复、已接受输入和共享播放器的离线音频。见
[项目场景测试](../reel-cli/tests/project_scenarios.rs)。

用 `cargo test --locked -p <crate>` 运行 crate 检查。`--all-targets`
让编译和 lint 命令也检查 Rust 示例。视频测试使用
`cargo test --locked -p reel-media --features video`。编译器执行用例的说明
放在[测试输入旁](../aside-compiler/tests/fixtures/quirrel/README.md)。

### 检查开发会话

```sh
reel-dev debug path/to/project path/to/capture
```

在游戏窗口复现问题后关闭窗口。用 `player.log` 查看编译器和运行时诊断，
用 `reports/status.json` 查看结果，用 `reports/summary.json` 查看场景计时
和主线程阶段。脚本报错时会保留已收集的帧。用 `--vm-timing` 记录解释器耗时
采样，用 `--save-directory` 结合特定存档复现。默认存档目录在采样输出目录内。

Vita 包从 `crates/reel-player/runtime/vita/` 和
`crates/reel-dev/runtime/vita/device-checks/` 读取固定资源。各自的 LiveArea
图片和布局放在 `sce_sys/`。

底层设备契约用启用 `device-checks` 功能的 `reel-device-checks` 检查。
用例文件是 `ux0:/data/reel-device-checks/case.txt`，可写入
`--case effect_region`。像素断言、完成事件和资源回收检查独立于游戏性能
采集运行。媒体契约是 `fixtures/media_contract` 中的普通项目，源码列出了
输入和导出命令。VPK 输出在 Cargo 配置的构建目录。

原生诊断程序使用目标系统的显示和音频设备。

```sh
cargo run --locked -p reel-player --features window --bin reel-diagnostic
cargo run --locked -p reel-player --features window --bin reel-session-host
cargo run --locked -p reel-media --example device_probe
```

将 `REEL_DIAGNOSTIC_FONT` 设为 TTF 或 OTF 字体路径。会话调试程序优先读
`REEL_SESSION_FONT`，再回退到该路径。F5 重建图形设备，F6 暂停渲染表面。
`REEL_DIAGNOSTIC_TRACE=1` 记录帧、epoch 和指针输入的路由。
