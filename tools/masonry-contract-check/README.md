# Masonry contract check — U01 / Issue #3

这是设计 1.1（Reel `5a7cfd4c0802e604928e72f623ffec024d864d71`）的隔离验证工具，不是生产 stage/render 实现或作者 API。唯一修改范围是 `tools/masonry-contract-check/**`；根 workspace、生产 crates、作者指南和 GitHub Actions 均未修改。

验证实际链接固定 Masonry 提交中的 `RenderRoot`、`Button`、`Slider` 和 `Image`。`Observed<W>` 只记录上下文并委托上游控件方法；`StageGroup` 是使用公开布局/绘制扩展点的容器，没有自行实现按钮、滑条或替代 UI。像素由真实 `imaging_vello_cpu` 渲染。呈现结果由宿主夹具注入，用来验证快照提交门；不代表真实窗口、wgpu 表面或设备已经验证。

## 运行与证据

需要已安装 Rust **1.96.0**、该版本的 rustfmt/clippy、Python 3、`patch`、Linux Fontconfig 链接环境。普通 Cargo 依赖由 `Cargo.lock` 固定；默认关闭 Masonry 的渲染/窗口 features。

`check.sh` 显式导出 `RUSTUP_TOOLCHAIN=1.96.0`，因此在仓库根调用、根工具链仍为 1.90 或环境中指定旧工具链时也选择正确版本。直接 Cargo 命令须带 `+1.96.0`；仅传 `--manifest-path` 不会改变 rustup 的工作目录工具链选择。需要准备工具链时：

```sh
rustup toolchain install 1.96.0 --profile minimal --component rustfmt --component clippy
```

```sh
./tools/masonry-contract-check/scripts/check.sh
```

脚本依次检查格式、两种构建的 clippy（all-targets，`-D warnings`）、未修改上游对照、生成并验证实验源码、补丁行为检查、release 测量和依赖闭包。默认 codeload 固定提交下载路径已实际完成并通过来源检查及后续完整构建。已有固定提交的 Xilem 检出时可传 `--source /path/to/xilem`；它读取该提交的 Git archive，忽略检出中的工作改动。

单独运行未修改上游：

```sh
cargo +1.96.0 run --locked --manifest-path tools/masonry-contract-check/Cargo.toml
```

单独运行输入补丁实验：

```sh
python3 tools/masonry-contract-check/scripts/prepare.py
cargo +1.96.0 run --locked --release \
  --manifest-path tools/masonry-contract-check/experiment/Cargo.toml \
  --features experimental --target-dir tools/masonry-contract-check/target
```

`experiment/` 和 `target/` 是忽略的生成物。持久实验修改放入 `patches/presented-geometry.patch`；生成器会重建 `experiment/`。源文件逐一核对 `upstream-source.sha256`，保留上游许可证；应用补丁前先做 dry-run。基线和实验使用各自 lockfile，避免把本地 patch 混入普通依赖验证。

真实命令、环境准备、结果和测量见 [REPORT.md](REPORT.md)；原始输出见 [evidence/verification.log](evidence/verification.log)。独立层责任边界与 slider 包装方案的源码分析见 [ADAPTATION-NOTES.md](ADAPTATION-NOTES.md)，其中尚未实现的方案不作为行为通过结论。

## 可直接使用的上游能力

- 保留恒等宿主根，在其子组上使用 `NewWidget::with_transform` / `MutateCtx::set_transform`，图片、控件和子组共享同一仿射变换。
- `LayoutCtx::set_clip_path` 同时影响子树绘制和默认矩形命中。
- 在 `Widget::pre_paint` 记录 `Painter::push_group`，在 `post_paint` 记录 `pop_group`，可把控件、图像和嵌套组包在同一个 Imaging 合成组。透明度在子树合成后应用一次。
- 通过 `VisualLayerPlan::replay_into` 消费完整画序记录流，保留组/裁剪命令。子树 `pre/post` 缓存片段可以是不完整的作用域，消费者必须保留整段记录顺序。
- `Button::new` 接任意作者子节点。真实 hover/active 状态可投影到普通 `Image`，再经 `Image::set_image_data` 更新；不需要安装 Masonry 默认主题。滑条提供颜色、厚度、半径属性，但没有作者轨道/滑块节点槽位。

## 已复现的上游差异

