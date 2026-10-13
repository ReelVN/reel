# Reel 游戏开发指南
设计版本 1.1 · 开发基线，接口实现状态见正文。

[English](en.md) · [文档目录 / Documentation index](../README.md)

## 阅读这份文档
这份指南从作者想要的画面出发，说明如何用 Aside 写剧情、组合 UI、安排演出和扩展效果。已实现部分可以直接使用；其余章节描述目标设计，并标明尚未接通的能力。中文与英文共用相同示例和接口名称。《Reel 引擎架构设计》说明运行时，《Aside VM 与语言设计》定义语言规则。
作者使用三个基本概念：函数组织可复用内容；节点树表达画面；活动表达随时间进行的变化。当前普通函数、基础节点组合与顺序剧情已经接通，活动及高级作者接口仍按各节状态区分。

当前可运行：text、image（PNG/JPEG）、row、column、stack、stage.replace，尾随 children 块与 if/for 组合，原节点属性修改，以及 narrator/choose 的等待与继续。check/build 与播放器使用同一套已注册 Reel 接口；dev 运行源码，run 直接运行 .asb。完整可运行画面见[作者舞台示例](../../examples/author_stage.aside)，启动命令见第一节。

动画、作者事件回调、bind/derive、媒体与存读档尚未接入作者源码；game.toml、多源码工程与资源包也仍待实现。后文包含这些能力的示例属于设计目标，不是当前可运行示例。更完整的能力边界见[当前实现状态](../README.md#当前状态)。

历史验证范围：R01/R02 的 Rust 渲染器与诊断窗口曾在软件 Vulkan、Xvfb/X11 上通过独立检查；这些记录只证明对应提交与环境。当前源码和 ASB 已接入真实窗口入口，旧诊断程序的范围不再代表作者入口的状态。物理 GPU、设备热插拔及平台生命周期未由这些检查证明。[渲染验证范围](../../crates/reel-render/R02-REPORT.md)。

## 一 最小项目与共同写法
当前可从一个 main.aside 和一个字体文件开始，使用 dev 运行源码，或 build 后用 run 播放 .asb。下面的 game.toml、game:: 模块拆分与标题页展示完整工程的目标组织方式；目前请以本节可运行命令和作者舞台示例为准。
完整工程设计中的 game.toml 使用 [game] 表的 entry 与 title，以及 [display] 表的 width、height、scale。entry 写 "main::main"；width=1920、height=1080、scale="contain" 是设计默认值。contain 等比例缩放并留出边缘，输入坐标采用同一换算。game:: 将从配置所在目录解析源码，外部模块使用 [modules] 别名。这些配置与多源码解析尚未实现；当前窗口初始大小为 960×540，不强制固定作者画布。
Reel 自动提供当前已注册的基础节点与剧情服务：text、image、row、column、stack、stage、narrator 和 choose，每个源码文件都可直接使用。作者的模块声明、局部绑定和显式导入优先于这些默认名称。若定义了自己的 text 函数，可用 `use reel::prelude::text as reel_text;` 为 Reel 构造函数另取名称。显式 prelude 导入仍是有效的普通导入。这套默认环境由 Reel 提供，独立 Aside 不包含 Reel 名称。动作和完整业务服务仍按后文设计。作者用自己的函数构造页面和外观。所有文档统一使用 fn、let、let mut、|参数| 闭包和 use 导入。除完整模块与函数示例外，独立代码片段放在正在运行的函数内部，沿用前文变量。普通语句写分号，函数末尾表达式可直接返回；对象字面量写 {字段: 值}。以下 game:: 导入与标题按钮是尚待实现的完整工程示例。
```aside
use game::ui::title::title;

fn main() {
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


### 当前可用的检查、构建与运行
从仓库根目录运行下列 Windows 命令。示例字体为微软雅黑；也可把 --font 改为自己的字体文件。运行 dev 或 run 后关闭窗口，再继续下一条命令。

```powershell
cargo build --locked -p reel-cli --features window
./target/debug/reel.exe check examples/author_stage.aside
./target/debug/reel.exe dev examples/author_stage.aside --font C:/Windows/Fonts/msyh.ttc
./target/debug/reel.exe build examples/author_stage.aside --output target/reel-artifacts/author-stage.asb --entry main::main --overwrite
./target/debug/reel.exe run target/reel-artifacts/author-stage.asb --font C:/Windows/Fonts/msyh.ttc
```

check 编译并报告源码错误，不运行剧情；加 `--format json` 可取得带 UTF-8 范围与行列位置的诊断。build 生成单个 .asb，包含字节码、模块初始化与选定入口。两者提供与播放器相同的默认 Reel 名称、已注册 native 与宿主默认参数。

dev 与 build 默认选择 main::main，可用 `--entry main::name` 选择其他函数。缺少入口或入口无效会在构建时报告；入口可以私有，但不能要求参数或捕获外层环境。run 直接加载包中的初始化函数和入口，不重新编译源码。源码路径与位置保存在包内，运行错误仍指向原来的 .aside 文件。已有输出只在指定 --overwrite 时替换。

启用 window 后，源码与构建产物都使用真实窗口、文字布局和呈现输入路径。同步构造组件在同一运行时间片内连续推进，到等待或时间片结束才更新画面，不会每创建一个节点就单独显示一帧。对白或选择等待时会先完成当前 GC，再休眠等待输入；自然结束后窗口保留最后画面，关闭窗口或按 Escape 退出并完成清理。

当前每次处理一个作者源码模块。包内宿主默认参数模块不代表已支持多源码工程；game.toml、.rpk 资源包与 release.json 仍是后续能力。

源码、完整源码定位文本和文字常量统一使用实际包预算。长文本和拼接按执行片推进，分片不会截断文字，也不规定台词长度。

左结合运算和属性、调用链按书写顺序编译，不按链条长度消耗嵌套额度。括号、代码块和子表达式的真正嵌套仍使用宿主配置的编译资源预算。

### 已接通的范围、容器与插值
在函数里可以用范围循环生成内容，再用插值展示结果。范围不会先创建一个大数组；`..` 不含终点，`..=` 包含终点。数组支持 `append`、`pop` 和 `len`，表支持 `has_own` 和 `len`。插值当前支持字符串、数字、布尔值和 null。
```aside
let values = [];
for value in 1..=3 { values.append(value); }
let latest = values.pop();
text(f"剩余 {values.len()} 项，最后一项 {latest}");
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

已注册的函数也可以保存和传递，例如 `let say = narrator; say("你好");`，等待行为与直接调用一致；函数缺省参数也可以是 `text` 等构造函数。可以用 `(int,int)` 约束坐标，用 `(int,int)|null` 表达可省略的坐标；`is`、参数、返回值和变量赋值都会检查相应类型，失败赋值保留旧值。完整可运行示例见 [function_values.aside](../../examples/function_values.aside)，使用前面的 `dev` 或 `build` 命令替换输入路径即可。

带尾随子节点块的组件调用作为独立语句时可省略结尾分号；块后换行结束该语句，下一行括号表达式不会被接到前一个调用上。普通调用和属性赋值仍使用分号。显式分号不改变子节点收集。

已实现：text、image、row、column、stack 与 stage.replace 使用同一节点模型。普通函数可以返回节点；尾随 children 块、if 与 array/tuple 上的 for 可以组合节点。value、x/y、width/height、color/background、opacity、visible 和 clip 等属性直接修改原节点，挂载后的身份保持不变。替换舞台会退休旧树，仍持有的旧节点不能继续读写，也不会因此保活整棵旧树。完整代码见[作者舞台示例](../../examples/author_stage.aside)。

stack 按书写顺序从后往前叠放子节点，row 横排，column 竖排。未指定宽高时按内容布局；x/y 在布局位置上叠加，逻辑坐标原点在左上，x 向右、y 向下。默认字体大小为 24、文字白色、背景透明，gap/padding 为 0；clip 限制容器的可见区域。专用 scroll 容器仍属于后续作者能力。

用 `image(source, fit: "contain", ...)` 将 PNG/JPEG 文件放到舞台。省略 width/height 时，图片的像素尺寸作为逻辑尺寸；只指定一边时保持宽高比。指定两边后，contain 完整显示并留空，cover 填满并裁剪，stretch 拉伸填满。图片也支持共同的位置、可见性、透明度和 padding 属性。可以直接修改原节点的 source 和 fit。

```aside
fn main() {
    let picture = image("assets/panels.png", width: 320, height: 180);
    stage.replace(column(gap: 16, padding: 24) {
        picture;
        image("assets/panels.png", width: 160, height: 160, fit: "cover");
    });
    narrator("The same resource can appear in different frames.");
    picture.source = "assets/sun.jpg";
    picture.fit = "cover";
    narrator("The original node now displays the new image.");
}
```

完整的 [author_images.aside](../../examples/author_images.aside) 随附所需图片，可用前面的 dev 命令运行。dev 的资源路径相对于源码文件目录，run 相对于 ASB 文件目录；`--assets <directory>` 可另选资源根目录。例如构建到 target/reel-artifacts 后，运行时加 `--assets examples`。构建 ASB 不会复制图片文件。

图片在后台加载，同一路径的节点共享像素。新场景所需图片全部就绪前保留上一完整画面，并暂停剧情输入；换源也保留旧图片直至新资源就绪。文件缺失或损坏会报告文件路径及 image 构造位置，并结束本次窗口运行。属性类型错误和非法 fit 可在赋值处捕获。图片挂载时才读取文件，构造未挂载组件不会产生文件 IO；会话内文件热重载尚未实现。

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

保留组件内任一节点引用，也会保留其未挂载父级组件；GC 不会让仍在使用的节点突然失效。属性类型、颜色和数值范围在赋值时检查，错误可在同一段 try/catch 捕获，失败保留旧值。--font 指定的字体用于它所覆盖的全部文字，包括中文，而非只用于拉丁文字。

## 三 剧情是普通的顺序代码

当前可用 narrator 和 choose 顺序等待并继续；以下 character、可绑定对白模型、自定义对白视图和语音接口仍是设计目标。

对白行数、选项数量、单次交互文字和会话历史按内容增长。历史保留整个会话的记录；它不会因达到展示缓存容量而中断剧情或丢弃旧记录。实际内存不足会报告错误。

角色在模块中定义一次。每句对白调用发言者对象，旁白调用 narrator。独立字符串仍是普通字符串。角色调用更新对话数据和历史，并等待当前句完成，普通函数调用沿同一执行顺序继续。
```aside
use game::ui::room::room_view;

let mio = character("Mio");

pub fn opening() {
    let view = room_view(game.dialogue);
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
    column(x: 160, y: 780, width: 1600, gap: 12) {
        text(value: bind(model.speaker_name),
             visible: derive(|| model.speaker_name != ""));
        text(value: bind(model.text), reveal: bind(model.reveal));
    }
}
```
game.dialogue.lines 是统一的可观察对白行列表。speaker_name/text/reveal 是当前活动行组第一行的只读快捷投影；ADV 常用这些字段，NVL 和气泡使用完整行列表。model.text 保存完整文本，reveal 控制显现范围，因此逐字显示保持稳定排版。对话视图更换、隐藏和重新显示都保留会话历史。角色调用可接收 voice 等资源参数；语音播放与当前句关联。多个角色同时发言也使用同一个推进交互：
```aside
let eri = character("Eri");
say([mio.line("你听见了吗？"), eri.line("在窗外。")],
    mode: "bubbles");
```
气泡与 NVL 视图通过 list 绑定 game.dialogue.lines，并用 current_line_ids 区分当前组与已累积行。当前组的快捷字段始终指当前第一行。选择也提供数据模型，作者通过按钮调用 game.choice.select(option_id)，choose 返回相同 key。
需要路线图与章节重启时，用普通 API 给函数登记稳定 ID 与显示标题。函数仍按同一套调用与等待规则执行。
```aside
fn opening() {
    // 剧情正文。
}
story.register("chapter01.opening", opening, title: "窗上的光");
```
普通 fn 即可承载剧情。story.register 的章节入口使用模块级函数且无必需参数。game.start 使用函数自己的稳定模块符号，普通新游戏入口无须额外登记；按章节 ID 访问的路线/重启操作才使用注册表。登记发生在模块候选中，验证与模块发布一起提交。game.start(opening) 接收同样的符号入口，从 UI 事件提交新游戏请求；引擎先在最新已发布模块集合验证入口，再建立新会话调用它，初始化全局剧情变量、自动播放和快进状态。普通 helper 调用保留原模块绑定。

## 四 时间顺序与同时发生的变化

本节动作组合、动画活动及其作者控制接口尚未实现。

语言底层现已支持等待宿主注册的可等待对象，普通 fn 可以直接写 `await request`；它会暂停当前调用链，让宿主继续处理其他工作。已完成对象也在下一调度步骤返回，错误在 await 位置报告并保留生产者原因。该协议供引擎服务接入使用，尚不提供 async fn、Future 构造或本节 run/tween 活动。可运行的宿主接入检查见 [host_awaitables.rs](../../crates/aside-compiler/tests/host_awaitables.rs)，命令为 `cargo test -p aside-compiler --test host_awaitables`。

tween 只描述目标属性、终值和时长。sequence 顺序组合动作，parallel 同时组合动作，repeat 重复动作。它们都是普通库函数，产生可复用的动作值。run 才启动活动，并返回可等待、可控制的活动；await activity 等待活动结束，输入、声音和其他活动继续进行。run(action) 默认归当前作用域所有，作用域外归当前任务的 owner；需要跨越此范围时显式指定 owner。
动作的起始属性在实际激活时读取。sequence 中后一段在前一段结束后读取起值。动作描述可以复用，每次 run 都产生独立进度。run 原子保留整个动作树会写入的属性，保留持续到根活动结束，包括未来的 sequence 分段、repeat 和暂停期间。sequence 可按顺序写同一属性；parallel 分支的写集合保持互斥。冲突时在调用位置报告并保持已有活动。需要接管时先停止旧活动。直接给已保留属性赋值会先停止其根活动，再采用赋值；未来分段保留的属性也遵守这个规则。终止的活动释放所有保留。
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

本节图片、动画、按钮回调和游戏入口切换尚未接入作者源码。

启动流程仍写普通 fn。媒体 play 返回活动句柄；点击可以通过正常事件停止播放，播放自然结束和主动停止都进入下一行。
```aside
use game::ui::title::title;

fn main() {
    let logo = video("assets/logo.webm");
    stage.replace(logo);
    let playback = logo.play();
    logo.on_click(|event| playback.stop());
    await playback;
    stage.replace(title());
}
```
视频为可选能力。纯图片 Logo 用 image 与 run(tween(...)) 表达同样流程。媒体与动画共享活动控制协议。视频节点创建时只创建画面，play 才启动播放；替换节点结束所属播放与事件订阅。
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

本节作者控件、settings、bind 和 derive 接口属于设计目标。

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
bind 连接可观察属性，属性同时保留原来的读写权限；text 可以读取只读属性，slider 写回数值时要求可写属性。derive 计算只读外观。派生函数对读取的可观察属性建立依赖，也可读取不可变捕获值；普通数据的内部变化保持普通语言语义。可观察属性变化仅更新相关节点。标量写入可写绑定属性会写回绑定源并保留绑定；动画也保留和写入最终源属性，因此两个节点绑定同一源会发现真实写入冲突。源的显式改写停止相关根活动；被动投影更新属于读取。只读派生属性用 scope 覆盖做临时演出；重新绑定或 unbind 会先结束依赖旧绑定的活动，unbind 保留当前有效基础值。捕获存档的短暂期间，脚本派生与作者外观更新暂停，捕获完成后按最新状态恢复。普通 {...} 数据记录适合传参，持续变化的业务值用 model({...}) 创建可观察模型。循环依赖和派生计算中的写入、等待给出源码诊断。
作者可用同样方式封装统一字体、按钮图片、留白和动效，再作为普通模块导入。风格复用来自组件和参数，共用的属性对象也可按普通语言规则组织。

## 八 风格与共同运动

普通函数和节点属性修改已可用；本节样式展开、变换、动画与临时覆盖接口仍待实现。

常用风格就是普通数据，组件函数决定如何把它应用到自己的节点。先写默认值，再用变体与显式参数覆盖；字段缺失才采用默认值，false、0 和 null 都保留作者的明确含义。临时 scope 覆盖优先于节点底层属性。
```aside
let default_look = {font: "assets/ui.ttf", font_size: 32,
                    color: "#ffffff", background: "assets/button.png"};

fn menu_button(label, clicked, style = {}) {
    let look = {...default_look, ...style};
    button(on_click: clicked) {
        image(look.background);
        text(label, font: look.font, font_size: look.font_size,
             color: look.color);
    }
}

menu_button("开始", |event| game.start(opening),
            style: {color: "#ffd98a", font_size: 36});
```
节点属性可以直接修改；持续跟随业务或偏好变化的外观使用 bind/derive。普通风格记录用于构造，运行期共享配色可以用 model 保存，再把其属性绑定到文字与图片。作者自己的组件既可完全手绘，也可沿用熟悉的布局与控件行为。
UI 与立绘共同运动时，把它们放进同一个组，再对组执行动作。
```aside
let cast = group() {
    image("assets/mio.png");
    dialogue_view(game.dialogue);
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
历史验证（R01 / Issue #25）：内部 Rust 帧记录、资源租约、分组、裁剪与透明度已有针对性用例。R01 当时尚无 GPU 窗口，记录型消费者不代表物理设备通过。本节 mask/blur 等作者效果仍是设计目标；当前基础舞台与真实窗口已接通，范围见第一、二节。[历史验证范围](../../crates/reel-ui/R01-REPORT.md)。

历史验证（R02）：wgpu 后端与 winit 诊断窗口包含图片、分组透明度、裁剪和 Parley 文字；这些历史 Rust 诊断能力不意味着高级效果接口已实现；当前 PNG/JPEG 作者图片使用共同舞台和播放器资源加载器。当前作者源码和 ASB 由 dev/run 进入真实剧情窗口。[窗口说明](../../crates/reel-player/README.md) · [设备验证范围](../../crates/reel-render/R02-REPORT.md)。

mask、blur 等效果值属于同一合成体系。效果组先合成自己的子节点，再应用效果，组透明度作用一次。裁剪用于几何范围；图像遮罩用于可见程度。mask.progress 从 0 到 1 表示由全隐藏到全显示，softness 控制边缘宽度；黑白遮罩的灰度决定显现先后。
```aside
let wipe = mask("assets/ink-mask.png", progress: 0, softness: 0.06);
let reveal = group(effects: [wipe]) {
    image("assets/window-cg.png");
};
view.picture.add(reveal);
await run(tween(wipe, progress: 1, duration: 1.2), owner: reveal);
```
效果参数与节点属性采用同一动画规则。更复杂画面可以嵌套 group：背景单独模糊，人物保持清晰，图片按钮仍在最上方；再对整组使用遮罩或镜头变换。所有子节点的前后顺序由树结构决定。
自定义效果通过版本化 Effect 描述输入图像、参数、输出范围和后端实现。高级作者可以开发宿主扩展，常用效果随后以普通 Aside 函数组合复用。shader 支持属于后端能力，项目检查列出缺少的能力。可移植效果应同时提供简化实现，游戏通过能力查询选择适合的平台版本。
特效改变的像素与逻辑输入区域分别声明。普通仿射变换与几何裁剪自动映射命中；弯曲、扭曲和任意着色器的交互区域由作者提供逻辑形状或专用映射。为每次点击从 GPU 读回画面会增加延迟，因此基础接口使用可直接计算的区域。

## 十 选择与会变化的内容

choose 已支持选项数组、真实窗口选择和返回原 key。以下自定义 choice_view、game.choice 绑定与 list 接口仍是设计目标；当前使用播放器的默认选择视图。

choose(options) 接收具有稳定 key 和 label 的选项数组，更新 game.choice 并等待 select(option_id)。当前句与选择共用主剧情交互权限。作者把选择视图放在自己的画面里，位置和背景均可自定。
```aside
fn choice_view(model) {
    list(items: bind(model.options),
         key: |option| option.id,
         build: |option| {
             button(on_click: |event| model.select(option.id)) {
                 text(value: bind(option.label));
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
choice_view 挂载在包含对话 UI 的画面中，by_window 和 by_door 是作者的剧情函数。choose 把传入的普通记录复制成引擎拥有的只读可观察行，id、key 和 label 都可读取并绑定，输入记录保持原值。id 是本次 InteractionId 与 key 形成的不透明稳定标识，列表用 option.id 保持这一身份；下一次选择使用新的 id。game.choice.options 是这些行构成的只读可观察列表，默认为空；等待选择时填入选项，选择完成后清空。select 接收包含交互身份的 option.id，返回剧情的仍是业务 key；过期或重复选择按这个身份过滤。
普通 children 中的 for 适合构造当下画面。list 用于持续变化的列表：items 连接可观察集合，key 确定元素身份，build 为新 key 创建节点；同 key 的可观察 item 保持身份并更新字段，移除 key 才销毁对应子树。默认竖排，虚拟显示由 viewport 范围和可复用测量结果控制。重复 key 在对应列表位置报错。普通数组作为 items 时按固定内容构造。

## 十一 存档 历史 路线与跳过

本节作者存读档、历史视图、路线跳转与跳过接口尚未实现。内部会话记录不代表这些作者服务已经可用。

saves 是独立服务。save(slot_id) 返回 SaveRequest，quick_save() 使用快速槽；load(slot_id) 和 quick_load() 返回 LoadRequest。请求有只读 status 与 error，可用 derive 或 bind 显示进度；请求本身可等待，保存按钮通常只提交请求并观察模型。成功意味着槽位已提交到存储。调用者也可直接等待自己发出的保存请求：捕获在调用后的可恢复边界进行，这份存档里的对应请求恢复为已成功结果，等待者继续执行后面的语句。保存调用始终只执行一次。其他在途外部写请求跨进程恢复时，以可处理的 OperationInterrupted 错误结算；当前存档对应的请求使用上述成功规则。载入请求成功会替换会话，旧会话拥有的回调任务结束，进程级服务继续发布结果。
saves.slots(page: 1, page_size: 12) 返回 SlotPage，可观察 items 包含稳定 id、occupied、chapter、saved_at、thumbnail 和 status。saves.delete(slot_id) 返回 DeleteRequest，成功后对应摘要变为空槽。每页页码从 1 开始，page_size 与页码决定固定普通槽位集合；快速槽独立。作者通过 grid、image、text、button 组成页面，翻页只更换显示的集合。
```aside
let slot_page = saves.slots(page: 1, page_size: 12);
let save_button = button("保存", on_click: |event| {
    saves.save(slot_page.items[0].id);
});
```
保存、读档和删除请求共用 Queued、Running、Succeeded、Failed 状态；保存额外公开 phase = Capturing 或 Writing。error 在失败时包含说明，成功时为 null。await request 返回请求结果；请求失败抛出对应错误。槽位摘要只在成功提交后替换，失败保留原摘要。
game.history 是只读可观察列表，每条是 HistoryGroup，含 group_id、interaction_id、lines 与恢复点信息；lines 保存当时的角色、内容身份、参数、显示文本和语音。单行快捷字段只投影该组第一行。用 scroll 包含 list 即可按需显示历史条目；读档或回滚时列表切换到恢复的会话状态。普通 UI 更换保持历史。
game.stories.entries 为章节索引，条目有 id、title、visited；game.stories.links 保存显式登记的 from/to 连接。模块发布从 story.register 收集入口，动态连接由 game.stories.link(from_id, to_id) 登记。作者用按钮代表章节、路径节点画连接，布局可手排或由自己的算法计算。
game.restart(story_id) 使用当前会话固定的主剧情模块集合，从命名章节入口重新执行剧情任务，保留当前剧情变量，结束旧主剧情任务及其交互；目标函数负责构造自己的画面。game.start(entry) 创建全新会话，适合新游戏。saves.load 恢复保存的位置、变量和画面。作者据此给路线按钮选择含义清楚的操作。
按住快进、自动播放、分段跳过使用统一的剧情推进服务。game.skip_to(story_id) 以正常控制流加速执行，抵达目标章节入口、选项或未读边界时停止；目标不可达时按正常剧情结束，并报告目标未抵达。game.set_auto(enabled) 控制自动播放。按住快进由输入层的 skip 动作保持状态驱动，作者可以将键盘或手柄绑定到该动作。
快进缩短可跳过的已读等待和有限演出，变量计算仍按顺序执行；松开、失焦、打开菜单、新游戏和读档清除按住状态。tween 完成到终值，有限等待结束，媒体按播放策略停止或继续；循环活动仍由 owner 管理。作者显示的控制提示跟随同一服务状态。
这些服务提供数据与操作，页面的外观由作者组合。本版列出的签名属于待实现规范；对应功能达到契约验收后，指南为该节标注已实现版本与运行命令。

## 十二 游戏变量与组件状态

普通局部变量、对象和闭包已可用；本节 model、state、persistent 及存档恢复接口仍是设计目标。

语言中的 let/let mut 决定绑定能否改写，model 决定属性是否可观察，状态的所属范围决定保存和释放。三者各司其职。
```aside
let route = state("route", {affection: 0, visited: set()});
let unlocks = persistent("unlocks", {cg: set()});

route.affection += 1;
route.visited.add("window");
unlocks.cg.add("window-cg");
```
state(id, defaults) 返回当前会话的具名可观察状态，同 ID 取得同一个对象。它随存档、读档与回滚恢复，新游戏重新从默认值建立。函数局部值和闭包捕获同样可以随完整调用栈保存，具名 state 主要用于共享游戏数据和热重载中的稳定定位。
persistent(id, defaults) 返回显式跨周目的可观察数据，独立写入全局存储。默认值只补缺失字段，重复获取保留已有进度。跨周目数据存放数据值和有持久数据适配的类型，引用会话节点、任务和可执行闭包会在赋值处报告寿命冲突。解锁推荐集合等幂等写法。persistent.flush() 可等待可靠提交，平时更新由服务合并写入。跨局赋值把数据复制进持久图，读取取得该图内部的可观察别名；任意嵌套修改均标脏。快照中这些别名记录稳定全局数据身份，读档重接当前进度，而不是写回旧内容。
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
settings 属于引擎的全局偏好服务；剧情存档恢复时读取当前偏好。业务 state 的修改采用常规控制流，scope 临时效果结束也保留其业务结果。

## 十三 作者怎样扩展自己的积木

普通节点函数、命名参数、缺省值、children 和闭包已可用。下方 arrive 示例依赖尚未实现的动画接口。

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

当前基础 text 使用真实文字布局；本节富文本类型、本地化、媒体和自定义绘制接口属于后续作者能力。

Rust 自定义绘制层的实色、文字和图像共用线性预乘颜色转换；描边和几何裁剪均支持虚线，包括曲线、闭合路径和虚线偏移。全零虚线模式显示为实线，较小的斜接限制使用斜角连接。以上属于现有 Rust 绘制接口能力，Aside 自定义绘制入口仍待接通。


实现状态（M01 / PR #14）：Rust 音频层已实现 Kira 桥、可选 CPAL 输出、有界回收和设备失败通知。Linux 离线真实 Kira、默认/关闭输出 feature 的契约检查及 Windows 交叉编译已独立通过；实体设备、硬件时钟、热插拔和原生启停/挂起仍未验收，CPAL 时钟为 Unavailable。以下作者 audio/video 接口、剧情等待与会话恢复仍是设计目标，不能由 Rust 探针通过推导为已接通。[音频验证范围](../../crates/reel-media/VERIFICATION.md)。

PlainText 表示普通文字，RichText 表示显式样式结构，LocalText 由 tr(id, fallback, params) 创建。翻译模板先解析结构，再把参数作为文字填入；参数中的标记字符保留为文字。当前对白组保持提交时的文字与语音，下组采用新语言，NVL 页在清页后切换；UI 和历史即时使用当前语言。reel loc 提取/更新目录并检查缺译和参数。
video 是普通节点，audio 提供音轨、队列、循环区间、滤镜与同步组。play 返回可等待活动，stop 停在当前状态，finish 用于有终点的活动。轨道与资源由稳定身份管理，设备重建和 seek 保留或重新绑定逻辑等待；错误有明确终态。
custom_node 可以直接注册 Aside 的 state、measure、record、route 和 event。measure/record 是受预算的只读布局/绘制回调；route 同步决定消费、捕获与焦点，然后 event 作为普通可暂停任务执行。这样小游戏节点能够阻止同次输入推进剧情，也能在业务回调中等待资源。新的底层图形/平台能力通过 native 模块注册。
Camera、Mesh、Material 和 Effect 提供透视舞台与可编程效果，状态和资源身份仍进入保存。不同后端用能力清单说明所支持的实现与后备。Live2D、系统朗读、移动/Web 打包和外部服务属于完整目标的适配能力，具体 API 文档随已验证实现提供同一风格的使用示例。
网络/购买等外部写入使用独立操作记录与幂等策略；未知结果先查询确认，回放默认只读。它们与普通可回滚游戏变量的寿命区分明确。普通作者组件与脚本库始终可以扩展自己的画面、布局、演出和业务逻辑。

### 错误定位与恢复（设计合同）
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

当前支持没有必填参数、没有捕获环境的普通模块级入口，包括私有函数。对白和选择按内容布局并支持滚动，不另设固定两行或两个选项的上限。源码窗口启动和 ASB 窗口启动已实现；自然结束保留最后画面，关闭窗口完成清理。上述无窗口示例专注对白与选择，完整基础节点组合请用第一节的作者舞台示例。角色接口、动画、事件绑定、媒体等待、完整模块/类、delegate/hooks、property-bag 和保存恢复仍未实现。
音频实现进展（PR #53，限定 Rust 范围已独立验收）：Rust 层新增有界播放活动账本、owner 取消、精确等待身份过滤和一次终态交付。设备故障不会吞掉已结算结果；接收队列满时，后续取消记录仍能释放。这里的内容消费完成与设备播放完毕、PCM 内存释放分别记录；尚未接通指南中的作者音频 API、剧情等待或保存恢复。seek/reload、单活动暂停与分组混音仍待实现。现有示例继续表示最终作者接口。[Rust 接口与边界](../../crates/reel-media/README.md#logical-audio-activities-pr-53)。

当前 Rust 音频层按实际 PCM 内存计账，不再额外规定单段音频的最大帧数或时长。宿主设定的队列和声音容量也不再被另一组固定数字压低；作者 audio 接口仍待接通。

音频退休进展（PR #56，限定 Rust 独立验收通过）：播放请求改用注册的 AudioOwner，关闭准入后分片取消，再等待该 owner 的 PCM/预算票据和活动描述符分别实际释放。停止播放或收到逻辑完成消息不等于资源已释放。保留完成凭据仍占有界登记槽，挂起输出可能延长退休。这里尚未连接作者音频语法、剧情等待或保存恢复，指南中的 audio 接口仍是最终设计目标。
