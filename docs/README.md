# Reel 设计与开发文档

三份主题文档共同规定实现依据。作者指南提供中英两版。

1. [引擎架构](engine-architecture.md)：模块、可替换平台接口、舞台、输入、媒体、持久化和性能要求。
2. [Aside VM 与语言](aside-language.md)：统一语法、执行与暂停、字节码、宿主协议和快照。
3. [游戏开发指南 中文](guide/zh-CN.md) / [Game creator guide English](guide/en.md)：用相同示例说明剧情、UI、演出和作者扩展。

## 当前状态

设计版本 1.0 已提交独立交叉审查；此提交仅包含文档。作者 API 和 CLI 示例目前为待实现规范。审查中发现的冲突先修正文档，再进入对应模块实现。

## 维护规则

- 游戏开发者文档由项目负责人维护，中文与英文使用相同代码片段。
- API、行为与默认值改变时，同一个 PR 更新对应规范、两版指南和示例。
- 每个实现任务列明接口版本、允许目录、行为与性能要求，以及可复现验证方式。
- 关键路径由负责人读代码、核对契约和重跑相关验证后合入。
- 上游依赖默认按锁定版本使用；需要修改的源码快照另记来源、许可证与本地差异。

## Reading order

Game authors can start with the [English creator guide](guide/en.md). Engine contributors should also read the architecture and Aside language specification. The creator APIs are design targets until marked as implemented for a specific version.