下列位置均指固定 [Xilem 提交](https://github.com/linebender/xilem/tree/271a27a6d4a930f7878d404f9014e3c50a3a9b88)，不是移动的 main。

| 路径与位置 | 观察 | 接入边界 / 最小方案 |
| --- | --- | --- |
| `masonry_core/src/passes/event.rs:18` / `:72` | 按当前 arena 命中及冒泡；没有冻结视图定向入口 | 输入目标和路径需统一来自一次选定的呈现视图 |
| `masonry_core/src/passes/update.rs:825` / `:940` | 布局重写会按当前布局重新计算 hover，捕获祖先和 cursor 查询也用当前树 | 指针更新必须用同一几何源；接受新帧时重算静止 hover |
| `masonry_core/src/core/contexts.rs:494` / `:1182` / `:1282` | 局部坐标、尺寸、变换、裁剪等读取当前状态 | EventCtx、指针更新中的 QueryCtx/UpdateCtx/RawCtx 使用同一冻结几何；状态和有效性继续实时读取 |
| `masonry_core/src/core/contexts.rs:1597` / `:1607` | 被捕获节点删除后捕获会清掉，但没有 Cancel 回调 | 删除前仍存活时交付一次 Cancel；实验补丁实现此行为 |
| `masonry_core/src/passes/paint.rs:297` / `:352` | base layer 的绘制用恒等层变换和逆根变换，命中仍包含根变换 | 使用恒等宿主根 + 变换的舞台子组；实验不修改绘制源码 |
| `masonry_core/src/passes/paint.rs:98` | 每次重绘先重置 `paint_layer_mode`，无 paint 回调的缓存帧丢失原边界 | 分层模式如成为生产依赖，需持久化模式；当前只使用完整记录流 |
| `masonry_core/src/passes/paint.rs:151` / `:272`，`app/visual_layers.rs:25` | `IsolatedScene` 分割穿过组/裁剪作用域：三个片段中两个不能独立通过 Scene 校验；串联可正确渲染 | 独立离屏层/External 混合不能直接逐层渲染；需要显式 Begin/End 组记录和祖先裁剪上下文或等价层树 |
| `masonry/src/widgets/slider.rs:32` / `:368` | 内置滑条直接绘制轨道和滑块，只有数值/风格属性 | 水平作者槽位可由行为包装器承载；完整几何/方向/值投影接口见 ADAPTATION-NOTES。本轮只分析，没有声称槽位已实现 |
| `masonry/src/widgets/button.rs:109` / `slider.rs:154` | 控件捕获/发出 Action，但没有自动 `set_handled` | stage 的同步路由策略需消费相应输入；实验由拥有组明确消费，不能仅依赖返回的 Handled |

## 实验补丁与待负责人决定的接口

负责人已确认最小输入 fork 的必要性并授权继续实验完善；**生产源码导入仍待审查，尚未实施**。可审查差异位于 [patches/presented-geometry.patch](patches/presented-geometry.patch)。仅修改 `masonry_core` 六个文件：增加实验 `app/presented_geometry.rs`，接入 `app/mod.rs`、`app/render_root.rs`、`core/contexts.rs`、`passes/event.rs`、`passes/update.rs`。

最低本地源码闭包是 `masonry_core` + 同级 `tree_arena`，加上原始 workspace 继承配置和许可证：72 个原始文件，592,487 字节。仅前者有代码差异；后者满足同级 path 依赖。生成器缩小 workspace members 并显式标注成员 workspace 路径。`masonry`、文档宏、Parley、Peniko、Imaging、Vello CPU 等继续普通依赖。依赖检查确认一个可执行文件只链接一份 Masonry core/UI 数学和记录类型；不并存仓库内与 Git 版 UI 类型。

原型冻结 WidgetId、几何状态、DPI、子序和祖先路径。每个输入固定同一视图：先按冻结画序/裁剪/逆矩阵选目标，再核验当前存活/stashed 状态；禁用目标保留命中路径，由事件循环跳过其行为并沿祖先冒泡，捕获目标禁用的取消规则单独保留。不按当前几何二次命中。捕获、hover、上下文几何、祖先事件/状态路由、cursor QueryCtx 和指针阶段 RawCtx 共用该源；布局、绘制和普通 mutation 仍读当前状态。接受匹配且未过期的帧时才替换视图。

审查修正有实际回归用例：无快照与冻结快照下禁用子 Slider 的父组均消费点击；真实 Button/Slider 的 Up/Cancel 在冒泡中删除子控件时仅交付一次终止，真实 handler 仍看到 active capture，Button Up 仍看到 hover。删除前已开始的终止回调用一个捕获终止标记识别，删除不再追加 Cancel。正常事件外删除仍交付一次 Cancel。QueryCtx cursor、指针 UpdateCtx 的尺寸/窗口变换和 EventCtx.get_raw 的局部变换均直接执行并验证，新帧接受后才切换；普通 QueryCtx/MutateCtx RawCtx 仍读当前几何。

建议负责人确认生产接口时保留“显式几何提供器 + 选定视图内的定向分发”边界，而不是照搬原型：

- 原型为最小可运行补丁克隆整个 `WidgetState`；生产应只投影几何字段，并接入 D01 的代际 NodeId/WidgetId 映射和预算。报告中的字节数只是 payload 下界，不能当成分配峰值。
- 原型只覆盖仿射矩形逻辑命中；未调用作者自定义 `Widget::find_widget_under_pointer`。任意形状、透视、深度、shader 扭曲及动态命中策略需要视图里的纯逻辑映射，不能改回读取当前控件状态。
- 输入回调查询“尚未进入已呈现视图的新子节点”的几何会明确失败，不会静默回退到当前布局；生产上下文需由负责人决定缺失几何的受限结果形式。
- 上游目前只维护一个全局捕获目标。实验用单个主鼠标和主键；多指针/设备身份、不同按键的释放配对、模态/焦点导航和 Aside 任务路由没有据此验证。
- Imaging 的资源记录带普通图片 blob；转换为引擎 RenderFrame 时还需映射稳定资源身份/版本并保持组序、裁剪及颜色/alpha 约定。

作者指南和生产接口建议交给负责人维护，本 PR 不发布新的作者语法或默认 UI。

## 尚待目标设备 / 后续能力验证

Windows/N100 实机、winit 焦点与平台 capture、wgpu acquire/submit/present 失败、实际呈现时间、GPU 回收、GPU/表面 epoch、跨后端颜色和预乘 alpha、遮罩/模糊/任意 shader、可访问性以及移动/Web/Vita 均没有设备结论。注入的接受/失败结果只验证快照门和真实 Masonry 行为。Linux CPU 数据不能代替 50 ms 输入、8/12 ms CPU/GPU 帧预算或 60 Hz 尾延迟验收。

关闭窗口/GPU features 的本工具没有活跃的 `winit`、`wgpu` 或 `masonry_winit`，但 Parley 默认 Fontique 系统字体依赖仍会在 Linux 链接 Fontconfig；`use_system_fonts=false` 是运行期开关，不会移除编译依赖。完整平台隔离仍需后续目标构建确认。
