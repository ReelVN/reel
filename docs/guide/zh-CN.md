# Reel 游戏开发指南

超出设备纹理尺寸的大图可先执行 `reel prepare-image panorama.png --output panorama.reel-image`，再使用 `image("panorama.reel-image")`。发布时同时携带清单与相邻的 `.tiles` 目录。播放器按可见范围和预算内的邻接区域加载瓦片，移动和缩放复用已驻留像素。参见[格式与预处理合同](../prepared-images.md)。 使用宽幅原图和下文的字体配置运行[大图示例](../../examples/author_large_image.aside)。目前预处理需显式执行，build 不复制或准备图片。

设计版本 1.1 · 开发基线，接口实现状态见正文。

[English](en.md) · [文档目录 / Documentation index](../README.md)


编译与 ASB 加载默认不设置作品大小、累计记录、模块或指令数量配额；实际格式宽度和可用内存仍适用。嵌入宿主可通过 Rust Limits 显式设定配额。出现容量错误时，诊断会区分记录数和内存请求，避免把较长剧本误报为语法错误。
## 阅读这份文档
Reel 的目标是让你用 Aside 表达想象中的画面和交互，组合自己的对白、UI、演出和嵌入的 2D 游戏。当前已有多文件作品、统一舞台、存读档和打包发布链路；完整 2D 创作接口、长内容成本和设备覆盖仍在完善。先从[基本画面](../../examples/author_stage.aside)、[动画](../../examples/author_activities.aside)或[交互按钮](../../examples/author_buttons.aside)开始；[能力表](../README.md#当前状态)列出实际边界，[总清单 #94](https://github.com/jhq223/reel/issues/94)记录后续顺序。

这份指南从作者想要的画面出发，说明如何用 Aside 写剧情、组合 UI、安排演出和扩展效果。已实现部分可以直接使用；其余章节描述目标设计，并标明尚未接通的能力。中文与英文共用相同示例和接口名称。《Reel 引擎架构设计》说明运行时，《Aside VM 与语言设计》定义语言规则。
作者使用三个基本概念：函数组织可复用内容；节点树表达画面；活动表达随时间进行的变化。当前普通函数、基础节点组合与顺序剧情已经接通，活动及高级作者接口仍按各节状态区分。

当前可运行：text、image（PNG/JPEG）、row、column、stack、stage.replace，尾随 children 块与 if/for 组合，原节点属性修改、局部增删子树、有限动画及其等待和控制，以及 narrator/choose 的等待与继续。check/build 与播放器使用同一套已注册 Reel 接口；dev 运行源码，run 直接运行 .asb。完整可运行画面见[作者舞台示例](../../examples/author_stage.aside)，启动命令见第一节。

button 与可等待的 on_click 回调已接通，见文末可运行示例。model、bind/unbind、derive、动态 view 和作者对白／选择界面已可用；音频已接入作者源码，视频与存读档已接入同一舞台和执行状态；脚本初始化与多源码工程已接通；开发和打包发布使用下面的 export 命令。各节标明可运行示例及尚未实现的设计。更完整的能力边界见[当前实现状态](../README.md#当前状态)。

历史验证范围：R01/R02 的 Rust 渲染器与诊断窗口曾在软件 Vulkan、Xvfb/X11 上通过独立检查；这些记录只证明对应提交与环境。当前源码和 ASB 已接入真实窗口入口，旧诊断程序的范围不再代表作者入口的状态。物理 GPU、设备热插拔及平台生命周期未由这些检查证明。[渲染验证范围](../../crates/reel-render/R02-REPORT.md)。

## 一 最小项目与共同写法
`game.exit()` 请求关闭作品；接收后不再执行脚本，播放器完成任务和资源清理后退出窗口。可先等待淡出动画再调用。嵌入宿主接收同一请求并决定外层应用的行为。完整按钮示例见 [author_exit.aside](../../examples/author_exit.aside)，运行前提供 fonts/body.ttf。

从 main.aside 的 main 启动工程。check/build/dev 接受源码文件或工程目录，run 执行已经构建的 .asb。可运行多文件工程见 [author_project](../../examples/author_project/main.aside)。标题页可用 game.start 切换到全新剧情会话。
在 main 中调用 game.configure(title: "窗边", width: 960, height: 540) 设置标题与逻辑画布，参数可以使用普通表达式。省略项保留原值；未设置画布时沿用窗口空间，设置后按 contain 等比缩放并留边，绘制与输入使用同一换算。首设一个维度时，另一个维度采用 1920×1080 的对应值。桌面窗口默认匹配首次配置的画布尺寸；未配置画布时为 1280×720。Rust 宿主可用 StoryWindowOptions.logical_size 显式指定其他初始尺寸。首次应用画布后不再自动调整窗口，玩家改变窗口尺寸时仍按 contain 缩放。fonts 接受字体文件字符串或按优先顺序排列的数组，例如 game.configure(fonts: ["fonts/body.ttf", "fonts/cjk.ttf"])；路径相对资源根，作品须在首次呈现前提供至少一个字体文件。text 和 button 的 font 属性选择字体族或逗号分隔的回退列表，例如 text("标题", font: "Noto Serif SC, system-ui")，font_size 控制大小。字体文件共享加载；system-ui 表示配置的默认列表。播放器只使用作品提供的字体，不读取系统字体；未配置字体会报错。
Reel 自动提供当前已注册的基础节点与剧情服务：text、image、button、row、column、stack、stage、narrator 和 choose，每个源码文件都可直接使用。作者的模块声明、局部绑定和显式导入优先于这些默认名称。若定义了自己的 text 函数，可用 `use reel::prelude::text as reel_text;` 为 Reel 构造函数另取名称。显式 prelude 导入仍是有效的普通导入。这套默认环境由 Reel 提供，独立 Aside 不包含 Reel 名称。动作和完整业务服务仍按后文设计。作者用自己的函数构造页面和外观。所有文档统一使用 fn、let、let mut、|参数| 闭包和 use 导入。除完整模块与函数示例外，独立代码片段放在正在运行的函数内部，沿用前文变量。普通语句写分号，函数末尾表达式可直接返回；对象字面量写 {字段: 值}。以下导入使用已实现的模块组织；标题按钮中的 game.start 已实现。
```aside
use game::ui::title::title;

fn main() {
    game.configure(title: "窗边", width: 1920, height: 1080, fonts: ["fonts/body.ttf"]);
    stage.replace(title());
}
```
上例的 ui/title.aside 可以直接用作者自己的节点组成：
```aside
use game::story::opening::opening;

pub fn title() {
    column(x: 760, y: 480, gap: 24) {
        text("窗上的光");
        button("新游戏", on_click: |event| game.start(opening));
    }
}
```
story/opening.aside 中的 opening 见第三节。示例的相对文件位置与导入路径一致，根模块 main.aside 只负责启动。
let 固定变量绑定，节点属性仍可修改；需要把变量改绑到另一个值时使用 let mut。普通表达式在执行时求值一次。bind(model.value) 连接可观察属性，derive(|| expression) 连接由其他属性计算的只读值。语言的完整规则见第二份文档。

桌面播放器遇到可恢复的图形设备丢失会重建渲染设备并保留当前剧情状态；诊断包含后端给出的丢失原因。无法重建设备时报告错误并退出。

### 当前可用的检查、构建与运行
桌面播放器等首个挂载场景的字体与图片就绪后再呈现；支持隐藏窗口的平台在首帧接受后显示窗口。需要自定义加载界面时，先用普通舞台节点挂载它，再安排后续任务。启动期间的错误仍报告原始源码位置。

示例在 main 中配置 fonts/body.ttf。从仓库根目录运行下列 Windows 命令，将微软雅黑复制到临时资源目录供本机验证；发布作品时应提供有分发授权的字体。运行 dev 或 run 后关闭窗口，再继续下一条命令。

```powershell
New-Item -ItemType Directory -Force target/reel-artifacts/author-assets/fonts | Out-Null
Copy-Item C:/Windows/Fonts/msyh.ttc target/reel-artifacts/author-assets/fonts/body.ttf
Copy-Item -Recurse -Force examples/assets target/reel-artifacts/author-assets/
cargo build --locked -p reel-cli --features window
./target/debug/reel.exe check examples/author_stage.aside
./target/debug/reel.exe dev examples/author_stage.aside --assets target/reel-artifacts/author-assets
./target/debug/reel.exe build examples/author_stage.aside --output target/reel-artifacts/author-stage.asb --entry main::main --overwrite
./target/debug/reel.exe run target/reel-artifacts/author-stage.asb --assets target/reel-artifacts/author-assets
```

check 编译并报告源码错误，不运行剧情；加 `--format json` 可取得带 UTF-8 范围与行列位置的诊断。build 生成单个 .asb，包含字节码、模块初始化与选定入口。两者提供与播放器相同的默认 Reel 名称、已注册 native 与宿主默认参数。

dev 与 build 默认选择 main::main，可用 `--entry main::name` 选择其他函数。缺少入口或入口无效会在构建时报告；入口可以私有，但不能要求参数或捕获外层环境。run 直接加载包中的初始化函数和入口，不重新编译源码。已有输出只在指定 --overwrite 时替换。

build 默认使用 Debug，完整源码与映射保存在包内，运行错误能指向原来的 .aside 文件。发布时加 `--release`，只生成去除源码、指令映射和声明位置的 ASB，减少文件体积及加载后驻留的调试数据。两种模式的语言行为和运行检查相同；Release 仍报告错误类型、函数和指令位置，保留执行需要的符号与文本常量。这个开关与编译 Rust 播放器的 `cargo build --release` 分开；不会把剧情文本移到外部文件。现有执行存档对应实际 ASB，同一源码的 Debug 与 Release 存档不互换。

```powershell
./target/debug/reel.exe build examples/author_stage.aside --release --output target/reel-artifacts/author-stage-release.asb --overwrite
```

启用 window 后，源码与构建产物都使用真实窗口、文字布局和呈现输入路径。同步构造组件在同一运行时间片内连续推进，到等待或时间片结束才更新画面，不会每创建一个节点就单独显示一帧。对白或选择等待时会先完成当前 GC，再休眠等待输入；自然结束后窗口保留最后画面，关闭窗口退出并完成清理；Escape 作为返回输入交给当前 UI。

工程只读取入口实际导入的源码。game::ui::scene 对应入口目录下的 ui/scene.aside；--module first=../library 可提供外部源码根，同一目录的别名共享环境。函数循环引用允许链接，顶层读取未初始化绑定在实际读取处报 ModuleInitializationCycle。build 把完整模块图和入口放入同一 .asb，移动后无需原源码，初始化设置仍由同一段 main 执行。资源根默认为源码或 ASB 所在目录，也可用 --assets 指定；export 另外生成资源包与可搬移的 release.json 清单。

源码、完整源码定位文本和文字常量统一使用实际包预算。长文本和拼接按执行片推进，分片不会截断文字，也不规定台词长度。

左结合运算和属性、调用链按书写顺序编译，不按链条长度消耗嵌套额度。括号、代码块和子表达式的真正嵌套仍使用宿主配置的编译资源预算。

### 导出开发工程和发布目录

`reel export` 在同一次构建中组织代码和资源；默认导出开发工程，`--release` 导出发布目录。先按前面的示例准备字体，再执行：

```powershell
./target/debug/reel.exe export examples/author_stage.aside --output target/reel-artifacts/development --resource assets=examples/assets --resource fonts=target/reel-artifacts/author-assets/fonts
./target/debug/reel.exe export examples/author_stage.aside --release --output target/reel-artifacts/release --resource assets=examples/assets --resource fonts=target/reel-artifacts/author-assets/fonts
./target/debug/reel.exe run target/reel-artifacts/release/release.json
./target/debug/reel.exe dev target/reel-artifacts/development/release.json
```

开发目录包含可修改的 `script/`、散资源、带源码诊断的 `game.asb` 和 `release.json`；可以直接 run，也可以 dev 重新编译。发布目录包含 `game.asb`、`game.rtp`、`resources.rpk` 和 `release.json`，另外携带对应平台播放器即可分发。复制整个目录后仍使用相同命令，不依赖构建机绝对路径。

`--resource prefix=directory` 把一个目录映射到脚本中的逻辑路径；可重复声明。`--text locales=build/locales` 将已合并的翻译目录放入文本包，脚本仍使用 `localization.load("en", "locales/en.ftl")`。开发导出复制这些文件，发布导出将它们打包。翻译源文件可继续按剧情和 UI 模块组织，先用 `reel loc bundle` 生成运行目录。

资源包默认按 2 GiB 目标体积自动分卷，包头和索引计入体积；可在发布命令添加 `--pack-bytes 1073741824`，改为 1 GiB。分卷沿完整资源边界进行，单个大资源独占一卷；后续文件名为 `resources-002.rpk` 等，清单自动列出，脚本路径无需修改。挂载只读取索引，资源内容按范围读取。

发布时原文常量移入 `game.rtp`，ASB 不携带完整源码。池目前整体驻留，字符串按使用共享引用；不是启动就把所有正文复制成永久 VM 对象。修改译文无需重编代码，但原文池必须与 ASB 对应。图片、音频和视频按资源包索引读取，不整体解包。

发布默认关闭散文件覆盖；需要模组或调试覆盖时显式 `reel run release.json --assets overrides`。清单里的包按顺序后者覆盖前者，散文件优先。`--overwrite` 会替换整个既有导出目录，旧存档与额外文件不复制到新导出；运行数据应单独保存。导出目录不能放在输入源码或资源目录里面。

### 已接通的范围、容器与插值
在函数里可以用范围循环生成内容，再用插值展示结果。范围不会先创建一个大数组；`..` 不含终点，`..=` 包含终点。数组支持 `append`、`pop` 和 `len`，表支持 `has_own`、`get` 和 `len`。插值当前支持字符串、数字、布尔值和 null。
```aside
let values = [];
for value in 1..=3 { values.append(value); }
let latest = values.pop();
text(f"剩余 {values.len()} 项，最后一项 {latest}");
```

`table.get(key, default: null)` 仅在键缺失时使用缺省，已有的 null、false、0 都保持原值，不添加字段；返回的对象仍是原对象。default 是普通实参，会照常求值。它也可用于 derive，随后插入或修改字段会更新结果。完整用法见 [collections.aside](../../examples/collections.aside)。

```aside
let settings = {volume: 0, wallpaper: null};
let volume = settings.get("volume", default: 0.7);
let wallpaper = settings.get("wallpaper", default: "default.png");
let language = settings.get("language", default: "zh-CN");
```

`array.sort(compare: null)` 稳定原地排序并返回原数组。缺省按数值（可混合 int/float）或字符串排序；记录使用返回负数、零、正数 int 的比较器。同序元素保留原来的顺序。比较器不能等待。排序按 VM 执行预算推进，成功后才提交；异常、取消或来源数组被修改时不发布工作副本。见 [collections.aside](../../examples/collections.aside)。

```aside
let cuts = [180, 20, 90];
cuts.sort();
let captions = [{text: "后景", layer: 2}, {text: "前景", layer: 1}];
captions.sort(compare: |a, b| a.layer - b.layer);
```

数组展开 `[...items, last]` 接受数组或 tuple；表展开 `{...base, color: value}` 复制自有字段，后项覆盖前项。展开按书写顺序捕获内容，后面的代码修改来源不会改变已捕获的元素或字段；内部对象仍共享引用。null 不能展开。完整可运行示例见 [collections.aside](../../examples/collections.aside)。

```aside
let base = {message: "窗上的光", x: 32, y: 48};
let opening = [base];
let captions = [...opening, {...base, message: "第二句", y: 90}];
for caption in captions {
    text(caption.message, x: caption.x, y: caption.y);
}
```

## 二 让代码表达整幅画面
### 组织组件数据

剧本 VM 默认不设固定的内存字节总额，对象和根存储按实际需求增长，不再设 16,384 个对象或 1,024 个根的默认门槛；嵌入引擎的宿主可按运行环境显式设置预算。增量 GC 会释放空对象存储页，不再保留峰值数量的完整对象槽；较小的身份索引和工作队列仍可复用。长操作会按执行切片让出，避免阻塞画面。

固定长度 tuple 可以在 let 和 for 中解构，支持嵌套和 `_` 忽略项；先准备所有值再建立名称，结构不符抛出 PatternMismatch。每轮循环的闭包保留当轮值。数组/对象/rest 解构及解构赋值仍待实现。见 [tuple_bindings.aside](../../examples/tuple_bindings.aside)。

```aside
let settings = {language: "zh-CN", volume: 0.8};
for (key, value) in settings.entries() {
    let pair = (key, value);
    let (name, _) = pair;
}
```

`match` 可以按值选择对白、界面或处理逻辑，支持字面值、名称、`_`、OR 和类型测试，guard 写在 `if` 后。只执行第一个匹配且 guard 为真的分支，分支末尾表达式提供结果；没有匹配时抛出 MatchError。结构解构尚未支持。完整例子见 [match_values.aside](../../examples/match_values.aside)。

```aside
let caption = match mode {
    "online" | "typing" => "在线",
    name is string if name != "" => name,
    _ => "离线",
};
```

数组支持插入、删除、扩缩、反转、扩展、清空和交换，tuple/array 可按范围切片。表的 keys/values/entries 返回自有字段的新数组；entries 每项是 `(key, value)`。批量编辑在成功时一次生效，原有数组引用保持有效；resize 的填充值在新增位置共享，不自动深复制。
完整可运行示例见 [collection_editing.aside](../../examples/collection_editing.aside)。insert/remove 使用从零开始的非负索引；slice 接受 `begin..end` 或 `begin..=last`，不自动截断越界范围。remove 返回删除项，top 返回末项，其余修改方法返回原容器。

```aside
let rows = ["序章", "第一章"];
rows.insert(index: 1, value: "幕间");
rows.extend(["第二章"]);
let first = rows.slice(0..2);
rows.reverse();
let theme = {title: "窗上的光", color: "#ffffff"};
let names = theme.keys();
let caption = " · ".join(rows);
let opening = caption.slice_scalars(0..2);
let has_opening = caption.starts_with(prefix: "第二章");
```

文本可用 byte_len 取得 UTF-8 字节数、scalar_count 取得 Unicode 标量数。slice_bytes 和 slice_scalars 使用显式范围，越界或截断 UTF-8 标量会报错；组合字符和 emoji 可能包含多个标量，标量切片不代表字素切片。starts_with/ends_with 精确匹配前后缀，空字符串总是匹配。`separator.join(items)` 连接字符串数组或 tuple，空集合得到空字符串，其他类型不会自动转成文字。长文本按执行预算推进，连接过程中修改来源数组会报错。完整示例见 [text_operations.aside](../../examples/text_operations.aside)。

`text.split(separator)` 按完整分隔字符串切分并返回字符串数组，保留首尾和连续分隔产生的空项。空分隔字符串按 Unicode 标量切分，仍保留首尾空项。例如 `"/角色//纱世里/".split("/")` 得到 `["", "角色", "", "纱世里", ""]`。扫描和片段复制按执行预算让出，不会先复制整段输入。

to_ascii_lowercase()/to_ascii_uppercase() 只转换 ASCII 字母，保留中文等其他文字。例如 "Emily你好".to_ascii_lowercase() 得到 "emily你好"，可用于 ASCII 标识或账号的比较。

`trim()/trim_start()/trim_end()` 移除两端、开头或末尾的 Unicode 空白，保留名字或正文内部的空格。例如 `let player_name = "\u{3000} 空 山 \u{a0}".trim();` 得到 `"空 山"`；全为空白时得到空字符串。参见 [text_operations.aside](../../examples/text_operations.aside)。

已注册的函数也可以保存和传递，例如 `let say = narrator; say("你好");`，等待行为与直接调用一致；函数缺省参数也可以是 `text` 等构造函数。可以用 `(int,int)` 约束坐标，用 `(int,int)|null` 表达可省略的坐标；`is`、参数、返回值和变量赋值都会检查相应类型，失败赋值保留旧值。完整可运行示例见 [function_values.aside](../../examples/function_values.aside)，使用前面的 `dev` 或 `build` 命令替换输入路径即可。

带尾随子节点块的组件调用作为独立语句时可省略结尾分号；块后换行结束该语句，下一行括号表达式不会被接到前一个调用上。普通调用和属性赋值仍使用分号。显式分号不改变子节点收集。

已实现：text、image、row、column、stack 与 stage.replace 使用同一节点模型。普通函数可以返回节点；尾随 children 块、if 与 array/tuple 上的 for 可以组合节点。value、x/y、width/height、color/background、opacity、visible 和 clip 等属性直接修改原节点，挂载后的身份保持不变。替换舞台会退休旧树，仍持有的旧节点不能继续读写，也不会因此保活整棵旧树。完整代码见[作者舞台示例](../../examples/author_stage.aside)。

stack 按书写顺序从后往前叠放子节点，row 横排，column 竖排。未指定宽高时按内容布局；x/y 在布局位置上叠加，逻辑坐标原点在左上，x 向右、y 向下。默认字体大小为 24、文字白色、背景透明，gap/padding 为 0；clip 限制容器的可见区域。`scroll(height: 360) { ... }` 创建纵向滚动区域；axis 可设为 "horizontal" 或 "both"。滚轮、触摸拖动和键盘/手柄焦点跟随共用正常输入；滚动不会重建文字布局。`follow_end: true` 使已处于末端的 NVL 页跟随新内容，上滚阅读后保留位置。

用共同属性 `stretch: "both"` 让背景图片跟随内容撑开的容器，无需计算文字行数或回写高度。`"x"`、`"y"` 只填满父 padding 内的对应轴，默认 `"none"`。被拉伸轴不参与父尺寸测量，并覆盖该轴的 width/height；x/y、anchor 仍有效。在 row/column 流轴上拉伸时，节点成为覆盖层，不增加位置或 gap；scroll 中按视口内框拉伸，不扩大该轴的滚动范围。

```aside
stack(max_width: 420) {
    image("images/panel.png", stretch: "both", slice: 12);
    column(padding: 24, gap: 16) {
        text("The frame follows wrapped text and buttons.", align_x: 0.5, text_align: "center");
        button("Continue", align_x: 0.5, on_click: |event| game.advance());
    }
}
```

完整示例：[author_stretch.aside](../../examples/author_stretch.aside)。准备 `fonts/body.ttf` 和 `images/panel.png`，运行 `reel dev examples/author_stretch.aside`；按钮切换内容时，背景保持身份并跟随新高度。

`max_width/max_height` 限制节点的自然或显式尺寸，默认 null；长文本会在可用宽度内换行，短文本仍按内容收缩。上限含 padding，stretch 对应轴仍填满父内框。`align_x/align_y` 默认 0，取 0 到 1：0 为起端，0.5 居中，1 为末端，按父 padding 内的剩余空间定位；stack 两轴生效，row/column 只在非流轴生效，scroll 只在非滚动轴生效。它们不改变 x/y 与 anchor 的变换含义，可绑定或动画。示例 [author_stretch.aside](../../examples/author_stretch.aside) 的弹窗随内容改变宽高，背景伸展、按钮保持居中。

`view` 的内容按 stack 布局，可同时使用 `align_x/align_y`。input 可以嵌入普通容器；没有焦点时，最前方可见且启用的内层 input 接收键盘和手柄输入，模态面板和已聚焦编辑框仍优先。调整 `game.configure(text_speed:)` 不会中断正在拖动的控件。窗口、画布或父节点放大文字时，WGPU 按实际显示倍率生成并缓存更高分辨率字形，不改变逻辑排版。

### 分层图片

图片的 `color` 用于乘色：`image("assets/panels.png", color: "#ffaaaa")` 让原图偏红，白色保持原样。透明度同时受颜色 alpha 与 opacity 控制。普通图片、分块大图及每个分层描述使用相同规则；给容器设置 color 不会改变子层。使用 bind/derive 调整悬浮等状态的颜色，无需为每种颜色准备或上传一份图片。

`image("assets/pattern.png", width: 1280, height: 720, repeat: "both", tile_scale: 1)` 以图片原始像素大小平铺。repeat 可选 none、x、y、both；重复时不使用 fit，不能同时指定 slice，也不能使用分块 `.reel-image`。tile_scale 必须为正数。`corner_colors: ["#ffffff", "#ffffff", "#ffaaaa", "#ffaaaa"]` 为左上、右上、右下、左下颜色，在节点内容框内双线性渐变并乘以 color；可与普通、九宫格或分块图片组合，不复制纹理。

完整示例：[author_image_patterns.aside](../../examples/author_image_patterns.aside)。准备 `fonts/body.ttf` 和 `images/pattern.png` 后运行 `reel dev examples/author_image_patterns.aside`；切换纹理比例复用同一张图片。

图片本身不接收指针输入，作为轨道、滑块或其他控件皮肤时不会挡住下方控件。需要图片可点击或拖动时，将它放入 button 或 input，由容器处理鼠标和触摸。

Rust 宿主可用 `ImageSource::empty()` 表示尚未到达的画面，再以 `ImageSource::resource(lease)` 更新同一个 image 节点。直接资源保留普通图片的 fit、裁剪、变换、颜色和透明度；同尺寸更新只重绘，不重新布局。`ResourceRegistry::register_yuv420` 接收生产者持有的 8 位 YUV420 平面及 BT.601/709、范围和传递函数，不复制为 RGBA。`release(handle)` 放弃注册表持有权后，已捕获画面仍保留其租约；播放时钟和解码由宿主负责。可运行的 CPU 合同检查：`cargo test -p reel-player --lib decoded_frames_preserve`。

`layered_image` 将图片描述组合成一个普通舞台节点；模型依赖变化时才重新计算。数组位置是稳定图层槽，按从后向前的顺序叠放；null 隐藏槽内原有图片，缩短数组会退休尾部槽。每份描述可使用 source、位置、尺寸、fit、color、opacity 等图片属性；省略的属性恢复 image 默认值。描述中的 bind/derive 值作为读取依赖。未变化的属性保持不动，图片节点复用，因此切换表情不会重建身体图层。

例如 `layered_image(|| [body, faces[state.face]], width: 320, height: 180)` 用 state.face 选择前景描述，同时共享 body 描述；给 state.face 赋值即可切换表情。

整体位置和动画直接作用于返回节点，选中图层和依赖沿用普通执行快照。姿势名称、服装与表情组合仍是游戏数据，图层和姿势没有固定数量限制；每帧不会额外合成图片。完整示例见 [author_layered_image.aside](../../examples/author_layered_image.aside)，同其他图片示例一样需提供 fonts/body.ttf。

### 随机选择

`let rng = random();` 创建一个独立随机流；`rng.int(0, 10)` 返回 0 到 9 的整数，`rng.float()` 返回大于等于 0、小于 1 的小数，`rng.pick(items)` 从非空数组或 tuple 返回一个原元素。`random(seed: 42)` 可复现同一调用序列，保存 `let pick = rng.pick` 仍使用原随机流。先在事件或剧情中抽取，再把结果写入 model；不要在 derive 中抽取。抽取失败不推进状态。示例见 [author_random.aside](../../examples/author_random.aside)。

`use std::math::{sin, cos};` 后可用 `cos(angle) * speed` 和 `sin(angle) * speed` 计算二维速度；angle 使用弧度。还提供 tan/asin/acos/atan、atan2(y,x)、sqrt/hypot(x,y)、pow(base,exponent)、exp/ln/log2/log10、abs/floor/ceil/round、radians/degrees。它们是可用于 derive 的纯 float64 运算，不创建脚本对象；round 中点远离零，无效数学定义域按浮点规则得到 NaN，溢出可得到 Infinity。示例见 [author_math.aside](../../examples/author_math.aside)。

`random` 来自独立的 Aside 标准库，Reel 自动导入，也支持显式 `use std::random;`。在 main 或事件函数中创建随机流；模块初始化与 derive 沿用现有副作用约束。固定种子的复现范围为同一标准库版本。

### 用类组织有行为的数据

`class` 可以把角色、词条等数据的字段、类型和方法放在一起。`let` 字段只初始化一次，`let mut` 字段允许修改；字段初值按实例分别求值。构造方法写 `fn init(self, ...)`，也支持命名参数和缺省值。参数注解可以引用本地或导入的类，派生实例满足基类注解。

```aside
class PoemWord {
    let text: string;
    let score: int;

    fn init(self, text: string, score: int = 1) {
        self.text = text;
        self.score = score;
    }

    fn caption(self) -> string {
        f"{self.text} · {self.score}"
    }
}

fn word_label(word: PoemWord) {
    text(word.caption())
}

let word = PoemWord("星光", score: 3);
let caption = word.caption;
```

`caption()` 保留 `word` 接收者，适合传给回调。继承写 `class Child: Parent`，覆盖方法用 `super.method()` 调用词法基类；派生类未声明 `init` 时继承基类构造，声明后可显式调用一次 `super.init(...)`。`static let` 和 `static fn` 属于类。构造结束必须完成所有字段初始化，失败或取消后，已发布的实例引用与方法均报告 ConstructionFailed。类的字段布局固定，派生类通过方法扩展行为。可运行示例见 [author_classes.aside](../../examples/author_classes.aside)。

`let state = model(MyState());` 复制已经构造好的实例数据，保留其类型、方法以及内部共享引用，不重新执行构造。使用 `bind(state.field)` 连接控件，使用 `derive(|| state.caption())` 组合显示内容；方法修改字段会更新对应视图。只读字段只能单向读取，可写字段的控件回写仍须符合类型。后续赋入的实例保留共享身份。默认数据中的闭包和资源仍不能复制，静态字段也不属于实例状态。示例见 [author_class_models.aside](../../examples/author_class_models.aside)。

### 在运行中修改组件

需要让演出从资源就绪后的画面开始，可先挂载节点，再 `await stage.presented(owner: node)`，之后启动动画。等待包含派生布局和图片、字体准备，以实际接受的帧为准；其他任务、音乐和动画继续运行。返回对象可重复等待，`.cancel()` 取消该请求，owner 节点退休也会取消。省略 owner 时沿用当前回调/任务节点，主剧情中归会话。见 [author_scene_entry.aside](../../examples/author_scene_entry.aside)：

```aside
stage.replace(scene);
await stage.presented(owner: scene);
await run(tween(scene, opacity: 1, duration: 0.5));
```

子节点块中可以先用局部变量构造控件，再添加依赖控件状态的外观，最后显式提供控件本身。add 返回的后代已归该控件所有，不会在外层再次挂载；显式 children 数组也遵循同一规则。不能重复提供同一节点，或借用另一个未提供的组件及已挂载舞台的子节点。

没有普通参数时可写 `column { ... }`，省去空的 `()`；普通函数组件也适用。

```aside
column(gap: 12) {
    let control = button(width: 240, height: 48, decorated: false);
    control.add(text("Hover me", color: derive(|| if control.hover { "#ffd580" } else { "#ffffff" })));
    control;
    text("Another component");
}
```

节点保持原身份、父级和回调所有权。完整示例见 [author_composition.aside](../../examples/author_composition.aside)。

滚动区域的 `scroll_x/scroll_y` 可读写或 bind；布局会把位置限制在实际内容范围。`viewport_width/viewport_height/content_width/content_height` 是只读可观察尺寸，可在 derive 中计算自定义滑块的位置与长度。`content_align: "end"` 让不足一屏的聊天记录从底部排列，`"center"` 居中，默认 `"start"`。`input(on_pointer: ...)` 提供 `{target,x,y,phase}`，x/y 是呈现节点的局部坐标，phase 为 pressed/moved/released/cancelled；鼠标主键或触摸按下后捕获拖动，失焦、隐藏和禁用会取消。回调可等待并归输入节点所有。尚未开始的同节点、同处理器连续 moved 会合并为最新坐标，避免鼠标输入积压；已经开始的回调参数保持稳定，按下、释放和取消各自交付。参见 [自定义滚动条](../../examples/author_scrollbar.aside)。

`container.add(child)` 将子树追加到 row、column 或 stack，返回原 child；`index:` 可指定从零开始的插入位置，允许等于当前子节点数，省略或传 null 表示末尾。子树必须尚未挂到舞台且没有父级。重复挂载、成环、向 text/image 添加或位置越界都会报错，原树保持不变。添加和删除保留其他节点及其控件身份，未改变的文字不重新整形。

`container.move(child, index: ...)` 调整已有直属子节点的顺序，无须先移除。index 必填，表示调整后的零起始位置；三个子节点的合法位置为 0、1、2。方法返回原 child，移到原位置不会改变场景。节点的属性、正在运行的动画、回调、焦点和已加载资源继续保留。stack 按新顺序从后向前叠放，row/column 按新顺序布局。非直属子节点或越界索引会报错，原树保持不变。

```aside
let back = image("assets/panels.png");
let front = image("assets/sun.jpg", x: 120);
let scene = stack { back; front; };
stage.replace(scene);
scene.move(back, index: 1);
```

完整的 [author_reorder.aside](../../examples/author_reorder.aside) 展示重叠卡片置前，同时保留正在运行的动画。不支持跨父级移动。

`node.remove()` 移除并退休该节点及其全部后代，也可用于舞台根；`container.clear()` 只退休子节点，保留容器自身的属性和身份。两者返回 null。退休使相关动画取消并释放舞台对图片的请求；旧节点不能重新挂载或继续读写。需要再次展示的组件应由函数重新构造。方法可以保存和传递，始终保留原接收者；已保存的 remove 可重复调用。

```aside
fn main() {
    let cards = row(x: 40, y: 60, gap: 20);
    stage.replace(cards);
    let card = cards.add(column(padding: 16, background: "#20304c") {
        text("A reusable component");
    });
    let dismiss = card.remove;
    narrator("The component has been added.");
    cards.add(text("Inserted first"), index: 0);
    dismiss();
    narrator("The same container now has different children.");
    cards.clear();
    cards.add(text("Start again"));
}
```

完整示例 [author_dynamic_stage.aside](../../examples/author_dynamic_stage.aside) 结合组件函数、局部编辑和淡入动画，可直接使用第一节的 dev/build/run 命令。当前接口负责插入和退休；跨父级移动尚未实现。

用 `image(source, fit: "contain", ...)` 将 PNG/JPEG 文件放到舞台。省略 width/height 时，图片的像素尺寸作为逻辑尺寸；只指定一边时保持宽高比。指定两边后，contain 完整显示并留空，cover 填满并裁剪，stretch 拉伸填满。图片也支持共同的位置、可见性、透明度和 padding 属性。可以直接修改原节点的 source 和 fit。

面板、按钮底图等可用 `slice: 24` 开启九宫格，或用 `slice: [24, 16, 24, 16]` 分别指定左、上、右、下边距。边距以源图像像素计，`slice_scale` 默认 1，用于调整目标边宽；数值均须有限且非负。九宫格填满内容框，角保持比例，边和中心拉伸；框过小或源边距超出图片时四边统一按比例收缩，零面积切片不绘制。`slice: null` 恢复普通 fit。两项属性可直接赋值或绑定，也可写入 layered_image 的图层描述。使用完整 PNG/JPEG 或图片 Bytes，分块 .reel-image 不适用于九宫格；多个切片共享图片，不生成或上传额外像素。

`slice_center: false` 让九宫格只绘边框，中心保持透明；默认 true。例如 `image("assets/frame.png", width: 320, height: 180, slice: 10, slice_center: false)`。

```aside
fn main() {
    let picture = image("assets/panels.png", width: 320, height: 180);
    stage.replace(column(gap: 16, padding: 24) {
        picture;
        image("assets/panels.png", width: 160, height: 160, fit: "cover");
        image("assets/panels.png", width: 480, height: 80,
            slice: [24, 16, 24, 16], slice_scale: 1);
    });
    narrator("The same resource can appear in different frames.");
    picture.source = "assets/sun.jpg";
    picture.fit = "cover";
    narrator("The original node now displays the new image.");
}
```

完整的 [author_images.aside](../../examples/author_images.aside) 随附所需图片，可用前面的 dev 命令运行。dev 的资源路径相对于源码文件目录，run 相对于 ASB 文件目录；`--assets <directory>` 可另选资源根目录。例如构建到 target/reel-artifacts 后，运行时加 `--assets examples`。构建 ASB 不会复制图片文件。

图片与字体共用资源加载服务，文件读取和图片解码在后台执行，同一路径的图片节点共享像素。新场景所需图片与配置字体全部就绪前保留上一完整画面，并暂停剧情输入；换源也保留旧图片直至新资源就绪。文件缺失或损坏会报告文件路径及 image 构造位置（字体对应 game.configure），并结束本次窗口运行。属性类型错误和非法 fit 可在赋值处捕获。图片挂载并且祖先均可见时才读取文件；构造未挂载组件或 visible=false 子树不会产生图片 IO。已加载的隐藏图片可在仍挂载时复用，有新图片加载需求时让出闲置缓存；卸载组件释放其缓存，上一完整画面与在途帧继续持有各自租约。opacity=0 仍准备图片，可用于资源就绪后淡入。会话内文件热重载尚未实现。

所有基础节点都支持 rotation、scale_x/scale_y 和 anchor_x/anchor_y，容器的变换作用于整个组件。rotation 是顺时针角度，默认 0；scale_x/scale_y 默认 1，负数可翻转，0 折叠画面且不参与命中。anchor_x/anchor_y 按布局宽高取比例，默认 (0,0) 为左上角，(0.5,0.5) 为中心；也可放到组件外。x/y 指定锚点相对容器排布位置的偏移。先围绕锚点缩放，再旋转，再放到该位置；父容器的变换继续作用于结果。宽高和 row/column 排布保持变换前的尺寸。裁剪与呈现几何使用相同变换，属性修改保留原节点；移动、旋转、缩放、锚点和透明度编辑不会重新排版。


```aside
fn main() {
    let card = column(x: 320, y: 220, anchor_x: 0.5, anchor_y: 0.5,
                     rotation: -12, scale_x: 1.25, scale_y: 1.25,
                     gap: 12, padding: 16, background: "#183048") {
        image("assets/panels.png");
        text("A scene is one component");
    };
    stage.replace(card);
    narrator("The entire component shares one transform.");
    card.rotation = 12;
    card.scale_x = -1.25;
    narrator("The same component is now mirrored.");
}
```

完整可运行示例见 [author_transforms.aside](../../examples/author_transforms.aside)，资源和运行方法与图片示例相同。

### 相机取景

使用 `camera { ... }` 对场景平移取景、缩放和旋转。`camera_x/camera_y` 是显示在视口中心的世界坐标，默认 (0,0)；左上角为原点的场景应显式指定场景中心。`zoom` 默认 1，必须为正数。`camera_rotation` 是相机顺时针角度，场景沿反方向旋转。普通 `x/y`、`rotation`、scale 和 anchor 定位整个视口。width/height 指定视口尺寸，省略时采用内容布局。子节点像 stack 一样叠放，padding 保留在视口坐标。`clip` 默认 true，可显式关闭。

```aside
let view = camera(width: 960, height: 540, camera_x: 480, camera_y: 270) {
    image("assets/panels.png", width: 960, height: 540);
    text("Look here", x: 620, y: 160);
};
stage.replace(view);
await stage.presented(owner: view);
await run(tween(view, camera_x: 660, camera_y: 210, zoom: 1.8,
                camera_rotation: 8, duration: 1.2, easing: "in_out"));
```

四个取景属性支持赋值、bind/derive 和 tween，沿用活动的取消和所有权规则。把 HUD 作为相机外的兄弟节点即可保持固定；相机也能嵌套组合。移动不会重新排版文字或重新加载图片；绘制与指针坐标共用已接受帧的变换和裁剪。完整的 [author_camera.aside](../../examples/author_camera.aside) 仅需提供 `fonts/body.ttf`，沿用第一节 dev/build/run 命令。这里是二维相机，尚未实现透视投影。



画面的节点、绘制记录和嵌套层级随实际内容增长。输入路径在新画面发布前扩容，已有按下、拖动和释放继续对应正确的节点。实际内存预算与 GPU 纹理尺寸由运行宿主和设备管理。
以下房间示例是完整画面的设计目标，尚不能直接运行：它依赖未接通的 game:: 模块和自定义对话视图。背景最底，立绘、CG 和作者对话 UI 依次叠放；函数把剧情需要操作的部分命名返回。
```aside
use game::ui::dialogue::dialogue_view;

pub fn room_view(dialogue) {
    let portrait = image("assets/mio.png", x: 220, y: 140);
    let cg = image("assets/window-cg.png", opacity: 0);
    let hud = dialogue_view(dialogue);
    let picture = stack() {
        image("assets/room.png");
        portrait;
        cg;
    };
    let root = stack(width: 1920, height: 1080) {
        picture;
        hud;
    };
    return {root: root, picture: picture,
             portrait: portrait, cg: cg, hud: hud};
}
```
尾随 children 块立即构造子节点列表。块内返回节点的表达式按顺序加入，let 声明只保存变量，单独写 portrait 才将它加入当前父级。普通 if 与 for 可以组织这份列表；其中的条件在构造时计算一次。父级接收一个节点后拥有它，节点始终只有一个父级。
room_view 只构造画面。stage.replace(view.root) 才把整幅画面挂到舞台，并释放旧画面。函数返回后舞台继续拥有它。作者自定义组件也接收 children，能力与内建组件相同。
```aside
fn card(children = []) {
    column(gap: 12, padding: 24) {
        for child in children { child; }
    }
}
```
props、布局与绘制方式属于节点，交互行为属于控件。作者可以把立绘放进按钮、把按钮放进画面组、让整个设置页一起旋转或淡出。层级规则由共同节点树决定。覆盖层由作者在根节点中明确安排。

保留组件内任一节点引用，也会保留其未挂载父级组件；GC 不会让仍在使用的节点突然失效。属性类型、颜色和数值范围在赋值时检查，错误可在同一段 try/catch 捕获，失败保留旧值。配置的字体列表按顺序参与缺字回退，包括中文。

## 三 剧情是普通的顺序代码

narrator、character、say 和 choose 支持顺序等待并继续。`let mio = character("Mio", id: "mio");` 后直接写 `mio("你好。");`；id 省略时采用名字，显式 id 可区分同名角色。角色不可修改，`.line(text)` 创建可复用的行描述，`say([mio.line("你好。"), eri.line("晚上好。")])` 同时显示一组 ADV 对白，共用一次揭示和推进。历史保留每行提交时的身份、名字和全文。完整示例：[author_dialogue.aside](../../examples/author_dialogue.aside)。

`game.configure(text_speed: 30)` 设置每秒显示的 Unicode 字素数，默认 30；0 立即显示全文，省略保留原值。普通推进先补全文字，再结束本句；角色、旁白和行组使用相同规则。挂起、资源等待和输入配对期间暂停揭示，不在恢复时突发补字。Rust 宿主与窗口共用播放逻辑。

可绑定对白、作者 ADV/NVL 视图和自定义选择已接通，见 [author_narrative.aside](../../examples/author_narrative.aside)。语音与章节启动仍待实现；say 提交共享推进的行组，布局由作者视图决定。

对白行数、选项数量、单次交互文字和会话历史按内容增长。历史保留整个会话的记录；它不会因达到展示缓存容量而中断剧情或丢弃旧记录。实际内存不足会报告错误。

角色在模块中定义一次。每句对白调用发言者对象，旁白调用 narrator。独立字符串仍是普通字符串。角色调用更新对话数据和历史，并等待当前句完成，普通函数调用沿同一执行顺序继续。

`history` 决定这一句如何记录：默认 true 保存显示全文，false 省略，字符串或 rich 值保存替代文本；角色调用、narrator 和 .line 都支持。它只影响历史，画面、推进与已读判断仍使用原对白。适合故障演出、隐藏旁白和回看时的完整句子；替代文本可照常调用 tr，提交后固定语言。混合行组只记录指定行，整组省略时不生成空历史项。示例见 [author_dialogue.aside](../../examples/author_dialogue.aside)。
```aside
mio("……", history: "I remember the rain.");
narrator("A fleeting thought.", history: false);
say([mio.line("Noise.", history: false), eri.line("Stay.", history: rich("[b]Stay.[/b]"))]);
```

只记录后台语音时，调用 `game.record("The train is arriving.", speaker: mio)`；省略 speaker 表示旁白。它立即返回，不改变正文或当前选项、不等待点击，也不标记已读。普通任务和控件回调同样可用；音频由 audio.play 独立控制。记录与其他历史一起保存，作者的历史界面可直接展示。
```aside
use game::ui::room::room_view;

let mio = character("Mio");

pub fn opening() {
    let view = room_view(game.dialogue());
    stage.replace(view.root);
    mio("先别开灯。看玻璃上那条光。");
    narrator("雨水往下落，光却停在原处。");
    show_cg(view);
    mio("你也看见了吗？");
}
```
文字逐字显示时，第一次推进完成当前文字；下一次推进结束当前句。按钮、滑条等控件消费过的事件留在控件内。剧情推进使用新的输入序号，同一次点击始终只产生一次推进。
角色描述身份与发言参数，对话框由作者的组件绘制。最简单的文字布局如下；背景图片、姓名框与快捷按钮可以照常加入节点树。
```aside
pub fn dialogue_view(model) {
    column(gap: 12) {
        text(value: bind(model.speaker_name),
             visible: derive(|| model.speaker_name != ""));
        text(value: bind(model.text), reveal: bind(model.reveal));
    }
}
pub fn nvl_view(model) {
    scroll(height: 600, follow_end: true) {
        view(|| column(gap: 24) {
            for line in model.lines { dialogue_view(line); }
        });
    }
}
```
game.dialogue().lines 是统一的可观察对白行列表。每行提供只读 id/speaker_id/speaker_name/content_id/text/reveal/read；speaker_id 区分同名角色，旁白为 null，content_id 标识用于已读记录的内容。speaker_id/content_id/speaker_name/text/reveal 是当前显示行组第一行的快捷投影；ADV 常用这些字段，NVL 和气泡使用完整行列表。行的 read 保存提交前是否已读，顶层 read 表示整组提交前是否均已读；交互结束后保留这些值，退出动画不会突然变色。用 `color: derive(|| if model.read { "#aaaaaa" } else { "#ffffff" })` 定义已读颜色，角色配色可按 speaker_id 查询作者的样式表。model.text 保存完整文本，reveal 控制显现范围，因此逐字显示保持稳定排版。对话视图更换、隐藏和重新显示都保留会话历史。语音参数尚未接入。多个角色同时发言也使用同一个推进交互：
```aside
let eri = character("Eri");
say([mio.line("你听见了吗？"), eri.line("在窗外。")]);
```
`game.dialogue()` 返回会话的稳定模型。NVL 用上面的 nvl_view 组合累积行，`model.clear()` 清除当前显示页，历史保留；ADV 使用第一行快捷字段。`current_line_ids` 标识当前发言组。逐字显现只更新 reveal，不重建 NVL 页；追加行或清页才重建 view 内容。位置、背景、行间距和气泡形状由普通布局决定。自定义对白中，未被控件消费的轻点推进当前句；触摸拖动只滚页。长菜单示例见 [author_scroll.aside](../../examples/author_scroll.aside)。
换句时的上滚可封装在普通对白函数中：先等待角色对白，再 `await run(tween(...))` 移出旧文本，之后调用下一句。完整组件见 [author_dialogue_transition.aside](../../examples/author_dialogue_transition.aside)，包含裁剪、角色配色与已读配色；运行前提供 fonts/body.ttf。打字速度用 game.configure(text_speed:) 调整；逐字淡入、弹跳等字形效果尚未接通。

句内样式用显式 `rich(markup)`：例如 `narrator(rich("仰望 [ruby=ほしぞら]星空[/ruby]，[color=#ffd580][b]记住这一刻[/b][/color]。"));`。text、角色调用、character.line 和 narrator 都接收普通字符串或富文本；对白模型和历史保留完整样式。普通字符串不解析标记；富文本的只读 `.plain` 取得不含标记的正文，`.markup` 取得原标记字符串，读取均不复制文字。组合历史条目时可用 `rich("[b]姓名：[/b] " + line.text.markup)` 保留样式；普通字符串使用 `rich_escape`。`model({text: rich("[b]初始内容[/b]")})` 可直接作为可观察状态；复制模型默认值时共享不可变富文本。可运行示例：[author_rich_text.aside](../../examples/author_rich_text.aside)。

`game.dialogue().ready` 表示当前整组对白的正文和全部时序（包括尾部 pause）已完成；没有当前对白时为 false。`lines` 中每行也有独立的 `ready`，已完成的旧行保留 true。它不受自动、快进或暂停策略改变；例如提示光标使用 `visible: derive(|| dialogue.ready && !reading.auto && !reading.skipping && !reading.paused)`，其中 reading 是 `game.reading()`。光标可用普通 text/image、布局和动画定制，不需要比较正文长度。见 [句内时序示例](../../examples/author_dialogue_timing.aside)。

句内节奏也写在 rich 中：`[speed=15]慢慢说[/speed]` 按每秒 15 个字素显示，`[speed=2x]快一些[/speed]` 使用玩家速度的两倍，`[speed=0]立即出现[/speed]` 显示即时段，`[pause=0.5]` 在当前位置停半秒。speed 可以嵌套，关闭时恢复外层速度。`[next]` 使本句播完并呈现后自动续句，不改变全局自动阅读；同时对白要求每行都有该标记。末尾停顿会先执行。手动补全、快进或全局 text_speed: 0 越过剩余节奏；打开菜单或挂起保留剩余停顿。标记不进入正文、历史或字数，普通 text 显示完整内容。完整示例见 [author_dialogue_timing.aside](../../examples/author_dialogue_timing.aside)。

支持成对嵌套的 `[b]`、`[i]`、`[u]`、`[s]`（粗体、斜体、下划线、删除线）、`[size=36]`、`[color=#RRGGBB]` / `[color=#RRGGBBAA]`、`[em]`（着重号）、`[ruby=注音]基文[/ruby]`、`[link=词条ID]正文[/link]`。`[[` 表示字面左方括号；把任意玩家输入插入富文本前用 rich_escape 转义。错误标记在 rich 调用位置报错，错误值包含标记的 UTF-8 字节位置。ruby 不嵌套；基文和半字号注音一起换行，基文逐字显示，基文全部显示后出现注音。字体仍由游戏提供，粗体、斜体的字形效果取决于字体。

text 按可用宽度自然换行；width、x/y、padding、scroll 决定 NVL 页面的位置与边界，列表 gap 决定段落间距。`text_align` 支持 start/end/left/right/center/justify，默认 start；`line_height` 是字号倍率，默认 1.2。不同字号、ruby 与着重号共同决定行高，逐字显示只改变绘制范围。节点 color 是基础颜色，显式句内颜色覆盖对应片段；省略句内颜色即可沿用角色或已读配色。

`text(value: rich("查看 [link=star-map][u]星图[/u][/link]"), on_link: |event| show_glossary(event.id))` 把词条 ID 交给普通回调。event.target 是文字节点；链接完整显示后可以鼠标或触摸点击，焦点在文字上时左右方向选择链接，确认键激活。链接消费输入，不推进对白，不自动打开浏览器；节点移除后其回调任务按节点所有权取消。完整示例：[author_rich_text.aside](../../examples/author_rich_text.aside)，运行前提供 fonts/body.ttf。需要语言切换时使用 `derive(|| rich(tr(...)))`；传入翻译模板的玩家文字先 rich_escape。提交过的对白和历史保留原语言和样式。当前无障碍树提供正文标签，尚未提供逐链接语义子节点。

需要路线图与章节重启时，用普通 API 给函数登记稳定 ID 与显示标题。函数仍按同一套调用与等待规则执行。
```aside
fn opening() {
    // 剧情正文。
}
story.register("chapter01.opening", opening, title: "窗上的光");
```
`game.start(entry)` 使用当前 ASB 中无捕获、无必需参数的普通函数。接受请求后，旧脚本不再继续；新会话重新执行模块初始化，再调用 entry。旧对白、UI 回调和活动结束，脚本全局、历史、自动和快进状态重建；已读记录、game.configure 配置、宿主资源与存档服务保留。目标入口负责构造画面；调用后无需清空旧舞台。跨 ASB 的热更新入口解析与持久脚本全局尚未实现。可运行示例见 [author_game_start.aside](../../examples/author_game_start.aside)。

## 四 时间顺序与同时发生的变化

`clock(interval: 60, owner: panel)` 创建可绑定的周期时钟，ticks 是完整周期数，elapsed 是这些周期的总秒数，初始均为 0。用 derive 组合时钟、倒计时或进度显示；时间跨过多个周期时直接更新到最新值。pause/resume/stop 和 await 沿用活动接口，暂停期间不计时，节点退休取消；clock 无终点，不能 finish。参见 [时钟示例](../../examples/author_clock.aside)。

现实时间使用 `use std::time::{now, parts, format};`：now() 返回 UTC Unix 秒整数，parts(timestamp, timezone: null) 返回 year/month/day/hour/minute/second/weekday/offset_seconds，weekday 为周一 1 至周日 7。format(timestamp, pattern: "%Y-%m-%d %H:%M", timezone: null) 按 strftime 模式返回文字；timezone 可设 IANA 时区（如 Asia/Shanghai、UTC），省略使用系统时区，系统无法提供时使用 UTC。保存时间可直接使用 saves.list 的 saved_at。调用不会自动刷新；在普通任务中更新模型，再绑定文字。见 [author_wall_clock.aside](../../examples/author_wall_clock.aside)。

动画与组合活动已接通：`tween(node, x: 320, duration: 0.5, easing: "in_out")` 描述变化，`run(action, owner: node)` 启动。owner 省略时，主剧情归会话、按钮回调归事件节点；`pause(activity)`、`resume(activity)`、`stop(activity)`、`finish(activity)` 控制同一活动。await 返回字符串 `Completed` 或 `Stopped`；目标或 owner 退休取消活动并在 await 处抛错。duration 使用非负秒数，0 立即完成；easing 支持 linear、三次 in/out/in_out、二次 quad_in/quad_out/quad_in_out 和正弦 sine_in/sine_out/sine_in_out。可以改变全部共同数值属性；width/height 的起值须先显式指定，null 表示该属性不参与动画。可运行示例：[author_activities.aside](../../examples/author_activities.aside)，使用 `reel dev examples/author_activities.aside --assets target/reel-artifacts/author-assets`。sequence/parallel/repeat 已接通；scope 尚未实现。

`tween(viewport, scroll_y: 400, duration: 0.5, easing: "out")` 平滑移动滚动区域；也可使用 `scroll_x`。目标必须是 scroll 节点，滚动范围仍由实际内容尺寸限制。

需要正弦曲线运动时，使用 `sine_in` 从慢到快加速，`sine_out` 从快到慢减速，或 `sine_in_out` 两端慢、中间快。所有曲线都精确到达指定终点；它们与三次 in/out/in_out、二次 quad_in/quad_out/quad_in_out 的运动节奏不同。

五次曲线 `quint_in`、`quint_out`、`quint_in_out` 提供更明显的加速与减速；`bounce_out` 在抵达终点时回弹，`bounce_in` 是其反向，`bounce_in_out` 在两端组合回弹。回弹不会越过起终值。用同一个 tween 描述菜单滑入和 Logo 落下，见 [author_activities.aside](../../examples/author_activities.aside)。

```aside
await run(tween(portrait, x: 720, duration: 0.8, easing: "sine_out"));
```

完整的 [author_sine_easing.aside](../../examples/author_sine_easing.aside) 用相同距离和时长对比三条正弦曲线；打开窗口前请提供 `fonts/body.ttf`。


语言底层现已支持等待宿主注册的可等待对象，普通 fn 可以直接写 `await request`；它会暂停当前调用链，让宿主继续处理其他工作。已完成对象也在下一调度步骤返回，错误在 await 位置报告并保留生产者原因。引擎活动现已接入这一协议；async fn 与 Future 构造尚未实现。可运行的宿主接入检查见 [host_awaitables.rs](../../crates/aside-compiler/tests/host_awaitables.rs)，命令为 `cargo test -p aside-compiler --test host_awaitables`。

空 sequence/parallel 与 times: 0 立即完成，不写属性。times 必须是非负整数或 null，省略或 null 表示持续循环；持续循环的单轮必须有正时长。含无限分支的组合不能 finish，调用会在改变活动前报错；用 stop 结束。并行分支的全部写入集合在 run 时检查，冲突不会接管已有活动。暂停、停止与取消作用于整组，后续段只在实际开始时接管属性。较大的时钟步长会继续推进跨过的动作段。

tween 只描述目标属性、终值和时长。sequence 顺序组合动作，parallel 同时组合动作，repeat 重复动作。它们都是普通库函数，产生可复用的动作值。run 才启动活动，并返回可等待、可控制的活动；await activity 等待活动结束，输入、声音和其他活动继续进行。当前 run(action) 默认归会话所有，显式 owner 可指定舞台节点。设计中的 scope 将提供自己的 s.run 入口。
动作的起始属性在实际激活时读取。sequence 中后一段在前一段结束后读取起值。动作描述可以复用，每次 run 都产生独立进度。活动只在动作段真正激活时取得属性写入权。新演出停止与之冲突的旧根活动并从当前值接管，暂停的旧活动也遵守此规则。sequence 可按顺序写同一属性；parallel 分支的写集合保持互斥。直接赋值，包括赋相同的值，停止正在写该属性的根活动。其他属性的赋值不受影响；终止活动释放全部写入权。
```aside
let entrance = parallel(
    tween(view.portrait, x: 320, duration: 0.5),
    tween(view.hud, opacity: 1, duration: 0.3)
);
await run(entrance);
```
每个活动有确定的 owner。启动与主剧情函数归会话所有，UI 回调归发出事件的节点所有。stage.replace 结束旧画面及其回调，调用它的会话剧情继续执行。动作目标只是经过验证的句柄；任一节点或效果目标销毁时，整个根活动结束为 Cancelled，释放所有属性保留。owner 从舞台销毁时，所属活动与回调任务一并结束。pause 暂停进度，resume 继续，stop 停在当前值并使等待返回 Stopped；finish 使有限动作到达其终值，返回 Completed，自然结束也返回 Completed。无限循环使用 stop 结束，其本身没有完成终值。资源错误或活动取消在仍存活的等待任务中产生对应错误，供普通 try/catch 处理。若等待任务自身的 owner 已结束，该任务直接结束；宿主释放其资源，脚本清理代码也随任务结束。
parallel 等待所有有限子动作。任一子动作失败时结束同组剩余子动作，并报告错误；sequence 随失败停止后续动作。repeat(action, times: 3) 有限重复；省略 times 表示循环。持续的呼吸、雨丝等循环单独运行，由画面所有权管理，剧情只等待自己需要的有限活动。
```aside
let breathing = repeat(sequence(
    tween(view.portrait, opacity: 0.92, duration: 1.4),
    tween(view.portrait, opacity: 1.0, duration: 1.4)
));
run(breathing, owner: view.root);
mio("听，雨又大了。");
```
上例展示循环组合规则。实际立绘呼吸可以改用细微位移或缩放，脚本结构相同。作者把常用动作封装成普通 fn，输入任意节点后返回动作值，UI 与立绘共用这些动作。

`delay(seconds)` 可以在演出中留出停顿，无需创建画面节点。它与动画使用同一活动时钟，支持暂停、停止、owner 取消和组合；纯等待期间播放器休眠到截止时间。秒数须有限且非负，零秒立即完成。可运行示例：[author_timing.aside](../../examples/author_timing.aside)。

```aside
await run(delay(1.5));
await run(sequence(delay(0.5), tween(panel, opacity: 0, duration: 0.3)));
```

## 五 作用域管理临时画面

本节作者 scope、临时覆盖与恢复行为属于设计目标。

scope 在当前剧情调用链执行一个普通闭包。它管理临时节点、属性覆盖、偏移和活动；正文可以正常对白、等待、调用其他函数。退出时只释放本次作用域持有的内容。
```aside
fn show_cg(view) {
    scope(body: |s| {
        s.override(view.hud, visible: false);
        s.add(view.picture, image("assets/window-cg.png"));
        await game.advance();
    });
}
```
这里 HUD 暂时隐藏，CG 加在 picture 最上方。玩家继续后，CG 移除，本次 visible 覆盖解除。HUD 原来的绑定与底层值始终保留；退出时显露最新底层值，也保留其他仍存在的覆盖。嵌套覆盖按后建立的优先，结束任意一层只移除自己的 token。
s.add(parent, node) 挂载临时子树，s.override(node, props...) 创建可动画的覆盖层，s.offset(node) 创建独立的 x/y 附加偏移，s.run(action) 启动归此作用域所有的活动。普通 node.add 继续使用明确父节点寿命，不因身处 scope 而偷偷改变含义。
普通返回、异常和场景退出都触发同步、幂等的宿主清理。退场动画由正文主动等待。作用域可以嵌套；记录了当前调用位置、活动和覆盖的存档恢复后，继续同一段正文，再按相同规则释放。游戏变量的正常写入保持其业务含义，scope 只管理显式登记的临时效果。
默认 scope 随当前任务存活。需要随某个节点结束时写 scope(owner: node, body: ...)。若该节点先结束，仅撤销 scope 区域并在外层调用点产生 ScopeCancelled，仍存活的主剧情可以在 scope 外捕获；区域内的失效节点代码结束，宿主完成清理。当前任务自身被撤销时，整个任务结束。
visible=false 隐藏子树并退出命中与焦点；enabled=false 保留画面并禁用交互；opacity 只改变透明度。永久换景用 stage.replace(new_root)，局部永久替换用容器的 replace(new_child)。

## 六 Logo 标题与局部交互

视频已接入共同舞台、媒体活动与存档。默认桌面支持非分片 MP4：渐进式 8-bit YUV420 H.264 Baseline 视频，可附带单声道/双声道 AAC-LC。自定义宿主通过 MediaBackend 接入其他格式或硬件解码；没有安装视频能力时，播放在调用处明确报错。

`video(source, ...image 节点属性)` 只创建画面；`movie.play(looping:false, volume:1, start:0, end:null, owner:null)` 返回活动。start/end 为片段起点与排他终点，省略 end 使用视频末尾。重复 play 停止该节点原播放。正常结束与 stop 都继续 await 后的代码；播放失败与节点/owner 退休产生可捕获错误。完整示例见 [author_video.aside](../../examples/author_video.aside)。

```aside
let movie = video("assets/logo.mp4", width: 1280, height: 720, fit: "contain");
let playing = movie.play();
stage.replace(button(width: 1280, height: 720, decorated: false,
    background: "#000000", on_click: || stop(playing)) {
    movie;
});
await playing;
```

`pause(playing)` / `resume(playing)` / `stop(playing)` 与其他活动一致；`finish(playing)` 完成有限视频，循环视频须 stop。`video.seek(playing, seconds)` 跳到绝对内容时间，`video.position(playing)` 返回当前秒数；视频音轨沿 Music 音量总线。隐藏节点不暂停，可直接组合裁剪、镜头、透明度和其他控件。

存档记录视频路径、区间、当前位置及暂停状态，恢复后继续同一活动；停止/结束后保留的画面也恢复，但不重播声音。像素、解码队列与 GPU 句柄不进入存档。解码在后台进行，有声视频跟随实际消费的音频时钟，静音视频使用会话时间；运行窗口挂起时一起暂停。

剧情中临时加入按钮时，向需要的父节点添加普通 button，然后正常移除。作者决定它遮住哪些内容、是否阻止下面输入、何时消失。一次性按钮使用同一事件和所有权规则。
```aside
let hint = view.hud.add(button("查看照片",
    on_click: |event| photo_panel.open()
));
mio("桌上那张照片，你还记得吗？");
hint.remove();
```
photo_panel 是作者定义的组件控制对象，上例只演示与剧情交错的按钮寿命。事件回调沿所属节点调度。打开模态节点时，引擎统一处理下层输入隔离、焦点进入及关闭后的焦点恢复。回调需要接管主剧情时使用 game 提交入口操作；普通画面修改可直接执行。

## 七 设置页与图片风格控件

button、slider、toggle 与节点 bind/unbind 已实现；页签、settings 与滑条外观槽位仍属于设计目标。

下面将页签、页面和滑条组合成设置页。switcher 保留各页节点和局部值，select 只改变选中页。隐藏页离开输入与焦点导航。
```aside
fn preferences() {
    let pages = switcher(selected: "audio") {
        page("audio") {
            text("音乐");
            slider(value: bind(settings.music), min: 0, max: 1,
                   width: 600,
                   track: image("assets/slider-track.png"),
                   thumb: image("assets/slider-thumb.png"));
        };
        page("reading") {
            text("文字速度");
            slider(value: bind(settings.text_speed), min: 1, max: 100);
        };
    };
    column(gap: 16) {
        row(gap: 12) {
            button("声音", on_click: |event| pages.select("audio"));
            button("阅读", on_click: |event| pages.select("reading"));
        };
        pages;
    }
}
```
拖动由控件完成捕获与数值换算，同一数值决定滑块位置和音量。作者提供轨道与滑块的绘制节点，控件按方向、范围和槽位布局它们。settings 属于全局设置，读剧情存档后继续使用当前设置。 普通移动、缩放和布局变化保持拖动，输入使用最近显示画面的几何。范围、步长、方向或轨道端点策略改变时，当前拖动结束，新配置显示后恢复交互；外部绑定值更新仍即时生效。
图片按钮也是作者自己的普通函数。按钮提供交互状态，图片只根据状态选择资源。
```aside
fn picture_button(normal, hovered, pressed, clicked) {
    let control = button(on_click: clicked);
    control.content = image(source: derive(|| {
        if control.pressed { pressed }
        else if control.hover { hovered }
        else { normal }
    }));
    return control;
}
```
bind 连接可观察属性，属性同时保留原来的读写权限；text 可以读取只读属性，slider 写回数值时要求可写属性。derive 计算只读外观。派生函数对实际读取的可观察属性建立依赖，条件变化后替换依赖；结果也可通过 `binding.value` 读取并继续派生。派生更新在同步脚本段结束后、舞台发布前集中完成，也可读取不可变捕获值；普通数据的内部变化保持普通语言语义。可观察属性变化仅更新相关节点。标量写入可写绑定属性会写回绑定源并保留绑定；动画也保留和写入最终源属性，因此两个节点绑定同一源会发现真实写入冲突。源的显式改写停止相关根活动；被动投影更新属于读取。只读派生属性用 scope 覆盖做临时演出；重新绑定或 unbind 会先结束依赖旧绑定的活动，unbind 保留当前有效基础值。捕获存档的短暂期间，脚本派生与作者外观更新暂停，捕获完成后按最新状态恢复。普通 {...} 数据记录适合传参，持续变化的业务值用 model({...}) 创建可观察模型。循环依赖和派生计算中的写入、等待给出源码诊断。
作者可用同样方式封装统一字体、按钮图片、留白和动效，再作为普通模块导入。风格复用来自组件和参数，共用的属性对象也可按普通语言规则组织。

稳定位置引用使用 `bind(node.x)` 或 `bind(node[key])`。接收者和键只在创建引用时求值一次。将引用传给节点构造参数或赋给节点属性即可连接；之后赋入普通值会写回来源，连接仍保留。`unbind(bind(node.x))` 断开连接并保留、返回当前值。多个控件共享来源时，范围等约束会一并校验，失败不修改任何控件。来源节点被显式销毁时，仍存活的目标冻结最后值并断开；已有引用不能再用于已销毁的来源。业务 `model` 和 `derive` 的实现状态见第十二节及已验证范围。

所有节点的 `hover`、`pressed`、`focused`、`disabled` 都可直接读取或用 `bind` 连接。它们由输入系统发布，脚本不可赋值，也不能绑定为 slider/toggle 的可写 value。hover/focused 包含后代；pressed 包含指针捕获与按钮自身的键盘、手柄确认按住；disabled 包含祖先禁用。取消、隐藏、禁用和失焦会清理交互状态。节点退休后，读取该节点的 derive 停止更新并保留最后成功值。移除最后一个消费者会停止其派生外观更新；共享消费者继续更新，重新绑定保留引用会恢复更新，但不能恢复已退休的输入。按钮和开关使用 `decorated: false` 关闭默认焦点框及开关指标/留白，再用普通 children 构成完整外观。实例见 [author_control_states.aside](../../examples/author_control_states.aside)。

悬停或聚焦需要改变业务状态时使用 `on_interaction`，例如鼠标离开后仍保留图库选中项。所有节点都接受这个回调或 null，构造后也可赋值。不可变事件包含 `target`、`hover`、`pressed`、`focused`、`disabled`、`focus_visible`；即使回调等待，其中的布尔值也保留该次变化的值。只有状态实际变化才触发回调，并先发布整批事件的只读状态。注册回调不会发送初始事件。等待中的回调属于节点，节点退休时取消。没有回调时不分配事件对象或任务；纯外观继续使用 `bind`/`derive`。

```aside
let gallery = model({selected: null});
let card = button("Sketch", on_interaction: |event| {
    if event.hover || event.focus_visible { gallery.selected = "sketch"; }
});
```

[author_control_states.aside](../../examples/author_control_states.aside) 同时展示瞬时外观和离开后保留的选中提示；准备 `fonts/body.ttf` 后用 `reel dev examples/author_control_states.aside` 运行。

## 八 风格与共同运动

图片使用相同资源时由播放器共享。wgpu 后端复用已提交的 GPU 图片，界面文字或悬停状态变化不会要求重新上传整套图片；缓存会在资源退休或需要腾出空间时释放，作者无需管理 GPU 句柄。

普通函数、样式展开、节点属性修改、变换和有限动画已可用；临时 scope 覆盖仍待实现。

常用风格就是普通数据，组件函数决定如何把它应用到自己的节点。先写默认值，再用变体与显式参数覆盖；字段缺失才采用默认值，false、0 和 null 都保留作者的明确含义。临时 scope 覆盖优先于节点底层属性。
```aside
let default_look = {width: 180, height: 64, background: "#284860"};

fn menu_button(label, clicked, style = {}) {
    button(label, on_click: clicked, ...{...default_look, ...style})
}

let status = text("Ready");
let start = menu_button("Start", |event| { status.value = "Started"; },
                        style: {background: "#603840"});
stage.replace(column(gap: 16) { start; status; });
```
`...style` 将对象自有字段作为具名参数传入，普通函数、节点构造器和提取的方法使用相同规则。覆盖值先合并为一个对象；重复传同一参数会报错。位置参数放在具名参数之前，尾随 children 也是具名参数。完整初始化和按钮交互见 [author_buttons.aside](../../examples/author_buttons.aside)。
节点属性可以直接修改；持续跟随业务或偏好变化的外观使用 bind/derive。普通风格记录用于构造，运行期共享配色可以用 model 保存，再把其属性绑定到文字与图片。作者自己的组件既可完全手绘，也可沿用熟悉的布局与控件行为。

按钮高亮可使用 `derive(|| control.hover || control.focus_visible)`。只读 `focus_visible` 在键盘/手柄导航时显示焦点，使用指针后隐藏该提示；`focused` 仍保留实际焦点，不影响随后按回车确认。两个属性均包含后代，隐藏、禁用或窗口失焦时停止显示。背景演出不会使无关按钮的输入失效；替换按钮内容仍须显示新画面后才能接受新选择。参见 [author_fitted_text.aside](../../examples/author_fitted_text.aside)。
UI 与立绘共同运动时，把它们放进同一个组，再对组执行动作。
```aside
let cast = group() {
    image("assets/mio.png");
    dialogue_view(game.dialogue());
};
stage.replace(cast);
await run(tween(cast, x: 80, duration: 0.5));
```
需要移动中叠加震动时，让基础位置与震动使用独立偏移。下面的 shake 是作者函数，返回普通动作值，可用于对话框、立绘或任何组。
```aside
fn shake(offset, strength = 8, step = 0.05, times = 3) {
    repeat(sequence(
        tween(offset, x: strength, duration: step),
        tween(offset, x: -strength, duration: step * 2),
        tween(offset, x: 0, duration: step)
    ), times: times)
}

scope(body: |s| {
    let offset = s.offset(view.hud);
    await s.run(shake(offset));
});
```
只给 hud 加偏移，就只震动对话 UI。它与底层 x/y 移动独立：多个 x/y 偏移在父坐标系相加，基础布局保持原值，绘制与命中共同使用合成结果。scope 结束时移除自己的偏移，移动后的基础位置仍正确。旋转、缩放按变换矩阵的明确顺序组合，另用组节点表达；加性 x/y 不暗含旋转相加。

## 九 遮罩 特效与自定义演出
历史验证（R01 / Issue #25）：内部 Rust 帧记录、资源租约、分组、裁剪与透明度已有针对性用例。R01 当时尚无 GPU 窗口，记录型消费者不代表物理设备通过。本节节点图像遮罩已接入作者接口，blur 等效果仍是设计目标；当前基础舞台与真实窗口已接通，范围见第一、二节。[历史验证范围](../../crates/reel-ui/R01-REPORT.md)。

历史验证（R02）：wgpu 后端与 winit 诊断窗口包含图片、分组透明度、裁剪和 Parley 文字；这些历史 Rust 诊断能力不意味着高级效果接口已实现；当前 PNG/JPEG 作者图片使用共同舞台和播放器资源加载器。当前作者源码和 ASB 由 dev/run 进入真实剧情窗口。[窗口说明](../../crates/reel-player/README.md) · [设备验证范围](../../crates/reel-render/R02-REPORT.md)。

节点的 mask 属性给整个子树应用灰度图像遮罩，沿用普通图片资源加载与存档。mask_progress 从 0 到 1 表示由全隐藏到全显示，mask_softness 控制软边宽度，0 为硬边；较暗区域先显现，mask_invert: true 反转先后顺序。遮罩 PNG/JPEG 按节点矩形拉伸，设置明确的 width/height 即可控制覆盖区域。
```aside
let reveal = stack(width: 1280, height: 720,
    mask: "assets/ink-mask.png", mask_progress: 0, mask_softness: 0.06) {
    image("assets/window-cg.png");
};
view.picture.add(reveal);
await run(tween(reveal, mask_progress: 1, duration: 1.2), owner: reveal);
reveal.mask = null;
```
mask_progress 和 mask_softness 使用普通 derive/tween。完整示例：[author_image_mask.aside](../../examples/author_image_mask.aside)。进度为 1 时跳过遮罩合成；用完可将 mask 设为 null 释放资源引用。遮罩不改变逻辑命中，需要禁用交互时使用 enabled/visible。遮罩需要完整 PNG/JPEG，不接受分块 .reel-image。

节点 `invert` 将整个子树的合成画面反色，范围 0..1，默认 0；1 完全反色，中间值渐变。颜色按 sRGB 反转，透明度保持不变；对白和菜单放在同级节点即可独立于反色场景。可以直接赋值、derive 或 tween，参数随存档恢复。

```aside
let scene = stack(width: 800, height: 450, invert: 0) {
    text("A different view", color: "#e08020", font_size: 48);
};
stage.replace(scene);
await run(tween(scene, invert: 1, duration: 0.4), owner: scene);
```

完整可运行示例：[author_invert.aside](../../examples/author_invert.aside)。反色和 opacity/mask 共用组离屏目标；不用时设为 0，独立反色组不再需要额外目标。

效果参数与节点属性采用同一动画规则。更复杂画面可以嵌套 group：背景单独模糊，人物保持清晰，图片按钮仍在最上方；再对整组使用遮罩或镜头变换。所有子节点的前后顺序由树结构决定。
自定义效果通过版本化 Effect 描述输入图像、参数、输出范围和后端实现。高级作者可以开发宿主扩展，常用效果随后以普通 Aside 函数组合复用。shader 支持属于后端能力，项目检查列出缺少的能力。可移植效果应同时提供简化实现，游戏通过能力查询选择适合的平台版本。
特效改变的像素与逻辑输入区域分别声明。普通仿射变换与几何裁剪自动映射命中；弯曲、扭曲和任意着色器的交互区域由作者提供逻辑形状或专用映射。为每次点击从 GPU 读回画面会增加延迟，因此基础接口使用可直接计算的区域。

## 十 选择与会变化的内容

固定宽度的选项或标题可以用 `text_fit: "shrink"` 自动缩小完整文字，`min_font_size` 指定最低字号；默认 `text_fit: "none"` 保留普通排版。适配只缩小、不放大，保留显式换行，富文本和 ruby 一起缩放；打字机不会逐字改变字号。最低字号仍放不下时可以配合 clip。`text_outline` 设置字形向外描边宽度，`text_outline_color` 设置颜色，适用于 text 和控件标签。例如 `text("Long option", width: 140, font_size: 30, text_fit: "shrink", min_font_size: 14, color: "#000000", text_outline: 2, text_outline_color: "#ffaaff")`。这些属性可以 derive，让悬停或焦点只改变文字外观。

独立演出或小游戏后台逻辑使用 `spawn(work, owner: root)`。work 是零参数函数、闭包或绑定方法，可以循环并等待动画/结果；`await task` 取得返回值或原错误，重复等待得到同一终态。`task.cancel()` 首次请求取消返回 true，之后返回 false；任务清理后等待者收到 Cancelled。取消不会执行脚本 finally，资源应交给节点所有权管理。节点移除自动取消所属任务，隐藏不取消；取消等待者不取消生产者。省略 owner 时继承回调/任务节点，在主剧情中归会话。正文在当前调用者等待或返回后开始调度，不占用主剧情的对白/选项权限；初始化、纯 derive 和 NoWait 中不能启动任务。见 [author_tasks.aside](../../examples/author_tasks.aside)。普通固定演出继续用 sequence/parallel/repeat；`tween` 可用 `quad_in`、`quad_out`、`quad_in_out` 表达二次曲线，in/out/in_out 表达三次曲线。

任务容量随实际并发数量增长和复用，不需要作者预先指定任务数。Rust 宿主可用 `VmLimits.max_tasks` 设置自己的准入配额；分配仍计入堆预算。

普通函数在当前任务内调用，内部等待完成后才返回。例如 `ask()` 内部已 `await result` 并返回 bool，调用者直接写 `if ask() { ... }`；只有返回活动或 completion 等可等待对象时才再写 await。对 bool 等普通值使用 await 会在该行报告 `NotAwaitable`。

可运行的缩字、语言切换和悬停描边示例见 [author_fitted_text.aside](../../examples/author_fitted_text.aside)。

表单、登录、设置或小游戏可以返回自己的结果。`completion(owner: root)` 创建一次性可等待对象，回调调用 `.resolve(value)` 提交任意值（省略为 null），主流程通过 `await` 继续；其他输入和活动照常执行。组件可以把节点和结果一起返回：
```aside
fn name_form() {
    let data = model({name: ""});
    let root = column(gap: 12);
    let result = completion(owner: root);
    root.add(text_input(value: bind(data.name)));
    root.add(button("确认", on_click: |event| result.resolve(data.name)));
    return {root: root, result: result};
}
let form = name_form();
stage.replace(form.root);
let name = await form.result;
```

`.reject(error)` 在 await 处抛出该值，`.cancel()` 抛出 Cancelled。三个结算方法首次返回 true，之后返回 false，便于处理重复点击；多次 await 得到同一终态。删除 owner 节点取消未结算结果，隐藏节点不会取消；取消一个等待者不影响其他等待者。省略 owner 时，主剧情归会话、回调归事件节点。resolve 原样传递对象，不自动等待其中的其他结果。此接口不占用 choose 或对白，也不产生历史记录。完整表单与子游戏组合见 [author_results.aside](../../examples/author_results.aside)；运行前提供 fonts/body.ttf。

choose、自定义 choice_view 和只读选择模型已实现。引擎不自动叠加对白或选择界面；作者将自己的组件挂到舞台，普通函数可封装并复用外观。完整 ADV/NVL 切换及选择示例见 [author_narrative.aside](../../examples/author_narrative.aside)。

choose(options) 接收具有稳定 key 和 label 的选项数组，更新 game.choice 并等待 select(option_id)。当前句与选择共用主剧情交互权限。作者把选择视图放在自己的画面里，位置和背景均可自定。
```aside
fn choice_view(model) {
    view(|| column(gap: 12) {
        for option in model.options {
            button(on_click: |event| model.select(option.id)) {
                text(value: bind(option.label));
            }
        }
    })
}
let answer = choose([
    {key: "window", label: "走到窗边"},
    {key: "door", label: "留在门口"}
]);
if answer == "window" { by_window(); }
else { by_door(); }
```
choice_view 挂载在包含对话 UI 的画面中，by_window 和 by_door 是作者的剧情函数。choose 把传入的普通记录复制成引擎拥有的只读可观察行，id、key 和 label 都可读取并绑定，输入记录保持原值。id 是本次 InteractionId 与 key 形成的不透明稳定标识，按钮捕获 option.id 保持这一交互身份；下一次选择使用新的 id。game.choice().options 是这些行构成的只读可观察列表，默认为空；等待选择时填入选项，选择完成后清空。select 接收包含交互身份的 option.id，返回剧情的仍是业务 key；过期或重复 select 返回 false，成功返回 true；choose 始终返回原 key。
普通 children 中的 for 构造当下内容；`view(|| ...)` 在实际读取的可观察数据变化后重新构造子树，外层容器身份保持。计算成功后才替换旧内容；旧节点拥有的回调和活动随之取消。用 bind/derive 更新单个属性时保留节点身份。需要保留控件身份的集合使用 `list(|| data.items, key: |item| item.id, build: |row| text(value: derive(|| row.value.label)))`。插入、排序保留相同 key 的节点、焦点和局部状态；删除则退休其任务和活动。key 可以是任意值，但在列表内必须唯一；标量区分类型，字符串按内容比较，其余对象按身份比较。因此可直接使用选项 id 或稳定的对象引用，不必另造数字 id。每个新 key 只调用一次 build，参数 row 提供只读可观察 key、value（当前条目）和 index（从 0 开始）；返回 null 时该 key 不产生节点。回调读取 row.value 取得最新记录；derive 会跟随记录替换，bind(row.value.label) 则固定原字段位置。只有来源函数和 key 函数的读取决定列表协调；行内变化使用 bind/derive/view。重复 key 或组件失败不替换原树。列表子节点由来源数据管理，外层可组合 scroll。当前仍实例化全部条目，可见范围虚拟化尚待实现。见 [author_list.aside](../../examples/author_list.aside)。

## 十一 存档 历史 路线与跳过

历史、自动阅读和存读档接口已实现；路线切换与按章节跳转仍为设计目标。界面由作者组合，服务不生成固定页面。

跨章后不再需要之前的历史时，调用 `game.clear_history()`。它清空现有可观察历史数组，保留数组身份。当前对白、NVL 显示页、推进权限、已读进度、偏好及存档槽不变；加载存档仍恢复保存时的历史，新组 ID 不复用。清理按记录数量释放会话引用，并请求增量 GC，即使后续交互很少分配也会继续回收。平时回收按分配压力启动，无需脚本定时清理。脚本自行持有的行引用或界面缓存仍会保留内容，需要同时退休旧视图或依据历史数组刷新缓存。引擎不自动限制历史长度。Rust 宿主调用 `Session::clear_history()`。见 [author_reading.aside](../../examples/author_reading.aside)。

```aside
game.dialogue().clear();
game.clear_history();
narrator("A new chapter begins.");
```

`saves.save(slot, title: null, thumbnail: null)`、`saves.load(slot)`、`saves.delete(slot)`、`saves.thumbnail(slot)` 和 `saves.list()` 立即返回可等待请求，具有只读的 `status`（pending、succeeded、failed）与 `error`。失败会在 await 处抛出错误；成功保存意味着完整文件已原子发布。`await saves.list()` 返回已占用槽位摘要，包含字符串 id、title、saved_at（UTC Unix 秒）与 has_thumbnail，不加载图片。槽位名是逻辑标识；作者决定页面与空槽，用 `"quick"` 等固定标识组合快速保存。

可以等待自己的保存请求。这份存档恢复时，该请求为已成功且只读 `restored` 为 true（正常写盘完成为 false），可据此关闭存档菜单；调用者继续执行 await 后的语句；保存调用不会重复执行。其他未完成的外部请求以 OperationInterrupted 结算。成功读档会替换会话，原来的读档调用不会在旧会话中继续。

执行快照按工作量分片，后台编码、压缩和写盘；快照捕获期间若业务输入需要修改状态，本次保存报告 CaptureBusy，输入继续处理，旧槽位保留。桌面默认存档在用户数据目录，Rust 宿主可设置 `StoryWindowOptions.save_directory` 与 `save_bytes`。自定义宿主通过 `RuntimeSaves` 接入自己的 `SaveStorage` 和音频后端。目录适配器先解析存档目录的真实路径，在同一目录树中写入临时文件，同步后原子替换旧档，缩略图随同一文件提交。当前 VM 执行快照格式为 4，会话快照格式为 6；存档要求匹配当前代码和格式，旧格式明确报告版本不匹配。系统数据 `persistent` 独立于剧情快照。当前不提供跨版本迁移。现有存档恢复检查命令：`cargo test -p reel-player --lib save_slot_survives`。

可运行示例：[author_saves.aside](../../examples/author_saves.aside)。

截图与预览示例：[author_capture.aside](../../examples/author_capture.aside)。`await stage.capture(width: 256, height: 144)` 取得最近已呈现画面的 PNG Bytes；在打开菜单前获取，再传给 thumbnail。`await saves.thumbnail(slot)` 只读取指定槽的图片，没有图片时返回 null；将结果交给 `image(bytes)` 即可显示。未呈现画面、宿主不支持截图或资源不足都会拒绝请求，可用 try/catch 显示错误。

便携桌面游戏可以在游戏目录执行 `reel run game.asb --save-directory saves`；`dev` 同样接受此选项，相对路径以启动时工作目录解析。启动器也可以传入游戏旁边的绝对路径。安装目录只读的平台应选择系统提供的可写用户数据目录；两种部署使用同一套脚本槽位 API。

捕获期间未按键的鼠标移动不会取消保存。窗口只保留最新悬停位置，捕获结束后应用；点击、拖动、键盘输入和焦点变化需要访问会话时仍会中断捕获。

```aside
let menu = model({slots: [], busy: false, error: ""});
fn refresh_slots() { menu.slots = await saves.list(); }
fn save_slot(id: string) {
    menu.busy = true;
    try { await saves.save(id, title: "Chapter 1"); refresh_slots(); }
    catch error { menu.error = error; }
    menu.busy = false;
}
button("Save", enabled: derive(|| !menu.busy), on_click: |e| save_slot("page1/slot1"));
```

game.restart(story_id) 使用当前会话固定的主剧情模块集合，从命名章节入口重新执行剧情任务，保留当前剧情变量，结束旧主剧情任务及其交互；目标函数负责构造自己的画面。game.start(entry) 创建全新会话，适合新游戏。saves.load 恢复保存的位置、变量和画面。作者据此给路线按钮选择含义清楚的操作。
`game.reading()` 返回稳定的只读可观察 `auto/paused/delay` 状态。`game.set_auto(enabled, delay: null)` 控制自动阅读；delay 为全文实际呈现后等待的有限非负秒数，默认 2 秒，省略时保留原值。`game.pause_reading(paused)` 暂停文字显现、自动推进和对白/选项输入，普通 UI 控件仍可用于关闭回看页。自动阅读在选择处等待玩家操作，之后继续；失焦、挂起、模态界面、待加载资源和正在进行的输入手势均停止计时，恢复后重新等待完整 delay。设置属于会话，更换 UI 保留，新会话默认手动、未暂停。有限演出和独立音频有各自控制，不随阅读暂停。按钮外观、布局、打开/关闭回看页的方式均由作者决定。
`game.set_skip(enabled, unread: false, after_choices: false)` 已实现。`game.reading().skipping/skip_unread/skip_after_choices` 可观察状态与策略，`game.dialogue().read` 表示整组提交前是否已读。默认遇未读对白或选择时关闭快进；unread: true 可跳过未读对白；after_choices: true 在选择时保留快进模式，等待玩家选择后继续，仍遵守已读策略。暂停、失焦和模态界面仍会取消该模式。快进沿正常控制流执行，跳过显现/停留，并完成主剧情拥有的运行中有限演出；无限、暂停、节点拥有的活动和独立音频继续按原控制工作。自动阅读与快进互斥。暂停阅读、失焦、挂起和模态界面会关闭快进；启动快进的 input 节点退休也会清除其保持状态。作者用 input 回调在 pressed 时开启，在 released/cancelled 时关闭，也可自制切换按钮。已读在对白推进完成后记录，取消或仅显现全文不算。Rust 宿主用 `Session::new_with_read_progress` 复用 `ReadProgress`，通过 `ids/from_ids` 接自己的持久存储；新建会话默认空记录。源码模块版本变化保守视为未读。
按章节跳过 `game.skip_to(story_id)` 仍依赖后续章节注册服务。

重置已读进度使用 `game.clear_read_progress()`。当前等待对白的组与各行 `read` 立即变为 false，仅跳已读的快进停止；之后的对白重新算未读，正常推进后重新登记。历史和已完成 NVL 行保留原有样式，自动阅读与允许跳过未读的策略保持原值。读档时，当前等待对白也按最新已读记录重新判断。此操作释放旧已读记录，耗时随记录容量增长，不读写磁盘，也不删除游戏存档或 `persistent` 系统数据。Rust 宿主提供相同的 `Session::clear_read_progress()`。运行示例见 [author_reading.aside](../../examples/author_reading.aside)。

```aside
button("Reset read progress", on_click: |event| game.clear_read_progress());
```

这些服务提供数据与操作，页面外观由作者组合。手动点击或确认对白会执行一次正常显现/推进，并关闭自动阅读和快进；无效输入不改变模式。存读档、历史与阅读控制已实现；路线和章节注册仍待补齐。

## 十二 游戏变量与组件状态

`model`、字段双向绑定、存档恢复和独立系统数据读写已实现；state、node.state 仍是设计目标。

用 model 保存独立于画面节点的业务数据。多个控件可以共享同一字段，输入直接写回数据，连接的文字、可见性和样式随之更新。运行 [author_models.aside](../../examples/author_models.aside)：`cargo run -p reel-cli --features window -- dev examples/author_models.aside`。

```aside
let settings = model({volume: 0.8, music: true, palette: {accent: "#70d890"}});
let volume = slider(value: bind(settings.volume), min: 0, max: 1);
let music = toggle(value: bind(settings.music));
let indicator = text("Music", visible: bind(settings.music), color: bind(settings.palette.accent));
settings.palette.accent = "#ffd580";
```

默认数据图会被复制，保留内部别名与环，不修改传入的默认对象。已完成构造的类实例、table、array、tuple、字符串、字节串及可变缓冲可作为数据；闭包和节点等宿主资源不能用作默认数据。后续赋入的对象保持共享身份，取得嵌套实例、table 或数组元素的别名后仍可绑定其字段。普通 table 方法继续可用；clear 冻结已有连接的最后值并使旧引用失效，新增同名字段需重新 bind。复制按预算分片，取消不留下部分模型。

语言中的 let/let mut 决定绑定能否改写，model 决定属性是否可观察，状态的所属范围决定保存和释放。三者各司其职。
```aside
let route = state("route", {affection: 0, visited: set()});

route.affection += 1;
route.visited.add("window");
```
state(id, defaults) 返回当前会话的具名可观察状态，同 ID 取得同一个对象。它随存档、读档与回滚恢复，新游戏重新从默认值建立。函数局部值和闭包捕获同样可以随完整调用栈保存，具名 state 主要用于共享游戏数据和热重载中的稳定定位。
系统数据使用 `persistent.read(key, default: null)`、`persistent.write(key, value)`、`persistent.delete(key)`。操作返回可等待请求，失败由 catch 接收；写入完成才表示文件已发布。缺失记录返回 default，保存的 null/false/0 保持原值。支持 null/bool/int/float、string/Bytes、array/table/tuple，保留别名与环；table 仅存自有字段。类实例、节点、任务和闭包先转换为数据。文件独立于剧情槽和 ASB 版本，位于同一 save-directory，不出现在 saves.list 中。见 [author_persistent.aside](../../examples/author_persistent.aside)。

```aside
let preferences = await persistent.read("settings", default: {volume: 0.8});
preferences.volume = 0.6;
await persistent.write("settings", preferences);
```

读取结果是普通副本，修改后需显式 write。剧情读档不会改写系统文件，但局部副本会随快照恢复；在 `saves.save` 请求的 `restored` 分支重新读取并应用当前设置。新游戏入口也应读取系统数据。已交给后台服务的写入不因切换会话撤销；未提交的请求不会在读档后重放。业务数据升级由游戏根据自己保存的版本字段迁移。
model/state 的默认数据图复制后建立可观察属性，复制保留内部别名和环。内嵌可变数组、table、Set、缓冲以及注册的可观察数据类型都通过统一变更钩子通知；取得内部别名后修改也会通知。随后赋入的会话对象保持普通共享身份，并被纳入可观察图。model 对象通过实际引用保活。组件临时状态可放入节点自己的状态槽。
```aside
fn settings_panel() {
    let root = column(key: "settings", gap: 16);
    let local = root.state("ui", {tab: "audio"});
    root.add(button("声音", on_click: |event| { local.tab = "audio"; }));
    root.add(button("阅读", on_click: |event| { local.tab = "reading"; }));
    // 页面容器的 selected 绑定 local.tab。
    return root;
}
```
node.state(key, defaults) 属于该组件实例。同一个局部 key 在不同组件实例中独立，节点被销毁后按可达引用正常释放；读档恢复该组件状态，热重载按稳定节点 key 与状态槽 key 复用。普通运行期调用对缺失字段做一次检查后的原子补充，下一语句即可读取。候选初始化/重建时，读取候选视图可见待加入的字段，活会话继续看旧值；补充在候选事务提交后才对外生效，失败保持旧值。数据结构升级通过带版本的显式迁移处理。
作者用 persistent 保存全局偏好，并在启动和剧情存档恢复后读取、应用当前设置。业务 state 的修改采用常规控制流，scope 临时效果结束也保留其业务结果。

## 十三 作者怎样扩展自己的积木

普通节点函数、命名参数、缺省值、children 和闭包已可用。下方 arrive 示例使用已实现的有限动画接口。

复用的最小单位是普通 fn。返回节点的函数用于画面，返回动作的函数用于演出，普通对象用于把需要操作的部分交给调用者。命名实参、children、闭包和模块导入共同组成这套方式。
参数缺省表达式在每次调用时按参数顺序运行，全部实际参数先完成求值。缺省可引用前序参数，以表达式或块尾值作为结果。显式 return 属于函数正文，不能写在该函数的参数缺省内，条件或短路分支也遵守这一规则；缺省内定义的嵌套函数按自己的正文返回。break 和 continue 只能指向缺省内部建立的循环，throw 则传播参数准备失败。
```aside
fn arrive(node, destination_x, seconds = 0.5) {
    parallel(
        tween(node, x: destination_x, duration: seconds),
        tween(node, opacity: 1, duration: seconds)
    )
}
```
同一个 arrive 可以用于人物、标题按钮或整组文字。更换图片、布局和动作参数即可形成自己的组件库。自定义节点通过宿主注册参与相同的属性、输入、绘制与恢复协议。新增语言关键字只用于普通组合确实难以表达、且多个创作场景共同需要的规则。

## 十四 文字 媒体与扩展入口

需要用 Rust 扩展脚本接口的开发者可参阅 [Aside host SDK](../aside-host.md)。注册的宿主函数使用与普通 Aside 函数相同的命名参数、默认参数和错误位置规则。

Rust 扩展也可以用 `#[aside(rest)]` 声明可变位置参数。作者按普通参数或 `...items` 调用；每次调用有独立的外层数组，元素保留通常的共享身份。默认参数与绑定方法使用相同规则。这项能力独立于 Reel，本身不代表动作组合已经实现。

当前基础 text 使用真实文字布局；外部语言包与响应式语言切换已实现。句内富文本已接通；视频和 Aside 自定义绘制接口仍属后续能力。

Rust 自定义绘制层的实色、文字和图像共用线性预乘颜色转换；描边和几何裁剪均支持虚线，包括曲线、闭合路径和虚线偏移。全零虚线模式显示为实线，较小的斜接限制使用斜角连接。以上属于现有 Rust 绘制接口能力，Aside 自定义绘制入口仍待接通。

作者音频已接入后台资源读取、真实混音器和剧情等待，见[作者音频](#作者音频)。原生设备验证与离线混音验证分别记录于[音频验证范围](../../crates/reel-media/VERIFICATION.md)。视频与同一混音器的内容时钟已接入，见上文视频节点。

`localization.load(locale, source)` 从资源目录读取 UTF-8 Fluent 语言包，当前任务等待后台读取和解析完成。成功后替换该语言的目录；读取、语法或重复 ID 错误可用 try/catch 处理，旧目录保留。取消调用任务会取消加载。`localization.set_locale(locale)` 切换语言，`localization.locale()` 返回可观察的语言标记。

`tr(source, id: null, params: null, locale: null)` 返回当前语言的普通字符串，查找精确语言、基础语言，再回退到原文。未传 params 时原文按字面使用；传入参数表时原文使用 Fluent 模板。译文始终使用 Fluent，参数接受 string/int/float，支持命名参数和复数选择。参数不会作为模板再次解析；缺参数等格式错误可捕获。Fluent 保留双向文本隔离标记，避免不同方向的姓名影响周围文字。

单次指定 `locale` 可同时取得多种语言，不改变全局语言；例如双语对白把下面两行加入自己的视图，再独立绑定主行的 reveal。未加载或缺少翻译时沿用相同的原文回退。两行共享已加载的目录，不复制语言包。
```aside
column(gap: 6) {
    text(tr("你好。", id: "greeting", locale: "zh-CN"), font_size: 28);
    text(tr("你好。", id: "greeting", locale: "en"), font_size: 18);
}
```

界面使用 `derive(|| tr(...))`，语言改变时只重新计算订阅的表达式；普通 tr 调用、已提交对白/选择、输入草稿与历史字符串保留求值时的内容。需要即时翻译的聊天历史或选择外观，可以保存 ID、原文和参数，在展示绑定中调用 tr；分支 key 始终独立于文案。无需特殊 LocalText 类型。

以下片段来自 [localization.aside](../../examples/localization.aside)，英文译文位于 [locales/en](../../examples/locales/en/)。按下方命令合并后，运行时以 target/reel-artifacts/localization 为资源根目录，并在宿主或 game.configure 中提供字体。

```aside
localization.load("en", "en.ftl");
localization.set_locale("zh-CN");
let data = model({name: "Mio", count: 2});
text(value: derive(|| tr("你好，{ $name }。", id: "welcome", params: {name: data.name})));
button("English", on_click: |event| localization.set_locale("en"));
```

通常直接写 `tr("雨停了。")`。提取工具为相同原文生成相同 ID，移动代码或调整缩进不改变 ID；修改原文则生成新 ID。相同文字需要不同译法，或希望修改原文后仍沿用原作编号时，使用 `tr("雨停了。", id: "opening")`。源文不含参数时不要传空参数表。

```sh
reel loc extract examples/localization.aside --output source.ftl
reel loc extract examples/localization.aside --output-dir source-catalog
reel loc bundle examples/locales/en --output target/reel-artifacts/localization/en.ftl
reel loc check target/reel-artifacts/localization/en.ftl --locale en
```

提取跟随导入模块，包含源码位置与函数上下文。`--output-dir` 镜像模块路径，例如 story/day_one.aside 对应 story/day_one.ftl。相同 ID 只写入按相对路径排序最早的模块，保留所有调用位置。普通正文自动转义，带 params 的原文作为 Fluent 模板检查。动态文字或 ID 会报告位置，需由内容生成器提供目录。`--overwrite` 显式替换指定输出。目录模式保留其他文件，并逐一报告本次无归属的旧 FTL；合并前检查并移除，或提取到新的目录。手工译文与生成的原文目录分开维护，提取不隐式合并或改写译文。

每种语言在独立目录中按剧情和 UI 组织，例如 locales/en/story/day_one.ftl 和 locales/en/ui/settings.ftl；公共术语可放在 common.ftl。`loc bundle` 按相对路径顺序合并为源目录之外的普通 FTL；作者只维护模块文件，发行语言包在构建时生成。跨文件引用照常使用，重复消息/术语 ID 与语法错误定位到原文件行列，失败保留旧产物；替换已有合并文件需加 `--overwrite`。移动译文文件不改变身份，运行时仍为每种语言加载一个目录，只加载游戏使用的语言。解析和静态文字准备在加载时完成，每次翻译不扫描文件。翻译资源替代仍属后续能力。
video 是普通节点，audio 提供音轨、队列、循环区间、滤镜与同步组。play 返回可等待活动，stop 停在当前状态，finish 用于有终点的活动。轨道与资源由稳定身份管理，设备重建和 seek 保留或重新绑定逻辑等待；错误有明确终态。
custom_node 可以直接注册 Aside 的 state、measure、record、route 和 event。measure/record 是受预算的只读布局/绘制回调；route 同步决定消费、捕获与焦点，然后 event 作为普通可暂停任务执行。这样小游戏节点能够阻止同次输入推进剧情，也能在业务回调中等待资源。新的底层图形/平台能力通过 native 模块注册。
Camera、Mesh、Material 和 Effect 提供透视舞台与可编程效果，状态和资源身份仍进入保存。不同后端用能力清单说明所支持的实现与后备。Live2D、系统朗读、移动/Web 打包和外部服务属于完整目标的适配能力，具体 API 文档随已验证实现提供同一风格的使用示例。
网络/购买等外部写入使用独立操作记录与幂等策略；未知结果先查询确认，回放默认只读。它们与普通可回滚游戏变量的寿命区分明确。普通作者组件与脚本库始终可以扩展自己的画面、布局、演出和业务逻辑。

### 错误定位与恢复（设计合同）
当前播放器会在源码文件、行列和错误种类后显示宿主接口的具体拒绝原因。在 catch 中可用 `f"{error}"` 显示 CoreError：优先使用字符串详情，否则使用固定错误说明；普通抛出值保持原有语义。

用 try/catch 处理资源加载、媒体播放或自己抛出的错误。catch 取得原值：抛字符串就得到字符串，抛对象就得到原对象。需要原样继续报告时写 throw;；写 throw error; 表示在当前位置再次抛出，诊断同时保留前一次错误作为原因。
core.error_info() 提供当前 catch 的错误码、原始位置、调用栈和原因。处理错误的普通 helper 也能读取这份信息；保存返回的视图会保留它引用的故障记录。finally 适合做同步收尾，普通 helper 可以在其中同步返回。若收尾又发生错误，诊断会显示它替代了哪个返回结果或错误。引擎负责跨暂停、宿主回调和存档保存这些信息。完整规范见 [语言规范：故障生命周期](../aside-language.md#二十三-完整故障记录与生命周期)。

## 十五 文档与代码同步
本指南由项目负责人维护。每个 API 标明设计中、已实现及适用版本。实现后的示例提取为随代码检查的源码片段：解析、类型与接口检查、必要的行为验证使用同一份示例。中文与英文引用相同样例文件；翻译更新随接口变更一起提交。
接口、行为、默认值或取消规则变更时，同一个 PR 修改实现、规范、两种语言说明和受影响示例。尚在规划的高级用法独立标注。用户可以从已实现部分开始创作，遇到组合困难时用最小源码和期望画面反馈，负责人据此修订通用接口。
保存此指南的同一个仓库提交是版本依据，功能状态按小节标注；接口改动同时更新中文、英文和共享示例。
核心组合规则是普通函数、统一节点、动作值和作用域。以下样式、临时界面、偏移动效与变量示例共同约束实现，能力完成后以同一份源码验收。

## 已验证的源码执行范围

当前编译器、VM 与会话已通过 Windows 原生执行检查，覆盖集合/成员、闭包与默认参数、异常/finally、children 与 for、等待恢复、GC 和取消。作者舞台示例还实际经过 check/build、ASB 加载和 SessionVm 执行：两次对白之间画面中的原节点保留身份，文字和外观更新。窗口入口使用同一程序与呈现输入路径。中文作者画面另在 Windows 的 AMD Radeon RX 7900 GRE / Vulkan 上完成离屏 GPU 读回；原生窗口生命周期与热插拔仍需单独验证。

可直接运行[最小 Aside 剧情](../../crates/reel-engine/examples/story_contract.aside)及其[无窗口宿主](../../crates/reel-engine/examples/story_contract.rs)：

```sh
cargo run --locked -p reel-engine --example story_contract
```

宿主自动推进旁白并选择第一项，实际经过编译、模块初始化、VM 等待/恢复和 owner 退休。导入符号及名称节点指向同一 native 时会正确合并；冲突注册仍拒绝。宿主链接包沿用请求的内容版本，ABI v1 保持独立。

当前支持没有必填参数、没有捕获环境的普通模块级入口，包括私有函数。对白和选择按内容布局并支持滚动，不另设固定两行或两个选项的上限。源码窗口启动和 ASB 窗口启动已实现；自然结束保留最后画面，关闭窗口完成清理。上述无窗口示例专注对白与选择，完整基础节点组合请用第一节的作者舞台示例。有限动画与按钮事件已接通；视频、delegate/hooks、property-bag 和保存恢复仍未实现。

## 可运行按钮与回调

button 与其他节点使用同一棵舞台树，支持 label、on_click、children 和共同外观属性。省略 label 时可用 children 组合图片、文字或任意组件；内容按 stack 排列。enabled 默认 true，禁用父节点会禁用其整棵交互子树。通过 btn.on_click = 新闭包 或 null 可以更换或清除回调。以下代码与 [author_buttons.aside](../../examples/author_buttons.aside) 相同。

```aside
fn main() {
    let caption = text("Ready", width: 320, height: 48);
    let controls = row(gap: 16) {
        button("Move", width: 180, height: 64, background: "#284860",
            on_click: |event| {
                event.target.enabled = false;
                await run(tween(caption, x: 120, duration: 0.4));
                caption.value = "Arrived";
                event.target.enabled = true;
            }
        );
        button("Finish", width: 180, height: 64, background: "#603840",
            on_click: |event| { stage.replace(text("Finished")); }
        );
    };
    stage.replace(column(x: 24, y: 24, gap: 24) { controls; caption; });
}
```

点击把原按钮作为 event.target 传入闭包。回调可以 await 动画；同步代码执行到等待或结束才切换其他脚本任务，普通读改写不会因引擎分片被打断。不同回调可以等待同一活动，取消一个等待者不会停止其他等待者共用的活动。

回调默认属于发出事件的按钮，在其中启动的 run 也默认属于该按钮。移除按钮、清空父容器或替换舞台会取消相关回调及活动；回调移除了自己之后，后续语句不会继续执行。narrator/choose 当前只由主剧情调用，回调调用会产生可捕获的上下文错误。game.start 和节点属性绑定已实现；通用事件订阅仍待实现。

主入口返回后，挂载的按钮回调和活动继续工作；空闲时不反复执行 VM。所有回调与活动结束且画面不再有事件处理器后，会话自然结束并保留最后画面。按钮消费的点击不会同时推进对白。默认宿主支持鼠标和触摸点击、Tab 焦点导航与 Enter/空格激活。手柄十字键/左摇杆按当前画面位置导航，长选项列表会随焦点滚动，南侧按钮确认，东侧按钮与 Escape 向当前 UI 发送返回。确认在配对松开时生效；切换弹窗、失焦或断连会取消旧输入，返回不直接退出应用。多个触点可同时操作不同控件；拖出按钮后松开不触发点击，系统取消或窗口挂起会释放交互。已有按钮无需为触屏重写，可运行 [author_buttons.aside](../../examples/author_buttons.aside)。内置桌面宿主已接入系统 IME，系统可访问性尚待适配。

用第一节的 dev/build/run 命令运行此文件。引擎的节点、输入和调度不依赖 Winit/WGPU；window feature 选择当前内置桌面组合，其他宿主可以复用同一会话和呈现接口。

移植或嵌入时，使用 reel-player 的 PlatformHost、Presentation 和 StoryRuntime，以及 reel-render 的 RenderBackend。单独启用 reel-player/winit 可复用 Winit 宿主而不编入 WGPU；reel-render/wgpu 提供独立的 WGPU 后端。内置播放器也使用这些接口：只有实际呈现的新帧才能更新输入几何，临时资源压力等待提交完成后重试，永久拒绝保留上一帧。应用挂起期间剧情动画暂停，恢复后继续；关闭仍会完成任务清理。作者不需要修改剧本或选择底层图形 API。触摸使用公共 PointerEvent；启用 winit 时可复用 TouchInput 转换原生触摸。Rust 游戏与宿主可使用 reel_ui::input::ActionMap 绑定键盘、指针和手柄，读取持续动作值并接收按下、变化、松开和取消；NavigationInput 提供可配置阈值、重复时钟和重映射。独立 gamepad feature 使用 Gilrs 接入桌面设备，不要求 Winit/WGPU。Winit 适配器已转发系统 IME 事件和候选窗位置，系统可访问性尚未接入。宿主接入与可复现检查见[播放器说明](../../crates/reel-player/README.md)。

图片首次显示或读档恢复时，后端会按剩余额度分批上传，单张大图也能按行继续上传，并复用已完整提交的纹理；等待期间保留上一幅完整画面，上传完成不提前改变输入区域。Rust 宿主通过 `RenderEvent::ResourcesAvailable` 唤醒等待的呈现请求；真正超过宿主资源预算仍会明确报错。

## 模态菜单与焦点

打开存档菜单前，可先用 `let preview = await stage.capture(width: 256, height: 144);` 获取当前已呈现画面的 PNG Bytes。`image(preview)` 可以直接显示它，`await saves.save("slot/1", thumbnail: preview)` 将图片与执行快照一起写入。列表的 `has_thumbnail` 表示该槽是否有预览；用 `await saves.thumbnail("slot/1")` 按需读取。capture 可省略尺寸或只指定一边，支持 owner 和取消；不要求作者操作窗口或 GPU。

对白光标、立绘和其他控件播放动画时，玩家仍可正常点击、触摸或使用键盘/手柄确认。按下后若换句、完成揭示、替换舞台根或切换模态范围，必须重新按下，避免旧手势确认新内容。

所有共同节点支持可写的 `modal` 属性，默认 false。可见的 modal 子树独占输入；它外部的点击和未处理的键盘事件不会继续推进剧情。多个 modal 按绘制顺序选择最上层，绘制层序由节点组合顺序决定。隐藏或移除节点释放输入范围；enabled 为 false 时仍隔离下层。

进入菜单会选择其中可用的焦点，Tab/Shift-Tab 在菜单内循环；嵌套菜单关闭时恢复上一层焦点，关闭最后一层后恢复原控件仍有效的焦点。输入范围在新画面实际呈现后切换，并取消范围外的拖动。完整示例见 [author_modal.aside](../../examples/author_modal.aside)，可用第一节的 dev/build/run 命令运行。

键盘激活需要在同一个按钮上按下并松开 Enter 或空格；切换焦点、隐藏、禁用或窗口失焦会取消未完成的按键。

```aside
fn main() {
    game.configure(fonts: ["fonts/body.ttf"]);
    let status = text("Ready", x: 24, y: 96, width: 300, height: 40);
    let menu = column(x: 260, y: 24, width: 220, gap: 12,
        padding: 16, background: "#284860", modal: true, visible: false);
    menu.add(button("Confirm", width: 180, height: 48,
        on_click: |event| { status.value = "Confirmed"; }));
    menu.add(button("Close", width: 180, height: 48,
        on_click: |event| { menu.visible = false; }));
    stage.replace(stack() {
        button("Open menu", x: 24, y: 24, width: 180, height: 48,
            on_click: |event| { menu.visible = true; });
        status;
        menu;
    });
    narrator("Open the menu. The story waits while you use it.");
    status.value = "Story continued";
    narrator("Closing the menu returns input to the story.");
}
```

## 命名动作与输入节点

`input` 是按 stack 布局的共同节点，绑定键盘、手柄与指针到自定义动作。根 input 在控件尚未获得焦点时即可接收键盘/手柄输入；已聚焦的子控件保持优先权，未消费的输入交给最近的祖先映射。因此菜单可统一绑定 Escape/手柄返回，场景可统一绑定快进，点击子按钮不会使这些映射失效。焦点改变取消保持中的动作；输入不跨越模态范围，文本框保留键盘与 IME。下面与 [author_input.aside](../../examples/author_input.aside) 相同，可用第一节的 dev/build/run 命令运行。将字体文件放在 `fonts/body.ttf`，或修改 main 中的路径。

```aside
fn main() {
    game.configure(fonts: ["fonts/body.ttf"]);
    let marker = text("Move with A / D or the left stick", x: 160, y: 64,
        width: 340, height: 48, background: "#284860");
    let status = text("Ready", x: 24, y: 160, width: 340, height: 48);
    let controls = input(modal: true, width: 640, height: 280,
        bindings: {
            move_x: [binding("KeyA", scale: -1), binding("KeyD"),
                binding("LeftX", dead_zone: 0.2)],
            jump: [binding("Space"), binding("South"), binding("PointerPrimary")],
            finish: [binding("Escape"), binding("East")],
        },
        on_action: |event| {
            if event.action == "move_x" {
                marker.x = 160 + 120 * event.target.value("move_x");
                status.value = event.phase;
            }
            if event.action == "jump" && event.phase == "pressed" {
                await run(sequence(
                    tween(marker, y: 16, duration: 0.15),
                    tween(marker, y: 64, duration: 0.15)
                ));
            }
            if event.action == "finish" && event.phase == "released" {
                stage.replace(text("Finished", x: 24, y: 24));
            }
        }
    ) { marker; status; };
    stage.replace(controls);
}
```

`bindings` 的键是动作名，值是 binding 数组。control 使用物理键名（KeyA、KeyD、Space、Escape 等）、标准手柄按钮（South、East、DpadLeft 等）、轴（LeftX、LeftY、RightX、RightY、LeftZ、RightZ）或指针按钮（PointerPrimary、PointerSecondary、PointerAuxiliary、PointerX1、PointerX2）。轴范围为 -1 到 1，Y 轴向下为正；扳机按钮范围为 0 到 1。scale 默认 1，dead_zone 默认 0；死区外的值重新归一化后应用 scale，同一动作的绑定相加并限制在 -1 到 1。两个设备或触点同时保持动作时，松开其中一个不会提前结束动作。

事件有 target、action、value、phase；phase 为 pressed、changed、released 或 cancelled，键盘系统重复不会再次产生 pressed。`node.value("move_x")` 读取最新保持值，未知动作报错；回调里的 event.value 保留该次事件值。回调可以 await，生命周期与节点一致。`node.bindings = 新表` 撤销旧状态并启用新映射；对原表内部的编辑不会自动更新映射。`node.on_action` 可赋闭包或 null。

输入节点自身拥有焦点时接收键盘和手柄动作。子控件取得焦点会取消父输入节点的保持，并独立处理输入，避免手柄确认被父动作抢走；使用 modal: true 可让独立交互场景直接接管焦点。非模态节点可通过 Tab 聚焦，绑定了指针按钮时点击也会聚焦并捕获该触点。隐藏、禁用、失焦、切换模态和清除已呈现画面会取消保持；移除节点会结束其任务和活动。动作消费的同一次输入不会继续导航或推进对白。普通按钮与对白由主指针按钮或触摸激活，其他鼠标按钮可用于作者动作。

Rust 宿主使用 `InputDeviceId::{Keyboard, Pointer, Gamepad}`、`InputBindings`、`NodeKind::Input` 和 `NodeAction`。原生控制先通过 `TextEvent::Input` 路由；此事件的 Handled 表示作用域内是否有动作声明消费，未绑定的控制可继续进入同一 UI 的 NavigationInput。原始事件不会越过模态边界。画面接受和撤销分别调用可失败的 `AsideSessionUi::presented` / `clear_presented`，把路由阶段产生的取消交给 VM；有新任务时唤醒 StoryRuntime。

## 数值控件与设置页

`checkbox` 与 `toggle` 接受相同参数，前者显示勾选框，后者显示开关。`radio(option, label: null, value: null, on_change: null, ...)` 表达单选项；让同组的 value 绑定到同一个业务字段，即可自动互斥，无需维护多份布尔值。option 可以修改或绑定；value 未匹配任何选项时不选中任何项。业务值比较与 list key 相同。方向键、手柄方向和 Tab 移动焦点，确认键、鼠标或触摸选择；重复选择不触发 on_change。脚本改值不会触发回调，用户选择先更新绑定再交付 `{target,value}`。

```aside
let settings = model({sound: true, language: "zh-CN"});
column(gap: 12) {
    checkbox("Sound", value: bind(settings.sound));
    radio("zh-CN", "简体中文", value: bind(settings.language));
    radio("en", "English", value: bind(settings.language));
}
```

两者可以用 children 与 `decorated: false` 完全自定义外观。多选框读取 value；单选项可在 derive 中比较 value 和 option 选择自己的图片或颜色。绑定维持选择状态，外观无需 on_change 手工同步。完整可运行示例见 [author_selection.aside](../../examples/author_selection.aside)，提供 fonts/body.ttf 后运行。

`slider(value: 0, min: 0, max: 1, step: null, on_change: null, ...)` 提供水平滑条；`toggle(label: null, value: false, on_change: null, children: [], ...)` 提供开关。二者共享舞台位置、大小、颜色、可见性、禁用和模态属性，开关还可组合子节点。下面是可运行的 [author_settings.aside](../../examples/author_settings.aside)，提供 fonts/body.ttf 后可直接运行。

滑条要求有限且递增的 min/max、区间内的有限 value；step 为 null 或有限正数，以 min 为步进起点，两端均可选。脚本可赋任意区间内 value，不受输入步长限制。写 min/max 时当前 value 也须满足新范围，错误保留原值。鼠标或触摸拖动、方向键、Home/End、Shift 加速、手柄左右导航与可访问性操作共用同一控件。

接受用户变更后先更新 node.value，再执行 on_change，事件含 target 和此次 value。脚本赋值不会触发回调；回调可等待，节点移除会取消它。更改数值和布局保留拖动；更改范围/步长会结束旧拖动，接受新画面后可重新操作。没有回调的控件也保存值。节点值可用 bind/unbind 连接；derive 生成只读值，可用于文字和外观，不能作为 slider/toggle 的写回源。

滑条也能组合子节点。设 `decorated: false` 后，用图片或普通舞台节点绘制轨道、滑块及悬停/焦点状态，输入和无障碍仍由 slider 处理。此时轨道从内容区左端延伸到右端，没有内置滑块半径带来的缩进；padding 可用于给外观留边。见 [author_slider_skin.aside](../../examples/author_slider_skin.aside)，运行 `reel dev examples/author_slider_skin.aside`（准备 `fonts/body.ttf`）。

```aside
fn main() {
    game.configure(title: "Reading settings", width: 640, height: 420,
                   fonts: ["fonts/body.ttf"], text_speed: 30);
    let speed = slider(value: 30, min: 0, max: 120, step: 1,
        width: 300, height: 40,
        on_change: |event| { game.configure(text_speed: event.value); });
    let instant = toggle("Instant text", width: 300, height: 40,
        on_change: |event| {
            speed.enabled = !event.value;
            if event.value { game.configure(text_speed: 0); }
            else { game.configure(text_speed: speed.value); }
        });
    stage.replace(column(gap: 12, padding: 20) {
        text("Text speed");
        speed;
        instant;
        button("Reset", width: 300, height: 40, on_click: |event| {
            speed.value = 30;
            speed.enabled = true;
            instant.value = false;
            game.configure(text_speed: 30);
        });
    });
    narrator("Adjust the controls while this text appears. The next line uses the same settings.");
    narrator("Settings remain interactive after the story function returns.");
}
```

Rust 宿主可通过 `SessionUi::handle_access` / `AsideSessionUi::handle_access` 投递 AccessKit ActionRequest，沿用接受帧和模态规则；原生屏幕阅读器接入由平台宿主提供。

## 连接舞台属性

`text_input` 提供单行文本编辑。value 可以连接可写模型字段；on_change 在提交文本改变后调用，on_submit 在 Enter 确认时调用，事件都有 target 和 value。中文输入法的预编辑不会提前写入模型或触发回调。支持光标移动、选区、删除及复制、剪切和粘贴。中文转换与候选词由系统输入法提供；嵌入宿主转发公共 IME 事件并响应候选窗位置通知。value 赋值须为单行字符串；玩家粘贴或输入的换行转换为空格。外观使用共同节点属性，默认不绘制额外边框，作者可以自行组合背景和焦点提示。

指定 height 时，输入文字在内容区内垂直居中，光标、选区和输入法位置随之对齐。普通 text 指定 width 后，自动高度包含该宽度下的全部换行。

富文本事件链接在悬停或键盘聚焦时显示下划线；反馈只重绘，不重新排版文字。自定义滚动条应将拖动比例限制在 0 到 1，再写入 scroll_y，以免外观跟随尚未由布局校正的越界值；完整实现见 author_scrollbar.aside。

```aside
let profile = model({name: ""});
text_input(value: bind(profile.name), width: 280, height: 40,
    color: "#202020", background: "#ffffff",
    on_submit: |event| { profile.name = event.value; });
```

可运行示例：[author_text_input.aside](../../examples/author_text_input.aside)。

按键打字演出或键盘小游戏使用 `input(on_key: |event| { ... })`。事件提供 code、key、phase 和 shift/ctrl/alt/meta，phase 为 pressed/repeated/released/cancelled；多键分别交付，失焦会取消旧按键。聚焦按钮先处理自己的确认键，其余未消费的键向祖先 on_key 冒泡，因此点击语言按钮后仍能继续按键打字。聚焦的文本编辑控件保留全部键盘和系统输入法事件。bindings 可省略，on_action 可同时用于手柄或鼠标映射，动作映射仍遵守焦点所有权。示例：[author_keyboard.aside](../../examples/author_keyboard.aside)。

`bind` 和 `unbind` 已支持现有节点属性。下面两个滑条共用一个位置值，文字位置和显隐也直接连接控件；不需要回调复制状态。可运行窗口示例见 [author_bindings.aside](../../examples/author_bindings.aside)，字体资源准备方式与第一节相同。

```aside
fn main() {
    let shown = toggle("Show the scene", value: true);
    let position = slider(value: 100, min: 0, max: 400, width: 400);
    let mirror = slider(value: bind(position.value), min: 0, max: 400, width: 400);
    stage.replace(column(gap: 12) {
        shown;
        position;
        mirror;
        text("Move me", x: bind(position.value), visible: bind(shown.value));
        button("Detach", on_click: |event| { unbind(bind(mirror.value)); });
        button("Reconnect", on_click: |event| { mirror.value = bind(position.value); });
    });
    narrator("Both sliders share a value.");
}
```

## 作者音频

```aside
fn main() {
    audio.group_volume("music", 0.7);
    let music = audio.play("assets/chime.wav", group: "music", looping: true, volume: 0.4, fade_in: 0.5);
    pause(music);
    resume(music);
    audio.volume(music, 0.2, duration: 0.5);
    let sound = audio.play("assets/chime.wav", pan: -0.5);
    await sound;
    audio.stop(music, fade: 0.5);
    await music;
}
```

`audio.play` 在后台读取、解码并播放，立即返回活动。`await` 返回 `Completed` 或 `Stopped`；加载与设备错误可以 `try/catch`。`volume` 是非负线性增益，1 是原始音量，0 静音。pause/resume 保留播放位置，stop 不重启声音，重复等待保留同一个结果。

图片、字体和音频共用宿主的资源读取容量。并发读取暂时占满容量时，后续读取等待已有任务释放输入；单个文件超过宿主配置的输入容量仍会报错，诊断包含路径和字节数。

音频解码仅在缓冲扩容和最终共享存储转换时计入新旧存储重叠；普通包解码复用已有 PCM 容量。内存预算不足时会报告解码阶段所需字节。

`start` 指定从第几秒起播，`audio.position(sound)` 读取当前内容位置秒数；加载中返回请求起点，暂停时保持，结束后仍可读取最后位置。`looping: true` 可配合 `loop_start` 和 `loop_end` 指定循环区间，默认从 0 循环至音轨结尾。start 在 loop_start 之前会先播放前奏，达到 loop_end 后回到 loop_start；start 超过 loop_end 会折回循环区间。循环终点不包含在区间内，边界必须位于资源内并至少包含一帧。未开启 looping 时使用默认循环边界。

`audio.seek(sound, seconds)` 定位同一个声音，返回 null，不需要 await。时间单位为秒且必须有限非负；暂停时定位仍保持暂停，正在播放则继续，增益与循环设置保持。加载中修改起播位置；非循环超出结尾时停在结尾并自然结束，暂停的声音等恢复后结束；循环的定位规则与 start 一致。连续定位采用最新目标，`audio.position` 和紧接的存档立即记录它，混音器应用后再跟随实际进度。已经结束的活动不会重新播放；`await sound` 始终等待原来的播放终态。

切换同一进度的音乐可以这样组合，淡入从新音轨完成加载并开始播放时计时：
```aside
let next = audio.play("assets/music-next.ogg", group: "music", looping: true,
    start: audio.position(music), loop_start: 4, loop_end: 64, fade_in: 2);
audio.stop(music, fade: 2);
music = next;
```
position 是显式读取，不会让界面持续刷新。存档记录音乐的内容位置与循环区间，读档后继续播放；当前语音重新起播，瞬时音效不重放。

分组音量用 `audio.group_volume("music", value)` 设置，可选 music/voice/effects/ui；master 控制总音量。每声、组、总增益相乘，设置也影响之后开始的声音。`audio.pan(sound, value, duration: seconds)` 将声像从 -1（左）移动到 1（右），0 居中；`audio.volume` 和 `audio.group_volume` 同样接受 duration，省略时立即生效。`fade_in` 从实际播放开始计时；`audio.stop(sound, fade: seconds)` 淡出结束后才让 await 返回 Stopped，加载中或已暂停的声音立即停止。暂停保留播放位置，音量/声像包络继续推进；输出挂起冻结全部包络。淡出停止期间再暂停会立即停止。音量过渡在分贝域进行，静音端点为 -60 dB；新命令从当前值接续，加载中的控制更新首次播放值。

缺省声音属于当前回调节点；在主剧情中属于会话。也可用 `owner: node` 指定：节点被移除时，加载与声音一起取消。取消等待者不停止共享声音。播放、节点取消与窗口退出复用同一资源回收路径。

Windows 播放器向系统提交双声道音频，默认设备配置为多声道时同样可以使用。

支持 WAV/PCM、Ogg/Vorbis、FLAC、MP3 预解码播放。PCM 按宿主内存预算计账，不另限时长。指定起播位置、定区间循环、播放中定位与存档恢复已接通；长音轨流式仍按完整设计推进。Completed 表示混音器消费完成，不能用于精确声卡同步。实体设备的验证范围见[音频验证](../../crates/reel-media/VERIFICATION.md)。

完整例子：[author_audio.aside](../../examples/author_audio.aside)，从 examples 资源根运行，与其他作者示例使用相同字体配置。示例 chime.wav 是仓库自制的短正弦提示音。

```powershell
./target/debug/reel.exe check examples/author_audio.aside
./target/debug/reel.exe dev examples/author_audio.aside --assets target/reel-artifacts/author-assets
```

### 窗口与呈现设置

使用自己的 `fonts/body.ttf` 运行 [author_window.aside](../../examples/author_window.aside)，资源目录可沿用上面的音频示例。

`window.info()` 返回当前客户区的逻辑 width/height、scale_factor、fullscreen、vsync、窗口模式的 windowed_width/windowed_height，以及 capabilities 和 resolutions。resolutions 是当前显示器可用尺寸换算为逻辑像素后的列表，不是舞台画布大小。先检查 capabilities.resize/fullscreen/vsync 再显示对应控件；无窗口宿主的能力均为 false。

```aside
let mut display = window.info();
if display.capabilities.resize {
    display = await window.configure(width: 1280, height: 720);
}
if display.capabilities.fullscreen {
    display = await window.configure(fullscreen: true);
}
```

configure 省略的项保持原值，返回活动，await 得到平台应用后的实际状态。fullscreen 是无边框全屏；全屏期间设置的尺寸用于退出后的窗口，不改变显示器模式。游戏可在全屏时禁用窗口尺寸选项。关闭垂直同步使用 `await window.configure(vsync: false)`，只改变呈现同步，空闲仍按需重绘。窗口状态不随剧情存档恢复，偏好可由 persistent 保存。另一项窗口更改尚未完成时会报 Busy；禁用正在执行的设置按钮即可。画布仍由 game.configure 独立配置。

### 图片粒子

`particles` 用一个舞台节点组合落雪、花瓣等图片效果。source 必须是完整 PNG/JPEG 精灵，分块 `.reel-image` 只供 `image` 节点使用；`emit` 启动并返回活动，width/height 指定出生矩形。示例使用你提供的 `assets/petal.png` 和 `fonts/body.ttf`，完整入口见 [author_particles.aside](../../examples/author_particles.aside)。

```aside
let petals = particles("assets/petal.png", width: 1280, height: 20,
    rate: 80, lifetime: 8, seed: 42, velocity_y: 90,
    velocity_x: 15, spread_x: 30, size: 18, size_spread: 6,
    spin_spread: 60);
stage.replace(petals);
let falling = emit(petals, duration: 10);
await falling;
```

缺省值：rate 每秒 60 粒，burst 0，lifetime 5 秒，lifetime_spread 0，seed 0，velocity_x/y 为 0/80，spread_x/y 为 20/20，acceleration_x/y 为 0/0，size 12，size_spread 4，size_end null，opacity_start/end 为 1/0，spin 0、spin_spread 每秒 30 度。spread 表示基值两侧均匀取样的半幅；size 为图片宽度，保留原宽高比，size_spread 不得超过 size，lifetime_spread 不得超过 lifetime。每粒寿命按种子独立取样；size_end:null 保留每粒随机出生宽度，非负终值使宽度在该粒寿命内线性变化。出生位置在 emit 启动时读取的 width/height 内均匀分布（未指定时为零）；burst 粒立即出生，随后每 1/rate 秒增加一粒。配置只读；共同位置、旋转、缩放、可见性、透明度及裁剪仍可更新和动画。

省略 duration 持续发射；有限时长包含截止时刻出生的粒子，等待最后一粒实际死亡后返回 Completed。rate:0 只产生瞬发粒子并自然结束；rate:0 且 burst:0 立即完成。duration:0 仍产生 burst。`pause(falling)` 冻结整个效果，`resume(falling)` 继续，`stop(falling)` 清空粒子并使 await 返回 Stopped。`finish(falling)` 清空有限效果并返回 Completed，持续效果不接受 finish。同一节点再次 emit 停止原活动，从同一种子重新开始；同一种子和累计时间与分帧无关。节点或活动 owner 退休取消发射，隐藏不暂停。缺省 owner 沿用其他活动规则；取消等待者不取消生产者。终态保留以供重复等待。粒子共用舞台图片资源和绘制路径，不逐粒子运行脚本。存档恢复保留运行效果、累计时间和暂停状态。

短暂喷射可以组合瞬发、不同寿命和逐渐缩小的宽度：

```aside
let spray = particles("assets/petal.png", x: 640, y: 360,
    rate: 0, burst: 48, lifetime: 1, lifetime_spread: 0.4,
    velocity_y: -160, spread_x: 180, spread_y: 100,
    acceleration_y: 240, size: 18, size_spread: 6, size_end: 0);
stage.replace(spray);
await emit(spray);
spray.remove();
```
