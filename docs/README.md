# Reel 设计与开发文档

三份主题文档共同规定实现依据。作者指南提供中英两版。

1. [引擎架构](engine-architecture.md)：模块、可替换平台接口、舞台、输入、媒体、持久化和性能要求。
2. [Aside VM 与语言](aside-language.md)：统一语法、执行与暂停、字节码、宿主协议和快照。
3. [游戏开发指南 中文](guide/zh-CN.md) / [Game creator guide English](guide/en.md)：用相同示例说明剧情、UI、演出和作者扩展。

## 当前状态

设计版本 1.1 是接口与行为的开发基线。代码已进入分模块实现和集成阶段；下列状态区分已合入 main 的 Rust 基础、尚在审查的分支，以及作者接口的设计目标。

- 已合入 main：reel-core 的共享身份、预算与服务契约；aside-vm 的 Value、堆、增量 GC、连续根区与 B01 字节码格式/验证；aside-compiler 的词法、语法、语义分析及 L04 有界 lowering；reel-stage 的 Masonry 呈现几何与输入适配；reel-render 的可移植帧、资源租约和记录型/null 消费者。能力边界见 [编译器](../crates/aside-compiler/README.md)、[L04 lowering](../crates/aside-compiler/LOWERING.md)、[VM 存储与根区](../crates/aside-vm/README.md)、[B01 字节码](../crates/aside-vm/src/bytecode/README.md)、[舞台](../crates/reel-stage/README.md)和[渲染契约](../crates/reel-render/README.md)。
- 待集成验收：[字节码执行器及源码执行联调（#23）](https://github.com/jhq223/reel/pull/23)、[Kira 音频适配（#14）](https://github.com/jhq223/reel/pull/14)，以及 [wgpu/winit 诊断窗口（#28）](https://github.com/jhq223/reel/pull/28)。分支实现、作者自测和独立验收分别记录；未合并能力不属于 main。
- 作者接口仍在实现：指南中的剧情、节点绑定、保存/回滚和 CLI 示例是目标合同。语法或语义分析通过不等于能够执行剧情；Rust 渲染诊断不等于完整游戏播放器。
- 设备验证按具体目标记录。CPU/null 模拟、软件 Vulkan、32 位或 Windows 交叉编译均不能替代相应物理设备的运行验收。

状态核对基线为 main 提交 `21e0ea09ddbd6c3926df72a6689183bb73b21598`。L04 已支持部分源码到验证字节码的编译，不代表完整语言支持；尚不支持的构造见其接口文档。

当前优先完成仓库结构、验证工具和生成产物的清理，再继续连接语言、舞台、输入、渲染与音频，推进会话、持久化和完整剧情体验。后续每次验收同步本节及受影响的双语指南，不用测试数量代替能力状态。

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
