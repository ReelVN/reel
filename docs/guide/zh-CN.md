# Reel 游戏开发指南

[English](en.md) · [文档目录 / Documentation index](../README.md)

## 阅读这份文档
Reel 的目标是让你用 Aside 表达想象中的画面和交互，组合自己的对白、UI、演出和嵌入的 2D 游戏。当前已有多文件作品、统一舞台、存读档和打包发布链路；完整 2D 创作接口、长内容成本和设备覆盖仍在完善。先从[基本画面](../../examples/author_stage/src/main.aside)、[动画](../../examples/author_activities/src/main.aside)或[交互按钮](../../examples/author_buttons/src/main.aside)开始；[能力表](../README.md#当前状态)列出实际边界，[总清单 #94](https://github.com/jhq223/reel/issues/94)记录后续顺序。

这份指南从作者想要的画面出发，说明如何用 Aside 写剧情、组合 UI、安排演出和扩展效果。已实现部分可以直接使用；其余章节描述目标设计，并标明尚未接通的能力。中文与英文共用相同示例和接口名称。《Reel 引擎架构设计》说明运行时，《Aside VM 与语言设计》定义语言规则。
作者使用三个基本概念：函数组织可复用内容；节点树表达画面；活动表达随时间进行的变化。当前普通函数、基础节点组合与顺序剧情已经接通，活动及高级作者接口仍按各节状态区分。

接口的参数、属性和用例由实现旁的英文源码文档维护；Aside 的 `///` 注释也可随声明查询。宿主声明提供可供编辑器读取的签名和说明，见 [host SDK](../aside-host.md)。本指南保留创作流程与组合示例。

## 一 最小项目与共同写法

### 安装工具

下载适合当前系统的 Reel 工具包，解压后将其中的 `bin` 目录加入 `PATH`，再打开终端。Windows 包包含 `reel.exe`、`reel-lsp.exe` 和 `reel-game.exe`：命令行负责检查、运行和打包作品，语言服务提供编辑提示，发布导出会复制同级播放器。作者使用工具包即可创作；从源码构建引擎时才需要 Rust 和 Cargo。

多个游戏目录共用这套工具。项目将脚本放在 `src/`，入口为 `src/main.aside`；资源放在 `resources/`，其中的相对路径就是脚本使用的资源路径。在项目根目录运行：

```powershell
reel check
reel dev
reel build
reel export --release
```

`build` 默认生成 `build/debug/game.asb`，`--release` 改用 `build/release/game.asb`；`export` 默认生成 `build/export/debug/`，`--release` 改用 `build/export/release/`。重复执行会更新默认产物。显式 `--output` 可另选位置，已有目标通过 `--overwrite` 替换。`resources/` 自动用于开发运行与导出，原脚本和资源保留在项目中。

### 在 VS Code 中编写

Aside 扩展提供语义着色、诊断、补全、参数提示、悬停说明和定义跳转。安装 VSIX 后打开 `.aside` 文件，或运行 **Aside: New File**。函数调用中按 Ctrl+Shift+Space 查看具名参数、类型和默认值；F12 跳转到作者源码，内置接口则打开由 Rust 声明生成的只读 Aside 定义。

扩展优先使用 PATH 中的 `reel-lsp`，Windows 扩展包也内含语言服务。`aside.server.path` 可指定其他服务端的完整路径；独立语言工程使用 `aside-lsp`。**Aside: Run Project** 保存文件并调用 PATH 中的 `reel` 启动当前项目，`aside.reel.path` 可另选 CLI，`aside.reel.assets` 指定资源目录。修改 PATH 后重新打开 VS Code。编辑检查直接使用未保存内容。

内置节点的共同类型是 `Node`，具体节点保留自己的成员提示。`bind` 和 `derive` 返回 `Binding`；属性提示分别显示读取的值类型和允许赋入的类型。

数组、tuple 和字符串的方法也提供参数与返回类型提示，F12 打开同一份内置声明。保存方法再调用时，参数提示和类型检查继续有效。

项目从 `src/main.aside` 的 `main` 启动。check/build/dev/export 从当前目录及祖先查找项目，run 执行已经构建的 .asb。可运行多文件工程见 [author_project](../../examples/author_project/src/main.aside)。标题页可用 game.start 切换到全新剧情会话。

保存为 `src/main.aside`，并提供 `resources/fonts/body.ttf`。下文命令会准备本地验证字体并运行工程。

```aside
fn main() {
    game.configure(title: "Window light", width: 960, height: 540,
        fonts: ["fonts/body.ttf"]);
    stage.replace(text("Hello", x: 24, y: 24));
}
```

### 格式化代码

保存 Aside 文件时，扩展默认自动格式化；也可执行 VS Code 的“格式化文档”。在项目目录运行 `reel fmt` 可整理 `src/` 下全部 Aside 文件，`reel fmt --check` 只检查排版。两者共用同一格式化器：4 空格缩进、100 字符目标行宽，短调用保持一行，长调用紧凑排列参数，长方法链按操作换行。字符串、数字和注释内容保持原样；语法错误修正后再格式化。可在 VS Code 的 Aside 语言设置中关闭保存时格式化。

### 模块与共同写法

Reel 自动提供当前已注册的基础节点与剧情服务：text、image、button、row、column、stack、stage、narrator 和 choose，每个源码文件都可直接使用。作者的模块声明、局部绑定和显式导入优先于这些默认名称。若定义了自己的 text 函数，可用 `use reel::prelude::text as reel_text;` 为 Reel 构造函数另取名称。显式 prelude 导入仍是有效的普通导入。这套默认环境由 Reel 提供，独立 Aside 不包含 Reel 名称。动作和完整业务服务仍按后文设计。作者用自己的函数构造页面和外观。所有文档统一使用 fn、let、let mut、|参数| 闭包和 use 导入。除完整模块与函数示例外，独立代码片段放在正在运行的函数内部，沿用前文变量。普通语句写分号，函数末尾表达式可直接返回；对象字面量写 {字段: 值}。以下导入使用已实现的模块组织；标题按钮中的 game.start 已实现。
```aside
use game::ui::title::title;

fn main() {
    game.configure(title: "窗边", width: 1920, height: 1080, fonts: ["fonts/body.ttf"]);
    stage.replace(title());
}
```
上例的 src/ui/title.aside 可以直接用作者自己的节点组成：
```aside
use game::story::opening::opening;

pub fn title() {
    column(x: 760, y: 480, gap: 24) {
        text("窗上的光");
        button("新游戏", on_click: |event| game.start(opening));
    }
}
```
src/story/opening.aside 中的 opening 见第三节。示例的相对文件位置与导入路径一致，根模块 src/main.aside 只负责启动。
let 固定变量绑定，节点属性仍可修改；需要把变量改绑到另一个值时使用 let mut。普通表达式在执行时求值一次。bind(model.value) 连接可观察属性，derive(|| expression) 连接由其他属性计算的只读值。语言的完整规则见第二份文档。

桌面播放器遇到可恢复的图形设备丢失会重建渲染设备并保留当前剧情状态；诊断包含后端给出的丢失原因。无法重建设备时报告错误并退出。

### 按创作单元组合项目

`src/main.aside` 直接配置作品、创建共享状态并开始流程。随着内容增长，按章节、菜单、人物演出或小游戏拆分模块；一个单元的画面、局部状态和行为一起维护。普通函数可以组合对白、临时控件、全屏画面和小游戏，按书写顺序推进，并将结果交回调用方。

简单画面直接返回节点；需要持续操控的组件可返回具有明确方法的类实例。等待玩家操作的页面用普通函数返回选择。共享进度与设置由调用方传入，局部状态留在使用它的组件中。参数声明实际类型，使跨模块调用保留检查、补全和定义跳转。

流程用普通函数、条件和循环表达；同一会话内调用另一个页面，真正重开会话时使用 `game.start`。节点、对白和动作优先沿对象链式组合，长链按操作换行。用 `scope` 管理一段交互中的任务、演出和临时节点；函数正常返回时结束这些活动，按钮也可调用 `s.finish(result)` 提前结束并交回结果。保存和退场动画应在 finish 前等待，或在 scope 返回后执行。出现真实复用后再提取公共组件。

### 当前可用的检查、构建与运行

编译与 ASB 加载默认不设置作品大小、累计记录、模块或指令数量配额；实际格式宽度和可用内存仍适用。嵌入宿主可通过 Rust Limits 显式设定配额。出现容量错误时，诊断会区分记录数和内存请求，避免把较长剧本误报为语法错误。
桌面播放器等首个挂载场景的字体与图片就绪后再呈现；支持隐藏窗口的平台在首帧接受后显示窗口。需要自定义加载界面时，先用普通舞台节点挂载它，再安排后续任务。启动期间的错误仍报告原始源码位置。

示例在 main 中配置 fonts/body.ttf。从仓库根目录运行下列 Windows 命令，将微软雅黑复制到临时资源目录供本机验证；发布作品时应提供有分发授权的字体。运行 dev 或 run 后关闭窗口，再继续下一条命令。

```powershell
New-Item -ItemType Directory -Force target/reel-artifacts/author-assets/fonts | Out-Null
Copy-Item C:/Windows/Fonts/msyh.ttc target/reel-artifacts/author-assets/fonts/body.ttf
Copy-Item -Recurse -Force examples/assets target/reel-artifacts/author-assets/
Push-Location examples/author_stage
reel check
reel dev --assets ../../target/reel-artifacts/author-assets
reel build
reel run --assets ../../target/reel-artifacts/author-assets
Pop-Location
```

check 编译并报告源码错误，不运行剧情；加 `--format json` 可取得带 UTF-8 范围与行列位置的诊断。build 生成单个 .asb，包含字节码、模块初始化与选定入口。两者提供与播放器相同的默认 Reel 名称、已注册 native 与宿主默认参数。

入口固定为 `src/main.aside` 中的 `main`。缺少入口或入口无效会在检查时报告；入口可以私有，且须能无参数调用。run 直接加载包中的初始化函数和入口，不重新编译源码。默认 build 产物随构建更新，显式输出通过 --overwrite 替换。

build 默认使用 Debug，完整源码与映射保存在包内，运行错误能指向原来的 .aside 文件。发布时加 `--release`，只生成去除源码、指令映射和声明位置的 ASB，减少文件体积及加载后驻留的调试数据。两种模式的语言行为和运行检查相同；Release 仍报告错误类型、函数和指令位置，保留执行需要的符号与文本常量。单独 build 的剧情文本保留在 ASB 内。现有执行存档对应实际 ASB，同一源码的 Debug 与 Release 存档不互换。

```powershell
Push-Location examples/author_stage
reel build --release
Pop-Location
```

工具包中的播放器为源码与构建产物提供真实窗口、文字布局和呈现输入路径。同步构造组件在同一运行时间片内连续推进，到等待或时间片结束才更新画面，不会每创建一个节点就单独显示一帧。对白或选择等待时会先完成当前 GC，再休眠等待输入；自然结束后窗口保留最后画面，关闭窗口退出并完成清理；Escape 作为返回输入交给当前 UI。

工程只读取入口实际导入的源码。`game::ui::scene` 对应 `src/ui/scene.aside`；共享代码复制到 `src/` 后沿普通 `use` 导入。函数循环引用允许链接，顶层读取未初始化绑定在读取处报 ModuleInitializationCycle。build 把完整模块图和入口放入同一 .asb，初始化设置由同一段 main 执行。开发运行使用项目的 `resources/`；`--assets` 可指定其他资源目录。导出包使用内嵌的资源清单。

源码、完整源码定位文本和文字常量统一使用实际包预算。长文本和拼接按执行片推进，分片不会截断文字，也不规定台词长度。

左结合运算和属性、调用链按书写顺序编译，不按链条长度消耗嵌套额度。括号、代码块和子表达式的真正嵌套仍使用宿主配置的编译资源预算。

### 导出开发工程和发布目录

`reel export` 在同一次构建中组织代码和资源；默认导出开发工程，`--release` 导出发布目录。先按前面的示例准备字体，再执行：

```powershell
Push-Location examples/author_stage
reel export --resource .=../../target/reel-artifacts/author-assets
reel export --release --resource .=../../target/reel-artifacts/author-assets
reel run build/export/release/game.asb
Push-Location build/export/debug
reel dev
Pop-Location
Pop-Location
```

开发目录包含可修改的 `src/`、`resources/` 和带源码诊断的 `game.asb`；进入目录后用 `reel dev` 运行源码，或用 `reel run game.asb` 运行构建产物。发布目录包含 `game.exe`、`game.asb`、`game.rtp` 和 `resources.rpk`（资源较多时还有后续卷）。将整个目录交给玩家，双击 `game.exe` 即可开始；其他平台使用对应可执行文件。清单内嵌于 `game.asb`，所有资源路径相对主包目录。

发布播放器从自身目录寻找 `game.asb`，存档默认写入同级 `saves/`；`--save-directory <directory>` 可另选位置。更新游戏时保留 saves。发布导出使用 reel 工具同级的 reel-game 播放器。工具包将播放器与 CLI 放在同一目录。ASB 使用 v1 格式。

`--resource prefix=directory` 把一个目录映射到脚本中的逻辑路径；可重复声明。`--resource .=directory` 挂载到资源根，`--text prefix=directory` 把额外文本资源放入文本包。译文写在源码的 `tr` 调用中，随项目参与检查、开发运行、构建和导出。开发导出保留这些源码，发布导出将语言内容装入文本包；脚本通过 `localization.set_locale("en")` 选择语言。

资源包默认按 2 GiB 目标体积自动分卷，包头和索引计入体积；可在发布命令添加 `--pack-bytes 1073741824`，改为 1 GiB。分卷沿完整资源边界进行，单个大资源独占一卷；后续文件名为 `resources-002.rpk` 等，清单自动列出，脚本路径无需修改。挂载只读取索引，资源内容按范围读取。

发布时原文常量移入 `game.rtp`，ASB 不携带完整源码。原文池整体驻留，字符串按使用共享引用。修改原文或译文后重新构建，文本包与 ASB 一起更新。图片、音频和视频按资源包索引读取。

发布默认关闭散文件覆盖；需要模组或调试覆盖时显式 `reel run game.asb --assets overrides`。清单里的包按顺序后者覆盖前者，散文件优先。`--overwrite` 会替换整个既有导出目录，旧存档与额外文件不复制到新导出；运行数据应单独保存。输出可位于项目的 `build/` 中，须避开实际源码文件与资源目录。

### 范围、容器与插值
在函数里可以用范围循环生成内容，再用插值展示结果。范围不会先创建一个大数组；`..` 不含终点，`..=` 包含终点。数组支持 `append`、`pop` 和 `len`，表支持 `has_own`、`get`、`remove` 和 `len`。插值当前支持字符串、数字、布尔值和 null。
```aside
let values = [];
for value in 1..=3 { values.append(value); }
let latest = values.pop();
stage.replace(text(f"剩余 {values.len()} 项，最后一项 {latest}"));
```

`table.get(key, default: null)` 仅在键缺失时使用缺省，已有的 null、false、0 都保持原值，不添加字段；返回的对象仍是原对象。default 是普通实参，会照常求值。它也可用于 derive，随后插入或修改字段会更新结果。完整用法见 [collections.aside](../../examples/collections/src/main.aside)。

```aside
let settings = {volume: 0, wallpaper: null};
let volume = settings.get("volume", default: 0.7);
let wallpaper = settings.get("wallpaper", default: "default.png");
let language = settings.get("language", default: "zh-CN");
let removed = settings.remove("wallpaper");
settings.wallpaper = "new.png";
```

`table.remove(key)` 返回被删除的值，缺键时报错；其余字段保持顺序，同名重插排在末尾。用于 model 时，保留字段的 bind 和控件写回继续有效，被删除字段的连接冻结最后值，重新插入后需重新 bind。通过 get/has_own 读取的 derive 会重新计算。大型删除按执行预算推进，取消时保留原表；存档能恢复未完成的操作。[collections.aside](../../examples/collections/src/main.aside) 演示删除颜色字段后使用默认颜色。

`array.sort(compare: null)` 稳定原地排序并返回原数组。缺省按数值（可混合 int/float）或字符串排序；记录使用返回负数、零、正数 int 的比较器。同序元素保留原来的顺序。比较器不能等待。排序按 VM 执行预算推进，成功后才提交；异常、取消或来源数组被修改时不发布工作副本。见 [collections.aside](../../examples/collections/src/main.aside)。

数组和 tuple 用 `items.find(value)` 查找首个相等元素的索引，未找到返回 null；只需判断存在时用 `items.contains(value)`。比较规则与 `==` 相同，记录按对象身份比较。查找期间修改来源数组会报告 ConcurrentModification。见 [collections.aside](../../examples/collections/src/main.aside)。

```aside
let cuts = [180, 20, 90];
cuts.sort();
let captions = [{text: "后景", layer: 2}, {text: "前景", layer: 1}];
captions.sort(compare: |a, b| a.layer - b.layer);
```

数组展开 `[...items, last]` 接受数组或 tuple；表展开 `{...base, color: value}` 复制自有字段，后项覆盖前项。展开按书写顺序捕获内容，后面的代码修改来源不会改变已捕获的元素或字段；内部对象仍共享引用。null 不能展开。完整可运行示例见 [collections.aside](../../examples/collections/src/main.aside)。

```aside
let base = {message: "窗上的光", x: 32, y: 48};
let opening = [base];
let captions = [...opening, {...base, message: "第二句", y: 90}];
stage.replace(stack {
    for caption in captions {
        text(caption.message, x: caption.x, y: caption.y);
    }
});
```

### 无窗口示例

通过[最小 Aside 剧情](../../crates/reel-engine/examples/story_contract.aside)及其[无窗口宿主](../../crates/reel-engine/examples/story_contract.rs)运行：

```sh
cargo run --locked -p reel-engine --example story_contract
```

## 二 让代码表达整幅画面
### 画布与字体

在 main 中调用 game.configure(title: "窗边", width: 960, height: 540) 设置标题与逻辑画布，参数可以使用普通表达式。省略项保留原值；未设置画布时沿用窗口空间，设置后按 contain 等比缩放并留边，绘制与输入使用同一换算。首设一个维度时，另一个维度采用 1920×1080 的对应值。桌面窗口默认匹配首次配置的画布尺寸；未配置画布时为 1280×720。Rust 宿主可用 StoryWindowOptions.logical_size 显式指定其他初始尺寸。首次应用画布后不再自动调整窗口，玩家改变窗口尺寸时仍按 contain 缩放。fonts 接受字体文件字符串或按优先顺序排列的数组，例如 game.configure(fonts: ["fonts/body.ttf", "fonts/cjk.ttf"])；路径相对资源根，作品须在首次呈现前提供至少一个字体文件。text 和 button 的 font 属性选择字体族或逗号分隔的回退列表，例如 text("标题", font: "Noto Serif SC, system-ui")，font_size 控制大小。`font_weight` 选择字重，默认为 400，接受 1 到 1000 的有限数值；例如 `text("标题", font: "HK Grotesk", font_weight: 600)` 从已配置的该字体族中选择合适字重。优先使用作品提供的对应字重；只有 Regular 字体时，请求粗体会使用合成加粗，普通文本、逐字显示及描边沿同一字形路径。text、控件标签和 text_input 共用该属性，可以直接赋值、bind 或 derive；更改后重新排版。富文本 `[b]` 将对应文字局部设为 700。字体文件共享加载；system-ui 表示配置的默认列表。播放器只使用作品提供的字体，不读取系统字体；未配置字体会报错。

### 组织组件数据

读取或写入表的单个字段时，其他任务或控件可继续更新已有字段值；查找完成时使用当前槽。普通 table 和 model 遵循同一规则。增删字段会中断尚未完成的查找并报告 ConcurrentModification。复制、展开和删除等批量操作仍要求来源内容在操作期间保持一致。

内存占用随仍存活的剧情数据和资源增长。及时释放已退休内容的引用，并限制长期缓存；嵌入宿主可显式设置资源预算。

固定长度 tuple 可以在 let 和 for 中解构，支持嵌套和 `_` 忽略项；先准备所有值再建立名称，结构不符抛出 PatternMismatch。每轮循环的闭包保留当轮值。数组/对象/rest 解构及解构赋值仍待实现。见 [tuple_bindings.aside](../../examples/tuple_bindings/src/main.aside)。

```aside
let settings = {language: "zh-CN", volume: 0.8};
for (key, value) in settings.entries() {
    let pair = (key, value);
    let (name, _) = pair;
}
```

`match` 可以按值选择对白、界面或处理逻辑，支持字面值、名称、`_`、OR 和类型测试，guard 写在 `if` 后。只执行第一个匹配且 guard 为真的分支，分支末尾表达式提供结果；没有匹配时抛出 MatchError。结构解构尚未支持。完整例子见 [match_values.aside](../../examples/match_values/src/main.aside)。

```aside
let caption = match mode {
    "online" | "typing" => "在线",
    name is string if name != "" => name,
    _ => "离线",
};
```

数组支持插入、删除、扩缩、反转、扩展、清空和交换，tuple/array 可按范围切片。表的 keys/values/entries 返回自有字段的新数组；entries 每项是 `(key, value)`。批量编辑在成功时一次生效，原有数组引用保持有效；resize 的填充值在新增位置共享，不自动深复制。
完整可运行示例见 [collection_editing.aside](../../examples/collection_editing/src/main.aside)。insert/remove 使用从零开始的非负索引；slice 接受 `begin..end` 或 `begin..=last`，不自动截断越界范围。remove 返回删除项，top 返回末项，其余修改方法返回原容器。

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

文本可用 byte_len 取得 UTF-8 字节数、scalar_count 取得 Unicode 标量数。slice_bytes 和 slice_scalars 使用显式范围，越界或截断 UTF-8 标量会报错；组合字符和 emoji 可能包含多个标量，标量切片不代表字素切片。starts_with/ends_with 精确匹配前后缀，空字符串总是匹配。`separator.join(items)` 连接字符串数组或 tuple，空集合得到空字符串，其他类型不会自动转成文字。长文本按执行预算推进，连接过程中修改来源数组会报错。完整示例见 [text_operations.aside](../../examples/text_operations/src/main.aside)。

`text.split(separator)` 按完整分隔字符串切分并返回字符串数组，保留首尾和连续分隔产生的空项。空分隔字符串按 Unicode 标量切分，仍保留首尾空项。例如 `"/角色//纱世里/".split("/")` 得到 `["", "角色", "", "纱世里", ""]`。扫描和片段复制按执行预算让出，不会先复制整段输入。

to_ascii_lowercase()/to_ascii_uppercase() 只转换 ASCII 字母，保留中文等其他文字。例如 "Emily你好".to_ascii_lowercase() 得到 "emily你好"，可用于 ASCII 标识或账号的比较。

`trim()/trim_start()/trim_end()` 移除两端、开头或末尾的 Unicode 空白，保留名字或正文内部的空格。例如 `let player_name = "\u{3000} 空 山 \u{a0}".trim();` 得到 `"空 山"`；全为空白时得到空字符串。参见 [text_operations.aside](../../examples/text_operations/src/main.aside)。

已注册的函数也可以保存和传递，例如 `let say = narrator; say("你好");`，等待行为与直接调用一致；函数缺省参数也可以是 `text` 等构造函数。可以用 `(int,int)` 约束坐标，用 `(int,int)|null` 表达可省略的坐标；`is`、参数、返回值和变量赋值都会检查相应类型，失败赋值保留旧值。完整可运行示例见 [function_values.aside](../../examples/function_values/src/main.aside)，使用前面的 `dev` 或 `build` 命令替换输入路径即可。

带尾随子节点块的组件调用作为独立语句时可省略结尾分号；块后换行结束该语句，下一行括号表达式不会被接到前一个调用上。普通调用和属性赋值仍使用分号。显式分号不改变子节点收集。

已实现：text、image、row、column、stack 与 stage.replace 使用同一节点模型。普通函数可以返回节点；尾随 children 块、if 与 array/tuple 上的 for 可以组合节点。value、x/y、width/height、color/background、opacity、visible 和 clip 等属性直接修改原节点，挂载后的身份保持不变。替换舞台会退休旧树，释放旧节点的内容；随后读写旧节点属性会报告错误。完整代码见[作者舞台示例](../../examples/author_stage/src/main.aside)。

组件函数用末尾表达式或 `return` 返回节点。普通函数里的 `text("Title");` 会丢弃构造结果，编译器在该调用处报错；改成末尾的 `text("Title")` 即可返回。children 块内的分号照常收集节点，返回 null 的可选组件照常省略。诊断识别静态已知的节点构造器及其不可变别名；动态调用和普通包装函数仍按各自的返回值处理。确实需要丢弃构造结果时写 `let _ = text("Title");`。

```aside
fn heading(title) {
    text(title, font_size: 28)
}
fn hint(message) {
    if message == "" { return null; }
    text(message, color: "#aab8cb")
}
let panel = column(gap: 12) {
    heading("Sound");
    hint("");
    hint("Adjust the music volume.");
};
stage.replace(panel);
```

可运行的 [author_stage.aside](../../examples/author_stage/src/main.aside) 同时使用返回节点的标题组件、可选提示和尾随 children。

stack 按书写顺序从后往前叠放子节点，row 横排，column 竖排。未指定宽高时按内容布局；x/y 在布局位置上叠加，逻辑坐标原点在左上，x 向右、y 向下。默认字体大小为 24、文字白色、背景透明，gap/padding 为 0；clip 限制容器的可见区域。`scroll(height: 360) { ... }` 创建纵向滚动区域；axis 可设为 "horizontal" 或 "both"。滚轮、触摸拖动和键盘/手柄焦点跟随共用正常输入；滚动不会重建文字布局。`follow_end: true` 使已处于末端的 NVL 页跟随新内容，上滚阅读后保留位置。

row/column 中，子节点的 `flex: 1` 占剩余流轴空间，多个正数按权重分配；默认 0 保留自然或显式尺寸。容器 `wrap: true` 按可用空间换行，`line_gap` 设置行距，`justify` 分配空白。规则排列使用 grid：columns 指定列数，column_width/row_height 可指定固定格子尺寸；省略列宽时均分已知宽度，省略行高时按各行内容测量，column_gap/row_gap 默认沿用 gap。

```aside
column(gap: 24) {
    row(width: 640, gap: 12) {
        text("Lucy", width: 120);
        text("The rest of this row belongs to the dialogue.", flex: 1);
    }
    grid(columns: 4, column_width: 140, row_height: 80, gap: 12) {
        for slot in 0..12 { button(f"{slot + 1}"); }
    }
}
```

用共同属性 `stretch: "both"` 让背景图片跟随内容撑开的容器，无需计算文字行数或回写高度。`"x"`、`"y"` 只填满父 padding 内的对应轴，默认 `"none"`。被拉伸轴不参与父尺寸测量，并覆盖该轴的 width/height；x/y、anchor 仍有效。在 row/column 流轴上拉伸时，节点成为覆盖层，不增加位置或 gap；scroll 中按视口内框拉伸，不扩大该轴的滚动范围。

```aside
stack(max_width: 420) {
    image("images/panel.png", stretch: "both", slice: 12);
    column(padding: 24, gap: 16) {
        text("The frame follows wrapped text and buttons.", align_x: 0.5, text_align: "center");
        button("Dismiss", align_x: 0.5, on_click: |event| event.target.remove());
    }
}
```

完整示例：[author_stretch.aside](../../examples/author_stretch/src/main.aside)。准备 `fonts/body.ttf` 和 `images/panel.png`，运行 `reel dev`（在 `examples/author_stretch/` 中）；按钮切换内容时，背景保持身份并跟随新高度。

`max_width/max_height` 限制节点的自然或显式尺寸，默认 null；长文本会在可用宽度内换行，短文本仍按内容收缩。上限含 padding，stretch 对应轴仍填满父内框。`align_x/align_y` 默认 0，取 0 到 1：0 为起端，0.5 居中，1 为末端，按父 padding 内的剩余空间定位；stack 两轴生效，row/column 只在非流轴生效，scroll 只在非滚动轴生效。它们不改变 x/y 与 anchor 的变换含义，可绑定或动画。示例 [author_stretch.aside](../../examples/author_stretch/src/main.aside) 的弹窗随内容改变宽高，背景伸展、按钮保持居中。

纵向 scroll 内的 `width` 与 `max_width` 仍约束子节点，`align_x` 使用剩余宽度；横向 scroll 对应 height/max_height/align_y。需要填满非滚动轴时使用 stretch。例如宽 1000 的纵向 scroll 内，宽 840、align_x 为 0.625 的 list 从左侧 100 像素处开始；普通与虚拟 list 相同。

`view` 的内容按 stack 布局，可同时使用 `align_x/align_y`。input 可以嵌入普通容器；没有焦点时，最前方可见且启用的内层 input 接收键盘和手柄输入，模态面板和已聚焦编辑框仍优先。调整 `game.configure(text_speed:)` 不会中断正在拖动的控件。窗口、画布或父节点放大文字时，WGPU 按实际显示倍率生成并缓存更高分辨率字形，不改变逻辑排版。

### 自定义鼠标光标

`stage.pointer()` 取得鼠标在当前已呈现舞台中的 `{x,y}`，坐标与 `stage.geometry(node)` 一致。鼠标离窗、失焦、触摸/键盘手柄导航或尚无呈现时返回 null。需要移动真实鼠标时用 `await stage.move_pointer(x, y, owner: panel)`；panel 须可用且位于当前模态范围，目标须在窗口客户区。成功返回 true，当前条件不允许或宿主不支持返回 false；支持情况可读 `window.info().capabilities.pointer_move`。节点退休会撤销其请求，读档后等真实鼠标输入再取得位置。示例：[author_pointer.aside](../../examples/author_pointer/src/main.aside)。

将 `cursor` 作为需要改变鼠标外观的面板或控件的普通子节点。图片、文字和动画继续使用舞台节点，鼠标移动由宿主跟踪。容器 x/y 是热点偏移，下面的图片 anchor 将图像中心放在鼠标位置：

```aside
let pointer = cursor {
    image("images/pointer.png", anchor_x: 0.5, anchor_y: 0.5);
};
panel.add(pointer);
```

光标外观使用窗口逻辑坐标，位于场景上方，不受面板的相机、缩放和裁剪影响；其自身的变换、透明度和动画照常工作。光标子树不占父布局，不接收输入或焦点。鼠标经过多个作用范围时，最近的面板优先，同一面板最后添加的可用光标优先；拖动期间沿捕获控件确定范围。`cursor()` 隐藏系统光标；设置 `pointer.visible = false` 或移除它恢复祖先或系统光标。面板隐藏、禁用、退休或退出当前模态范围时失效；鼠标离窗、窗口失焦或切换触摸也会撤销。存档保存外观和动画，恢复后等待真实鼠标输入，不恢复旧指针位置。完整可运行示例：[author_cursor.aside](../../examples/author_cursor/src/main.aside)，包含普通动画、隐藏和移除。

### 分层图片

图片的 `color` 用于乘色：`image("assets/panels.png", color: "#ffaaaa")` 让原图偏红，白色保持原样。透明度同时受颜色 alpha 与 opacity 控制。普通图片、分块大图及每个分层描述使用相同规则；给容器设置 color 不会改变子层。使用 bind/derive 调整悬浮等状态的颜色，无需为每种颜色准备或上传一份图片。

`image("assets/pattern.png", width: 1280, height: 720, repeat: "both", tile_scale: 1)` 以图片原始像素大小平铺。repeat 可选 none、x、y、both；重复时不使用 fit，不能同时指定 slice，也不能使用分块 `.reel-image`。tile_scale 必须为正数。`corner_colors: ["#ffffff", "#ffffff", "#ffaaaa", "#ffaaaa"]` 为左上、右上、右下、左下颜色，在节点内容框内双线性渐变并乘以 color；可与普通、九宫格或分块图片组合，不复制纹理。

完整示例：[author_image_patterns.aside](../../examples/author_image_patterns/src/main.aside)。准备 `fonts/body.ttf` 和 `images/pattern.png` 后运行 `reel dev`（在 `examples/author_image_patterns/` 中）；切换纹理比例复用同一张图片。

图片本身不接收指针输入，作为轨道、滑块或其他控件皮肤时不会挡住下方控件。需要图片可点击或拖动时，将它放入 button 或 input，由容器处理鼠标和触摸。

Rust 宿主可用 `ImageSource::empty()` 表示尚未到达的画面，再以 `ImageSource::resource(lease)` 更新同一个 image 节点。直接资源保留普通图片的 fit、裁剪、变换、颜色和透明度；同尺寸更新只重绘，不重新布局。`ResourceRegistry::register_yuv420` 接收生产者持有的 8 位 YUV420 平面及 BT.601/709、范围和传递函数，不复制为 RGBA。`release(handle)` 放弃注册表持有权后，已捕获画面仍保留其租约；播放时钟和解码由宿主负责。可运行的 CPU 合同检查：`cargo test -p reel-player --lib decoded_frames_preserve`。

`layered_image` 将图片描述组合成一个普通舞台节点；模型依赖变化时才重新计算。数组位置是稳定图层槽，按从后向前的顺序叠放；null 隐藏槽内原有图片，缩短数组会退休尾部槽。每份描述可使用 source、位置、尺寸、fit、color、opacity 等图片属性；省略的属性恢复 image 默认值。描述中的 bind/derive 值作为读取依赖。未变化的属性保持不动，图片节点复用，因此切换表情不会重建身体图层。

例如 `layered_image(|| [body, faces[state.face]], width: 320, height: 180)` 用 state.face 选择前景描述，同时共享 body 描述；给 state.face 赋值即可切换表情。

整体位置和动画直接作用于返回节点，选中图层和依赖沿用普通执行快照。姿势名称、服装与表情组合仍是游戏数据，图层和姿势没有固定数量限制；每帧不会额外合成图片。完整示例见 [author_layered_image.aside](../../examples/author_layered_image/src/main.aside)，同其他图片示例一样需提供 fonts/body.ttf。

### 随机选择

`let rng = random();` 创建一个独立随机流；`rng.int(0, 10)` 返回 0 到 9 的整数，`rng.float()` 返回大于等于 0、小于 1 的小数，`rng.pick(items)` 从非空数组或 tuple 返回一个原元素。`random(seed: 42)` 可复现同一调用序列，保存 `let pick = rng.pick` 仍使用原随机流。先在事件或剧情中抽取，再把结果写入 model；不要在 derive 中抽取。抽取失败不推进状态。示例见 [author_random.aside](../../examples/author_random/src/main.aside)。

`use std::math::{sin, cos};` 后可用 `cos(angle) * speed` 和 `sin(angle) * speed` 计算二维速度；angle 使用弧度。还提供 tan/asin/acos/atan、atan2(y,x)、sqrt/hypot(x,y)、pow(base,exponent)、exp/ln/log2/log10、abs/floor/ceil/round、radians/degrees。它们是可用于 derive 的纯 float64 运算，不创建脚本对象；round 中点远离零，无效数学定义域按浮点规则得到 NaN，溢出可得到 Infinity。示例见 [author_math.aside](../../examples/author_math/src/main.aside)。

`random` 来自独立的 Aside 标准库，Reel 自动导入，也支持显式 `use std::random;`。在 main 或事件函数中创建随机流；模块初始化与 derive 沿用现有副作用约束。固定种子的复现范围为同一标准库版本。

随机流的类型是 `std::Random`，曲线的类型是 `std::math::Curve`，可用在组件的参数与字段中。编辑器会补全其真实方法并提供英文说明。接受数组或任意 tuple 的参数可标为 `array | tuple`；需要固定结构时使用 `(int, string)` 等具体类型。

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

`caption()` 保留 `word` 接收者，适合传给回调。继承写 `class Child: Parent`，覆盖方法用 `super.method()` 调用词法基类；派生类未声明 `init` 时继承基类构造，声明后可显式调用一次 `super.init(...)`。`static let` 和 `static fn` 属于类。构造结束必须完成所有字段初始化，失败或取消后，已发布的实例引用与方法均报告 ConstructionFailed。类的字段布局固定，派生类通过方法扩展行为。可运行示例见 [author_classes.aside](../../examples/author_classes/src/main.aside)。

`let state = model(MyState());` 复制已经构造好的实例数据，保留其类型、方法以及内部共享引用，不重新执行构造。使用 `bind(state.field)` 连接控件，使用 `derive(|| state.caption())` 组合显示内容；方法修改字段会更新对应视图。只读字段只能单向读取，可写字段的控件回写仍须符合类型。后续赋入的实例保留共享身份。默认数据中的闭包和资源仍不能复制，静态字段也不属于实例状态。示例见 [author_class_models.aside](../../examples/author_class_models/src/main.aside)。

### 在运行中修改组件

用 `.with(...)` 把内容和外观按创作顺序组合；它也可以更新已有节点，返回的仍是原节点。省略的属性保持原值，相关属性按本次配置一起检查，例如滑条的 min、max 和 value。

```aside
let caption = text("窗外下起了雨。")
    .with(font_size: 28, color: "#eeeeee")
    .with(x: 48, y: 420, max_width: 720);
caption.with(opacity: 0.5);
```

编辑器将可省略的配置属性显示为 `opacity?: int | float | Binding`；省略时保留原值。

字段名由数据决定时，可以写 `state[field_name]`；字符串索引与点访问使用相同的类型、只读和绑定规则。`bind(state[field_name])` 固定选中的字段，需要随字段名变化重新选择时使用 `derive(|| state[field_name])`。

需要让演出从资源就绪后的画面开始，可先挂载节点，再 `await stage.presented(owner: node)`，之后启动动画。等待包含派生布局和图片、字体准备，以实际接受的帧为准；其他任务、音乐和动画继续运行。返回对象可重复等待，`.cancel()` 取消该请求，owner 节点退休也会取消。省略 owner 时沿用当前回调/任务节点，主剧情中归会话。见 [author_scene_entry.aside](../../examples/author_scene_entry/src/main.aside)：

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

节点保持原身份、父级和回调所有权。完整示例见 [author_composition.aside](../../examples/author_composition/src/main.aside)。

滚动区域的 `scroll_x/scroll_y` 可读写或 bind；布局会把位置限制在实际内容范围。`viewport_width/viewport_height/content_width/content_height` 是只读可观察尺寸，可在 derive 中计算自定义滑块的位置与长度。`content_align: "end"` 让不足一屏的聊天记录从底部排列，`"center"` 居中，默认 `"start"`。`input(on_pointer: ...)` 提供 `{target,x,y,phase}`，x/y 是呈现节点的局部坐标，phase 为 pressed/moved/released/cancelled；鼠标主键或触摸按下后捕获拖动，失焦、隐藏和禁用会取消。回调可等待并归输入节点所有。尚未开始的同节点、同处理器连续 moved 会合并为最新坐标，避免鼠标输入积压；已经开始的回调参数保持稳定，按下、释放和取消各自交付。参见 [自定义滚动条](../../examples/author_scrollbar/src/main.aside)。

`container.add(child)` 将子树追加到 row、column 或 stack，返回原 child；`index:` 可指定从零开始的插入位置，允许等于当前子节点数，省略或传 null 表示末尾。子树必须尚未挂到舞台且没有父级。重复挂载、成环、向 text/image 添加或位置越界都会报错，原树保持不变。添加和删除保留其他节点及其控件身份，未改变的文字不重新整形。

`container.move(child, index: ...)` 调整已有直属子节点的顺序，无须先移除。index 必填，表示调整后的零起始位置；三个子节点的合法位置为 0、1、2。方法返回原 child，移到原位置不会改变场景。节点的属性、正在运行的动画、回调、焦点和已加载资源继续保留。stack 按新顺序从后向前叠放，row/column 按新顺序布局。非直属子节点或越界索引会报错，原树保持不变。

```aside
let back = image("assets/panels.png");
let front = image("assets/sun.jpg", x: 120);
let scene = stack { back; front; };
stage.replace(scene);
scene.move(back, index: 1);
```

完整的 [author_reorder.aside](../../examples/author_reorder/src/main.aside) 展示重叠卡片置前，同时保留正在运行的动画。不支持跨父级移动。

`node.remove()` 移除并退休该节点及其全部后代，也可用于舞台根；`container.clear()` 只退休子节点，保留容器自身的属性和身份。两者返回 null。退休使相关动画取消并释放舞台对图片的请求；旧节点的属性读写和重新挂载会报告错误。需要再次展示的组件由函数重新构造。方法可以保存和传递，始终保留原接收者；`node.remove()` 及已保存的 remove 都可重复调用，清理已退休节点会立即完成。

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

完整示例 [author_dynamic_stage.aside](../../examples/author_dynamic_stage/src/main.aside) 结合组件函数、局部编辑和淡入动画，可直接使用第一节的 dev/build/run 命令。当前接口负责插入和退休；跨父级移动尚未实现。

用 `image(source, fit: "contain", ...)` 将 PNG/JPEG 文件放到舞台。省略 width/height 时，图片的像素尺寸作为逻辑尺寸；只指定一边时保持宽高比。指定两边后，contain 完整显示并留空，cover 填满并裁剪，stretch 拉伸填满。图片也支持共同的位置、可见性、透明度和 padding 属性。可以直接修改原节点的 source 和 fit。

面板、按钮底图等可用 `slice: 24` 开启九宫格，或用 `slice: [24, 16, 24, 16]` 分别指定左、上、右、下边距。边距以源图像像素计，`slice_scale` 默认 1，用于调整目标边宽；数值均须有限且非负。九宫格填满内容框，角保持比例，边和中心拉伸；框过小或源边距超出图片时四边统一按比例收缩，源中间跨度为零时，重合边界的颜色延展到目标中间；两个轴都重合时使用交点颜色。目标面积为零的切片不绘制。`slice: null` 恢复普通 fit。两项属性可直接赋值或绑定，也可写入 layered_image 的图层描述。使用完整 PNG/JPEG 或图片 Bytes，分块 .reel-image 不适用于九宫格；多个切片共享图片，不生成或上传额外像素。

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

完整的 [author_images.aside](../../examples/author_images/src/main.aside) 随附所需图片，可用前面的 dev 命令运行。dev 的资源路径相对于源码文件目录，run 相对于 ASB 文件目录；`--assets <directory>` 可另选资源根目录。例如构建到 target/reel-artifacts 后，运行时加 `--assets examples`。构建 ASB 不会复制图片文件。

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

完整可运行示例见 [author_transforms.aside](../../examples/author_transforms/src/main.aside)，资源和运行方法与图片示例相同。

### 预处理大图

超出设备纹理尺寸的大图可先执行 `reel prepare-image panorama.png --output panorama.reel-image`，再使用 `image("panorama.reel-image")`。发布时同时携带清单与相邻的 `.tiles` 目录。播放器按可见范围和预算内的邻接区域加载瓦片，移动和缩放复用已驻留像素。参见[格式与预处理合同](../prepared-images.md)。 使用宽幅原图和下文的字体配置运行[大图示例](../../examples/author_large_image/src/main.aside)。目前预处理需显式执行，build 不复制或准备图片。

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

四个取景属性支持赋值、bind/derive 和 tween，沿用活动的取消和所有权规则。把 HUD 作为相机外的兄弟节点即可保持固定；相机也能嵌套组合。移动不会重新排版文字或重新加载图片；绘制与指针坐标共用已接受帧的变换和裁剪。完整的 [author_camera.aside](../../examples/author_camera/src/main.aside) 仅需提供 `fonts/body.ttf`，沿用第一节 dev/build/run 命令。这里是二维相机，尚未实现透视投影。

画面的节点、绘制记录和嵌套层级随实际内容增长。输入路径在新画面发布前扩容，已有按下、拖动和释放继续对应正确的节点。实际内存预算与 GPU 纹理尺寸由运行宿主和设备管理。
将这个组件保存为 ui/room.aside，并提供引用的图片；它导入第三节定义的 dialogue_view。背景、立绘、CG 与作者对白 UI 依次叠放，返回的记录暴露剧情函数需要操作的部分。
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

### 查询已显示的节点位置

文本节点的 `geometry.text_end` 给出完整正文末尾插入光标下端的 `{x,y}`，使用同一参考空间，跟随换行、对齐、富文本和 ruby 排版。可以在打字完成后将对白推进标记放在这里。空正文及其他节点返回 null。

`stage.geometry(node, relative_to: overlay, owner: root)` 等待请求后真正显示的帧，返回一次只读几何快照。查询绑定该帧录制时已发布的布局；尚未呈现的属性修改不会混入旧帧。舞台准备期间保留的旧画面不会完成新查询。width/height 是节点自身尺寸；bounds 给出参考空间中的 x/y/width/height，transform 的 a/b/c/d/tx/ty 将节点局部点映射到参考空间。省略 relative_to 使用逻辑舞台坐标；可指定兄弟浮层或祖先，包含滚动、相机、旋转与镜像，不裁剪。

```aside
overlay.visible = true;
overlay.opacity = 0;
let geometry = await stage.geometry(thumbnail, relative_to: overlay, owner: root);
indicator.x = geometry.bounds.x + geometry.bounds.width / 2;
indicator.y = geometry.bounds.y;
overlay.opacity = 1;
```

隐藏节点没有可查询的当前布局；接受帧中目标或参考仍隐藏、未挂载，或参考变换不可逆时，catch 收到 code 为 GeometryUnavailable 的值。透明节点仍正常布局。目标、参考或 owner 退休会取消请求；取消沿 await 传播，不假定任意 catch 原值具有 code。结果不会逐帧更新；虚拟行可先 await list.scroll_to(key)，再查询行节点。完整示例：[author_geometry.aside](../../examples/author_geometry/src/main.aside)。

## 三 剧情是普通的顺序代码

narrator、character、say 和 choose 支持顺序等待并继续。`let mio = character("Mio", id: "mio");` 后直接写 `mio("你好。");`；id 省略时采用名字，显式 id 可区分同名角色。角色不可修改，`.line(text)` 创建可复用的行描述，`say([mio.line("你好。"), eri.line("晚上好。")])` 同时显示一组 ADV 对白，共用一次揭示和推进。历史保留每行提交时的身份、名字和全文。完整示例：[author_dialogue.aside](../../examples/author_dialogue/src/main.aside)。

`game.configure(text_speed: 30)` 设置每秒显示的 Unicode 字素数，默认 30；0 立即显示全文，省略保留原值。普通推进先补全文字，再结束本句；角色、旁白和行组使用相同规则。挂起、资源等待和输入配对期间暂停揭示，不在恢复时突发补字。Rust 宿主与窗口共用播放逻辑。

需要保持某句的显示节奏时，传 `reveal_skippable: false`。点击和快进会等待正文及句内、尾部停顿完成；完成后的推进、自动阅读和 `[next]` 照常工作。它只影响这次调用，存读档后继续同一节奏。玩家速度设为 0 时，false 仍执行显式 speed 和 pause；未指定固定速度的段落立即显示。默认 true 允许点击补全。示例见 [author_dialogue.aside](../../examples/author_dialogue/src/main.aside)。

```aside
mio(rich("Listen.[pause=0.5]"), reveal_skippable: false);
narrator("The light is moving.", reveal_skippable: false);
say([mio.line("One."), eri.line("Two.")], reveal_skippable: false);
```

可绑定对白、作者 ADV/NVL 视图和自定义选择见 [author_narrative.aside](../../examples/author_narrative/src/main.aside)。语音与章节启动仍待实现；say 提交共享推进的行组，布局由作者视图决定。

对白行数、选项数量、单次交互文字和会话历史按内容增长。历史保留整个会话的记录；它不会因达到展示缓存容量而中断剧情或丢弃旧记录。实际内存不足会报告错误。

角色在模块中定义一次。每句对白调用发言者对象，旁白调用 narrator。独立字符串仍是普通字符串。角色调用更新对话数据和历史，并等待当前句完成，普通函数调用沿同一执行顺序继续。

`history` 决定这一句如何记录：默认 true 保存显示全文，false 省略，字符串或 rich 值保存替代文本；角色调用、narrator 和 .line 都支持。它只影响历史，画面、推进与已读判断仍使用原对白。适合故障演出、隐藏旁白和回看时的完整句子；替代文本可照常调用 tr，提交后固定语言。混合行组只记录指定行，整组省略时不生成空历史项。示例见 [author_dialogue.aside](../../examples/author_dialogue/src/main.aside)。
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
`game.dialogue()` 返回会话的稳定模型。NVL 用上面的 nvl_view 组合累积行，`model.clear()` 同时清空 NVL 行和 ADV 的文字与姓名，历史及当前发言的等待保留。继续追加会显示当前发言更新后的全文；开始新对白会建立新页。ADV 使用第一行快捷字段。`current_line_ids` 标识当前发言组。逐字显现只更新 reveal，不重建 NVL 页；追加行或清页才重建 view 内容。位置、背景、行间距和气泡形状由普通布局决定。自定义对白中，未被控件消费的轻点推进当前句；触摸拖动只滚页。长菜单示例见 [author_scroll.aside](../../examples/author_scroll/src/main.aside)。
换句时的上滚可封装在普通对白函数中：先等待角色对白，再 `await run(tween(...))` 移出旧文本，之后调用下一句。完整组件见 [author_dialogue_transition.aside](../../examples/author_dialogue_transition/src/main.aside)，包含裁剪、角色配色与已读配色；运行前提供 fonts/body.ttf。打字速度用 game.configure(text_speed:) 调整；逐字透明度使用 `reveal_fade`，逐字位移等效果仍属规划。

句内样式用显式 `rich(markup)`：例如 `narrator(rich("仰望 [ruby=ほしぞら]星空[/ruby]，[color=#ffd580][b]记住这一刻[/b][/color]。"));`。text、角色调用、character.line 和 narrator 都接收普通字符串或富文本；对白模型和历史保留完整样式。普通字符串不解析标记；富文本的只读 `.plain` 取得不含标记的正文，`.markup` 取得原标记字符串，读取均不复制文字。组合历史条目时可用 `rich("[b]姓名：[/b] " + line.text.markup)` 保留样式；普通字符串使用 `rich_escape`。`model({text: rich("[b]初始内容[/b]")})` 可直接作为可观察状态；复制模型默认值时共享不可变富文本。可运行示例：[author_rich_text.aside](../../examples/author_rich_text/src/main.aside)。

`game.dialogue().ready` 表示当前整组对白的正文和全部时序（包括尾部 pause）已完成；没有当前对白时为 false。`lines` 中每行也有独立的 `ready`，已完成的旧行保留 true。它不受自动、快进或暂停策略改变；例如提示光标使用 `visible: derive(|| dialogue.ready && !reading.auto && !reading.skipping && !reading.paused)`，其中 reading 是 `game.reading()`。光标可用普通 text/image、布局和动画定制，不需要比较正文长度。见 [句内时序示例](../../examples/author_dialogue_timing/src/main.aside)。

对白完成推进后标记已读。引擎用剧情的函数调用路径识别对白，因此不同位置调用同一个自定义对白组件，仍各自记录已读；重复执行同一路径沿用记录。切换译文不改变身份。同一循环位置用数据驱动的多句对白共享该位置的记录。

句内节奏也写在 rich 中：`[speed=15]慢慢说[/speed]` 按每秒 15 个字素显示，`[speed=2x]快一些[/speed]` 使用玩家速度的两倍，`[speed=0]立即出现[/speed]` 显示即时段，`[pause=0.5]` 在当前位置停半秒。speed 可以嵌套，关闭时恢复外层速度。`[next]` 使本句播完并呈现后自动续句，不改变全局自动阅读；同时对白要求每行都有该标记。末尾停顿会先执行。手动补全、快进或全局 text_speed: 0 越过剩余节奏；打开菜单或挂起保留剩余停顿。标记不进入正文、历史或字数，普通 text 显示完整内容。完整示例见 [author_dialogue_timing.aside](../../examples/author_dialogue_timing/src/main.aside)。

支持成对嵌套的 `[b]`、`[i]`、`[u]`、`[s]`（粗体、斜体、下划线、删除线）、`[font=SimSun]`、`[size=36]`、`[color=#RRGGBB]` / `[color=#RRGGBBAA]`、`[em]`（着重号）、`[ruby=注音]基文[/ruby]`、`[link=词条ID]正文[/link]`。`[[` 表示字面左方括号；把任意玩家输入插入富文本前用 rich_escape 转义。错误标记在 rich 调用位置报错，错误值包含标记的 UTF-8 字节位置。ruby 不嵌套；基文和半字号注音一起换行，基文逐字显示，基文全部显示后出现注音。字体由游戏提供；粗体优先选择对应字重，只有 Regular 时使用合成加粗，斜体沿字体匹配和合成倾斜。 `[font=family]` 使用游戏配置的字体族；嵌套关闭时恢复外层字体。片段字体参与空行度量并由 ruby 注音继承，已追加的片段保留自己的字体。`[weight=400]` 使用 1..1000 内的有限字重，`[b]` 为 700；嵌套关闭恢复外层字重。`[line_height=46]` 为有限非负的绝对逻辑行高，与节点的字号倍率行高分别设置。

text 默认按可用宽度自然换行；width、x/y、padding、scroll 决定 NVL 页面的位置与边界，列表 gap 决定段落间距。`text_align` 支持 start/end/left/right/center/justify，默认 start；`line_height` 是字号倍率，默认 1.2。不同字号、ruby 与着重号共同决定行高，逐字显示只改变绘制范围。节点 color 是基础颜色，显式句内颜色覆盖对应片段；省略句内颜色即可沿用角色或已读配色。

`text(value: rich("查看 [link=star-map][u]星图[/u][/link]"), on_link: |event| show_glossary(event.id))` 把词条 ID 交给普通回调。event.target 是文字节点；链接完整显示后可以鼠标或触摸点击，焦点在文字上时左右方向选择链接，确认键激活。链接消费输入，不推进对白，不自动打开浏览器；节点移除后其回调任务按节点所有权取消。完整示例：[author_rich_text.aside](../../examples/author_rich_text/src/main.aside)，运行前提供 fonts/body.ttf。需要语言切换时使用 `derive(|| rich(tr(...)))`；传入翻译模板的玩家文字先 rich_escape。提交过的对白和历史保留原语言和样式。当前无障碍树提供正文标签，尚未提供逐链接语义子节点。

`game.start(entry)` 使用当前 ASB 中无捕获、无必需参数的普通函数。接受请求后，旧脚本不再继续；新会话重新执行模块初始化，再调用 entry。旧对白、UI 回调和活动结束，脚本全局、历史、自动和快进状态重建；已读记录、game.configure 配置、宿主资源与存档服务保留。目标入口负责构造画面；调用后无需清空旧舞台。跨 ASB 的热更新入口解析与持久脚本全局尚未实现。可运行示例见 [author_game_start.aside](../../examples/author_game_start/src/main.aside)。

传 `wait: false` 开始一句可继续追加的对白。`line.append(text)` 追加下一段并等待推进，与普通角色发言一致。需要衔接音效、动画或延时时写 `line.append(text, wait: false)`，到下一个推进点再调用 `line.wait()`。等待之后仍可追加，显示前文和同一条历史会保留；下一句对白或选择开始后，旧句柄结束。正文可以换行，普通字符串和 rich 都支持。多角色 say 行组可以等待，追加用于单角色行组。完整示例为 [author_dialogue_append.aside](../../examples/author_dialogue_append/src/main.aside)。

```aside
let line = mio("Starting", wait: false);
await run(delay(0.5));
line.append(" ...", wait: false);
audio.play("assets/chime.wav");
line.append(" ready.");
line.append("\nWelcome back.");
```

脚本在推进点之间执行时，输入可补全文字。等待型 append 与 line.wait 共用手动、自动和快进规则。追加保留已经显示的字素，append 的 history 参数与角色调用采用相同选择：true 追加显示正文、false 省略这一段、字符串或 rich 追加替代正文。原条目已省略或清除时继续保持省略。句柄只在主剧情使用，保存恢复继续当前段和等待位置。


`line.revealed()` 等待这句文字与句内停顿完成，再衔接音效或演出。它保留当前推进点；`line.wait()` 等待玩家推进。`append`、`revealed` 和 `wait` 都返回原句柄，可以继续链式组合。自定义文字动画由作者组件组合自己的活动与对白等待。

## 四 时间顺序与同时发生的变化

动作描述可以复用：`.then(other)` 接着执行另一动作，`.together(other)` 同时执行，`.repeat(times: 3)` 重复整段，`.run()` 启动并返回可等待的活动。每一步组合整个前缀；用命名变量表达需要单独分组的段落。活动的 `.pause()`、`.resume()`、`.stop()` 和 `.finish()` 返回原活动。

```aside
let entrance = tween(portrait, opacity: 1, duration: 0.3);
let move = tween(portrait, x: 640, duration: 1);
await entrance.then(move).then(delay(0.5)).run();
```

速度随时间改变时，把标量曲线直接交给 tween。以下曲线表示匀加速：开始速度为平均速度的一半，结束为一点五倍。曲线的 sample(0) 和 sample(1) 须为 0 和 1；可以共享曲线，暂停、取消、重复与保存恢复照常使用。

```aside
use std::math::curve;

let acceleration = curve([
    {time: 0, value: 0, mode: "cubic", out_tangent: 0.5},
    {time: 1, value: 1, in_tangent: 1.5},
]);
await tween(portrait, x: 640, duration: 2, easing: acceleration).run();
```

音乐、视频、动画和时钟可以先创建为暂停状态，再随画面开始：`audio.play(..., paused: true)`、`movie.play(..., paused: true)`、`action.run(paused: true)` 和 `clock(..., paused: true)`。省略 paused 时照常启动。媒体在暂停中继续加载、解码和准备首帧，位置保持 start；seek 后仍保持暂停。动画在创建时检查动作，首次 resume 时才读取属性起值并取得写入权；时钟此前保持 ticks 为 0、elapsed 为 0.0。

```aside
panel.opacity = 0;
let music = audio.play("assets/chime.wav", owner: panel, paused: true);
let entrance = tween(panel, opacity: 1, duration: 0.25).run(owner: panel, paused: true);
stage.replace(panel);
await stage.presented(owner: panel);
music.resume();
entrance.resume();
```

`s.run(action, paused: true)` 使用相同规则。暂停的空组合和零时长动作也等到 resume 才完成；finish 可以主动完成有限动作，stop 保留当前属性。存档保留暂停状态，节点和作用域结束时按原所有权清理。完整的 [音乐进度示例](../../examples/author_media_position/src/main.aside) 使用这一写法等画面准备好后开始播放。

`clock(interval: 60, owner: panel)` 创建可绑定的周期时钟。只读 ticks 是 int 完整周期数，初值 0，可直接作为数组索引或字符串 slice 范围；elapsed 是这些周期的 float 总秒数，初值 0.0。用 derive 组合时钟、倒计时或进度显示；时间跨过多个周期时直接更新到最新值。一批时间更新触发的派生会共同完成后再提交画面；计算跨帧时合并期间的新时间，再计算下一批，避免一边计算一边被时钟反复打断。pause/resume/stop 和 await 沿用活动接口，暂停期间不计时，停止保留最后值，节点退休取消；clock 无终点，不能 finish。读档保留两者类型、已有计数和暂停状态，继续原周期。参见 [时钟示例](../../examples/author_clock/src/main.aside)。

等间隔帧动画可以直接用周期数选择图片来源。先保存帧数，再在派生中只做索引；同一个 image 节点持续更新来源。先暂停时钟，首帧呈现后再恢复，动画便从实际可见的第一帧开始：

```aside
let frames = ["assets/frame-a.png", "assets/frame-b.png"];
let count = frames.len();
let timer = clock(interval: 0.125, paused: true, owner: root);
root.add(image(source: derive(|| frames[timer.ticks % count]), width: 64, height: 64));
stage.replace(root);
await stage.presented(owner: root);
timer.resume();
```

`timer.pause()` 保留当前帧，`timer.resume()` 接续周期，`timer.stop()` 冻结最后一帧；owner 退休取消时钟。存读档保留周期数与暂停状态。时间跨过多帧时直接选择当前帧，适合只需要显示当前画面的动画。需要逐次执行音效、计分或其他事件时，用普通顺序代码安排这些事件。完整示例 [author_frame_animation.aside](../../examples/author_frame_animation/src/main.aside) 使用仓库现有图片，可运行 `reel dev --assets ..`（在 `examples/author_frame_animation/` 中）。

现实时间使用 `use std::time::{now, parts, format};`：now() 返回 UTC Unix 秒整数，parts(timestamp, timezone: null) 返回 year/month/day/hour/minute/second/weekday/offset_seconds，weekday 为周一 1 至周日 7。format(timestamp, pattern: "%Y-%m-%d %H:%M", timezone: null) 按 strftime 模式返回文字；timezone 可设 IANA 时区（如 Asia/Shanghai、UTC），省略使用系统时区，系统无法提供时使用 UTC。保存时间可直接使用 saves.list 的 saved_at。调用不会自动刷新；在普通任务中更新模型，再绑定文字。见 [author_wall_clock.aside](../../examples/author_wall_clock/src/main.aside)。

动画与组合活动：`tween(node, x: 320, duration: 0.5, easing: "in_out")` 描述变化，`action.run(owner: node)` 启动。owner 省略时，优先归当前 scope；作用域外的主剧情归会话、按钮回调归事件节点；`activity.pause()`、`activity.resume()`、`activity.stop()`、`activity.finish()` 控制同一活动。await 返回字符串 `Completed` 或 `Stopped`；目标或 owner 退休取消活动并在 await 处抛错。duration 使用非负秒数，0 立即完成；easing 支持 linear、三次 in/out/in_out、二次 quad_in/quad_out/quad_in_out 和正弦 sine_in/sine_out/sine_in_out。可以改变全部共同数值属性；width/height 须在节点或 from 提供有限起值，null 表示该属性不参与动画。可运行示例：[author_activities.aside](../../examples/author_activities/src/main.aside)，使用 `reel dev --assets ../../target/reel-artifacts/author-assets`（在 `examples/author_activities/` 中）。sequence/parallel/repeat 与 scope 共同管理活动和任务所有权。

隐藏滚动区域会保留位置和已测量尺寸；在菜单中存读档后，再次显示时仍从原位置继续，内容尺寸发生变化时按新范围调整。

`tween(viewport, scroll_y: 400, duration: 0.5, easing: "out")` 平滑移动滚动区域；也可使用 `scroll_x`。目标必须是 scroll 节点，滚动范围仍由实际内容尺寸限制。

需要正弦曲线运动时，使用 `sine_in` 从慢到快加速，`sine_out` 从快到慢减速，或 `sine_in_out` 两端慢、中间快。所有曲线都精确到达指定终点；它们与三次 in/out/in_out、二次 quad_in/quad_out/quad_in_out 的运动节奏不同。

五次曲线 `quint_in`、`quint_out`、`quint_in_out` 提供更明显的加速与减速；`bounce_out` 在抵达终点时回弹，`bounce_in` 是其反向，`bounce_in_out` 在两端组合回弹。回弹不会越过起终值。用同一个 tween 描述菜单滑入和 Logo 落下，见 [author_activities.aside](../../examples/author_activities/src/main.aside)。

`quart_in`、`quart_out`、`quart_in_out` 使用四次曲线。`elastic_out` 在终点附近衰减振荡，`elastic_in` 是其反向，`elastic_in_out` 组合两端；弹性会越过起终值，适合位置、旋转和缩放，opacity 等有限区间属性应选择不超调曲线。暂停、停止和存档恢复与其他 tween 相同。[author_activities.aside](../../examples/author_activities/src/main.aside) 将四次淡入与弹性入场组合在一起。

```aside
await tween(portrait, x: 720, duration: 0.8, easing: "sine_out").run();
```

完整的 [author_sine_easing.aside](../../examples/author_sine_easing/src/main.aside) 用相同距离和时长对比三条正弦曲线；打开窗口前请提供 `fonts/body.ttf`。

对活动、completion 或其他已注册的可等待对象使用 `await`，暂停当前调用链，同时继续处理输入和其他工作。失败在 await 处传播并保留原始原因；普通函数即可等待，无需 async 声明。

默认启动时，空 sequence/parallel 与 times: 0 立即完成，不写属性。times 必须是非负整数或 null，省略或 null 表示持续循环；持续循环的单轮必须有正时长。含无限分支的组合不能 finish，调用会在改变活动前报错；用 stop 结束。并行分支的全部写入集合在 run 时检查，冲突不会接管已有活动。暂停、停止与取消作用于整组，后续段只在实际开始时接管属性。较大的时钟步长会继续推进跨过的动作段。

tween 描述目标属性、终值、时长和可选起值。sequence 顺序组合动作，parallel 同时组合动作，repeat 重复动作。它们都是普通库函数，产生可复用的动作值。run 才启动活动，并返回可等待、可控制的活动；await activity 等待活动结束，输入、声音和其他活动继续进行。run(action) 默认归当前 scope，否则继承任务的资源 owner；主剧情使用会话寿命。显式 owner 可指定舞台节点，s.run(action) 指定所属作用域。
省略显式起值时，动作在实际激活时读取当前属性。sequence 中后一段在前一段结束后读取起值。动作描述可以复用，每次 run 都产生独立进度。活动只在动作段真正激活时取得属性写入权。新演出停止与之冲突的旧根活动并从当前值接管，暂停的旧活动也遵守此规则。sequence 可按顺序写同一属性；parallel 分支的写集合保持互斥。直接赋值，包括赋相同的值，停止正在写该属性的根活动。其他属性的赋值不受影响；终止活动释放全部写入权。
```aside
let entrance = tween(view.portrait, x: 320, duration: 0.5)
    .together(tween(view.hud, opacity: 1, duration: 0.3));
await entrance.run();
```
`from` 把起值写进动作，适合淡入、滑入和每轮从头开始的循环。它接受普通记录，键对应本次动画的终值属性；其余属性仍从激活时的当前值开始。记录在构造动作时按目标属性的约束校验并复制，之后修改记录不改变动作。延迟结束、重复的每一轮、初始暂停后的首次 resume 都在激活时应用起值；需要等待期间就隐藏的节点在创建时设 `opacity: 0`。零时长或 finish 直接到终值。显式 width/height 起值也可用于当前尺寸为 auto 的节点。

```aside
fn fade_in(node, seconds = 0.5) {
    tween(node, from: {opacity: 0}, opacity: 1, duration: seconds)
}
await delay(0.5).then(fade_in(view.hud)).run();
```

同一个动作可以重复运行；循环移动可直接写 `tween(particle, from: {y: -20}, y: 740, duration: 3).repeat()`。完整入场与重放例子见 [author_activities.aside](../../examples/author_activities/src/main.aside)。

每个活动有确定的 owner。启动与主剧情函数归会话所有，UI 回调归发出事件的节点所有。stage.replace 结束旧画面及其回调，调用它的会话剧情继续执行。动作目标只是经过验证的句柄；任一节点或效果目标销毁时，整个根活动结束为 Cancelled，释放所有属性保留。owner 从舞台销毁时，所属活动与回调任务一并结束。pause 暂停进度，resume 继续，stop 停在当前值并使等待返回 Stopped；finish 使有限动作到达其终值，返回 Completed，自然结束也返回 Completed。无限循环使用 stop 结束，其本身没有完成终值。资源错误或活动取消在仍存活的等待任务中产生对应错误，供普通 try/catch 处理。若等待任务自身的 owner 已结束，该任务直接结束；宿主释放其资源，脚本清理代码也随任务结束。
parallel 等待所有有限子动作。任一子动作失败时结束同组剩余子动作，并报告错误；sequence 随失败停止后续动作。repeat(action, times: 3) 有限重复；省略 times 表示循环。持续的呼吸、雨丝等循环单独运行，由画面所有权管理，剧情只等待自己需要的有限活动。
```aside
let breathing = tween(view.portrait, opacity: 0.92, duration: 1.4)
    .then(tween(view.portrait, opacity: 1.0, duration: 1.4))
    .repeat();
breathing.run(owner: view.root);
mio("听，雨又大了。");
```
上例展示循环组合规则。实际立绘呼吸可以改用细微位移或缩放，脚本结构相同。作者把常用动作封装成普通 fn，输入任意节点后返回动作值，UI 与立绘共用这些动作。

`delay(seconds)` 可以在演出中留出停顿，无需创建画面节点。它与动画使用同一活动时钟，支持暂停、停止、owner 取消和组合；纯等待期间播放器休眠到截止时间。秒数须有限且非负，零秒立即完成。可运行示例：[author_timing.aside](../../examples/author_timing/src/main.aside)。

```aside
await run(delay(1.5));
await run(sequence(delay(0.5), tween(panel, opacity: 0, duration: 0.3)));
```

要让画面跟随音乐位置、拖动进度或游戏变量变化，可以用 `std::math::curve` 定义数值曲线，再调用 `sample`。曲线只计算数值，输入时间由作者决定：

```aside
use std::math::curve;

fn main() {
    let height = curve([
        {time: 0, value: 120, mode: "cubic", out_tangent: 0},
        {time: 1, value: 40, in_tangent: 0, mode: "step"},
        {time: 2, value: 120},
    ]);
    let timer = clock(interval: 1.0 / 60, paused: true);
    stage.replace(stack(width: 320, height: 180) {
        stack(x: 140, y: derive(|| height.sample(timer.elapsed)),
            width: 40, height: 40, background: "#ec769e");
    });
    await stage.presented();
    resume(timer);
    await run(delay(3));
    stop(timer);
}
```

这个方块在第一秒平滑上升，停留一秒，然后回到原处。换成 `height.sample(audio.position(music))` 就跟随音乐，暂停、定位和读档后仍按当前播放位置取值。默认 `mode: "linear"` 在相邻关键点之间直线插值；`step` 保持左侧数值，到下一点时跳变；`cubic` 使用当前点的 `out_tangent` 和下一点的 `in_tangent`，切线单位是数值/秒，省略时为 0。mode 描述该点之后的一段。

关键点按有限时间从小到大排列，时间可以为负；同一时刻取最后一个关键点，范围外保持端点值，单点曲线始终返回该值。构造时复制关键点，完成后修改原数组或记录不会改变曲线；采样时间也须有限。曲线随普通执行快照保存，可以在多个属性中复用。曲线在构造时准备插值系数，采样二分定位所在段。完整示例见 [author_curves.aside](../../examples/author_curves/src/main.aside)。

## 五 作用域管理临时画面

scope 统一管理任务、活动、等待、临时节点、属性覆盖和加性偏移。

scope 在当前剧情调用链执行普通闭包；正文可以对白、等待、调用其他函数，退出时退休本次作用域拥有的工作。临时画面按下面的形式加入同一寿命管理：
```aside
fn show_cg(view) {
    scope(body: |s| {
        s.add(view.picture, image("assets/window-cg.png"));
        let next = completion();
        s.add(view.hud, button("Continue", on_click: || next.resolve()));
        await next;
    });
}
```
CG 加在 picture 最上方，玩家继续后随作用域退出移除。s.add(parent, node, index: 0) 可指定插入位置，返回原 node；提前移除页面、替换场景和读档后退出也沿同一退休规则处理。
s.add(parent, node) 挂载临时子树，s.run(action) 启动归此作用域所有的活动。s.override(node, properties) 创建可动画的覆盖层；s.offset(node, x: 0, y: 0) 创建独立的 x/y 附加偏移。普通 node.add 继续使用明确父节点寿命，不因身处 scope 而偷偷改变含义。
属性覆盖使用记录参数保留 null 的含义，覆盖只读 derive 时无需保存和回写原值：
```aside
scope(body: |s| {
    let effect = s.override(panel, {opacity: 0.4, width: null});
    await s.run(tween(effect, opacity: 1, duration: 0.3));
    let next = completion();
    s.add(panel, button("Continue", on_click: || next.resolve()));
    await next;
});
```
node 的属性仍是基础值，effect 的属性是本层值；画面、布局、命中和 geometry 使用有效值。同字段后建立的层优先，修改旧层不改变顺序。基础绑定和被遮挡层的动画继续更新，移除后露出最新结果。effect.remove() 可提前撤销，退出作用域或目标退休也会撤销。可以覆盖共同布局与外观字段，token 只接受创建时列出的字段；控件值、回调、hover 等只读状态及 camera/scroll 专属字段不包含在内。各层动画独立，写同一字段或最终绑定源仍按普通活动冲突处理。

加性偏移用独立层表达基础移动之上的摇晃：

```aside
let moving = run(tween(panel, x: 500, duration: 4));
scope(body: |s| {
    let shake = s.offset(panel, y: -3);
    await run(repeat(sequence(
        tween(shake, x: 6, duration: 0.05),
        tween(shake, x: -6, duration: 0.05)
    ), times: 8));
});
await moving;
```

未指定的 x/y 默认为 0，支持赋值、bind/derive 和普通 tween。多层偏移叠加在当前基础值或覆盖值之后，坐标按父容器的方向和单位解释；node.x/y 继续表示基础值。摇晃结束退出 scope 后，画面回到基础动画当时的位置。shake.remove() 提前撤销该层；目标退休、取消和读档沿作用域的共同规则。普通 run 在这里自动归当前 scope。完整交互示例：[author_offsets.aside](../../examples/author_offsets/src/main.aside)。

`s.cancel()` 请求结束当前作用域，首次请求返回 true，重复请求返回 false。事件闭包可保存 `let cancel = s.cancel`；已提取的方法在作用域结束后仍返回 false。作用域结束时 handle 退休，之后通过 handle 读取属性会报错。取消复用 owner 退休的区域清理，并在 scope 外交付 `ScopeCancelled`；正文调用自己的 cancel 也沿同一路径展开。

```aside
fn closeable_scene() {
    let state = model({cancel: null});
    let close = button("Close", on_click: || state.cancel());
    stage.replace(stack() { close; });
    try {
        scope(body: |s| {
            state.cancel = s.cancel;
            narrator("The window is open.");
        });
    } catch error {
        if error != "ScopeCancelled" { throw; }
    }
}
```

普通返回、异常和场景退出都触发同步、幂等的宿主清理。退场动画由正文主动等待。作用域可以嵌套；记录了当前调用位置、活动和临时子树的存档恢复后，继续同一段正文，再按相同规则释放。游戏变量的正常写入保持其业务含义，scope 只管理显式登记的临时效果。
默认 scope 随当前任务存活。需要随某个节点结束时写 scope(owner: node, body: ...)。若该节点先结束，仅撤销 scope 区域并在外层调用点产生 ScopeCancelled，仍存活的主剧情可以在 scope 外捕获；区域内的失效节点代码结束，宿主完成清理。当前任务自身被撤销时，整个任务结束。
visible=false 隐藏子树并退出命中与焦点；enabled=false 保留画面并禁用交互；opacity 只改变透明度。永久换景用 stage.replace(new_root)，局部永久替换用容器 clear() 后再 add(new_child)。

## 六 Logo 标题与局部交互

视频已接入共同舞台、媒体活动与存档。默认桌面支持非分片 MP4：渐进式 8-bit YUV420 H.264 Baseline 视频，可附带单声道/双声道 AAC-LC。自定义宿主通过 MediaBackend 接入其他格式或硬件解码；没有安装视频能力时，播放在调用处明确报错。

`video(source, ...image 节点属性)` 只创建画面；`movie.play(looping:false, volume:1, start:0, end:null, owner:null)` 返回活动。start/end 为片段起点与排他终点，省略 end 使用视频末尾。重复 play 停止该节点原播放。正常结束与 stop 都继续 await 后的代码；播放失败与节点/owner 退休产生可捕获错误。完整示例见 [author_video.aside](../../examples/author_video/src/main.aside)。

```aside
let movie = video("assets/logo.mp4", width: 1280, height: 720, fit: "contain");
let playing = movie.play();
stage.replace(button(width: 1280, height: 720, decorated: false,
    background: "#000000", on_click: || stop(playing)) {
    movie;
});
await playing;
```

`pause(playing)` / `resume(playing)` / `stop(playing)` 与其他活动一致；`finish(playing)` 完成有限视频，循环视频须 stop。`video.seek(playing, seconds)` 跳到绝对内容时间，`video.position(playing)` 返回当前秒数，可在 derive 中驱动字幕和外观。`await video.at(playing, seconds)` 等待发布位置达到目标并返回 true，提前正常结束或 stop 返回 false，取消和失败抛出原错误；使用与音频位置等待相同的暂停、seek 和恢复规则。视频音轨沿 Music 音量总线。隐藏节点不暂停，可直接组合裁剪、镜头、透明度和其他控件。

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

### 关闭作品

`game.exit()` 请求关闭作品；接收后不再执行脚本，播放器完成任务和资源清理后退出窗口。可先等待淡出动画再调用。嵌入宿主接收同一请求并决定外层应用的行为。完整按钮示例见 [author_exit.aside](../../examples/author_exit/src/main.aside)，运行前提供 fonts/body.ttf。

### 可运行按钮与回调

button 与其他节点使用同一棵舞台树，支持 label、on_click、children 和共同外观属性。省略 label 时可用 children 组合图片、文字或任意组件；内容按 stack 排列。enabled 默认 true，禁用父节点会禁用其整棵交互子树。通过 btn.on_click = 新闭包 或 null 可以更换或清除回调。以下代码与 [author_buttons.aside](../../examples/author_buttons/src/main.aside) 相同。

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

点击把原按钮作为 event.target 传入闭包。回调可以 await 动画；同步代码执行到显式 await、普通调用实际等待或结束时切换其他脚本任务；await 已完成的对象也会给其他就绪任务运行机会。普通调用和引擎分片不会打断没有 await 的读改写。不同回调可以等待同一活动，取消一个等待者不会停止其他等待者共用的活动。

回调默认属于发出事件的按钮，在其中启动的 run 也默认属于该按钮。移除按钮、清空父容器或替换舞台会取消相关回调及活动；回调移除了自己之后，后续语句不会继续执行。narrator/choose 当前只由主剧情调用，回调调用会产生可捕获的上下文错误。game.start 和节点属性绑定已实现；通用事件订阅仍待实现。

主入口返回后，挂载的按钮回调和活动继续工作；空闲时不反复执行 VM。所有回调与活动结束且画面不再有事件处理器后，会话自然结束并保留最后画面。按钮消费的点击不会同时推进对白。默认宿主支持鼠标和触摸点击、Tab 焦点导航与 Enter/空格激活。手柄十字键/左摇杆按当前画面位置导航，长选项列表会随焦点滚动，南侧按钮确认，东侧按钮与 Escape 向当前 UI 发送返回。确认在配对松开时生效；切换弹窗、失焦或断连会取消旧输入，返回不直接退出应用。多个触点可同时操作不同控件；拖出按钮后松开不触发点击，系统取消或窗口挂起会释放交互。已有按钮无需为触屏重写，可运行 [author_buttons.aside](../../examples/author_buttons/src/main.aside)。内置桌面宿主已接入系统 IME，系统可访问性尚待适配。

用第一节的 dev/build/run 命令运行此文件。引擎的节点、输入和调度不依赖 Winit/WGPU；window feature 选择当前内置桌面组合，其他宿主可以复用同一会话和呈现接口。

自定义平台宿主的接入方法见[播放器说明](../../crates/reel-player/README.md)。

图片首次显示或读档恢复时，后端会按剩余额度分批上传，单张大图也能按行继续上传，并复用已完整提交的纹理；等待期间保留上一幅完整画面，上传完成不提前改变输入区域。Rust 宿主通过 `RenderEvent::ResourcesAvailable` 唤醒等待的呈现请求；真正超过宿主资源预算仍会明确报错。

### 模态菜单与焦点

打开存档菜单前，可先用 `let preview = await stage.capture(width: 256, height: 144);` 获取当前已呈现画面的 PNG Bytes。`image(preview)` 可以直接显示它，`await saves.save("slot/1", thumbnail: preview)` 将图片与执行快照一起写入。列表的 `has_thumbnail` 表示该槽是否有预览；用 `await saves.thumbnail("slot/1")` 按需读取。capture 可省略尺寸或只指定一边，支持 owner 和取消；不要求作者操作窗口或 GPU。

对白光标、立绘和其他控件播放动画时，玩家仍可正常点击、触摸或使用键盘/手柄确认。按下后若换句、完成揭示、替换舞台根或切换模态范围，必须重新按下，避免旧手势确认新内容。

所有共同节点支持可写的 `modal` 属性，默认 false。可见的 modal 子树独占输入；它外部的点击和未处理的键盘事件不会继续推进剧情。多个 modal 按绘制顺序选择最上层，绘制层序由节点组合顺序决定。隐藏或移除节点释放输入范围；enabled 为 false 时仍隔离下层。

进入菜单会选择其中可用的焦点，Tab/Shift-Tab 在菜单内循环；嵌套菜单关闭时恢复上一层焦点，关闭最后一层后恢复原控件仍有效的焦点。输入范围在新画面实际呈现后切换，并取消范围外的拖动。完整示例见 [author_modal.aside](../../examples/author_modal/src/main.aside)，可用第一节的 dev/build/run 命令运行。

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

### 命名动作与输入节点

滚轮交互使用 `input(on_wheel: |event| { ... })`，事件含 target、x、y、delta_x、delta_y。x/y 是已显示画面中节点内容区的局部位置；delta 是逻辑窗口轴像素，正值向右/向下，不再按画布或节点变换缩放。行滚轮每行40像素，像素滚轮除窗口DPI，页滚轮乘接收input内容区宽高。内层scroll能移动时消费；到边界后向祖先冒泡，第一个有on_wheel的input消费整件事件。回调可以等待并归节点所有，沿模态和隐藏/禁用/退休规则，不产生pressed/released。示例：[author_wheel.aside](../../examples/author_wheel/src/main.aside)。

`input` 是按 stack 布局的共同节点，绑定键盘、手柄与指针到自定义动作。根 input 在控件尚未获得焦点时即可接收键盘/手柄输入；已聚焦的子控件保持优先权，未消费的输入交给最近的祖先映射。因此菜单可统一绑定 Escape/手柄返回，场景可统一绑定快进，点击子按钮不会使这些映射失效。焦点改变取消保持中的动作；输入不跨越模态范围，文本框保留键盘与 IME。下面与 [author_input.aside](../../examples/author_input/src/main.aside) 相同，可用第一节的 dev/build/run 命令运行。将字体文件放在 `fonts/body.ttf`，或修改 main 中的路径。

聚焦按钮的手柄 South 确认由按钮接收，父 input 即使绑定 South 也不会同时触发动作。East 返回键和其他未消费的控制继续交给祖先映射；未绑定的方向用于移动焦点。焦点回到 input 后，South 恢复为游戏动作。

```aside
fn main() {
    game.configure(fonts: ["fonts/body.ttf"]);
    let marker = text("Move with A / D or the left stick", x: 160, y: 64,
        width: 340, height: 48, background: "#284860");
    let status = text("Ready", x: 24, y: 160, width: 340, height: 48);
    let controls = input(modal: true, width: 640, height: 280,
        actions: {
            move_x: {
                bindings: [binding("KeyA", scale: -1), "KeyD",
                    binding("LeftX", dead_zone: 0.2)],
                on_action: |event| {
                    marker.x = 160 + 120 * event.target.value("move_x");
                    status.value = event.phase;
                },
            },
            jump: {
                bindings: ["Space", "South", "PointerPrimary"],
                on_press: || {
                    await run(sequence(
                        tween(marker, y: 16, duration: 0.15),
                        tween(marker, y: 64, duration: 0.15)
                    ));
                },
            },
            finish: {
                bindings: ["Escape", "East"],
                on_release: || stage.replace(text("Finished", x: 24, y: 24)),
            },
        }
    ) { marker; status; };
    stage.replace(controls);
}
```

actions 的每个键是动作名，每项含 bindings 数组。普通控制名字符串使用 scale=1、dead_zone=0，需要配置时才调用 binding。控制名包括物理键（KeyA、Space、Escape）、标准手柄按钮（South、East、DpadLeft）、轴（LeftX、LeftY、RightX、RightY、LeftZ、RightZ）和指针按钮（PointerPrimary、PointerSecondary、PointerAuxiliary、PointerX1、PointerX2）。轴范围为 -1..1，正 Y 向下，扳机为 0..1。越过死区的值重新归一化、缩放、求和并限制为 -1..1；释放一个绑定不会取消另一设备仍保持的动作。

按需要配置 on_press、on_change、on_release 和 on_cancel。单个 on_action 接收完整流，与同动作的阶段处理函数互斥。处理函数可为零参或接收一个含 target、action、value、phase 的事件；phase 为 pressed/changed/released/cancelled，键盘重复不产生新的 pressed。没有处理函数的阶段仍更新保持值并消费输入，省去事件对象和回调任务。回调可以等待并归节点所有；返回 Task 仍按普通返回值处理。

`node.value("move_x")` 读取最新保持值，event.value 保留那次事件的数值。赋给 `node.actions` 的新表在舞台同步时整体生效，同时安装绑定和处理函数并重置保持值。构造立即安装声明；后续同步脚本段可继续组装待生效的新表。生效后的内部修改不更新映射，后续更改赋一张新表。仅替换处理函数也会向旧处理函数取消旧保持，旧释放不会调用新处理函数。存档保留已安装声明，恢复时保持值归零。

输入节点自身拥有焦点时接收键盘和手柄动作。子控件取得焦点会取消父输入节点的保持，并独立处理输入；使用 modal: true 可让独立交互场景直接接管焦点。声明 on_key 或键盘/手柄绑定的 input 可通过 Tab 聚焦，绑定了指针按钮时点击也会聚焦并捕获该触点。只处理指针或滚轮的 input 保留当前键盘焦点。聚焦容器包含可用的子控件时，未绑定的下/右方向进入首项，上/左进入末项，随后按画面位置导航并跳过焦点祖先；Tab 仍可访问父容器。隐藏、禁用、失焦、切换模态和清除已呈现画面会取消保持；移除节点会结束其任务和活动。动作消费的同一次输入不会继续导航或推进对白。普通按钮与对白由主指针按钮或触摸激活，其他鼠标按钮可用于作者动作。

## 七 设置页与图片风格控件

button、slider、toggle 与节点 bind/unbind 已实现；页签、settings 与滑条外观槽位仍属于设计目标。

拖动由控件完成捕获与数值换算，同一数值决定滑块位置和音量。作者提供轨道与滑块的绘制节点，控件按方向、范围和槽位布局它们。settings 属于全局设置，读剧情存档后继续使用当前设置。 普通移动、缩放和布局变化保持拖动，输入使用最近显示画面的几何。范围、步长、方向或轨道端点策略改变时，当前拖动结束，新配置显示后恢复交互；外部绑定值更新仍即时生效。
图片按钮也是作者自己的普通函数。按钮提供交互状态，图片只根据状态选择资源。
```aside
fn picture_button(normal, hovered, pressed, clicked) {
    let control = button(on_click: clicked);
    control.add(image(source: derive(|| {
        if control.pressed { pressed }
        else if control.hover { hovered }
        else { normal }
    })));
    return control;
}
```
bind 连接可观察属性，属性同时保留原来的读写权限；text 可以读取只读属性，slider 写回数值时要求可写属性。derive 计算只读外观。派生函数对实际读取的可观察属性建立依赖，条件变化后替换依赖；结果也可通过 `binding.value` 读取并继续派生。派生更新在同步脚本段结束后、舞台发布前集中完成，也可读取不可变捕获值；普通数据的内部变化保持普通语言语义。可观察属性变化仅更新相关节点。连续时钟和对白揭示按批更新；一批派生跨帧时保留上一完整画面，计算完成后一起提交，再处理期间合并的新值。派生内部再次读取 game.dialogue() 会取得同一批的对白投影，显式剧情命令仍按原顺序生效。标量写入可写绑定属性会写回绑定源并保留绑定；动画也保留和写入最终源属性，因此两个节点绑定同一源会发现真实写入冲突。源的显式改写停止相关根活动；被动投影更新属于读取。只读派生属性用 scope 覆盖做临时演出；重新绑定或 unbind 会先结束依赖旧绑定的活动，unbind 保留当前有效基础值。捕获存档的短暂期间，脚本派生与作者外观更新暂停，捕获完成后按最新状态恢复。普通 {...} 数据记录适合传参，持续变化的业务值用 model({...}) 创建可观察模型。循环依赖和派生计算中的写入、等待给出源码诊断。
派生计算跟踪实际读取的可观察字段，包括条件分支产生的依赖。虚拟列表减少存活的行节点，来源数据和 key 索引仍常驻；使用稳定 key，并使来源数据适合宿主预算。

在 derive 或组件工厂中读取另一个派生值时，引擎先完成该来源的待重算，再继续这次读取；条件分支首次读取的新来源也如此。可以把数组范围判断与图片路径等消费分别写成派生，保持同一次模型更新的值一致。同步按钮回调或故事函数中的普通赋值仍在该段结束后统一更新派生。

作者可用同样方式封装统一字体、按钮图片、留白和动效，再作为普通模块导入。风格复用来自组件和参数，共用的属性对象也可按普通语言规则组织。

稳定位置引用使用 `bind(node.x)` 或 `bind(node[key])`。接收者和键只在创建引用时求值一次。将引用传给节点构造参数或赋给节点属性即可连接；之后赋入普通值会写回来源，连接仍保留。`unbind(bind(node.x))` 断开连接并保留、返回当前值。多个控件共享来源时，范围等约束会一并校验，失败不修改任何控件。来源节点被显式销毁时，仍存活的目标冻结最后值并断开；已有引用不能再用于已销毁的来源。业务 `model` 数据见第十二节。

所有节点的 `hover`、`pressed`、`focused`、`disabled` 都可直接读取或用 `bind` 连接。它们由输入系统发布，脚本不可赋值，也不能绑定为 slider/toggle 的可写 value。hover/focused 包含后代；pressed 包含指针捕获与按钮自身的键盘、手柄确认按住；disabled 包含祖先禁用。取消、隐藏、禁用和失焦会清理交互状态。节点退休后，读取该节点的 derive 停止更新并保留最后成功值。移除最后一个消费者会停止其派生外观更新；共享消费者继续更新，重新绑定保留引用会恢复更新，但不能恢复已退休的输入。按钮和开关使用 `decorated: false` 关闭默认焦点框及开关指标/留白，再用普通 children 构成完整外观。实例见 [author_control_states.aside](../../examples/author_control_states/src/main.aside)。

悬停或聚焦需要改变业务状态时使用 `on_interaction`，例如鼠标离开后仍保留图库选中项。所有节点都接受这个回调或 null，构造后也可赋值。不可变事件包含 `target`、`hover`、`pressed`、`focused`、`disabled`、`focus_visible`；即使回调等待，其中的布尔值也保留该次变化的值。只有状态实际变化才触发回调，并先发布整批事件的只读状态。注册回调不会发送初始事件。等待中的回调属于节点，节点退休时取消。没有回调时不分配事件对象或任务；纯外观继续使用 `bind`/`derive`。

```aside
let gallery = model({selected: null});
let card = button("Sketch", on_interaction: |event| {
    if event.hover || event.focus_visible { gallery.selected = "sketch"; }
});
```

[author_control_states.aside](../../examples/author_control_states/src/main.aside) 同时展示瞬时外观和离开后保留的选中提示；准备 `fonts/body.ttf` 后用 `reel dev`（在 `examples/author_control_states/` 中）运行。

### 数值控件与设置页

`checkbox` 与 `toggle` 接受相同参数，前者显示勾选框，后者显示开关。`radio(option, label: null, value: null, on_change: null, ...)` 表达单选项；让同组的 value 绑定到同一个业务字段，即可自动互斥，无需维护多份布尔值。option 可以修改或绑定；value 未匹配任何选项时不选中任何项。业务值比较与 list key 相同。组内导航：Tab/Shift-Tab 整组只停一次，进入选中的可用项，否则首项；组在 Tab 顺序中的位置取其第一个可用节点；方向键和手柄方向按节点顺序直接选择前后项，跳过禁用/隐藏项并在边界循环。虚拟列表先滚动挂载下一逻辑行，接受画面后才选择，不在当前窗口提前循环。到组末尾后回到逻辑首项，反向亦然；跨虚拟列表时进入另一列表的逻辑端点，只构造需要的窗口。Tab 离开组时不为寻找下一停靠点构造虚拟行；跨窗口访问选项使用方向键。确认键、鼠标或触摸也可选择；重复选择不触发 on_change。脚本改值不会抢焦点。脚本改值不会触发回调，用户选择先更新绑定再交付 `{target,value}`。

```aside
let settings = model({sound: true, language: "zh-CN"});
column(gap: 12) {
    checkbox("Sound", value: bind(settings.sound));
    radio("zh-CN", "简体中文", value: bind(settings.language));
    radio("en", "English", value: bind(settings.language));
}
```

两者可以用 children 与 `decorated: false` 完全自定义外观。多选框读取 value；单选项可在 derive 中比较 value 和 option 选择自己的图片或颜色。绑定维持选择状态，外观无需 on_change 手工同步。完整可运行示例见 [author_selection.aside](../../examples/author_selection/src/main.aside)，提供 fonts/body.ttf 后运行。

`slider(value: 0, min: 0, max: 1, step: null, on_change: null, ...)` 提供水平滑条；`toggle(label: null, value: false, on_change: null, children: [], ...)` 提供开关。二者共享舞台位置、大小、颜色、可见性、禁用和模态属性，开关还可组合子节点。下面是可运行的 [author_settings.aside](../../examples/author_settings/src/main.aside)，提供 fonts/body.ttf 后可直接运行。

滑条要求有限且递增的 min/max、区间内的有限 value；step 为 null 或有限正数，以 min 为步进起点，两端均可选。脚本可赋任意区间内 value，不受输入步长限制。写 min/max 时当前 value 也须满足新范围，错误保留原值。鼠标或触摸拖动、方向键、Home/End、Shift 加速、手柄左右导航与可访问性操作共用同一控件。

接受用户变更后先更新 node.value，再执行 on_change，事件含 target 和此次 value。脚本赋值不会触发回调；回调可等待，节点移除会取消它。更改数值和布局保留拖动；更改范围/步长会结束旧拖动，接受新画面后可重新操作。没有回调的控件也保存值。节点值可用 bind/unbind 连接；derive 生成只读值，可用于文字和外观，不能作为 slider/toggle 的写回源。

滑条也能组合子节点。设 `decorated: false` 后，用图片或普通舞台节点绘制轨道、滑块及悬停/焦点状态，输入和无障碍仍由 slider 处理。此时轨道从内容区左端延伸到右端，没有内置滑块半径带来的缩进；padding 可用于给外观留边。见 [author_slider_skin.aside](../../examples/author_slider_skin/src/main.aside)，准备 `fonts/body.ttf` 后，在 `examples/author_slider_skin/` 中运行 `reel dev`。

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

### 窗口与呈现设置

使用自己的 `fonts/body.ttf` 运行 [author_window.aside](../../examples/author_window/src/main.aside)，资源目录可沿用上面的音频示例。

`window.info()` 返回当前客户区的逻辑 width/height、scale_factor、fullscreen、vsync、窗口模式的 windowed_width/windowed_height，以及 platform、capabilities 和 resolutions。platform 为宿主提供的 windows、macos、linux、android、ios、web 或 unknown；无窗口或未声明的平台为 unknown。适配平台文案可读取这个字段，显示设置按 capabilities.resize/fullscreen/vsync 决定；无窗口宿主的能力均为 false。resolutions 是当前显示器可用尺寸换算为逻辑像素后的列表，不是舞台画布大小。存档恢复后重新调用 info 可读取当前宿主，先前保存的 display 表仍保留原值。[可运行窗口示例](../../examples/author_window/src/main.aside)同时展示平台与客户区尺寸。

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

### 连接舞台属性

`text_input` 提供单行文本编辑。value 可以连接可写模型字段；on_change 在提交文本改变后调用，on_submit 在 Enter 确认时调用，事件都有 target 和 value。中文输入法的预编辑不会提前写入模型或触发回调。支持光标移动、选区、删除及复制、剪切和粘贴。中文转换与候选词由系统输入法提供；嵌入宿主转发公共 IME 事件并响应候选窗位置通知。value 赋值须为单行字符串；玩家粘贴或输入的换行转换为空格。外观使用共同节点属性，默认不绘制额外边框，作者可以自行组合背景和焦点提示。

指定 height 时，输入文字在内容区内垂直居中，光标、选区和输入法位置随之对齐。普通 text 指定 width 后，自动高度包含该宽度下的全部换行。

富文本事件链接在悬停或键盘聚焦时显示下划线；反馈只重绘，不重新排版文字。自定义滚动条应将拖动比例限制在 0 到 1，再写入 scroll_y，以免外观跟随尚未由布局校正的越界值；完整实现见 author_scrollbar.aside。

```aside
let profile = model({name: ""});
stage.replace(text_input(value: bind(profile.name), width: 280, height: 40,
    color: "#202020", background: "#ffffff",
    on_submit: |event| { profile.name = event.value; }));
```

可运行示例：[author_text_input.aside](../../examples/author_text_input/src/main.aside)。

多行笔记或留言使用 `text_area`，参数与 text_input 相同。两者使用 `text_align` 设置水平对齐，`line_height` 设置字号倍率行高。text_area 默认按宽度自动换行，`text_wrap: false` 保留显式换行并允许横向滚动；正文从顶部开始。Enter 换行，Ctrl+Enter（macOS 为 Command+Enter）触发 on_submit，Tab 切换焦点。上下方向键移动光标，配合 Shift 扩展选区；Home/End 定位当前显示行，配合 Ctrl/Command 定位全文首尾。滚轮查看溢出内容，编辑时自动将光标滚入可见区域。粘贴的 CRLF/CR 转为 LF；IME 预编辑在确认后才写回 value。保存恢复保留正文和绑定，选区及内部滚动位置随输入控件重建。可运行示例：[author_text_area.aside](../../examples/author_text_area/src/main.aside)。

按键打字演出或键盘小游戏使用 `input(on_key: |event| { ... })`。事件提供 code、key、phase 和 shift/ctrl/alt/meta，phase 为 pressed/repeated/released/cancelled；多键分别交付，失焦会取消旧按键。聚焦按钮先处理自己的确认键，其余未消费的键向祖先 on_key 冒泡，因此点击语言按钮后仍能继续按键打字。聚焦的文本编辑控件保留全部键盘和系统输入法事件。actions 可省略，命名动作可同时用于手柄或鼠标映射，动作映射仍遵守焦点所有权。示例：[author_keyboard.aside](../../examples/author_keyboard/src/main.aside)。

`bind` 和 `unbind` 已支持现有节点属性。下面两个滑条共用一个位置值，文字位置和显隐也直接连接控件；不需要回调复制状态。可运行窗口示例见 [author_bindings.aside](../../examples/author_bindings/src/main.aside)，字体资源准备方式与第一节相同。

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

## 八 风格与共同运动

图片使用相同资源时由播放器共享。wgpu 后端复用已提交的 GPU 图片，界面文字或悬停状态变化不会要求重新上传整套图片；缓存会在资源退休或需要腾出空间时释放，作者无需管理 GPU 句柄。

普通函数、样式展开、节点属性修改、变换、有限动画和临时 scope 覆盖已可用。节点的 `corner_radius` 设置背景和边框圆角，`border_width`、`border_color` 设置内描边；例如 `panel.with(background: "#182638", corner_radius: 12, border_width: 2, border_color: "#92a8c8")`。内容留白由 padding 控制，样式沿用节点的布局与命中矩形。输入框和多行笔记的组合见 [author_text_area](../../examples/author_text_area/src/main.aside)。

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
`...style` 将对象自有字段作为具名参数传入，普通函数、节点构造器和提取的方法使用相同规则。覆盖值先合并为一个对象；重复传同一参数会报错。位置参数放在具名参数之前，尾随 children 也是具名参数。完整初始化和按钮交互见 [author_buttons.aside](../../examples/author_buttons/src/main.aside)。
节点属性可以直接修改；持续跟随业务或偏好变化的外观使用 bind/derive。普通风格记录用于构造，运行期共享配色可以用 model 保存，再把其属性绑定到文字与图片。作者自己的组件既可完全手绘，也可沿用熟悉的布局与控件行为。

按钮高亮可使用 `derive(|| control.hover || control.focus_visible)`。只读 `focus_visible` 在键盘/手柄导航时显示焦点，使用指针后隐藏该提示；`focused` 仍保留实际焦点，不影响随后按回车确认。两个属性均包含后代，隐藏、禁用或窗口失焦时停止显示。背景演出不会使无关按钮的输入失效；替换按钮内容仍须显示新画面后才能接受新选择。参见 [author_fitted_text.aside](../../examples/author_fitted_text/src/main.aside)。
UI 与立绘共同运动时，把它们放进同一个组，再对组执行动作。
```aside
let cast = stack() {
    image("assets/mio.png");
    dialogue_view(game.dialogue());
};
stage.replace(cast);
await run(tween(cast, x: 80, duration: 0.5));
```
`scope(body:, owner: null)` 在当前任务中运行一段演出，返回 body 的值。默认 run、spawn、clock、声音、视频、粒子、completion 和呈现等待归当前作用域；body 返回或抛出后统一取消未完成工作。`s.run(action)`、`s.spawn(work)` 可以显式加入该作用域。

```aside
scope(body: |s| {
    s.run(repeat(sequence(
        tween(view.hud, opacity: 0.4, duration: 0.5),
        tween(view.hud, opacity: 1, duration: 0.5)
    )));
    narrator("The light keeps pulsing while this line is shown.");
});
```

普通动画在停止位置保留属性值。scope 覆盖和加性偏移在退出时撤销并显露最新底值。绑定到 owner 节点的作用域在节点移除时取消内层工作，外层 catch 收到 "ScopeCancelled" 后可以继续；内层脚本 finally 被跳过，外层 finally 正常执行。取消外层任务会清理它的作用域。显式指定另一个 owner 的工作按该 owner 的寿命运行。作用域及其等待、活动和取消清理过程随同包存档恢复。

[author_scope.aside](../../examples/author_scope/src/main.aside) 展示一个按钮同时启动动画和后台任务，另一个按钮通过一个任务句柄停止整段演出。运行 `reel dev`（在 `examples/author_scope/` 中）。

## 九 遮罩 特效与自定义演出
舞台支持分组透明度、裁剪、图像遮罩、混合及转场。源码和 ASB 通过 dev/run 使用同一舞台与资源加载器。blur 等效果仍待实现；平台验收范围见[窗口说明](../../crates/reel-player/README.md)。

`stage.transition` 保留目标当前画面，执行 change，再将旧画面过渡到继续运行的新内容。调用完成后返回 change 的结果；与 scope 一样直接调用。并发使用 spawn。

保留准备只提取目标范围的绘制数据并共享图像、字体和视频内容；目标越复杂，准备的复制工作越多。这项工作发生在请求转场时，播放中继续复用旧画面。

```aside
stage.transition(target: scene, duration: 0.6, change: || {
    scene.clear();
    scene.add(image("assets/room-evening.png"));
});
```

省略 target 可在 change 中用 stage.replace 更换整个逻辑舞台。指定 target 时保留节点本身，修改属性或子节点。image 可以直接更新 source，text 可以直接更新 value；游标仍按窗口独立呈现。默认淡化；mask 接受下文的图像路径或组合表达式，mask_softness、mask_invert、mask_x/y 沿已有规则，easing 使用 tween 的内置具名曲线，转场进度始终夹在 0..1；弹性超调仍等到 duration 结束。图像路径按进度推进阈值；组合表达式通过 mask.param("progress") 表达随时间变化的覆盖率。duration 为零直接换景并等待呈现。图片和字体准备好后才开始计时。

转场会保留实际粒子、文字和视频画面，作者负责构造新内容。目标的输入暂停到最终画面接受，外部菜单继续工作；change 可以等待，交互等待放在转场返回后。互不重叠的子树可同时过渡；相同或相互包含的目标会报告冲突。取消解除过渡并显示当前舞台，已经执行的 change 写入保留。播放中存档恢复旧画面和进度，继续原调用。

运行 `reel dev`（在 `examples/author_transition/` 中）可试用淡化和图像擦除，并在转场时操作目标之外的按钮。完整代码见 [author_transition.aside](../../examples/author_transition/src/main.aside)。

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
mask_progress 和 mask_softness 使用普通 derive/tween。完整示例：[author_image_mask.aside](../../examples/author_image_mask/src/main.aside)。进度为 1 时跳过遮罩合成；用完可将 mask 设为 null 释放资源引用。遮罩不改变逻辑命中，需要禁用交互时使用 enabled/visible。遮罩需要完整 PNG/JPEG，不接受分块 .reel-image。

`mask.alpha` 采样图片 alpha，`mask.gray` 采样编码 sRGB 灰度。用 `mask.add`、`subtract`、`multiply`、`pow`、`clamp` 和 `smoothstep` 组合采样值。采样接受 `x`、`y`、`scale_x`、`scale_y` 及 `repeat`（`"none"`、`"x"`、`"y"`、`"both"`）；坐标按蒙版节点归一化，随节点变换。`mask.param("x")` 和 `mask.param("y")` 读取可动画的 `mask_x`、`mask_y`，`"progress"` 和 `"softness"` 读取已有对应属性。

```aside
let edge = mask.alpha("assets/mask-waves.png", x: mask.param("x"), repeat: "x");
let reveal = stack(width: 640, height: 360, mask: edge, mask_x: 0) {
    stack(width: 640, height: 360, background: "#2d8aab");
};
stage.replace(reveal);
await run(tween(reveal, mask_x: 1, duration: 3), owner: reveal);
```

`pow` 先把底数夹到零，指数是有限非负常数，`0^0` 为 1。`clamp` 接受有序常量边界。`smoothstep` 支持倒序边界，相等边界则在该值产生硬阶跃。最终结果夹到 0..1。组合表达式在 `mask_progress: 1` 时仍然生效；参数动画复用表达式结构和图片资源。[author_composed_mask.aside](../../examples/author_composed_mask/src/main.aside) 使用随附的两张 alpha 图片展示移动显现效果。

节点 `invert` 将整个子树的合成画面反色，范围 0..1，默认 0；1 完全反色，中间值渐变。颜色按 sRGB 反转，透明度保持不变；对白和菜单放在同级节点即可独立于反色场景。可以直接赋值、derive 或 tween，参数随存档恢复。

```aside
let scene = stack(width: 800, height: 450, invert: 0) {
    text("A different view", color: "#e08020", font_size: 48);
};
stage.replace(scene);
await run(tween(scene, invert: 1, duration: 0.4), owner: scene);
```

完整可运行示例：[author_invert.aside](../../examples/author_invert/src/main.aside)。反色和 opacity/mask 共用组离屏目标；不用时设为 0，独立反色组不再需要额外目标。

图片、文字、UI 与演出可以用共同节点属性 `blend` 混合整个子树：`normal` 正常叠放，`add` 加色发光，`multiply` 正片叠底，`screen` 滤色。`opacity`、遮罩与反色先处理子树，再与后方画面混合；混合不改变布局或交互。颜色在线性空间计算。下面的光层也可放进图片、粒子或组件，完整示例见 [author_blend.aside](../../examples/author_blend/src/main.aside)。

```aside
stage.replace(stack(width: 320, height: 180, background: "#204060") {
    stack(width: 320, height: 180, background: "#800000", blend: "add", opacity: 0.5);
});
```

效果参数与节点属性采用同一动画规则。更复杂画面可以嵌套 group：背景单独模糊，人物保持清晰，图片按钮仍在最上方；再对整组使用遮罩或镜头变换。所有子节点的前后顺序由树结构决定。
自定义效果通过版本化 Effect 描述输入图像、参数、输出范围和后端实现。高级作者可以开发宿主扩展，常用效果随后以普通 Aside 函数组合复用。shader 支持属于后端能力，项目检查列出缺少的能力。可移植效果应同时提供简化实现，游戏通过能力查询选择适合的平台版本。
特效改变的像素与逻辑输入区域分别声明。普通仿射变换与几何裁剪自动映射命中；弯曲、扭曲和任意着色器的交互区域由作者提供逻辑形状或专用映射。为每次点击从 GPU 读回画面会增加延迟，因此基础接口使用可直接计算的区域。

### 图片粒子

粒子可以与节点的透明度、变换等 tween 同时播放；`stage.presented` 可在演出中间帧完成。普通属性更新沿共同舞台合成，目标、owner 或作用域退休仍会取消对应批次。

`particles(source, seed:0, ...节点属性)` 创建一个共享精灵与坐标空间的舞台节点。source 必须是完整 PNG/JPEG 精灵；预处理的 `.reel-image` 只供 image 使用。每次 `emit` 追加一个批次并返回独立活动。示例需要自行提供 `assets/petal.png` 和 `fonts/body.ttf`，完整示例见 [author_particles.aside](../../examples/author_particles/src/main.aside)。

```aside
let petals = particles("assets/petal.png", width: 1280, height: 20, seed: 42);
stage.replace(petals);
let falling = emit(petals, burst: 0, rate: 80, duration: 10,
    lifetime: 8, velocity_y: 90, velocity_x: 15, spread_x: 30,
    size: 18, size_spread: 6, spin_spread: 60);
await falling;
```

`emit(node, burst:1, rate:0, duration:null, lifetime:5, lifetime_spread:0, velocity_x:0, velocity_y:80, spread_x:20, spread_y:20, acceleration_x:0, acceleration_y:0, size:12, size_spread:4, size_end:null, opacity_start:1, opacity_end:0, spin:0, spin_spread:30, x:0, y:0, owner:null)` 描述本批粒子。默认立即发射一粒。burst 为零时，定时流的第一粒在 1/rate 秒后出生。duration 为 null 时，正 rate 持续发射；有限 duration 包含截止时刻的出生。rate 为零只产生瞬发粒子，即使 duration 为 null。duration 为零仍发射 burst。最后一粒实际死亡后，活动完成。

spread 是围绕基值的均匀半幅；size 是保留图片宽高比的宽度。size_spread 不超过 size，lifetime_spread 不超过正数 lifetime，每粒分别确定性取样寿命。size_end 为 null 时保留随机出生宽度，非负终值则在该粒寿命内线性插值。每次 emit 捕获节点 width/height 作为均匀出生矩形尺寸，缺省为零。本批 x/y 指定节点本地坐标中的矩形原点，接受正负有限数并在发射时固定。修改节点 x/y 仍移动全部批次；用 emit 的 x/y 可在同一节点的不同位置追加效果。速度单位是逻辑像素/秒，加速度为像素/秒²，spin 为度/秒。source 和 seed 只读；节点的位置、旋转、缩放、可见性、透明度和裁剪仍同时作用于全部批次。

`pause(falling)` 只冻结本批的出生与运动，`resume(falling)` 继续，`stop(falling)` 清除本批并使 await 返回 Stopped。`finish(falling)` 清除有限批次并返回 Completed，连续发射不可 finish。再次 emit 保留旧批次。相同节点种子、成功发射顺序和累计时间产生相同粒子，不依赖分帧；连续发射使用不同随机样本。节点退休取消全部批次，owner 或作用域退休取消归属它的批次。隐藏不暂停；取消等待者不取消生产者；终态可重复 await。存读档保留各批的配置、时间、暂停与归属，以及下一发射序号，不执行逐粒脚本。

以下喷射在同一节点上重叠两批。通用风格可用普通函数封装 emit：

```aside
let spray = particles("assets/petal.png", x: 640, y: 360, seed: 42);
stage.replace(spray);
fn burst() {
    emit(spray, burst: 48, lifetime: 1, lifetime_spread: 0.4,
        velocity_y: -160, spread_x: 180, spread_y: 100,
        acceleration_y: 240, size: 18, size_spread: 6, size_end: 0)
}
let first = burst();
await run(delay(0.2));
let second = burst();
await first;
await second;
spray.remove();
```

## 十 选择与会变化的内容

文字可以逐字淡入。`reveal_fade` 默认null，开启后用数值表示透明度波前；`reveal_fade_from` 是起点的UTF-8 byte边界，起点之前保持不透明；`reveal_fade_step` 表示相邻字素的透明度差，默认0。第k个字素的透明度乘 `clamp(reveal_fade - k * reveal_fade_step, 0, 1)`，既有reveal继续限制显现末端。进度与步长可以bind/derive或tween，起点支持绑定。淡显复用全文排版；连字保持完整，描边、注音与文字一起淡入，链接完整显现且每个字素的透明度均大于0时才可激活。下面的速率与字间差可由作品自定。完整示例：[author_text_fade.aside](../../examples/author_text_fade/src/main.aside)。

```aside
let caption = text("The light is moving.", reveal_fade: 0,
    reveal_fade_from: 0, reveal_fade_step: 20.0 / 255.0);
stage.replace(caption);
await run(tween(caption, reveal_fade: 3, duration: 3.0 * 255.0 / 1400.0), owner: caption);
caption.reveal_fade = null;
```

text 默认按可用宽度自动折行。设置 `text_wrap: false` 保留每个显式书写的行，超出宽度的部分仍显示，或由 clip 裁剪；它可以直接赋值，也可以 bind/derive。这个开关也适用于 text_area；text_input 保持单行，按钮等控件的标签仍不自动折行。完整示例：[author_text_wrap.aside](../../examples/author_text_wrap/src/main.aside)。

```aside
stage.replace(text("A long authored line without automatic wrapping.\nA second explicit line.",
    width: 240, text_wrap: false, clip: true));
```

固定宽度的选项或标题可以用 `text_fit: "shrink"` 自动缩小完整文字，`min_font_size` 指定最低字号；默认 `text_fit: "none"` 保留普通排版。shrink 始终按不自动折行的完整文本适配，不受 text_wrap 开关影响。适配只缩小、不放大，保留显式换行，富文本和 ruby 一起缩放；打字机不会逐字改变字号。最低字号仍放不下时可以配合 clip。`text_outline` 设置字形向外描边宽度，`text_outline_color` 设置颜色，适用于 text 和控件标签。例如 `text("Long option", width: 140, font_size: 30, text_fit: "shrink", min_font_size: 14, color: "#000000", text_outline: 2, text_outline_color: "#ffaaff")`。这些属性可以 derive，让悬停或焦点只改变文字外观。

独立演出或小游戏后台逻辑使用 `spawn(work, owner: root)`。work 是零参数函数、闭包或绑定方法，可以循环并等待动画/结果；`await task` 取得返回值或原错误，重复等待得到同一终态。`task.cancel()` 首次请求取消返回 true，之后返回 false；任务清理后等待者收到 Cancelled。取消不会执行脚本 finally，资源应交给节点所有权管理。节点移除自动取消所属任务，隐藏不取消；取消等待者不取消生产者。省略 owner 时继承回调/任务节点，在主剧情中归会话。正文在当前调用者等待或返回后开始调度，不占用主剧情的对白/选项权限；初始化、纯 derive 和 NoWait 中不能启动任务。见 [author_tasks.aside](../../examples/author_tasks/src/main.aside)。普通固定演出继续用 sequence/parallel/repeat；`tween` 可用 `quad_in`、`quad_out`、`quad_in_out` 表达二次曲线，in/out/in_out 表达三次曲线。

任务容量随实际并发数量增长和复用。连续启动短任务会复用已结束的任务槽，每次启动按当前驻留情况准备执行空间。调用入栈时，VM 按当前执行片预算准备所需窗口；预算不足保留该调用，获得所需额度后继续。Rust 宿主可用 `VmLimits.max_tasks` 设置自己的准入配额；分配和执行空间整理计入堆预算。

普通函数在当前任务内调用，内部等待完成后才返回。例如 `ask()` 内部已 `await result` 并返回 bool，调用者直接写 `if ask() { ... }`；只有返回活动或 completion 等可等待对象时才再写 await。对 bool 等普通值使用 await 会在该行报告 `NotAwaitable`。

`text_shadow_x`、`text_shadow_y` 设置文字阴影偏移，`text_shadow_color` 设置统一阴影色，双零偏移关闭阴影。例如 `text("标题", text_shadow_x: 1, text_shadow_y: 1)`。阴影与正文复用排版，跟随逐字显示和淡入；富文本局部颜色保留在正文中。

可运行的缩字、语言切换和悬停描边示例见 [author_fitted_text.aside](../../examples/author_fitted_text/src/main.aside)。

事件回调不需要事件数据时可以写 `on_click: || confirm()`，也可直接传零参数绑定方法；需要事件时写 `on_click: |event| inspect(event.target)`。声明参数或 rest 的回调收到一个事件，绑定方法的 self 不计入这些参数。所有事件回调采用相同规则，仍然可以等待，随所属节点退休。

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

`.reject(error)` 在 await 处抛出该值，`.cancel()` 抛出 Cancelled。三个结算方法首次返回 true，之后返回 false，便于处理重复点击；多次 await 得到同一终态。删除 owner 节点取消未结算结果，隐藏节点不会取消；取消一个等待者不影响其他等待者。省略 owner 时，主剧情归会话、回调归事件节点。resolve 原样传递对象，不自动等待其中的其他结果。此接口不占用 choose 或对白，也不产生历史记录。完整表单与子游戏组合见 [author_results.aside](../../examples/author_results/src/main.aside)；运行前提供 fonts/body.ttf。

`completion` 和 `spawn` 的结果都提供只读的 `pending`。它在实际工作结束后变成 false，可以用 bind/derive 表达按钮可用性、忙碌提示或组件重入条件；任务取消清理完成前保持 true。在 scope 内创建 completion()，让结果和临时画面一起结束：

```aside
scope(owner: root, body: |s| {
    let result = completion();
    s.add(root, button("Confirm", enabled: bind(result.pending),
        on_click: || result.resolve(true)));
    await result
});
```

长期组件可以保存当前 request，用 `request != null && request.pending` 判断是否已有请求。普通返回、错误、调用者取消和 owner 退休都由 scope 收起临时画面；等待者取消本身不会取消作用域外的共享结果。完整可重复打开的提示框见 [author_prompt.aside](../../examples/author_prompt/src/main.aside)。

[author_embedded_puzzle.aside](../../examples/author_embedded_puzzle/src/main.aside) 将一个二维开关谜题嵌进自定义对白：点击图形或通过键盘/手柄焦点操作，暂停时同时停止组件时钟和提示动画，完成后将步数交回剧情；离开组件会取消未决结果并退休所属活动。谜题、对白和菜单都使用普通舞台节点。示例使用现有事件与活动，不提供物理或固定步进更新循环；运行前提供 fonts/body.ttf。

choose、自定义 choice_view 和只读选择模型已实现。引擎不自动叠加对白或选择界面；作者将自己的组件挂到舞台，普通函数可封装并复用外观。完整 ADV/NVL 切换及选择示例见 [author_narrative.aside](../../examples/author_narrative/src/main.aside)。

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
if 分支可以直接以普通 table 收尾，适合按条件组合输入绑定或组件参数。块内有名称、字符串或计算键的字段，以及 table 展开，都沿用相同写法；返回空 table 写 `({})`。
```aside
let flag = true;
let actions = if flag {
    {accept: [binding("Enter")]}
} else {
    {back: [binding("Escape")]}
};
```

普通 children 中的 for 构造当下内容；`view(|| ...)` 在实际读取的可观察数据变化后重新构造子树，外层容器身份保持。计算成功后才替换旧内容；旧节点拥有的回调和活动随之取消。用 bind/derive 更新单个属性时保留节点身份。需要保留控件身份的集合使用 `list(|| data.items, key: |item| item.id, build: |row| text(value: derive(|| row.value.label)))`。插入、排序保留相同 key 的节点、焦点和局部状态；删除则退休其任务和活动。key 可以是任意值，但在列表内必须唯一；标量区分类型，字符串按内容比较，其余对象按身份比较。因此可直接使用选项 id 或稳定的对象引用，不必另造数字 id。每个新 key 只调用一次 build，参数 row 提供只读可观察 key、value（当前条目）和 index（从 0 开始）；返回 null 时该 key 不产生节点。回调读取 row.value 取得最新记录；derive 会跟随记录替换，bind(row.value.label) 则固定原字段位置。只有来源函数和 key 函数的读取决定列表协调；行内变化使用 bind/derive/view。重复 key 或组件失败不替换原树。列表子节点由来源数据管理，外层可组合 scroll。默认保留全部条目。见 [author_list.aside](../../examples/author_list/src/main.aside)。

长菜单、历史和图库可使用虚拟列表 `list(..., virtual:true)`，放在有限高度纵向 scroll 内，作为唯一的流式内容。它只保留窗口附近的行；离开后组件会退休，回来时重新构造，长期状态应保存在 model 中。正在编辑或拖动的行保留到交互结束。来源先过滤不显示的项，build 必须返回节点，行的自然高度须为正；list 使用自然高度，不设置 height/max_height、纵向 stretch 或自身位置/旋转/缩放/锚点，整体变换可放在外层 scroll。行内仍可组合普通文字、图片、控件和自定义外观。不同高度由布局自动测量，缓冲区在未测行进入视口前提前补充；新窗口先按本次测量连续排列并保留阅读锚点，支持跨窗口键盘/手柄导航。滚动与行偏移保留小数精度，缩放画布也使用同一绘制和命中坐标。尚未测量的内容使用当前宽度已测行的平均高度，所以自定义滚动条的 content_height 仍会逐步修正；远跳和新图片的实际尺寸无法提前保证。它减少组件和排版成本，全文与来源数据仍保留。多列图库可以先把每一排照片组织为稳定 key 的条目，再在 build 中返回普通 row 组合；整排一起测量和退休，照片的长期状态仍放在 model 中。列数改变或重新分组后，新行按新 key 构造，这不提供二维网格虚拟化。可交互的千项菜单见 [author_list.aside](../../examples/author_list/src/main.aside)：支持变高说明、展开状态、过滤、重排、键盘和手柄选择。运行 `reel dev --assets ../../target/reel-artifacts/author-assets`（在 `examples/author_list/` 中），先按示例准备 fonts/body.ttf。

可运行的自动测量图库及自定义滚动条见 [author_list_gallery.aside](../../examples/author_list_gallery/src/main.aside)。准备 fonts/body.ttf 后运行 `reel dev --assets ../../target/reel-artifacts/author-assets`（在 `examples/author_list_gallery/` 中）。行内按普通布局表达内容，不需要另传行高。拖动以指针位移更新当前阅读位置，避免总高度修正时静止指针再次改变位置。补充缓冲行的图片在后台并发加载；已准备的连续行覆盖视口、实际可见内容的资源完整时，滚轮和拖动可以继续推进，按钮、文本和悬停状态沿当前画面更新。离屏缺图使用已明确的 width 与 height 排版；需要图片自然尺寸时会等待资源。远处窗口构造、测量期间保留上一完整画面，准备就绪后发布；新的 stage.presented、geometry 和截图等待新画面接受。

按 key 定位：`let row = await items.scroll_to(key, align: "nearest")` 先构造和测量目标附近的行，在实际呈现后返回该行节点。align 可选 start、center、end、nearest，默认只做必要滚动。重排仍跟随同一 key；过滤掉目标、移除列表或新的滚动/定位会取消旧请求，可用 try/catch 处理。返回节点仍遵循普通离屏退休规则，持久状态保存在 model。
等待期间改变视口尺寸会重新校准目标；自动夹取滚动范围不会取消请求。来源重算后，会等待新位置的实际呈现再返回。

虚拟列表补充屏外行时，已有行仍覆盖画面就继续响应滚轮和拖动。每个新行完成后，滚动条和悬停外观的派生先结算，再继续构造；补窗的小位移保留构造进度。来源、布局宽度或定位目标改变时重新准备。stage.presented、stage.geometry、list.scroll_to 和截图继续等待完整提交的画面；行工厂与普通组件使用相同的取消和保存规则。

## 十一 存档 历史 路线与跳过

历史、自动阅读和存读档接口已实现；路线切换与按章节跳转仍为设计目标。界面由作者组合，服务不生成固定页面。

跨章后不再需要之前的历史时，调用 `game.clear_history()`；用 `game.clear_history(keep_last: 37)` 保留最近 37 条实际历史行，较早记录被删除。数量按行计算，不按对白组计算，`history:false` 的行不计入；可以只保留最早一个保留组的尾部。剩余组和行保持原 ID、对象、内容及顺序，不重新编号，组 speaker_name/text 更新为剩余首行。顶层可观察数组和保留组的 lines 数组保持身份。缺省或 0 清空全部，数量不少于现有行数时不改动，负数或非整数报错。当前对白、NVL 显示页、推进权限、已读进度、偏好及存档槽不变；加载存档仍恢复保存时的历史，新组 ID 不复用。裁剪工作量随记录数增长，并释放会话持有的引用。脚本持有的行引用或界面缓存仍会保留内容，需要同时退休旧视图或依据历史数组刷新缓存。引擎不自动限制历史长度，无需脚本定时清理。见 [author_reading.aside](../../examples/author_reading/src/main.aside)。

```aside
game.dialogue().clear();
game.clear_history();
narrator("A new chapter begins.");
```

`game.dialogue().advance_requested` 与当前活动行的同名字段是只读可观察的逐段手动推进请求。有效手动 Advance 在允许跳过显现或正文已经 ready 时置 true；尚未完成且禁止跳显时保持 false。自动阅读与快进驱动不会设置它。新对白、append 新段和清页重置；快照保留当前值。互动结束后根字段为 false，仍展示的旧行保留最后一次投影值；用 `current_line_ids` 确定描述当前段的行。作者可用它结束自定义文字演出，输入仍由共同对白服务接受和推进。

```aside
game.clear_history(keep_last: 37);
```

`saves.save(slot, title: null, thumbnail: null)`、`saves.load(slot)`、`saves.delete(slot)`、`saves.thumbnail(slot)` 和 `saves.list()` 立即返回可等待请求，具有只读的 `status`（pending、succeeded、failed）与 `error`。失败会在 await 处抛出错误；成功保存意味着完整文件已原子发布。`await saves.list()` 返回已占用槽位摘要，包含字符串 id、title、saved_at（UTC Unix 秒）与 has_thumbnail，不加载图片。槽位名是逻辑标识；作者决定页面与空槽，用 `"quick"` 等固定标识组合快速保存。

可以等待自己的保存请求。这份存档恢复时，该请求为已成功且只读 `restored` 为 true（正常写盘完成为 false），可据此关闭存档菜单；调用者继续执行 await 后的语句；保存调用不会重复执行。其他未完成的外部请求以 OperationInterrupted 结算。成功读档会替换会话，原来的读档调用不会在旧会话中继续。

保存会捕获执行状态并在后台写入槽位；捕获期间若输入需要改变状态，请求以 CaptureBusy 失败，输入继续处理，旧槽位保留。缩略图与快照原子发布。存档要求代码与格式匹配，当前没有跨版本迁移；persistent 系统数据独立于剧情快照。

可运行示例：[author_saves.aside](../../examples/author_saves/src/main.aside)。

截图与预览示例：[author_capture.aside](../../examples/author_capture/src/main.aside)。`await stage.capture(width: 256, height: 144)` 取得最近已呈现画面的 PNG Bytes；在打开菜单前获取，再传给 thumbnail。`await saves.thumbnail(slot)` 只读取指定槽的图片，没有图片时返回 null；将结果交给 `image(bytes)` 即可显示。未呈现画面、宿主不支持截图或资源不足都会拒绝请求，可用 try/catch 显示错误。

将菜单、推进标记等节点设为 `capture_included: false`，它们及子节点会从 `stage.capture` 的结果中排除。显示和交互照常；捕获使用已呈现帧保存的属性，转场的前后画面分别保留自己的排除范围。

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
stage.replace(button("Save", enabled: derive(|| !menu.busy), on_click: |e| save_slot("page1/slot1")));
```

`game.start(entry)` 创建全新会话；`saves.load(slot)` 恢复保存时的变量、位置和画面。同会话的章节重启 `game.restart(story_id)` 属于规划接口，当前作者环境尚未注册。
`game.reading()` 返回稳定的只读可观察 `auto/paused/delay` 状态。`game.set_auto(enabled: null, delay: null, condition: null)` 控制自动阅读；delay 为全文实际呈现后等待的有限非负秒数，默认 2 秒，省略时保留原值。`game.pause_reading(paused)` 暂停文字显现、自动推进和对白/选项输入，普通 UI 控件仍可用于关闭回看页。自动阅读在选择处等待玩家操作，之后继续；失焦、挂起、模态界面、待加载资源和正在进行的输入手势均停止计时，恢复后重新等待完整 delay。设置属于会话，更换 UI 保留，新会话默认手动、未暂停。有限演出和独立音频有各自控制，不随阅读暂停。按钮外观、布局、打开/关闭回看页的方式均由作者决定。 `condition` 接受 bool 或 `bind`/`derive` 引用；各参数为 null 时保留当前值及绑定，condition 为 true 时解除旧绑定。只读 `auto_ready` 显示条件的当前值，初值 true。false 只重置自动倒计时，文字显现、手动推进、快进与 `[next]` 保持原路径；恢复 true 后，全文实际呈现完毕再等待完整 delay。条件及绑定随快照恢复。
`game.set_skip(enabled, unread: false, after_choices: false)` 已实现。`game.reading().skipping/skip_unread/skip_after_choices` 可观察状态与策略，`game.dialogue().read` 表示整组提交前是否已读。默认遇未读对白或选择时关闭快进；unread: true 可跳过未读对白；after_choices: true 在选择时保留快进模式，等待玩家选择后继续，仍遵守已读策略。暂停、失焦和模态界面仍会取消该模式。快进沿正常控制流执行，跳过显现/停留，并完成主剧情拥有的运行中有限演出；无限、暂停、节点拥有的活动和独立音频继续按原控制工作。自动阅读与快进互斥。暂停阅读、失焦、挂起和模态界面会关闭快进；启动快进的 input 节点退休也会清除其保持状态。作者用 input 回调在 pressed 时开启，在 released/cancelled 时关闭，也可自制切换按钮。已读在对白推进完成后记录，取消或仅显现全文不算。新会话默认保留由宿主提供的已读进度。源码模块版本变化保守视为未读。
按章节跳过 `game.skip_to(story_id)` 仍依赖后续章节注册服务。

重置已读进度使用 `game.clear_read_progress()`。当前等待对白的组与各行 `read` 立即变为 false，仅跳已读的快进停止；之后的对白重新算未读，正常推进后重新登记。历史和已完成 NVL 行保留原有样式，自动阅读与允许跳过未读的策略保持原值。读档时，当前等待对白也按最新已读记录重新判断。此操作释放旧已读记录，耗时随记录容量增长，不读写磁盘，也不删除游戏存档或 `persistent` 系统数据。运行示例见 [author_reading.aside](../../examples/author_reading/src/main.aside)。

```aside
stage.replace(button("Reset read progress", on_click: |event| game.clear_read_progress()));
```

这些服务提供数据与操作，页面外观由作者组合。手动点击或确认对白会执行一次正常显现/推进，并关闭自动阅读和快进；无效输入不改变模式。存读档、历史与阅读控制已实现；路线和章节注册仍待补齐。

## 十二 游戏变量与组件状态

`model`、字段双向绑定、存档恢复和独立系统数据读写已实现；state、node.state 仍是设计目标。

用 model 保存独立于画面节点的业务数据。多个控件可以共享同一字段，输入直接写回数据，连接的文字、可见性和样式随之更新。运行 [author_models.aside](../../examples/author_models/src/main.aside)：在 `examples/author_models/` 中执行 `reel dev --assets ../../target/reel-artifacts/author-assets`，使用第一节准备的字体。

```aside
let settings = model({volume: 0.8, music: true, palette: {accent: "#70d890"}});
let volume = slider(value: bind(settings.volume), min: 0, max: 1);
let music = toggle(value: bind(settings.music));
let indicator = text("Music", visible: bind(settings.music), color: bind(settings.palette.accent));
settings.palette.accent = "#ffd580";
```

默认数据图会被复制，保留内部别名与环，不修改传入的默认对象。已完成构造的类实例、table、array、tuple、字符串、字节串及可变缓冲可作为数据；闭包和节点等宿主资源不能用作默认数据。后续赋入的对象保持共享身份，取得嵌套实例、table 或数组元素的别名后仍可绑定其字段。普通 table 方法继续可用；clear 冻结已有连接的最后值并使旧引用失效，新增同名字段需重新 bind。复制按预算分片，取消不留下部分模型。

语言中的 let/let mut 决定绑定能否改写，model 决定属性是否可观察，状态的所属范围决定保存和释放。三者各司其职。
系统数据使用 `persistent.read(key, default: null)`、`persistent.write(key, value)`、`persistent.delete(key)`。操作返回可等待请求，失败由 catch 接收；写入完成才表示文件已发布。缺失记录返回 default，保存的 null/false/0 保持原值。支持 null/bool/int/float、string/Bytes、array/table/tuple，保留别名与环；table 仅存自有字段。类实例、节点、任务和闭包先转换为数据。文件独立于剧情槽和 ASB 版本，位于同一 save-directory，不出现在 saves.list 中。见 [author_persistent.aside](../../examples/author_persistent/src/main.aside)。

```aside
let preferences = await persistent.read("settings", default: {volume: 0.8});
preferences.volume = 0.6;
await persistent.write("settings", preferences);
```

读取结果是普通副本，修改后需显式 write。剧情读档不会改写系统文件，但局部副本会随快照恢复；在 `saves.save` 请求的 `restored` 分支重新读取并应用当前设置。新游戏入口也应读取系统数据。已交给后台服务的写入不因切换会话撤销；未提交的请求不会在读档后重放。业务数据升级由游戏根据自己保存的版本字段迁移。
model/state 的默认数据图复制后建立可观察属性，复制保留内部别名和环。内嵌可变数组、table、Set、缓冲以及注册的可观察数据类型都通过统一变更钩子通知；取得内部别名后修改也会通知。随后赋入的会话对象保持普通共享身份，并被纳入可观察图。model 对象通过实际引用保活。组件临时状态可放入节点自己的状态槽。
作者用 persistent 保存全局偏好，并在启动和剧情存档恢复后读取、应用当前设置。业务 state 的修改采用常规控制流，scope 临时效果结束也保留其业务结果。

## 十三 作者怎样扩展自己的积木

普通节点函数、命名参数、缺省值、children 和闭包已可用。下方 arrive 示例使用已实现的有限动画接口。

复用的最小单位是普通 fn。返回节点的函数用于画面，返回动作的函数用于演出，普通对象用于把需要操作的部分交给调用者。命名实参、children、闭包和模块导入共同组成这套方式。
全部实际参数先完成求值，再按参数顺序求省略参数的缺省值。缺省中创建的数组和表在每次调用时独立创建。缺省可引用前序参数，以表达式或块尾值作为结果。显式 return 属于函数正文，不能写在该函数的参数缺省内，条件或短路分支也遵守这一规则；缺省内定义的嵌套函数按自己的正文返回。break 和 continue 只能指向缺省内部建立的循环，throw 则传播参数准备失败。
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

### 字体排版

`letter_spacing` 和 `word_spacing` 分别调整字簇间距和空格宽度，默认 0，单位为逻辑舞台像素。正值拉开、负值收紧；词距作用于普通空格和不换行空格，与字距叠加。字距按整形后的字簇增加 advance，包含末字簇，组合字符和连字保留字体整形。text、控件标签和 text_input 共用这两个属性，支持直接赋值、bind/derive；实际变化时更新换行、光标和命中位置。富文本和 ruby 注音继承间距，text_fit: "shrink" 将间距与字号一起缩放。示例：[author_text_spacing.aside](../../examples/author_text_spacing/src/main.aside)。

```aside
stage.replace(column(gap: 12) {
    text("CHAPTER ONE", font_size: 32, letter_spacing: 2);
    text("made with love", font_size: 28, word_spacing: -2);
});
```

需要用 Rust 扩展脚本接口的开发者可参阅 [Aside host SDK](../aside-host.md)。注册的宿主函数使用与普通 Aside 函数相同的命名参数、默认参数和错误位置规则。

Rust 扩展也可以用 `#[aside(rest)]` 声明可变位置参数。作者按普通参数或 `...items` 调用；每次调用有独立的外层数组，元素保留通常的共享身份。默认参数与绑定方法使用相同规则。这项能力独立于 Reel，本身不代表动作组合已经实现。

当前基础 text 使用真实文字布局；源码内翻译与响应式语言切换沿共同本地化服务运行。句内富文本与视频节点已实现；Aside 自定义绘制接口仍属规划。

作者音频已接入后台资源读取、真实混音器和剧情等待，见[作者音频](#作者音频)。原生设备验证与离线混音验证分别记录于[音频验证范围](../../crates/reel-media/VERIFICATION.md)。视频与同一混音器的内容时钟已接入，见上文视频节点。

原文和译文写在同一个 `tr` 调用中。`reel check` 检查语言标签和插值，`dev` 直接运行源码，`build` 与 `export` 自动生成语言包。单语言作品直接写普通字符串。

```aside
localization.set_locale("zh-CN");
let data = model({name: "Mio"});
stage.replace(column(gap: 12) {
    text(derive(|| tr(f"你好，{data.name}。", {en: f"Hello, {data.name}."})));
    button("English", on_click: |event| localization.set_locale("en"));
});
```

插值沿普通 Aside 表达式书写。原文中的表达式按顺序各求值一次，译文引用这些值，可以调整顺序、重复或省略；共享表达式复杂时，先给它取一个局部变量名。语言标签使用 `en`、`ja` 或带引号的 `"zh-CN"` 等写法。原文、译文和上下文一起移动，内部消息身份由编译器生成。

`localization.set_locale(locale)` 等待所选语言及其基础语言准备完成，再一起更新语言和界面。查找顺序为精确语言、基础语言、原文；加载失败可用 try/catch 处理，原语言保持可用。`localization.locale()` 返回可观察的当前语言。

界面通过 `derive(|| tr(...))` 随语言更新。普通调用、已提交的对白、选项和历史保留求值时的文字；分支 key 与显示文案分别定义。复数等文案选择可以用普通条件分支，每个分支就近写出它的译文。需要 Fluent 数值选择模板时，使用英文源码说明中的 `params` 接口。

同时显示多种语言时，先用 `localization.load(locale)` 预热，再用单次调用的 `locale` 指定语言：

```aside
localization.load("en");
column(gap: 6) {
    text(tr("你好。", {en: "Hello."}, locale: "zh-CN"), font_size: 28);
    text(tr("你好。", {en: "Hello."}, locale: "en"), font_size: 18);
}
```

发布包按需读取当前语言及其基础语言，显式预热的语言继续保留。开发运行共享编译得到的译文文本，按需解析目录。完整写法见[翻译示例](../../examples/localization/src/main.aside)和[英文接口说明](../../crates/reel-engine/src/aside/localization.rs)。

视频与音频播放返回可等待活动，所有权、控制及存档行为见各自章节。作者滤镜与同步组接口仍属规划。
规划中的 `custom_node` 将开放 Aside 的 state、measure、record、route 和 event 回调：布局与绘制回调只读，路由先决定消费、捕获与焦点，再执行可暂停的事件任务。当前作者接口尚未注册；宿主扩展使用 [Aside host SDK](../aside-host.md)。
透视舞台、Mesh、Material 与可编程 Effect 接口仍属规划；已实现的 `camera` 用于二维取景。Live2D、系统朗读、移动/Web 打包及外部服务适配需要分别实现并提供能力说明。
网络/购买等外部写入使用独立操作记录与幂等策略；未知结果先查询确认，回放默认只读。它们与普通可回滚游戏变量的寿命区分明确。普通作者组件与脚本库始终可以扩展自己的画面、布局、演出和业务逻辑。

### 错误定位与恢复
当前播放器会在源码文件、行列和错误种类后显示宿主接口的具体拒绝原因。在 catch 中可用 `f"{error}"` 显示 CoreError：优先使用字符串详情，否则使用固定错误说明；普通抛出值保持原有语义。

用 try/catch 处理资源加载、媒体播放或自己抛出的错误。catch 取得原值：抛字符串就得到字符串，抛对象就得到原对象。需要原样继续报告时写 throw;；写 throw error; 表示在当前位置再次抛出，诊断同时保留前一次错误作为原因。
`use std::core;` 后，可在 catch 中用 `core.error_info()` 读取当前故障、源码位置、调用栈和原因；调用栈与原因支持 for 遍历。完整字段与可执行示例见 [英文源码文档](../../crates/aside-std/src/core.rs)。业务恢复沿用 catch 收到的原值与 `throw;`。

### 作者音频

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

`audio.queue(activity, source, start: 0, end: null, looping: false, loop_start: 0, loop_end: null)` 将音乐或旁白片段追加到同一活动，返回 null。活动保留 owner、分组、音量、声像和控制。已准备的片段在混音边界接续；后继准备迟到时保持内容时钟并输出静音，准备好后继续。新音频可提前在后台准备，创建它的任务开始显式 await、普通调用实际等待或返回时，将同一同步执行段追加的片段一起准备后起播；await 已完成的对象也会提交；内部工作切片不触发起播，未捕获错误或取消会撤销尚未提交的声音。当前片段循环时，后继让它在本轮循环结尾退出；没有后继则继续循环。position 与 seek 对应当前素材。停止或 owner 退休取消剩余队列；加载错误使原活动失败，已结束的活动不能追加。存档保留当前片段、位置和剩余队列。
可运行示例：[author_audio_queue.aside](../../examples/author_audio_queue/src/main.aside) 用内置音频演示 intro → loop → outro 与连续语音片段；创作时把路径换成自己的素材。

```aside
let mut music = audio.play("assets/intro.ogg", group: "music");
audio.queue(music, "assets/loop.ogg", looping: true);
narrator("Continue to leave the loop after this pass.");
audio.queue(music, "assets/outro.ogg");
await music;
```

为该例提供自己的三个音频文件。队列容量与 PCM 驻留沿用宿主已有音频预算；容量不足报告错误，脚本无需配置新的限额。

图片、字体和音频共用宿主的资源读取容量。并发读取暂时占满容量时，后续读取等待已有任务释放输入；单个文件超过宿主配置的输入容量仍会报错，诊断包含路径和字节数。

音频解码仅在缓冲扩容和最终共享存储转换时计入新旧存储重叠；普通包解码复用已有 PCM 容量。内存预算不足时会报告解码阶段所需字节。

`audio.play("assets/chime.wav", start: 0.1, end: 0.2)` 只播放到素材的 0.2 秒之前，由音频混音器在终点完成；省略 end 使用素材结尾。play 与 queue 都接受 end，显式终点必须大于起点且位于素材内。start/position/seek 都使用素材的绝对秒数；起点不限制之后向前定位，非循环 seek 超过 end 时停在 end。起点和终点若量化到同一采样帧，片段会无声完成；存档保留每个片段的终点。

`start` 指定从第几秒起播，`audio.position(sound)` 读取当前素材的内容位置秒数。把它放进 `derive`，位置更新就会驱动画面；加载中保持请求起点，暂停时保持，结束后保留最后位置。`looping: true` 可配合 `loop_start` 和 `loop_end` 指定循环区间，默认从 0 循环至 end（省略时为音轨结尾）。start 在 loop_start 之前会先播放前奏，达到 loop_end 后回到 loop_start；start 超过 loop_end 会折回循环区间。循环终点不包含在区间内，循环边界必须位于 end 以内并至少包含一帧。未开启 looping 时使用默认循环边界。

`audio.seek(sound, seconds)` 定位同一个声音，返回 null，不需要 await。时间单位为秒且必须有限非负；暂停时定位仍保持暂停，正在播放则继续，增益与循环设置保持。加载中修改起播位置；非循环超出结尾时停在结尾并自然结束，暂停的声音等恢复后结束；循环的定位规则与 start 一致。连续定位采用最新目标，`audio.position` 和紧接的存档立即记录它，混音器应用后再跟随实际进度。已经结束的活动不会重新播放；`await sound` 始终等待原来的播放终态。

切换同一进度的音乐可以这样组合，淡入从新音轨完成加载并开始播放时计时：
```aside
let next = audio.play("assets/music-next.ogg", group: "music", looping: true,
    start: audio.position(music), loop_start: 4, loop_end: 64, fade_in: 2);
audio.stop(music, fade: 2);
music = next;
```
存档对所有音频分组记录当前条目、实际位置、循环区间与待播队列，读档后继续播放；已经结束的活动保持终态，不会重播。从头重听正在播放或暂停的语音时，seek 到 0；已经结束的语音重新 play。

`audio.at(sound, seconds)` 返回一个位置等待对象，`await` 在宿主发布的位置达到或超过目标时得到 true；声音提前自然结束或被 stop 时得到 false，取消和播放失败沿原错误传播。目标使用有限非负的素材秒数；调用时已达到目标，也按普通 await 继续。seek 立即发布新位置，循环和队列换段则沿当前素材位置判断；尚未达标时，位置回退后继续等待后续发布值。完成后，同一对象重复 await 保留原结果。取消等待者只解除自己的订阅；其余等待者和持有者继续共享票据，原声音继续播放。存档保留尚未完成的目标和播放活动身份。

[音乐驱动画面示例](../../examples/author_media_position/src/main.aside) 将同一内容位置用于方块移动和剧情等待：先 play/pause，构建绑定画面，等待首帧接受后 resume。加载所用时间自然保持在起点。示例用仓库内 0.25 秒的 chime.wav，在 0.125 秒改变文字；换成自己的音乐与时间点即可用于字幕、歌词或片尾。按第一节准备字体和资源后运行 `reel dev --assets ../../target/reel-artifacts/author-assets`（在 `examples/author_media_position/` 中）。

分组音量用 `audio.group_volume("music", value)` 设置，可选 music/voice/effects/ui；master 控制总音量。每声、组、总增益相乘，设置也影响之后开始的声音。`audio.pan(sound, value, duration: seconds)` 将声像从 -1（左）移动到 1（右），0 居中；`audio.volume` 和 `audio.group_volume` 同样接受 duration，省略时立即生效。`fade_in` 从实际播放开始计时；`audio.stop(sound, fade: seconds)` 淡出结束后才让 await 返回 Stopped，加载中或已暂停的声音立即停止。暂停保留播放位置，音量/声像包络继续推进；输出挂起冻结全部包络。淡出停止期间再暂停会立即停止。音量过渡在分贝域进行，静音端点为 -60 dB；新命令从当前值接续，加载中的控制更新首次播放值。

省略 owner 时，声音优先属于最内层的活动 scope；在作用域外继承当前任务的资源 owner，事件回调使用事件节点，主剧情使用会话。显式 owner 接受舞台节点或作用域对象，例如 `owner: node` 或 `owner: s`；该所有者结束时，加载与播放一起取消。取消等待者只解除本次等待，共享声音继续播放。播放、所有者取消与窗口退出复用同一资源回收路径。

Windows 播放器向系统提交双声道音频，默认设备配置为多声道时同样可以使用。

支持 WAV/PCM、Ogg/Vorbis、FLAC、MP3。音频自动准备：普通audio.play与audio.queue保持相同写法，宿主自动预解码短声音，以有界PCM窗读取长音轨；默认以双声道f32 PCM的8MiB作为静态阈值，作者无需按素材长度选择播放器。宿主可以配置阈值，音频分组仍只控制混音。补给不足时内容位置暂停，声音恢复后继续；保存恢复重新准备同一资源和位置。未知长度在EOF确定实际末尾，整曲循环使用该末尾，显式end/loop_end超过实际末尾时活动失败。指定起播位置、定区间循环、播放中定位与存档恢复沿同一活动合同。Completed 表示混音器消费完成，不能用于精确声卡同步。实体设备的验证范围见[音频验证](../../crates/reel-media/VERIFICATION.md)。

完整例子：[author_audio.aside](../../examples/author_audio/src/main.aside)，使用第一节准备的字体与素材。示例 chime.wav 是仓库自制的短正弦提示音。

可提供自行断言并结束的检查项目，入口仍为 `src/main.aside` 中的 `main`，通过下面的命令分别运行源码和 ASB。宿主使用项目资源根、独立临时存档、真实 CPU 布局与接受帧，并默认启用真实音频解码和混音；48 kHz 双声道 PCM 按单调经过时间离线消费，不发声。源码和 ASB 每轮默认最多 90 秒；长场景可用 `REEL_SCENARIO_TIMEOUT_SECS` 设置秒数上限，必须是大于 0 的 u64 整数，非法值直接报错。此变量只影响外部工程检查，内置测试固定 90 秒。宿主不自动点击、选择或快进。音频等待、位置、队列和恢复可在此检查，实机音画同步仍需在设备上验收。

```powershell
$env:REEL_SCENARIO = "path/to/project"
$env:REEL_SCENARIO_TIMEOUT_SECS = "600"
cargo test -p reel-cli --test project_scenarios external_project_scenario -- --ignored --nocapture
```

```powershell
Push-Location examples/author_audio
reel check
reel dev --assets ../../target/reel-artifacts/author-assets
Pop-Location
```

### 规划接口

- 具名 `state`、组件 `node.state`、页签与滑条外观槽位：[状态与样式设计](../engine-architecture.md#六-作用域-样式与状态)。
- 章节注册、重启与按章跳过：[剧情交互设计](../engine-architecture.md#十-热重载)。
- 自定义绘制、透视场景与外部适配：[扩展范围](../engine-architecture.md#十一-完整制作能力与扩展)。

这些名称属于设计目标；当前作品使用普通函数、model 数据及已实现的控件组合。

## 十五 继续查阅

语言规则与嵌入开发可查阅[文档目录](../README.md)、[语言规范](../aside-language.md)及[宿主 SDK](../aside-host.md)。
