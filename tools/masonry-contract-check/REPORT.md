# 阶段报告 — 2026-10-08 13:14 UTC

应负责人要求，此轮先保存并推送可读阶段结果。Issue #3 保持开放；生产 fork、release 结论与目标设备验收均未完成。具体源码位置和适配边界见 [README.md](README.md)。

## 已完成和实际观察

- 在 `/workspace/reel-masonry` 从已审查基线 `5a7cfd4c0802e604928e72f623ffec024d864d71` 建立任务分支 `task/issue-3-masonry-contract`。改动仅在 `tools/masonry-contract-check/**`。
- 固定 Xilem/Masonry `271a27a6d4a930f7878d404f9014e3c50a3a9b88` 已核对、构建和运行。普通 Git 依赖基线与受控最小核心补丁是两个独立构建。
- 真实图片、图片按钮、嵌套组，经 Vello CPU 得到图片区 alpha=64、重叠区 alpha=64、裁剪外 alpha=0；旋转/缩放命中一致。完整记录流可用。
- `IsolatedScene` 初次产生 3 层，其中 2 个片段独立 Scene 校验失败；串联后可渲染。缓存重绘变为 1 层。原生 base layer 根变换在绘制中抵消、在命中中保留，已通过像素和真实 hover 复现。
- 未修改上游在布局更新、未呈现新帧时，旧位置 hover=false、capture=None、值事件为空。删除捕获节点收到 0 次 Cancel 回调。这些是基线缺口，不是成功能力。
- 实验补丁统一冻结命中、hover、capture 坐标/尺寸、DPI、画序和祖先路由。真实滑条在旧呈现几何中产生 0.25 → 0.7 → 1.0；跳过、表面不可用、错误、错误帧 ID、过期帧和表面代数都不切换视图；匹配的新接受结果才切换坐标和静止 hover。
- 失焦/销毁/禁用各交付一次 Cancel；正常释放交付一次 Up，捕获清掉，后续移动不再产生值变化。按钮孤立释放不点击、正常配对释放点击一次；真实状态可投影到作者 Image。失败呈现后的父变换、clip 和重排不改输入路径，新接受帧才改变这些行为。
- 奇异仿射矩阵不进入命中/捕获。只验证单个主鼠标主键、仿射矩形；透视、自定义命中映射、多指针及模态不在已验证集合。
- 格式检查、未修改上游对照测试与补丁 debug 行为检查已完成并通过。原始输出快照见 [evidence/verification.log](evidence/verification.log)。

## 真实命令与运行到哪一步

已成功：

```sh
cargo check --locked --manifest-path tools/masonry-contract-check/Cargo.toml
cargo run --locked --manifest-path tools/masonry-contract-check/Cargo.toml
python3 tools/masonry-contract-check/scripts/prepare.py --source /tmp/masonry-upstream
cargo run --locked --manifest-path tools/masonry-contract-check/experiment/Cargo.toml \
  --features experimental --target-dir tools/masonry-contract-check/target
cargo fmt --manifest-path tools/masonry-contract-check/Cargo.toml --check
cargo test --locked --manifest-path tools/masonry-contract-check/Cargo.toml
cargo test --locked --manifest-path tools/masonry-contract-check/experiment/Cargo.toml \
  --features experimental --target-dir tools/masonry-contract-check/target
```

上述生成过程逐一验证 72 个源文件哈希，原始源码闭包 592,487 字节；只有 `masonry_core` 六个文件有实验差异。`tree_arena` 同级路径、继承配置和许可证保留，其他生态没有导入生产仓库。

一条完整核对命令正在后台运行：

```sh
./tools/masonry-contract-check/scripts/check.sh --source /tmp/masonry-upstream
```

它已执行完格式、原始对照及补丁 debug 检查，当前在 **首次 release 依赖编译**，尚无 release 测量结果。后台 unified exec session 为 **75373**；同一执行环境可用 `write_stdin(session_id=75373, chars="")` 读取完成状态。完整运行日志保留在：

