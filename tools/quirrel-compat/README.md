# Quirrel 原始测试与 Aside 转换基线

Issue [#13](https://github.com/jhq223/reel/issues/13)，Aside 规范 1.1。
[实际结果](REPORT.md) · [逐 fixture CSV](results/inventory.csv) · [完整记录 JSON](results/inventory.json)

本项只修改 `third_party/quirrel/**`、`tools/quirrel-compat/**`。结果是可复现的原语言
oracle、逐 fixture 处理状态和首批可审查的转换。依赖固定 Quirrel
`19f17df9b6ee43c91ef5c635fd45970ec76c5dbe` 和隔离的 Aside 前端
`fc418d9a26ebf5f0dfdbde2d6c25670e78c94926`；未更改语言接口、作者指南或 VM。
没有 Cargo 工作区依赖、播放器链接或 CI/Actions 配置。

## 三种结果

| 列 | 实际做了什么 | 尚不能证明什么 |
| --- | --- | --- |
| `original_run` | 构建并执行上游 `sq`，按原 runner 比较原 golden；执行 fixture 内的原断言 | Aside 行为 |
| `aside_parse` | 真正调用固定提交的 `aside_compiler::parse`，保留诊断与映射 | 名称、绑定、类型、效果、lowering、执行 |
| `aside_execution` | 当前所有行均为 `waiting_runtime` | 完整执行器尚未实现，没有 Aside 语义通过项 |

## 复现

从仓库根目录运行。需要 Python 3、GCC/G++、Make、CMake 和 Rust 1.96.0。
本环境最初没有 CMake 和 Rust，安装在 `/tmp`，没有修改系统工具链：

```sh
python3 -m pip install --target /tmp/reel-build-tools cmake==3.31.6
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/reel-rustup-init.sh
CARGO_HOME=/tmp/reel-cargo RUSTUP_HOME=/tmp/reel-rustup \
  sh /tmp/reel-rustup-init.sh -y --no-modify-path --profile minimal --default-toolchain 1.96.0
export PATH="/tmp/reel-build-tools/cmake/data/bin:/tmp/reel-cargo/bin:$PATH"
export CARGO_HOME=/tmp/reel-cargo RUSTUP_HOME=/tmp/reel-rustup
```

构建沿用上游 `doc/build.py::build_sq` 的 Release `sq` 目标。
默认 `ENABLE_VAR_TRACE=OFF`、`SQUSEDOUBLE` 未定义，因此 int64/float32；**Aside 目标是 int64/float64**。
没有改编译选项以消除原测试失败。

```sh
cmake -S third_party/quirrel -B tools/quirrel-compat/build -DCMAKE_BUILD_TYPE=Release
cmake --build tools/quirrel-compat/build --target sq --parallel 4
python3 tools/quirrel-compat/run_upstream.py \
  --sq tools/quirrel-compat/build/bin/sq \
  --out /tmp/reel-compat-results \
  --work tools/quirrel-compat/work-repro
```

此基线的 runner **退出 1**，有 8 个真实 golden 不匹配，详情见报告。
`--work` 必须不存在；`--out` 不能包含既有 `upstream.json`，避免混入旧结果。
工具调用未修改的上游 `testRunner.py::main`：原 12 worker、10 秒单次 timeout、
`-optCH`、`--check-stack` / `-diag-file` / `-sa` / `-ast-dump` / `--parse-types` 和
默认/0/255 iteration seed 选择均保留。观察 hook 只记录命令、返回码、比较结果及每次输出，
不改选择或断言。runner 在副本运行，因为它可能自动创建缺失的 golden；这种运行记为
`unasserted`，不能算通过。本次没有创建或修改任何 golden。

`upstream.json` 索引 `upstream-artifacts.zip` 中每次运行的完整实际输出、进程 stdout/stderr、hash。
失败输出另外放在可读的 `failures/` 中。也保存了完全不挂观察 hook 的直接运行日志；
复现命令是在副本中运行 `python3 testRunner.py -sq <绝对 sq 路径> -wd <输出目录> -ci`。

首批转换和固定前端检查：

```sh
python3 tools/quirrel-compat/convert.py
git fetch origin codex/issue-8-aside-frontend
git worktree add --detach /tmp/reel-aside-frontend-repro fc418d9a26ebf5f0dfdbde2d6c25670e78c94926
python3 tools/quirrel-compat/check_frontend.py \
  --frontend /tmp/reel-aside-frontend-repro \
  --work tools/quirrel-compat/work-probe-repro \
  --out /tmp/reel-compat-results
python3 tools/quirrel-compat/inventory.py --results /tmp/reel-compat-results
```

probe 使用本目录的 Rust 调用代码和固定 `probe.lock`，`cargo build --release --locked`；
通过 path dependency 引用隔离工作区的原前端，不复制其实现，不改其文件。
工具拒绝错误 HEAD 或有改动的前端工作区。该提交来自 draft PR #9，未随远程分支移动。
probe 每份输入的 SHA-256、诊断、诊断对应原源码范围和执行等待状态见 `frontend.json`。

检查提交内的证据和生成物：

```sh
python3 tools/quirrel-compat/convert.py --check
python3 tools/quirrel-compat/inventory.py --check
python3 -m unittest discover -s tools/quirrel-compat -p 'test_*.py' -v
```

完整性检查重新读取实际输出，对照原 golden 和原 runner 的行比较规则；还验证所有
fixture/seed 都有记录、无丢弃断言、字面量边界、UTF-8 字节偏移和双向映射。
这是证据校验，不能算 Aside 执行测试。

## 转换和分类

`recipes.json` 是 **10 份固定 hash 的人工逐项审查清单**，不是全库自动转换器。
每个编辑写明原 UTF-8 字节范围、完整原文本、新文本和具体语法/绑定理由。
`lexical.py` 区分 token、字符串、注释和空白，禁止切开 token；只处理已审查的源文件。
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
