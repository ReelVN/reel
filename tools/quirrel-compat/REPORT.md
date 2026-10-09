# Issue #13 实测报告

Quirrel 已真正构建并按原 runner 跑完：**1296 个 fixture / 1494 次运行，1486 次通过，8 次 golden 不匹配，退出 1**。
首批 **10 份 Aside 转换解析通过，32 个原 assert 调用保留**。**Aside 执行仍全部等待运行时**，没有语义通过项。

[复现命令](README.md#复现) · [完整逐 fixture CSV](results/inventory.csv) · [结构化清单](results/inventory.json)

## 固定来源和环境

- Quirrel 4.41.0：`19f17df9b6ee43c91ef5c635fd45970ec76c5dbe`，
  [来源/许可](../../third_party/quirrel/PROVENANCE.md)、[每个导入文件的 SHA-256](../../third_party/quirrel/UPSTREAM.json)。
  所有上游文件无改动，无嵌套 .git。
- 构建：Linux x86_64，G++ 14.2.0，Make 4.4.1，CMake 3.31.6，Release `sq`，parallel 4。
  `ENABLE_VAR_TRACE=OFF`，int64 / 默认 float32；未定义 SQUSEDOUBLE。
  CMake 与 Rust 原先缺失，通过官方安装方式放在 /tmp 后完成构建，未改系统工具链。
- 测试：Python 3.12.14，原 12 worker / 单次 10 秒超时；六个原入口和 iteration seeds 全保留。
  构建 warning、命令、原结果、返回码和每次完整输出均保留。
  [环境](results/environment.json)、[配置日志](results/configure.log)、[完整构建日志](results/build.log)。
- Aside 前端：隔离 detached worktree `/tmp/reel-aside-frontend`，固定 PR #9 的
  `fc418d9a26ebf5f0dfdbde2d6c25670e78c94926`；Rust/Cargo 1.96.0，
  外部 probe `cargo build --release --locked`。前端工作区前后无改动，本 PR 不含前端实现。
  [probe 源码](probe.rs)、[固定依赖](probe.lock)、[构建日志](results/frontend-build.log)、[实测解析](results/frontend.json)。

## 原套件结果

| 原 suite（上游 testRunner.py 选择） | fixture | 实际运行（含 seed） | 通过 | 不匹配 |
| --- | ---: | ---: | ---: | ---: |
| testData/exec | 361 | 455 | 455 | 0 |
| diagnostics | 221 | 225 | 217 | 8 |
| AST | 19 | 21 | 21 | 0 |
| static analyzer | 232 | 264 | 264 | 0 |
| parse types | 5 | 5 | 5 | 0 |
| doc/examples | 458 | 524 | 524 | 0 |

[原 runner 日志](results/upstream.log)、[逐次命令/seed/返回码/比较结果](results/upstream.json)、
[所有实际字节输出的归档](results/upstream-artifacts.zip)。zip 内路径由 JSON 的 actual/process_stdout/process_stderr 字段索引。
每个实际输出都保存 SHA-256；同一个 fixture 的默认/0/255 输出分别保留，不被后一次覆盖。

两个带记录的完整运行具有相同的 1494 个结果和实际输出 hash；另一次完全不挂 hook 的
[原 runner 直接运行](results/upstream-uninstrumented.log)同样退出 1、同样 8 个失败。
没有崩溃、超时、缺失 golden 或新建 golden。上游 runner 的 numOfTests 会统计非源码文件，
这里用实际 sq 调用和完整 fixture/seed 集合核对，不以文件数冒充执行结果。

### 保留的 8 个原失败

以下都是 Diagnostics suite 的实际输出不匹配，sq 返回码均为 0。
错误类别/文字相同，差异在列号或光标跨度；这是从 actual/expected 直接比较得到的现象，
未进一步归因到平台或上游代码。原 golden 未修改，失败未降级为通过。

| fixture | 差异 | actual / 原 golden |
| --- | --- | --- |
| `testData/diagnostics/compilation_errors/lex_errors/newline_in_const.nut` | 光标跨度短 1 字符 | [actual](results/failures/testData/diagnostics/compilation_errors/lex_errors/newline_in_const.nut/default.actual.txt) / [golden](../../third_party/quirrel/testData/diagnostics/compilation_errors/lex_errors/newline_in_const.diag.txt) |
| `testData/diagnostics/compilation_errors/syntax_errors/expected_bracket.nut` | 列 30 / 31 | [actual](results/failures/testData/diagnostics/compilation_errors/syntax_errors/expected_bracket.nut/default.actual.txt) / [golden](../../third_party/quirrel/testData/diagnostics/compilation_errors/syntax_errors/expected_bracket.diag.txt) |
| `testData/diagnostics/compilation_errors/syntax_errors/expected_expression.nut` | 列 7 / 8 | [actual](results/failures/testData/diagnostics/compilation_errors/syntax_errors/expected_expression.nut/default.actual.txt) / [golden](../../third_party/quirrel/testData/diagnostics/compilation_errors/syntax_errors/expected_expression.diag.txt) |
| `testData/diagnostics/compilation_errors/syntax_errors/expected_token_brace.nut` | 列 28 / 29 | [actual](results/failures/testData/diagnostics/compilation_errors/syntax_errors/expected_token_brace.nut/default.actual.txt) / [golden](../../third_party/quirrel/testData/diagnostics/compilation_errors/syntax_errors/expected_token_brace.diag.txt) |
| `testData/diagnostics/compilation_errors/syntax_errors/expected_while.nut` | 列 1 / 2 | [actual](results/failures/testData/diagnostics/compilation_errors/syntax_errors/expected_while.nut/default.actual.txt) / [golden](../../third_party/quirrel/testData/diagnostics/compilation_errors/syntax_errors/expected_while.diag.txt) |
| `testData/diagnostics/import_04.nut` | 列 0 / 1 | [actual](results/failures/testData/diagnostics/import_04.nut/default.actual.txt) / [golden](../../third_party/quirrel/testData/diagnostics/import_04.diag.txt) |
| `testData/diagnostics/import_07.nut` | 光标跨度短 1 字符 | [actual](results/failures/testData/diagnostics/import_07.nut/default.actual.txt) / [golden](../../third_party/quirrel/testData/diagnostics/import_07.diag.txt) |
| `testData/diagnostics/import_error.nut` | 列 19 / 20 | [actual](results/failures/testData/diagnostics/import_error.nut/default.actual.txt) / [golden](../../third_party/quirrel/testData/diagnostics/import_error.diag.txt) |

## 首批可追溯转换

以下全部是完整原 fixture，没有截取断言片段。三列独立记录：

| fixture（点击转换源码） | 分类 | 原 Quirrel | Aside 解析 | Aside 执行 | 原 assert 保留 |
| --- | --- | --- | --- | --- | ---: |
| [testData/exec/basics.nut](converted/testData/exec/basics.aside) | syntax_converted | pass | parse_ok | waiting_runtime | 1 |
| [testData/exec/for_loop_at_function_start.nut](converted/testData/exec/for_loop_at_function_start.aside) | syntax_converted | pass | parse_ok | waiting_runtime | 0 |
| [testData/exec/locals_chain.nut](converted/testData/exec/locals_chain.aside) | syntax_converted | pass | parse_ok | waiting_runtime | 0 |
| [testData/exec/optimizer_mul.nut](converted/testData/exec/optimizer_mul.aside) | syntax_converted | pass | parse_ok | waiting_runtime | 2 |
| [testData/exec/valid_syntax/closures_scope.nut](converted/testData/exec/valid_syntax/closures_scope.aside) | harness_adapted | pass | parse_ok | waiting_runtime | 17 |
| [testData/exec/valid_syntax/error_handling.nut](converted/testData/exec/valid_syntax/error_handling.aside) | syntax_converted | pass | parse_ok | waiting_runtime | 12 |
| [doc/examples/pages/language/errors-rethrow.nut](converted/doc/examples/pages/language/errors-rethrow.aside) | syntax_converted | pass | parse_ok | waiting_runtime | 0 |
| [doc/examples/pages/language/functions-minimal.nut](converted/doc/examples/pages/language/functions-minimal.aside) | syntax_converted | pass | parse_ok | waiting_runtime | 0 |
| [doc/examples/types/array/len.nut](converted/doc/examples/types/array/len.aside) | preserved | pass | parse_ok | waiting_runtime | 0 |
| [doc/examples/types/table/rawin.nut](converted/doc/examples/types/table/rawin.aside) | harness_adapted | pass | parse_ok | waiting_runtime | 0 |

数值：保留两项 int64 乘法断言；变量：顺序声明和算术重绑；闭包：共享可变 cell、
嵌套捕获、循环 captured_i、返回 table 中的闭包和 IIFE；表：自身 membership 的 missing/null；
数组：len/append/pop；控制流：条件 return、循环更新与提前 return；异常：抛 string/int/null/table、
缺失方法、嵌套 catch、loop 内 throw 和 rethrow。

[逐个精确范围的 recipe](recipes.json)记录理由；每份转换旁的 `.map.json` 包含双向完整字节映射
和精确 assert 对应。源文件 hash、原 golden hash 和分类均在清单中。所有 32 个原 assert 的
调用 token 序列不变，空输出 oracle 也保留。rawin→has_own 仅用于无 delegate/hook 的普通表；
top→最后项索引仅用于原测试已知非空的数组。未转换原 this/default/overflow 断言来伪造兼容。

最初解析发现 error_handling 的空对象 let 初始化缺少分号；已修正 recipe，最终 10 份解析
均无诊断且未耗尽预算。[初次诊断记录](results/frontend-initial.json)仍保留。
解析不检查绑定、类型、效果和执行；[未来测试宿主](HARNESS.md)尚未实现。

## 全量处理状态和有意差异

| 主分类 | fixture |
| --- | ---: |
| preserved | 1 |
| syntax_converted | 7 |
| harness_adapted | 2 |
| deliberate_delta | 15 |
| compiler_specific | 477 |
| blocked | 794 |

未转换的 1286 项全部在清单中：794 项尚待绑定/语义审查，477 项是原编译器专用 oracle，
15 项已识别有意差异。**blocked 是转换工作未完成，不是原语言运行失败**；上游原结果继续保留。
六类状态不是兼容率。没有把尚未处理项、语法通过项或原语言通过项算成 Aside 语义通过。

[差异登记](deliberate-deltas.json)分别列出规范段落、实际原 fixture 和待建的原生正反例：
逐调用默认值、每实例字段、显式 self/强绑定、严格 bool、检查整数与 float64、
普通表自身槽写入、Unicode 单位与缓冲。tuple 为 Aside 原生缺口，无对应原生 Quirrel oracle，
不能由 array/destructuring 转换代替。其浅别名、NaN、hash 拒绝路径、rest、单值多返回等仍待
真正 Aside 运行时验收。任务、迭代器、标准库、GC、预算和快照的完整目标继续以规范为准。

## 验证和剩余边界

`convert.py --check`、`inventory.py --check` 和完整性 unittest 通过。检查了快照 hash、
每个 fixture/seed 的完整记录、全部实际输出对原 golden、原断言保留、完整双向映射、
UTF-8 字节定位以及对改源、切开字符串/注释 token 和削弱断言的拒绝。
新增工具/文档的 diff whitespace 检查通过；原始失败输出末尾的空行触发 diff 检查提示，
按实际字节保留，没有为消除提示改写输出。

原 suite 仍退出 1，8 个诊断 golden 失败保留供审查；本项没有更改上游诊断器。
Aside 执行器和实际测试 prelude 是后续差分执行的阻塞项。未覆盖转换项需继续审查，
未验证 32 位、Vita、其他设备、VM 语义或 VM 性能/快照。
本报告是 draft PR 的开发证据，尚待集成负责人独立复核。
