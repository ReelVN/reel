# 未来 Aside 执行宿主契约（未实现）

当前没有 Aside 执行结果；此文件不提供模拟 VM、替身 interpreter 或成功 stub。
固定前端只解析语法，不解析下面名字的绑定，也不检查类型/效果。

首批转换期待测试模块的显式宿主 prelude 提供 `assert`、`print`、`println`，不依赖 Reel。
运行时具备能力后，宿主必须提供：

- `assert(condition: bool, message = "assertion failed")`：false 时抛出真实测试失败；
  绝不能吞掉失败。首批均为单个 bool 条件，没有 lazy-message API 适配。
- `print(...values)` / `println(...values)`：捕获顺序执行的输出；多参数之间一个空格，
  println 末尾一个换行。首批使用字符串、普通整数和 bool；bool 格式 true/false。
  更广的 float、Unicode、实例 `_tostring` 和错误格式必须另外验收，不能假装已适配。
- 数组 `len/append/pop`、普通 table 读取、`has_own` 及共享 closure cell 都必须使用真实
  Aside 语义。`closures_scope` 的 `top()` 只在原测试已知非空的数组上改为最后项索引；
  不能把这个转换推广为一个忽略空数组错误的通用 adapter。
- 每份 fixture 独立模块/堆、固定执行预算；执行正常结束且原断言全部通过，再对照原
  `.out`，才可把 `aside_execution` 改为实际结果。失败记录源码映射和原 assertion ID。

原输出 oracle 与断言同时保留：空 `.out` 不表示没有行为检查（例如 optimizer_mul 的两项
assert）；有输出且没有 assert 的 fixture 则必须真实比较完整输出，不能只检查退出码。
原 runner 的 whitespace 比较规则与原始字节同时留存，以便独立核对。

已分类的有意差异使用原 Quirrel 结果加单独 Aside 规范 oracle，不能重写原 golden。
默认值、self、严格 bool、整数/float64、tuple、文本、集合迭代、异常展开、标准库、
任务及快照的完整验收仍以语言规范为准。
