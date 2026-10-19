# Reel 设计与开发文档

设计版本 1.1。架构、语言规范与作者指南共同定义接口；规范中尚未实现的完整目标不等于当前产品能力。

- [引擎架构](engine-architecture.md)：模块、舞台、输入、媒体、持久化和资源预算。
- [Aside 语言](aside-language.md)：语法、执行、字节码、宿主协议和快照。
- [Aside Rust 宿主扩展](aside-host.md)：函数绑定宏、属性声明、编译链接与运行时注册。
- [中文作者指南](guide/zh-CN.md) / [English creator guide](guide/en.md)：相同代码示例与各功能的实现状态。

## 当前状态

**目前已具备复杂交互作品的主要基础，仍需改善完整流程的可靠性、长内容成本和设备覆盖。** 目标是让创作者直观表达自定义对白、UI、演出和嵌入的 2D 游戏。下表区分实际可用能力与仍待完成的部分，不以测试数量换算完成百分比。

本表随能力交付更新；实施顺序和验收进度集中维护在[引擎完善总清单 #94](https://github.com/jhq223/reel/issues/94)。

| 领域 | 现在实际可用 | 主要缺口与跟踪 |
| --- | --- | --- |
| Aside / VM | 多文件、类型、类与继承、静态成员、可观察状态、宿主等待、任务与取消、执行快照、轻量标准库；GC 按分配压力调度。[语言执行范围](../crates/aside-compiler/LOWERING.md) | 结构模式、原生 async/generator 执行、剩余有价值的 Quirrel 用例与整体运行成本。[#65](https://github.com/jhq223/reel/issues/65) |
| 剧情 | 自定义 ADV/NVL、选择、历史、自动/快进、已读记录、本地化和全新剧情入口。[阅读服务](../examples/author_reading.aside) · [作者剧情界面](../examples/author_narrative.aside) | 完整游戏流程中菜单、演出、取消和恢复的连续验证；全文和翻译目录仍整体驻留。 |
| 舞台 / UI | 函数组件、响应式绑定、动态 keyed list、变高虚拟列表、自绘控件、文本输入与 IME、滚动、布局和富文本。[列表](../examples/author_list.aside) · [控件外观](../examples/author_control_states.aside) · [换行](../examples/author_text_wrap.aside) | dropdown、多行编辑、单选组键盘交互、程序化列表定位与接受帧几何查询；继续用复杂组件打磨写法。[#63](https://github.com/jhq223/reel/issues/63) |
| 演出 | tween、sequence/parallel/repeat、run/await、pause/resume/stop/finish、可观察时钟、属性接管、owner 退休与活动快照。[组合动作](../examples/author_activities.aside) | 通用演出作用域、取消恢复及更多转场组合。[#88](https://github.com/jhq223/reel/issues/88) |
| 渲染 / 效果 | 共同舞台、分层图像、2D 相机、大图分块、粒子、图像遮罩、子树反相与 WGPU 绘制；输入使用已呈现几何。[反相](../examples/author_invert.aside) | 更多滤镜、转场、自定义效果和完整 2D 创作 API。[#92](https://github.com/jhq223/reel/issues/92) |
| 工程 / 资源 | check/build/dev/run/export；开发与发布分离、外部文本池、资源分卷、后台读取与资源计账。[发布命令](guide/zh-CN.md#当前可用的检查构建与运行) | 长内容加载成本、编辑工具与跨平台发行验收。 |
| 音频 / 视频 | 音频播放、分组、循环区间、定位、音量/声像与淡变；H.264/AAC 视频节点使用共同舞台。[音频](../examples/author_audio.aside) | 长音轨流式、设备恢复和实机音画同步。[#67](https://github.com/jhq223/reel/issues/67) |
| 保存 / 恢复 | VM 执行、剧情、节点和活动快照；独立系统数据、缩略图、分片捕获与后台写盘。[存档](../examples/author_saves.aside) · [系统数据](../examples/author_persistent.aside) | 复杂同包恢复可靠性、暂停与峰值内存。要求匹配代码和格式，不提供旧包/旧档兼容。[#91](https://github.com/jhq223/reel/issues/91) |
| 输入 / 嵌入游戏 | 键鼠、触摸和手柄公共输入；命名动作、模拟值、重映射、焦点导航；小游戏与对白共用节点、结果、时钟和存档。[开关谜题](../examples/author_embedded_puzzle.aside) | 连续 2D 游戏所需能力仍需补齐；示例不代表已有完整物理、碰撞或固定步进 API。[#101](https://github.com/jhq223/reel/issues/101) |
| 平台 | 平台、呈现、运行时和资源 HAL；Winit 与 WGPU 可独立启用。[宿主接入](../crates/reel-player/README.md) | 实体设备、设备恢复、原生 IME/可访问性及跨平台验收。[#93](https://github.com/jhq223/reel/issues/93) |

Quirrel 原始输入、预期输出、来源和迁移登记保留在 [compiler fixtures](../crates/aside-compiler/tests/fixtures/quirrel/README.md)。已迁移用例经过编译、B01 往返和真实 VM 执行；保留资产不等于已迁移或已通过。

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

自检工程可以复用源码/ASB 执行检查。入口应自行断言结果并返回；检查使用真实存储服务和每轮独立的临时存档目录，可执行保存、恢复和 `game.start`，不读写玩家存档。CPU 呈现用于推进布局与呈现屏障，不包含 GPU、音频设备、窗口配置或截图后端，也不会自动点击或推进对白。

```powershell
$env:REEL_SCENARIO = 'D:/your-project/flow_check.aside'
cargo test --locked -p reel-cli --test project_scenarios external_project_scenario -- --ignored --nocapture
```

通用驱动本身由 `cargo test --locked -p reel-cli --test project_scenarios` 验证保存恢复、系统数据不回退和入口切换。游戏专属场景和资源留在作品目录，公共仓库只保留通用检查。

- 作者指南两版使用相同 Aside 片段；API、默认值和行为改变时同步规范与指南。
- 关键路径按身份、预算、取消和交付合同审查，并实际运行相关检查。
- 普通依赖固定版本；导入源码保留来源、必要许可与修改记录。
- 可再生成日志、测量和报告放入忽略的 target/reel-artifacts；历史结果链接固定提交。
