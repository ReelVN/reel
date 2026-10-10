# Reel 设计与开发文档

三份主题文档共同规定实现依据。作者指南提供中英两版。

1. [引擎架构](engine-architecture.md)：模块、可替换平台接口、舞台、输入、媒体、持久化和性能要求。
2. [Aside VM 与语言](aside-language.md)：统一语法、执行与暂停、字节码、宿主协议和快照。
3. [游戏开发指南 中文](guide/zh-CN.md) / [Game creator guide English](guide/en.md)：用相同示例说明剧情、UI、演出和作者扩展。

## 当前状态

设计版本 1.1 是接口与行为的开发基线。代码已进入分模块实现和集成阶段；下列状态区分已合入 main 的 Rust 基础、尚在审查的分支，以及作者接口的设计目标。

- 已合入 main：reel-core 的共享身份、预算与服务契约；aside-vm 的 Value、堆、增量 GC、连续根区与 B01 字节码格式/验证；aside-compiler 的词法、语法、语义分析及 L04 有界 lowering、L05 初始化局部绑定、L06 固定集合/索引编译、L07 非可选成员编译与 L08 可选取值链编译；reel-ui 的控件、布局、呈现几何与输入桥接；reel-render 的可移植帧、资源租约、记录型/null 消费者和 wgpu 消费者；reel-player 的 winit 诊断窗口；reel-media 的 Kira 音频桥、可选 CPAL 输出与有界后台 PCM 回收。能力边界见 [编译器](../crates/aside-compiler/README.md)、[L04 lowering](../crates/aside-compiler/LOWERING.md)、[VM 存储与根区](../crates/aside-vm/README.md)、[B01 字节码](../crates/aside-vm/src/bytecode/README.md)、[舞台](../crates/reel-ui/README.md)和[渲染契约](../crates/reel-render/README.md)。
- 待集成验收：[字节码执行器及源码执行联调（#23）](https://github.com/jhq223/reel/pull/23)。分支实现、作者自测和独立验收分别记录；未合并能力不属于 main。
- 作者接口仍在实现：指南中的剧情、节点绑定、保存/回滚和 CLI 示例是目标合同。语法或语义分析通过不等于能够执行剧情；Rust 渲染诊断不等于完整游戏播放器。
- 设备验证按具体目标记录。CPU/null 模拟、软件 Vulkan、32 位或 Windows 交叉编译均不能替代相应物理设备的运行验收。

本次 L09 的开发基线为 `244bf5c7f15ff697f336388c06958472f13eeccd`，已包含 UI 整合、R04 和 L05–L08；调用 spread/rest 位于 PR #47，最终合并状态以该 PR 为准。L04 已支持部分源码到验证字节码的编译，不代表完整语言支持；尚不支持的构造见其接口文档。

仓库结构与验证工具整改已合入：[PR #29](https://github.com/jhq223/reel/pull/29) 曾将 UI 借用代码纳入 crates 和统一 workspace，第一方验证工具迁入 [reel-dev](../tools/reel-dev/README.md)，可再生成结果移至忽略的 target/reel-artifacts；Quirrel 的必要来源与测试输入继续保留。Linux 独立验收通过；Rust 上游观测桥的真实 Quirrel 重跑及 Windows 进程终止路径仍未验证。

[音频适配（#14）](https://github.com/jhq223/reel/pull/14) 已完成整理后主线的独立集成验收：Linux 离线真实 Kira、default/feature-off 和 Windows 交叉编译通过；实体输出、时钟、热插拔及原生启停行为仍待目标设备验证，CPAL 时钟保持 Unavailable。见[音频验证边界](../crates/reel-media/VERIFICATION.md)。

[窗口/GPU 呈现（#28）](https://github.com/jhq223/reel/pull/28) 已通过软件 Vulkan（llvmpipe）及 Xvfb/X11 的独立组合验收：描边参数拒绝与定位、预算压力完成后重试、永久错误停止重复准备、恢复能力重新协商和系统 Expose 后无输入恢复画面均通过。永久坏模型只重绘已接受的冻结帧，不重新编译坏模型或替换输入几何。debug/release 的 GPU、窗口、音频及相关契约检查通过；物理 GPU、真实设备切换/热拔、OS 挂起、Wayland 和原生 Windows/macOS 仍待验。见[渲染验证范围](../crates/reel-render/R02-REPORT.md)。

[UI 模块整合（#35）](https://github.com/jhq223/reel/pull/35) 将原五个 UI 相关 crate 收敛为 reel-ui 内的职责模块，不保留转发壳或文档宏包。借用代码由 Reel 自主维护，保留原始许可和必要版权，不为上游同步保留目录、版本或核验流程。render/player 的后端和平台隔离保持。[模块说明](../crates/reel-ui/MIGRATION.md)。

[有界管线复用（#33）](https://github.com/jhq223/reel/pull/33) 已通过 native wgpu-core 的独立验收：每个 Renderer 按 format/samples 最多惰性维护三套管线，同档重复准备不重建。构造错误须完成内部错误作用域确认后才允许入缓存；外层作用域、跨档并发、sticky 故障和 Pending 拒绝路径已覆盖。仅内部 GPU 对象复用，未新增作者 API，未改变质量选择或帧/资源预算；driver 内存不属于硬字节保证。浏览器 WebGPU 与物理设备故障仍未验。

[单次准备白纹理复用（#38）](https://github.com/jhq223/reel/pull/38) 已独立验证：同一 prepare/crossfade 内的纯色绘制和裁剪按需共享一次白纹理上传，未使用时不创建；不同事务不共享。重复使用的保守应用资源账目只计一次 536B，提交/完成、取消和失败的寿命保持。该数字不代表 GPU 总内存，软件设备 CPU 测量不代表物理 GPU 性能。

[初始化局部绑定编译（#39）](https://github.com/jhq223/reel/pull/39) 将模块根 global 与 initializer-local 明确分离，支持局部闭包与转捕获、入口 Cell 重建的静态生成，并通过 B01 验证/编解码检查。尚未提供的外部 Value/Cell 链接仍定位拒绝。运行时 Cell 共享与重入隔离、GC/宿主恢复及完整脚本模块初始化仍需各自验收。

[固定集合与索引编译（#41）](https://github.com/jhq223/reel/pull/41) 已完成独立静态验收：tuple/array/table、有序键值求值、普通索引、混合位置赋值链与复合赋值生成通过 CFG/寄存器身份、B01 验证/编解码及账本检查。右值重绑 receiver/key 后仍使用预先保存的位置。集合展开/模式继续拒绝；可选取值链见 L08；生成正确结构不等于实际集合运行或完整作者示例已接通。

接下来继续打磨预算内质量选择和渲染资源复用，并连接语言、舞台、输入、渲染与音频，推进会话、持久化和完整剧情体验。后续每次验收同步本节及受影响的双语指南，不用测试数量代替能力状态。

## 本次定稿

- 角色可直接调用，旁白显式使用 narrator；剧情和 UI 回调共享普通函数与等待语义。
- 普通对象字面量、tuple、完整文本与缓冲、标准集合、异步和宿主续体纳入语言合同。
- 节点、绑定、作用域、活动和持久变量采用统一的所有权、取消和恢复规则。
- 引擎公共服务归入 reel-core；窗口、渲染、音频输出可独立替换。
- VM 默认采用 16 字节显式标签、完整整数载荷和增量追踪回收；选型依据和测量范围见语言文档。
- 发布产物分为 .asb 字节码、.rpk 资源包和 release.json。

## 维护规则

- 游戏开发者文档由项目负责人维护，中文与英文使用相同代码片段。
- API、行为与默认值改变时，同一个 PR 更新对应规范、两版指南和示例。
- 每个实现任务列明接口版本、允许目录、行为与性能要求，以及可复现验证方式。
- 关键路径由负责人读代码、核对契约和重跑相关验证后合入。
- 上游依赖默认按锁定版本使用；需要修改的源码快照另记来源、许可证与本地差异。

## Reading order

Game authors can start with the [English creator guide](guide/en.md). Engine contributors should also read the architecture and Aside language specification. The creator APIs are design targets until marked as implemented for a specific version.

[非可选成员编译（#43）](https://github.com/jhq223/reel/pull/43) 已合入，扩展 MemberGet/MemberSet、Name/Index/Member 混合位置和十种复合赋值；普通成员调用复用动态 Call，不由编译器注入 self。简单写入不提前读取目标，位置保存原对象值与符号。class/super、native 函数值、完整统一 callable 和实际运行验收仍未完成。

[可选取值链（#45）](https://github.com/jhq223/reel/pull/45) 使用既有 BranchNull 编译显式 null guard、同链后缀短路和独立嵌套汇合。Group 结束传播；非 null 读/调用继续使用原异常契约。同链可选赋值、可选 native 值/调用仍拒绝。静态 CFG 与寄存器检查不能替代实际副作用、hook、GC 和恢复验收。

[会话宿主与真实视图（#50）](https://github.com/jhq223/reel/pull/50) 已合入主线：reel-engine 的 Rust 会话模型接通真实 Label/Button、渐显推进、选择、取消和呈现输入门。使用显式 Rust 请求，尚未接 VM 剧情、音频或保存恢复；软件 GPU 验证不代表物理硬件通过。[接口与限制](../crates/reel-engine/README.md) · [窗口入口](../crates/reel-player/README.md#rust-session-host-window-s01)。

[调用展开与 rest（#47）](https://github.com/jhq223/reel/pull/47) 增加源码顺序参数 builder、Packed 调用及 rest 参数身份；集合展开仍待独立设计。静态编译不等于 V03 支持或剧情会话已接通，真实浅捕获、并发修改、缺省执行及 GC/恢复尚未验收。

[执行器源码整合（#49）](https://github.com/jhq223/reel/pull/49) 基于含 L09 的 main，保留原 B01 与编译器成果；当前只有原生/i686 cargo check 证据，仍为独立执行待验的集成工作。集合/索引/成员及脚本 rest 的后续源码位于 [草稿 #54](https://github.com/jhq223/reel/pull/54)，仍只有编译与源码审查证据；property-bag、完整模块/类和快照等尚未接通，不代表完整剧情可运行。取消任务和宿主根退休的责任已明确分离。[VM 状态与生命周期](../crates/aside-vm/README.md#v03-source-integration-and-host-lifecycle)。

[集合执行增量（#54）](https://github.com/jhq223/reel/pull/54) 基于 #49 提供分片集合构造、索引/成员、tuple 常量及调用 spread/rest 的执行源码，并公开有界宿主集合读取接口。原生/i686 all-targets 编译通过；异常路径与 packed 计费缺陷已修，并通过独立源码复审。测试仅编译未执行，实际语义、GC/取消和性能仍未验收。线性 table 查找与同步 tuple 常量单项额度明确保留。[状态和接口](../crates/aside-vm/README.md#collection-host-access-and-execution-source-boundaries-pr-54)。

[Aside 会话适配（#55）](https://github.com/jhq223/reel/pull/55) 在 #54 之上连接真实编译入口、可信 native 注册、等待/交付根和 SessionUi，支持当前 narrator/choose 子集。原生/i686 编译与独立源码审查不等于运行验收；现有窗口命令仍使用 Rust 请求，尚无 .asb 播放入口。角色、作者自定义脚本 UI、媒体和保存恢复仍属后续完整目标。[适配契约](../crates/reel-engine/README.md#aside-session-adapter-pr-55) · [播放器边界](../crates/reel-player/README.md#aside-session-library-assembly-pr-55)。
