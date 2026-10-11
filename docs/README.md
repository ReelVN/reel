# Reel 设计与开发文档

设计版本 1.1。架构、语言规范与作者指南共同定义接口；规范中尚未实现的完整目标不等于当前产品能力。

- [引擎架构](engine-architecture.md)：模块、舞台、输入、媒体、持久化和资源预算。
- [Aside 语言](aside-language.md)：语法、执行、字节码、宿主协议和快照。
- [中文作者指南](guide/zh-CN.md) / [English creator guide](guide/en.md)：相同代码示例与各功能的实现状态。

## 当前状态

- **编译与执行**：L04–L09 源码编译接通 B01 和 V03，支持普通调用、缺省参数、共享闭包 Cell、异常/finally、固定集合、索引/成员、可选取值链及位置调用 spread/rest。Windows 调试与发布检查实际执行微小预算、交错 GC、等待、取消和终态交付。[编译接口](../crates/aside-compiler/LOWERING.md) · [VM 与资源边界](../crates/aside-vm/README.md)。
- **剧情会话**：narrator/choose 接通可信 native 注册、模块初始化、真实 VM 等待/恢复、原选择 key 保活和 owner 退休。[可运行 Aside 示例](../crates/reel-engine/examples/story_contract.aside) · [无窗口宿主](../crates/reel-engine/examples/story_contract.rs) · [会话接口](../crates/reel-engine/README.md)。
- **作者工具**：reel check/build 编译独立单模块并读写 ASB v1；提供源码定位与 JSON 诊断。完整项目资源发行和 ASB 窗口播放器尚未实现。[使用命令](guide/zh-CN.md#当前单模块工具进展pr-52)。
- **UI 与渲染**：单一 reel-ui 包维护控件、布局、输入与呈现几何；reel-render/reel-player 保持 GPU 和窗口隔离。会话投影按内容布局并滚动，不另设两行或两个选项的功能上限。[窗口入口](../crates/reel-player/README.md) · [渲染验证范围](../crates/reel-render/R02-REPORT.md)。
- **音频**：Kira/可选 CPAL 桥、有界活动账本和物理 owner 退休已实现。逻辑完成、PCM/预算票据实际释放、描述符释放分别观察。脚本媒体等待、完整恢复和硬件完成时钟尚未接通。[接口](../crates/reel-media/README.md) · [验证边界](../crates/reel-media/VERIFICATION.md)。
- **开发工具**：第一方检查归入 Rust 测试和 reel-dev；Quirrel 保留固定来源、许可、原断言与转换映射。转换解析与真实执行分别记录。[可复现命令](../tools/reel-dev/README.md)。

完整语言仍包括尚未实现的类/元方法、模式、集合与具名展开、完整模块/标准库、generator/thread/async，以及引擎节点绑定、媒体等待和保存恢复。具体拒绝边界以各模块契约为准。CPU/软件 GPU 检查不替代物理设备运行或性能测量。

## 运行与维护

```sh
cargo run --locked -p reel-engine --example story_contract
cargo test --locked --workspace
cargo run --locked -p reel-dev -- check
```

- 作者指南两版使用相同 Aside 片段；API、默认值和行为改变时同步规范与指南。
- 关键路径按身份、预算、取消和交付合同审查，并实际运行相关检查。
- 普通依赖固定版本；导入源码保留来源、必要许可与修改记录。
- 可再生成日志、测量和报告放入忽略的 target/reel-artifacts；历史结果链接固定提交。
