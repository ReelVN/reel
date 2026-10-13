# Reel 设计与开发文档

设计版本 1.1。架构、语言规范与作者指南共同定义接口；规范中尚未实现的完整目标不等于当前产品能力。

- [引擎架构](engine-architecture.md)：模块、舞台、输入、媒体、持久化和资源预算。
- [Aside 语言](aside-language.md)：语法、执行、字节码、宿主协议和快照。
- [中文作者指南](guide/zh-CN.md) / [English creator guide](guide/en.md)：相同代码示例与各功能的实现状态。

## 当前状态

- **编译与执行**：源码编译接通 B01 和 VM，支持普通调用、缺省参数、共享闭包 Cell、异常/finally、固定集合、索引/成员、可选取值链及位置调用 spread/rest；尾随 children 与 array/tuple 的 for 可用于组件组合。可组合宿主对象保持身份并支持属性读写。[编译接口](../crates/aside-compiler/LOWERING.md) · [VM 与资源边界](../crates/aside-vm/README.md)。
- **剧情会话**：narrator/choose 接通可信 native 注册、模块初始化、真实 VM 等待/恢复、原选择 key 保活和 owner 退休。[可运行 Aside 示例](../crates/reel-engine/examples/story_contract.aside) · [无窗口宿主](../crates/reel-engine/examples/story_contract.rs) · [会话接口](../crates/reel-engine/README.md)。
- **作者工具**：reel check/build 与引擎共用 Reel 接口注册、默认名称环境和编译入口，作者无需逐文件导入 prelude。开启 window 后，dev 从源码运行，run 直接加载 ASB v1；两者使用同一个原生窗口宿主。完整工程和素材包尚未实现。[使用命令](guide/zh-CN.md#当前可用的检查构建与运行)。
- **UI 与渲染**：text、image（PNG/JPEG）、row、column、stack 与 stage.replace 接通共享舞台；普通函数、条件和循环组合节点，属性修改保留原控件身份。位置、旋转、缩放、锚点、尺寸、颜色、透明度、裁剪、可见性和内容布局进入生产呈现路径。窗口使用脚本画面与默认对白/选择覆盖层。[作者示例](../examples/author_stage.aside) · [组件变换](../examples/author_transforms.aside) · [窗口入口](../crates/reel-player/README.md) · [渲染验证范围](../crates/reel-render/R02-REPORT.md)。
- **音频**：Kira/可选 CPAL 桥、有界活动账本和物理 owner 退休已实现。逻辑完成、PCM/预算票据实际释放、描述符释放分别观察。脚本媒体等待、完整恢复和硬件完成时钟尚未接通。[接口](../crates/reel-media/README.md) · [验证边界](../crates/reel-media/VERIFICATION.md)。
- **开发工具**：第一方检查归入 Rust 测试和 reel-dev；Quirrel 保留固定来源、许可、原断言与转换映射。转换解析与真实执行分别记录。[可复现命令](../crates/reel-dev/README.md)。

集合字面量展开、范围切片和基础容器编辑已接通；[集合编辑示例](../examples/collection_editing.aside) 可用源码和 ASB 运行。完整语言仍包括尚未实现的类/元方法、模式、具名展开、完整模块/标准库、generator/thread/async。节点事件、响应式绑定、动画活动、媒体等待和保存恢复仍需接通。具体边界以各模块契约为准。CPU/软件 GPU 检查不替代物理设备运行或性能测量。

## 运行与维护

```sh
cargo run --locked -p reel-engine --example story_contract
cargo run --locked -p reel-cli -- check examples/author_stage.aside
cargo run --locked -p reel-cli --features window -- dev examples/author_stage.aside --font path/to/font.ttf
cargo test --locked --workspace
cargo run --locked -p reel-dev -- check
```

- 作者指南两版使用相同 Aside 片段；API、默认值和行为改变时同步规范与指南。
- 关键路径按身份、预算、取消和交付合同审查，并实际运行相关检查。
- 普通依赖固定版本；导入源码保留来源、必要许可与修改记录。
- 可再生成日志、测量和报告放入忽略的 target/reel-artifacts；历史结果链接固定提交。
