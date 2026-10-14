# Reel 设计与开发文档

设计版本 1.1。架构、语言规范与作者指南共同定义接口；规范中尚未实现的完整目标不等于当前产品能力。

- [引擎架构](engine-architecture.md)：模块、舞台、输入、媒体、持久化和资源预算。
- [Aside 语言](aside-language.md)：语法、执行、字节码、宿主协议和快照。
- [Aside Rust 宿主扩展](aside-host.md)：函数绑定宏、属性声明、编译链接与运行时注册。
- [中文作者指南](guide/zh-CN.md) / [English creator guide](guide/en.md)：相同代码示例与各功能的实现状态。

## 当前状态

**目前能运行简单的交互场景，仍处于开发原型阶段，尚不能独立完成并发行一部完整作品。** 编译器接受语法、Rust 层有实现、作者能够实际使用是三个不同边界；以下按最后一项说明状态。不以测试数量换算引擎完成百分比。

本表随能力交付更新；实施顺序和验收进度集中维护在[引擎完善总清单 #94](https://github.com/jhq223/reel/issues/94)。

| 领域 | 现在实际可用 | 主要缺口与跟踪 |
| --- | --- | --- |
| Aside / VM | 多源码链接→B01→执行；函数、闭包、默认/命名参数、位置/具名 spread、rest、集合编辑、异常/finally、可调用宿主对象/方法及 await、增量 GC。[语言执行边界](../crates/aside-compiler/LOWERING.md) | 类/元方法、完整模式/标准库、通用 async/Future、generator/thread 和快照。[#65](https://github.com/jhq223/reel/issues/65) |
| 剧情 | 角色/旁白、ADV 行组、choose 的等待与恢复、历史、Unicode 字素揭示和公共播放时钟；只读对白/选择模型、自定义 ADV/NVL 视图与清页。[作者剧情界面](../examples/author_narrative.aside) · [角色对白](../examples/author_dialogue.aside) | 历史回看、自动/跳读和本地化等完整创作流程，见总清单 |
| 舞台 / UI | text/image/row/column/stack/button/input/slider/toggle/scroll、PNG/JPEG、布局/变换/裁剪、原节点属性更新、局部 add/remove/clear；按钮及值变更回调可以等待活动，设置控件支持脚本回写和连续输入，model、bind/unbind、derive 连接可观察状态并共享动画写入权；view 按实际数据依赖重建内容；只读交互状态和 decorated 支持作者自绘按钮/开关；modal 隔离输入，支持嵌套焦点恢复。[动态组件](../examples/author_dynamic_stage.aside) · [按钮](../examples/author_buttons.aside) · [模态菜单](../examples/author_modal.aside) · [设置页](../examples/author_settings.aside) · [属性连接](../examples/author_bindings.aside) · [控件外观](../examples/author_control_states.aside) | 滑条外观槽位、文本输入、列表复用/虚拟化、通用事件和 scope。[#90](https://github.com/jhq223/reel/issues/90) · [#63](https://github.com/jhq223/reel/issues/63) |
| 演出 | tween、sequence/parallel/repeat、run/await、pause/resume/stop/finish、属性接管和 owner 退休；纯变换不重新布局。[可运行例](../examples/author_activities.aside) | 通用 scope、快进和活动快照尚未接通。组合动作合同：[#88](https://github.com/jhq223/reel/issues/88) |
| 渲染 / 效果 | 共同帧描述、可选 WGPU 实际绘制、已呈现几何输入；作者可用图片、文字、透明度和基础裁剪。[渲染器](../crates/reel-render/README.md) | 内部遮罩/滤镜记录尚未接成作者接口；通用转场和自定义效果。[#92](https://github.com/jhq223/reel/issues/92) |
| 工程 / 资源 | check/build/dev/run；脚本初始化、多字体与节点字体选择、多源码导入、循环链接与独立环境、固定画布；完整代码 ASB；图片与字体使用可替换 AssetIo/WorkExecutor，后台读取、共享输入及像素、原子画面切换与租约；资源根移动不改变资源身份。[命令](guide/zh-CN.md#当前可用的检查构建与运行) | 资源包/发布与媒体消费方接入。[#66](https://github.com/jhq223/reel/issues/66) · [#67](https://github.com/jhq223/reel/issues/67) |
| 音频 / 视频 | 作者 audio.play、循环/音量、pause/resume/stop、await、节点取消及 PCM 退休已接通；源码与 ASB 共用后台读取、解码和真实混音。[示例](../examples/author_audio.aside) | 当前是预解码播放；长轨流式、seek、分组混音、设备重建和视频继续由 [#67](https://github.com/jhq223/reel/issues/67) 跟踪。 |
| 保存 / 恢复 | 有存储服务合同；ASB 是代码产物 | **VM、剧情及活动快照尚未实现**；没有可用的游戏存读档。[#91](https://github.com/jhq223/reel/issues/91) |
| 输入 | 鼠标、多触点、键盘和手柄；命名动作、保持/模拟值与重映射；已接受画面的空间导航及焦点滚动。[作者动作](../examples/author_input.aside) | 手柄 Windows 后端初始化已验证，实体设备及其他平台仍待实测；小游戏会话/更新由 [#101](https://github.com/jhq223/reel/issues/101) 继续 |
| 平台 | PlatformHost/Presentation/StoryRuntime/RenderBackend 与作者资源 HAL 已贯通，Winit 与 WGPU 可独立启用；内置窗口复用相同接口。[宿主接入](../crates/reel-player/README.md) | 媒体适配、原生 IME/可访问性及各设备运行仍需交付。[#93](https://github.com/jhq223/reel/issues/93) |

Quirrel 原始输入、预期输出及来源保留在 [compiler fixtures](../crates/aside-compiler/tests/fixtures/quirrel/README.md)。当前映射登记了 **67 个完整文件的迁移**，会编译、往返 B01 并在真实 VM 中执行；其余保留资产不等于已迁移或已通过。新宿主测试不计入原始测试迁移。

### 先运行已支持的示例

- [基本画面](../examples/author_stage.aside)：函数、文本、布局与剧情。
- [图片](../examples/author_images.aside)和[变换](../examples/author_transforms.aside)：共同舞台与资源。
- [组合演出](../examples/author_activities.aside)和[交互按钮](../examples/author_buttons.aside)：实际活动、等待与回调。

作者指南中标为目标设计的片段不能作为当前运行入口。示例经过源码/ASB 的针对性合同验证；历史 GPU 报告只证明记录中的提交与环境，CPU 检查不替代实际设备验收。

## 运行与维护

```sh
cargo run --locked -p reel-engine --example story_contract
cargo run --locked -p reel-cli -- check examples/author_stage.aside
cargo run --locked -p reel-cli --features window -- dev examples/author_stage.aside
cargo test --locked --workspace
cargo run --locked -p reel-dev -- check
```

- 作者指南两版使用相同 Aside 片段；API、默认值和行为改变时同步规范与指南。
- 关键路径按身份、预算、取消和交付合同审查，并实际运行相关检查。
- 普通依赖固定版本；导入源码保留来源、必要许可与修改记录。
- 可再生成日志、测量和报告放入忽略的 target/reel-artifacts；历史结果链接固定提交。
