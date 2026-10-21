# Reel 设计与开发文档

架构与语言规范定义共同合同，作者指南说明组合用法。API 的英文注释随实现维护，签名和说明通过源码查询接口提供给编辑器。待实现能力在对应章节标注。

- [引擎架构](engine-architecture.md)：模块、舞台、输入、媒体、持久化和资源预算。
- [Aside 语言](aside-language.md)：语法、执行、字节码、宿主协议和快照。
- [Aside Rust 宿主扩展](aside-host.md)：函数绑定宏、属性声明、编译链接与运行时注册。
- [中文作者指南](guide/zh-CN.md) / [English creator guide](guide/en.md)：相同代码示例与各功能的实现状态。

## 当前状态

Reel 用于创作自定义对白、UI、演出和嵌入的 2D 游戏。下表列出可用能力与待完成事项。

本表随能力交付更新；实施顺序和验收进度集中维护在[引擎完善总清单 #94](https://github.com/jhq223/reel/issues/94)。

| 领域 | 现在实际可用 | 主要缺口与跟踪 |
| --- | --- | --- |
| Aside / VM | 多文件、类型、类与继承、静态成员、可观察状态、宿主等待、任务与取消、执行快照、轻量标准库；GC 按分配压力调度。[语言执行范围](../crates/aside-compiler/LOWERING.md) | 结构模式、原生 async/generator 执行、剩余有价值的 Quirrel 用例与整体运行成本。[#65](https://github.com/jhq223/reel/issues/65) |
| 剧情 | 自定义 ADV/NVL、选择、历史、自动/快进、已读记录、本地化和全新剧情入口。[阅读服务](../examples/author_reading/src/main.aside) · [作者剧情界面](../examples/author_narrative/src/main.aside) | 完整游戏流程中菜单、演出、取消和恢复的连续验证；原文池和已加载的翻译目录仍整体驻留。 |
| 舞台 / UI | 函数组件、响应式绑定、动态 keyed list、变高虚拟列表及按 key 定位、已显示节点的几何查询、自绘控件、单行/多行编辑与 IME、滚动、布局和富文本。[列表](../examples/author_list/src/main.aside) · [节点几何](../examples/author_geometry/src/main.aside) · [控件外观](../examples/author_control_states/src/main.aside) · [换行](../examples/author_text_wrap/src/main.aside) | dropdown、跨稀疏虚拟列表的单选组发现；继续用复杂组件打磨写法。[#90](https://github.com/jhq223/reel/issues/90) |
| 演出 | tween、组合动作、可观察时钟、scope 临时子树/属性覆盖/加性偏移、owner 退休和同包恢复。[组合动作](../examples/author_activities/src/main.aside) · [作用域](../examples/author_scope/src/main.aside) · [偏移](../examples/author_offsets/src/main.aside) | 冷资源首次显示的演出起点、更多对白/弹层/2D 取消恢复消费者。[#63](https://github.com/jhq223/reel/issues/63) |
| 渲染 / 效果 | 共同舞台、分层图像、2D 相机、大图分块、独立粒子批次、共享遮罩、场景转场、子树滤镜/混合与 WGPU 绘制；输入使用已呈现几何。[转场](../examples/author_transition/src/main.aside) · [遮罩](../examples/author_composed_mask/src/main.aside) | 更多滤镜、转场、自定义效果和完整 2D 创作 API。[#92](https://github.com/jhq223/reel/issues/92) |
| 工程 / 资源 | check/fmt/build/dev/run/export；固定项目入口、开发与发布分离、外部文本池、资源分卷、后台读取与资源计账。[发布命令](guide/zh-CN.md#当前可用的检查构建与运行) | 长内容加载成本、编辑工具与跨平台发行验收。 |
| 编辑器 | Rust LSP 的诊断、补全、参数提示、悬停、跨模块定义跳转和共享格式化器；未保存内容与依赖失效；Rust 声明生成只读内置定义，VS Code 客户端提供编辑入口。[编写流程](guide/zh-CN.md#在-vs-code-中编写) | 重命名、请求取消与更细粒度的增量分析。[#142](https://github.com/jhq223/reel/issues/142) |
| 音频 / 视频 | 短音效预解码、长音轨流式读取、片段队列、分组、循环区间、定位、音量/声像与淡变；H.264/AAC 视频节点使用共同舞台。[音频](../examples/author_audio/src/main.aside) · [音频队列](../examples/author_audio_queue/src/main.aside) | 解码器内部工作空间的完整预算、资源复用、设备恢复和实机音画同步。[#67](https://github.com/jhq223/reel/issues/67) |
| 保存 / 恢复 | VM 执行、剧情、节点和活动快照；独立系统数据、缩略图、分片捕获与后台写盘。[存档](../examples/author_saves/src/main.aside) · [系统数据](../examples/author_persistent/src/main.aside) | 复杂同包恢复可靠性、暂停与峰值内存。要求匹配代码和格式，不提供旧包/旧档兼容。[#91](https://github.com/jhq223/reel/issues/91) |
| 输入 / 嵌入游戏 | 键鼠、触摸和手柄公共输入；命名动作、模拟值、重映射、焦点导航；小游戏与对白共用节点、结果、时钟和存档。[开关谜题](../examples/author_embedded_puzzle/src/main.aside) | 连续 2D 游戏所需能力仍需补齐；示例不代表已有完整物理、碰撞或固定步进 API。[#101](https://github.com/jhq223/reel/issues/101) |
| 平台 | 平台、呈现、运行时和资源 HAL；Winit 与 WGPU 可独立启用。[宿主接入](../crates/reel-player/README.md) | 实体设备、设备恢复、原生 IME/剪贴板的实机验收、系统可访问性适配及跨平台验收。[#93](https://github.com/jhq223/reel/issues/93) |

Quirrel 原始输入、预期输出、来源和迁移状态记录在 [compiler fixtures](../crates/aside-compiler/tests/fixtures/quirrel/README.md)。已迁移用例执行编译、ASB 往返和 VM 检查。

### 先运行已支持的示例

- [基本画面](../examples/author_stage/src/main.aside)：函数、文本、布局与剧情。
- [图片](../examples/author_images/src/main.aside)和[变换](../examples/author_transforms/src/main.aside)：共同舞台与资源。
- [组合演出](../examples/author_activities/src/main.aside)和[交互按钮](../examples/author_buttons/src/main.aside)：实际活动、等待与回调。
- [作用域弹层](../examples/author_prompt/src/main.aside)：函数组件、可观察等待状态和取消清理。
- [虚拟列表](../examples/author_list_gallery/src/main.aside)：自然行高、稳定 key 与按需节点。
- [多行编辑](../examples/author_text_area/src/main.aside)：正文绑定、换行、选区与提交。

示例直接引用当前接口；验证命令与平台要求见下文及各模块入口。

## 运行与维护

```sh
cargo run --locked -p reel-engine --example story_contract
cd examples/author_stage
reel check
reel dev --assets ../../target/reel-artifacts/author-assets
cd ../..
cargo test --locked --workspace
cargo run --locked -p reel-dev -- check
```

自检工程的 `src/main.aside` 中由 `main` 自行断言结果并返回。检查分别执行源码和 ASB，使用真实存储服务、独立的临时存档目录、CPU 布局和离线音频消费，支持保存、恢复和 `game.start`。时间取自单调时钟，每个 VM 工作片的截止时间为 2 ms。对白推进和选择由自检脚本安排；窗口、GPU、声卡和截图后端通过各自的平台检查验证。

```powershell
$env:REEL_SCENARIO = 'D:/your-project'
cargo test --locked -p reel-cli --test project_scenarios external_project_scenario -- --ignored --nocapture
```

通用驱动本身由 `cargo test --locked -p reel-cli --test project_scenarios` 验证保存恢复、系统数据不回退和入口切换。游戏专属场景和资源留在作品目录，公共仓库只保留通用检查。

- 作者指南两版使用相同 Aside 片段；API、默认值和行为改变时同步规范与指南。
- 关键路径按身份、预算、取消和交付合同审查，并实际运行相关检查。
- 普通依赖固定版本；导入源码保留来源、必要许可与修改记录。
- 可再生成日志、测量和报告放入忽略的 target/reel-artifacts；历史结果链接固定提交。
