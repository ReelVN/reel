# 收尾报告 — 2026-10-08 14:03 UTC

在原 Draft PR #7 继续收尾。负责人确认最小输入 fork 必要，授权实验完善；生产源码导入和目标设备验收仍待审查。clippy、默认 codeload 下载及完整核对脚本均通过，审查发现的三项输入回归已修正。具体适配建议见 [ADAPTATION-NOTES.md](ADAPTATION-NOTES.md)，它与实际行为验证分开标注。Issue #3 保持开放。

## 已完成和实际观察

- 在 `/workspace/reel-masonry` 从已审查基线 `5a7cfd4c0802e604928e72f623ffec024d864d71` 建立任务分支 `task/issue-3-masonry-contract`。改动仅在 `tools/masonry-contract-check/**`。
- 固定 Xilem/Masonry `271a27a6d4a930f7878d404f9014e3c50a3a9b88` 已核对、构建和运行。普通 Git 依赖基线与受控最小核心补丁是两个独立构建。
- 真实图片、图片按钮、嵌套组，经 Vello CPU 得到图片区 alpha=64、重叠区 alpha=64、裁剪外 alpha=0；旋转/缩放命中一致。完整记录流可用。
- `IsolatedScene` 初次产生 3 层，其中 2 个片段独立 Scene 校验失败；串联后可渲染。缓存重绘变为 1 层。原生 base layer 根变换在绘制中抵消、在命中中保留，已通过像素和真实 hover 复现。
- 未修改上游在布局更新、未呈现新帧时，旧位置 hover=false、capture=None、值事件为空。删除捕获节点收到 0 次 Cancel 回调。这些是基线缺口，不是成功能力。
- 实验补丁统一冻结命中、hover、capture 坐标/尺寸、DPI、画序和祖先路由。真实滑条在旧呈现几何中产生 0.25 → 0.7 → 1.0；跳过、表面不可用、错误、错误帧 ID、过期帧和表面代数都不切换视图；匹配的新接受结果才切换坐标和静止 hover。
- 失焦/销毁/禁用各交付一次 Cancel；正常释放交付一次 Up，捕获清掉，后续移动不再产生值变化。按钮孤立释放不点击、正常配对释放点击一次；真实状态可投影到作者 Image。失败呈现后的父变换、clip 和重排不改输入路径，新接受帧才改变这些行为。
- 奇异仿射矩阵不进入命中/捕获。只验证单个主鼠标主键、仿射矩形；透视、自定义命中映射、多指针及模态不在已验证集合。
- 格式、两种构建 all-targets clippy（`-D warnings`）、未修改上游对照、补丁 debug 行为、release 运行与依赖审计均通过。完整原始输出见 [evidence/verification.log](evidence/verification.log)。

## 审查回归：修正前失败，修正后实际验证

新增用例先针对 PR 原 `152f401` 的输入 patch 运行，exit 101，实际观察三项失败：禁用子控件点击返回 No、Button Up 后又收到 Cancel、RawCtx 返回未接受的局部平移 (7,3)。原始失败输出保存为 [evidence/review-regressions-before.log](evidence/review-regressions-before.log)。修正后相同用例 exit 0，输出见 [evidence/review-regressions-after.log](evidence/review-regressions-after.log)：

- pointer_hit_test 保留 disabled 目标的祖先路径，只过滤已删除/stashed 节点。无快照与冻结视图下，父 StageGroup 均返回 Handled::Yes；禁用 Slider 的非取消指针事件回调、动作和捕获均为零。状态更新回调仍按上游策略执行，捕获目标变禁用仍独立发送一次 Cancel。
- 新增 capture 终止已交付标记，在真实 Up/Cancel handler 开始前设置；capture 保持到其回调结束，删除识别已终止状态。Button/Slider、Up/Cancel、无快照/冻结模式均在父组同次冒泡删除子控件后只有一个终止回调；active 仍为 true、最终 capture=None。事件外删除用例继续验证一次 Cancel。
- 删除源控件后宿主收到的 Button action 为零，符合未改上游 `passes/action.rs:65` 的寿命规则；不能把这当成按钮正常释放未点击。存活按钮正常配对释放仍点击一次。
- RawCtx 在指针分发/更新阶段接入同一视图，普通 mutation 阶段保持 live。Observed 转发 get_cursor/on_access_event，最小几何观察直接检查 QueryCtx cursor、指针 UpdateCtx（含父组）及 EventCtx.get_raw。布局、父平移和子局部平移已改变时，旧尺寸/窗口变换/局部变换保持；接受帧 2 后切到新值。普通 QueryCtx 和 MutateCtx RawCtx 另断言 live。这里是实际运行，External 与作者 slider 包装仍只是源码分析。

## 真实命令与运行到哪一步

已成功：