```
/workspace/reel-masonry/tools/masonry-contract-check/target/verification-running.log
```

提交中的 `evidence/verification.log` 是交付时快照，后台继续输出到忽略的 target 文件，不会悄悄改变已提交证据。进程若无法保留，可重新运行上述脚本复用 Cargo 缓存；完成后把最终日志复制回 evidence 并更新阶段状态。

## 环境与暂时测量

Linux x86_64 / KVM，5 个可用 CPU，Intel Xeon Platinum 8573C，约 17 GiB 内存。Rust 1.96.0 (`ac68faa20`)，dev 无优化。正式 release 测量仍在编译，未作为通过结论。

此前实际 debug 运行（包含诊断 trace，100 次 warm-up）：三节点指针 p95=974,195 ns，捕获快照 p95=24,424 ns；1080p 坐标范围的 256 个真实滑条/258 个节点，指针 p95=514,704 ns，捕获快照 p95=3,349,405 ns，未命中扫描 p95=501,152 ns。VM 调度抖动明显；这些只是阶段观察。三节点 payload 下界 2,392 B，258 节点下界 208,432 B，均排除了 HashMap 桶与嵌套状态分配；不是内存峰值或帧预算证明。

本环境最初没有 Rust。实际准备命令：

```sh
curl --fail --location https://sh.rustup.rs --output /tmp/masonry-rustup.sh
CARGO_HOME=/tmp/masonry-cargo RUSTUP_HOME=/tmp/masonry-rustup \
  sh /tmp/masonry-rustup.sh -y --no-modify-path --profile minimal --default-toolchain 1.96.0
CARGO_HOME=/tmp/masonry-cargo RUSTUP_HOME=/tmp/masonry-rustup \
  /tmp/masonry-cargo/bin/rustup component add rustfmt clippy
```

实际 Cargo 命令均设置 `PATH=/tmp/masonry-cargo/bin:$PATH`、`CARGO_HOME=/tmp/masonry-cargo`、`RUSTUP_HOME=/tmp/masonry-rustup`、`PKG_CONFIG_PATH=/tmp/masonry-fontconfig`。

第一次构建因缺 Fontconfig 开发元数据失败。`RUST_FONTCONFIG_DLOPEN=1` 的尝试与 Fontique 不兼容，未采用。最终使用容器已安装的 Debian `libfontconfig1 2.15.0-2.3`，只在 `/tmp` 补链接元数据，没有伪造函数或修改该依赖：

```sh
mkdir -p /tmp/masonry-fontconfig
ln -s /usr/lib/x86_64-linux-gnu/libfontconfig.so.1 /tmp/masonry-fontconfig/libfontconfig.so
cat > /tmp/masonry-fontconfig/fontconfig.pc <<'PC'
prefix=/tmp/masonry-fontconfig
libdir=${prefix}
Name: Fontconfig
Description: installed Debian libfontconfig1 2.15.0-2.3 (local linker metadata)
Version: 2.15.0
Libs: -L${libdir} -lfontconfig
Cflags:
PC
```

## 下一步与决策边界

先收取后台 release 和依赖核对结果，再补充最终测量与独立默认下载方式验证；clippy、目标设备验证仍未完成。正式接入前需负责人确认最小输入 fork 方案：几何提供器和定向入口、销毁取消，以及生产几何投影/代际身份/预算接口。若采用独立离屏/External 层，另需固定层模式和组/裁剪上下文记录；本实验没有把这项当成已实现。

原型克隆完整 WidgetState，不应直接作为生产内存设计。自定义命中、缺失呈现几何的查询结果、作者滑条槽位、输入消费策略与稳定资源身份转换的接口建议均交负责人。根 workspace 和作者指南保持目录隔离。Windows/N100、winit/wgpu、实际 GPU/surface 生命周期及其他目标设备仍待验证。
