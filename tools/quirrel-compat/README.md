# Quirrel 原始测试与 Aside 转换基线

`third_party/quirrel` 保留 Quirrel 4.41 固定快照、上游 Python runner/build、
COPYRIGHT、来源哈希和全部原始 fixtures。Aside 的 Rust 迁移完成前继续作为行为参照，
不链接进发行播放器。转换规则、deliberate deltas、衍生源码和双向字节映射仍被版本管理。

## 当前检查

从仓库根运行；要求 Rust 1.96.0。以下命令只检查文件或解析/分析，不执行 Aside：

```sh
cargo run --locked -p reel-dev -- check
cargo test --locked -p reel-dev
cargo run --locked -p reel-dev -- inventory
cargo test --locked -p aside-compiler --test semantic_quirrel
```

需要主动再生成已审查衍生文件时使用 `cargo run --locked -p reel-dev -- convert --write`。
Rust 检查器覆盖源 SHA、token 边界、原断言完整保留、完整双向 UTF-8 映射、
1,296 个 fixtures 的六类分类，以及 accepted T01 的 10 个源文件/32 处断言映射。
源码 inventory 明确标记 `original_run=not_run`、`aside_parse=not_run` 和
`aside_execution=waiting_runtime`；原始运行、解析/分析、Aside 执行相互独立。

所有可再生成结果写到 `target/reel-artifacts/`。旧完整观测在
[清理前提交](https://github.com/jhq223/reel/tree/21e0ea09ddbd6c3926df72a6689183bb73b21598/tools/quirrel-compat/results)。
必要 ZIP/SHA 回归输入精简保存在 `tools/reel-dev/tests/fixtures/quirrel-observations`：
原 8 个 mismatch、一个通过 fixture 的 default/0/255 三次观测、可读失败字节及原 golden 引用。
完整运行包可用 `cargo run --locked -p reel-dev -- verify-results DIR --t01-baseline` 核验，
包含原种子选择、每个 returncode=0、runner_exit_code=1、8 mismatch、源/golden 未修改和前端哈希。

## 上游观测的独立复现

以下命令执行的是 Quirrel 上游测试，不是 Aside；只在确需重新采集时运行。
需要 CMake、C/C++ 编译器和 Python 3。构建沿用上游 Release `sq` 目标；
默认 int64/float32 与 Aside 的 int64/float64 目标差异保留，不改 golden 消除失败。

```sh
cmake -S third_party/quirrel -B target/reel-artifacts/quirrel-build -DCMAKE_BUILD_TYPE=Release
cmake --build target/reel-artifacts/quirrel-build --target sq --parallel 4
python3 tools/quirrel-compat/run_upstream.py \
  --sq target/reel-artifacts/quirrel-build/bin/sq \
  --out target/reel-artifacts/quirrel-results \
  --work target/reel-artifacts/quirrel-work
```

这是唯一保留的第一方 Python：为未修改的上游 `testRunner.py` 安装观测 hook，
保留原 12 worker、10 秒 timeout、seed 选择、比较和退出码；源完整性验证调用 Rust。
`--work` 必须不存在，`--out` 不能混有旧运行，二者都必须位于产物根目录。
该历史基线实际退出 1，8 个 golden 不匹配；不能把它报告成全部通过。
旧 fc418d9 外部前端工程/lock 和 Python wrapper 已退休；当前解析/分析使用工作区 Rust 测试。
首批运行宿主仍见 [HARNESS.md](HARNESS.md)，历史观察见 [REPORT.md](REPORT.md)。

## 转换和分类

`recipes.json` 是 **10 份固定 hash 的人工逐项审查清单**，不是全库自动转换器。
每个编辑写明原 UTF-8 字节范围、完整原文本、新文本和具体语法/绑定理由。
`tools/reel-dev/src/fixtures.rs` 的 Rust 扫描器 区分 token、字符串、注释和空白，禁止切开 token；只处理已审查的源文件。
它不宣称实现完整 Quirrel grammar。未匹配 hash、重叠范围、错误原文本或断言变化均直接失败。
没有全库 regex 替换；上游 runner 自身的 seed 选择 heuristic 原样保留。

已审查转换包括声明/闭包参数边界、对象字段、catch 绑定、完整语句分号，以及具体常量
for 循环。循环转换逐份检查更新顺序、return、continue 和捕获：只对本批中无相关冲突
的循环转换，不推导一般 C 风格 for 的语义。`local` 的共享可变捕获用 `let mut` 保留。
表内闭包不使用原 `this`，不添加隐式接收者。

`converted/**/*.aside` 保留来源路径/commit/许可证指针；同名 `.map.json` 覆盖全部
原文和生成文本。`convert.map_range` 可按任一方向查询：复制段精确偏移，编辑段保守映射
到整段人工编辑（多对多），生成注释对应原文零宽范围。每个原 assert 调用另有精确
原/新范围和不变 token 序列，禁止删除、重排或减弱。原 `.out` / `.diag.txt` / `.opt.txt`
仍在快照中，以 hash 引用，不重新生成。未来宿主契约见 [HARNESS.md](HARNESS.md)。

| 分类 | 使用条件 |
| --- | --- |
| `preserved` | 原 body 已符合 Aside 语法，逐字节保留，仅加来源注释 |
| `syntax_converted` | 有已审查的语法/绑定转换，原断言及输出 oracle 保留 |
| `harness_adapted` | 除语法转换外，有明确、受限的 API 适配（本批 rawin→has_own、非空 top→索引） |
| `deliberate_delta` | 已确定规范差异，原 oracle 保留；需要独立 Aside 正反例，尚未转换 |
| `compiler_specific` | Quirrel AST、诊断、静态分析、类型解析的专用 oracle；需建立 Aside 对应契约 |
| `blocked` | 尚未完成绑定/语义审查和显式 recipe，未转换；不暗示前端或运行时失败 |

这是一个主分类加差异 tag 的处理清单，**分类不是兼容率**。
`deliberate-deltas.json` 记录规范段落、实际原 fixture、保留方式和待实现的 Aside 原生用例。
tuple 没有对应原生 oracle，明确列为缺口；文本、逐调用缺省、每实例字段、显式 self、
强绑定、严格 bool、检查整数和普通表自身槽规则均保持规范，不追求抹平差异。

本次预算是上游 runner 的单次 10 秒超时和 12 worker；前端 probe 批次超时 30 秒。
本项没有扩大到 VM 性能、GC、快照、32 位或设备验收。