```sh
cargo +1.96.0 check --locked --manifest-path tools/masonry-contract-check/Cargo.toml
cargo +1.96.0 run --locked --manifest-path tools/masonry-contract-check/Cargo.toml
python3 tools/masonry-contract-check/scripts/prepare.py --source /tmp/masonry-upstream
cargo +1.96.0 run --locked --manifest-path tools/masonry-contract-check/experiment/Cargo.toml \
  --features experimental --target-dir tools/masonry-contract-check/target
cargo +1.96.0 fmt --manifest-path tools/masonry-contract-check/Cargo.toml --check
cargo +1.96.0 test --locked --manifest-path tools/masonry-contract-check/Cargo.toml
cargo +1.96.0 test --locked --manifest-path tools/masonry-contract-check/experiment/Cargo.toml \
  --features experimental --target-dir tools/masonry-contract-check/target
```

上述生成过程逐一验证 72 个源文件哈希，原始源码闭包 592,487 字节；只有 `masonry_core` 六个文件有实验差异。`tree_arena` 同级路径、继承配置和许可证保留，其他生态没有导入生产仓库。

本轮完整核对命令从仓库根成功结束（exit 0）。有意设置旧工具链环境，验证脚本会明确切换到已安装的 1.96.0：

```sh
RUSTUP_TOOLCHAIN=1.90.0 ./tools/masonry-contract-check/scripts/check.sh
```

没有传 --source，实际从固定 codeload URL 下载并校验来源，然后完成格式、两种 clippy、原始对照、补丁 debug、release 和依赖审计；没有残留后台构建。最后一轮 release 缓存重编用时 1m 01s，完整运行日志另保留在：

```
/workspace/reel-masonry/tools/masonry-contract-check/target/final-verification.log
```

提交中的 `evidence/verification.log` 已复制最终完整日志。默认下载也单独执行了 `python3 tools/masonry-contract-check/scripts/prepare.py` 并 exit 0，输出见 [evidence/codeload-prepare.log](evidence/codeload-prepare.log)。独立依赖审计此前已 exit 0，本轮完整脚本再次完成；check-deps.py 自身也显式使用 cargo +1.96.0。两种构建均确认隔离 workspace、统一类型版本且无活跃 winit/wgpu/masonry_winit。

clippy 的实际命令分别为 `cargo +1.96.0 clippy --locked --manifest-path tools/masonry-contract-check/Cargo.toml --all-targets -- -D warnings`，以及实验 manifest 的同一命令加 `--features experimental --target-dir tools/masonry-contract-check/target`。两者 exit 0，没有压制 clippy 告警。工具修正主要是显式版本选择、转发观察入口和真实回归用例；三个行为缺陷在实验 core patch 中修复。

## 环境与暂时测量

Linux x86_64 / KVM，5 个可用 CPU，Intel Xeon Platinum 8573C，约 17 GiB 内存。Rust 1.96.0 (`ac68faa20`)，dev 无优化，release 为优化编译并保留调试信息。

最后一轮 release 测量（包含诊断 trace，100 次 warm-up）：三节点指针 p50=1,878 ns / p95=5,260 ns / p99=1,033,090 ns，捕获快照 p95=1,310 ns；1080p 坐标范围的 32 个真实滑条/34 节点，指针 p95=7,293 ns、捕获快照 p95=359,252 ns、未命中扫描 p95=1,828 ns；256 个真实滑条/258 节点，指针 p95=68,240 ns、捕获快照 p95=1,987,506 ns、未命中扫描 p95=215,202 ns。三节点 payload 下界 2,296 B，258 节点下界 200,176 B。debug/release 状态布局可能不同，仍排除 HashMap 桶与嵌套分配。上一轮同一 VM 的 256 控件指针/快照 p95 为 16,380 / 1,040,883 ns，尾延迟受调度影响明显，不能挑最好一轮作验收；这些不能当成设备帧预算证明。

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

默认 codeload 与 clippy 已完成。下一步由负责人审查生产几何投影/代际身份/预算接口及源码导入。独立层建议优先 Reel 完整记录流适配；Masonry 的缓存模式丢失需 fork 修正，若采用原生 External 还需记录真实变换及完整作用域。slider 可复用真实行为包装，但默认 thumb 中心与输入公式不同、方向及内嵌值投影无完整公开接口，最小 widgets 扩展建议见 ADAPTATION-NOTES。以上方案未实现新生产渲染器或控件。

原型克隆完整 WidgetState，不应直接作为生产内存设计。自定义命中、缺失呈现几何的查询结果、作者滑条槽位、输入消费策略与稳定资源身份转换的接口建议均交负责人。根 workspace 和作者指南保持目录隔离。Windows/N100、winit/wgpu、实际 GPU/surface 生命周期及其他目标设备仍待验证。
