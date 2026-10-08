# Reel 游戏开发指南
设计版本 1.0 · 开发基线，接口实现状态见正文。

[English](en.md) · [文档目录 / Documentation index](../README.md)

## 阅读这份文档
这份指南从作者想要的画面出发，说明如何用 Aside 写剧情、组合 UI、安排演出和扩展效果。当前所有接口与示例均为设计规范，正在进入实现；每节随代码验收更新为已实现，并记录适用版本。中文与英文共用相同示例和接口名称。《Reel 引擎架构设计》说明运行时，《Aside VM 与语言设计》定义语言规则。
作者使用三个基本概念：函数组织可复用内容；节点树表达画面；活动表达随时间进行的变化。剧情、按钮事件和工具函数使用同一种 fn 声明。组件是返回节点或命名节点集合的普通函数。

## 一 最小项目与共同写法
项目起步只需 game.toml、main.aside 和 assets/。ui/、story/ 等目录按创作规模自行拆分。配置指定入口 main::main 与逻辑画布 1920×1080。reel dev 运行源码并显示定位到文件的错误；reel build 生成字节码和资源包；精简播放器运行构建产物。这些命令属于实现目标。
game.toml 使用 [game] 表的 entry 与 title，以及 [display] 表的 width、height、scale。entry 写 "main::main"；width=1920、height=1080、scale="contain" 作为默认值。contain 等比例缩放并保留边缘留空，输入坐标按同一缩放换算。game:: 模块从 game.toml 所在目录解析 .aside 文件。文件名与目录名必须可作模块标识符；外部源码模块通过 [modules] 的别名到相对目录映射导入，game 与 reel 是保留别名。
use reel::prelude::* 导入常用节点、布局、动作和服务的名字，作者的页面与外观仍由自己的函数构造。game:: 前缀从游戏源码根目录寻找模块。所有文档统一使用 fn、let、let mut、|参数| 闭包和 use 导入。除完整模块与函数示例外，独立代码片段放在正在运行的函数内部，沿用前文的 view、mio 等变量。普通语句写分号，函数末尾表达式可直接返回。对象字面量写 #{字段: 值}。
```aside
use reel::prelude::*;
use game::ui::title::title;

fn main() {
    stage.replace(title());
}
```
上例的 ui/title.aside 可以直接用作者自己的节点组成：
```aside
use reel::prelude::*;
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

## 二 让代码表达整幅画面
stack 将子节点按书写顺序从后往前叠放；row 横排，column 竖排。普通节点共享位置、尺寸、透明度、可见性、变换和裁剪。绝对位置使用逻辑画布坐标，原点在左上，x 向右，y 向下。布局容器负责排列其子节点；容器自己的 x、y 决定它在父级中的位置。内容超过容器时由 clip 或 scroll 容器决定可见区域。
下面是一间房间：背景最底，立绘其上，CG 再上，对话 UI 最上。作者函数把需要在剧情中操作的部分命名返回。
```aside
use reel::prelude::*;
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
    return #{root: root, picture: picture,
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

## 三 剧情是普通的顺序代码
角色在模块中定义一次。每句对白明确写发言者，旁白写 narrator.say。独立字符串仍是普通字符串。say 更新对话数据和历史，并等待当前句完成，普通函数调用沿同一执行顺序继续。
```aside
use reel::prelude::*;
use game::ui::room::room_view;

let mio = character("Mio");

pub fn opening() {
    let view = room_view(game.dialogue);
    stage.replace(view.root);
    mio.say("先别开灯。看玻璃上那条光。");
    narrator.say("雨水往下落，光却停在原处。");
    show_cg(view);
    mio.say("你也看见了吗？");
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
model.text 保存完整文本，reveal 控制显现范围，因此逐字显示保持稳定排版。对话视图更换、隐藏和重新显示都保留会话历史。say 可接收 voice 等资源参数；语音播放与当前句关联。选择也提供数据模型，作者通过按钮调用 game.choice.select(key)，choose 返回相同 key。
章节元数据在需要路线索引、明确重启点时添加。它为函数增加稳定名字，函数调用与等待行为保持一致。
```aside
#[story(id: "chapter01.opening", title: "窗上的光")]
fn opening() {
    // 剧情正文。
}
```
普通 fn 即可承载剧情。#[story] 的章节入口使用模块级函数且无必需参数。game.start(opening) 接收同样的符号入口，从 UI 事件提交新游戏请求；引擎先在最新已发布模块集合验证入口，再建立新会话调用它，初始化全局剧情变量、自动播放和快进状态。普通 helper 调用保留原模块绑定。

## 四 时间顺序与同时发生的变化
tween 只描述目标属性、终值和时长。sequence 顺序组合动作，parallel 同时组合动作，repeat 重复动作。它们都是普通库函数，产生可复用的动作值。run 才启动活动，并返回控制句柄；await activity.finished 等待活动结束，输入、声音和其他活动继续进行。
动作的起始属性在实际激活时读取。sequence 中后一段在前一段结束后读取起值。动作描述可以复用，每次 run 都产生独立进度。run 原子保留整个动作树会写入的属性，保留持续到根活动结束，包括未来的 sequence 分段、repeat 和暂停期间。sequence 可按顺序写同一属性；parallel 分支的写集合保持互斥。冲突时在调用位置报告并保持已有活动。需要接管时先停止旧活动。直接给已保留属性赋值会先停止其根活动，再采用赋值；未来分段保留的属性也遵守这个规则。终止的活动释放所有保留。
```aside
let entrance = parallel(
    tween(view.portrait, x: 320, duration: 0.5),
    tween(view.hud, opacity: 1, duration: 0.3)
);
let activity = run(entrance, owner: view.root);
await activity.finished;
```
一个活动归显式 owner 所有。启动与主剧情函数归会话所有，UI 回调归发出事件的节点所有。stage.replace 结束旧画面及其回调，调用它的会话剧情继续执行。动作目标只是经过验证的句柄；任一节点或效果目标销毁时，整个根活动结束为 Cancelled，释放所有属性保留。owner 从舞台销毁时，所属活动与回调任务一并结束。pause 暂停进度，resume 继续，stop 停在当前值并使等待返回 Stopped；自然结束返回 Completed。资源错误或活动取消在仍存活的等待任务中产生对应错误，供普通 try/catch 处理。若等待任务自身的 owner 已结束，该任务直接结束；宿主释放其资源，脚本清理代码也随任务结束。
parallel 等待所有有限子动作。任一子动作失败时结束同组剩余子动作，并报告错误；sequence 随失败停止后续动作。repeat(action, times: 3) 有限重复；省略 times 表示循环。持续的呼吸、雨丝等循环单独运行，由画面所有权管理，剧情只等待自己需要的有限活动。
```aside
let breathing = repeat(sequence(
    tween(view.portrait, opacity: 0.92, duration: 1.4),
    tween(view.portrait, opacity: 1.0, duration: 1.4)
));
run(breathing, owner: view.root);
mio.say("听，雨又大了。");
```
上例展示循环组合规则。实际立绘呼吸可以改用细微位移或缩放，脚本结构相同。作者把常用动作封装成普通 fn，输入任意节点后返回动作值，UI 与立绘共用这些动作。

## 五 临时全屏 CG 与还原画面
这个例子让 HUD 淡出、CG 淡入，等待玩家欣赏，然后恢复原来的房间与 UI。原房间、立绘和组件数据始终保留。show_cg 是普通辅助函数。
```aside
fn show_cg(view) {
    let before = #{visible: view.hud.visible,
                   enabled: view.hud.enabled,
                   opacity: view.hud.opacity,
                   cg_opacity: view.cg.opacity};
    let mut current = null;
    try {
        view.hud.enabled = false;
        current = run(parallel(
            tween(view.hud, opacity: 0, duration: 0.3),
            tween(view.cg, opacity: 1, duration: 0.6)
        ), owner: view.root);
        await current.finished;
        view.hud.visible = false;
        await game.advance();
        view.hud.visible = before.visible;
        current = run(parallel(
            tween(view.hud, opacity: before.opacity, duration: 0.3),
            tween(view.cg, opacity: before.cg_opacity, duration: 0.6)
        ), owner: view.root);
        await current.finished;
    } finally {
        if current != null { current.stop(); }
        if view.root.alive {
            view.hud.visible = before.visible;
            view.hud.enabled = before.enabled;
            view.hud.opacity = before.opacity;
            view.cg.opacity = before.cg_opacity;
        }
    }
}
```
该例在进入时记录原值，正常返回或可处理的异常离开时，finally 结束剩余活动并恢复原值。finally 只做同步清理；如果整个场景的 owner 已结束，宿主直接清理所属任务与节点。一个 HUD 与 CG 对同一时刻执行一个 show_cg，作者的模态入口负责这一调用范围。
visible=false 隐藏子树并退出命中和焦点；enabled=false 保留画面并禁用子树交互；opacity 只控制画面透明度。渐隐开始时禁用 HUD，让视觉过渡期间的按钮也保持明确行为。game.advance 等待新的、未被控件消费的推进输入，属于当前主剧情交互。
永久换景可以用 stage.replace(new_root)。局部永久替换使用容器的 replace(new_child)，旧子树销毁后其句柄进入已结束状态；临时叠加保留原节点，适合 CG、地图、照片与插入式操作面板。

## 六 Logo 标题与局部交互
启动流程仍写普通 fn。媒体 play 返回活动句柄；点击可以通过正常事件停止播放，播放自然结束和主动停止都进入下一行。
```aside
use reel::prelude::*;
use game::ui::title::title;

fn main() {
    let logo = video("assets/logo.webm");
    stage.replace(logo);
    let playback = logo.play();
    logo.on_click(|event| playback.stop());
    await playback.finished;
    stage.replace(title());
}
```
视频为可选能力。纯图片 Logo 用 image 与 run(tween(...)) 表达同样流程。媒体与动画共享活动控制协议。视频节点创建时只创建画面，play 才启动播放；替换节点结束所属播放与事件订阅。
剧情中临时加入按钮时，向需要的父节点添加普通 button，然后正常移除。作者决定它遮住哪些内容、是否阻止下面输入、何时消失。一次性按钮使用同一事件和所有权规则。
```aside
let hint = view.hud.add(button("查看照片",
    on_click: |event| photo_panel.open()
));
mio.say("桌上那张照片，你还记得吗？");
hint.remove();
```
photo_panel 是作者定义的组件控制对象，上例只演示与剧情交错的按钮寿命。事件回调沿所属节点调度。打开模态节点时，引擎统一处理下层输入隔离、焦点进入及关闭后的焦点恢复。回调需要接管主剧情时使用 game 提交入口操作；普通画面修改可直接执行。

## 七 设置页与图片风格控件
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
拖动由控件完成捕获与数值换算，同一数值决定滑块位置和音量。作者提供轨道与滑块的绘制节点，控件按方向、范围和槽位布局它们。settings 属于全局设置，读剧情存档后继续使用当前设置。
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
bind 连接可观察属性，属性同时保留原来的读写权限；text 可以读取只读属性，slider 写回数值时要求可写属性。derive 计算只读外观。派生函数对读取的可观察属性建立依赖，也可读取不可变捕获值；普通数据的内部变化保持普通语言语义。可观察属性变化仅更新相关节点。捕获存档的短暂期间，脚本派生与作者外观更新暂停，捕获完成后按最新状态恢复。普通 #{...} 数据记录适合传参，持续变化的业务值用 model(#{...}) 创建可观察模型。循环依赖和派生计算中的写入、等待给出源码诊断。
作者可用同样方式封装统一字体、按钮图片、留白和动效，再作为普通模块导入。风格复用来自组件和参数，共用的属性对象也可按普通语言规则组织。

## 八 遮罩 特效与自定义演出
mask、blur 等效果值属于同一合成体系。效果组先合成自己的子节点，再应用效果，组透明度作用一次。裁剪用于几何范围；图像遮罩用于可见程度。mask.progress 从 0 到 1 表示由全隐藏到全显示，softness 控制边缘宽度；黑白遮罩的灰度决定显现先后。
```aside
let wipe = mask("assets/ink-mask.png", progress: 0, softness: 0.06);
let reveal = group(effects: [wipe]) {
    image("assets/window-cg.png");
};
view.picture.add(reveal);
let activity = run(tween(wipe, progress: 1, duration: 1.2),
                   owner: reveal);
await activity.finished;
```
效果参数与节点属性采用同一动画规则。更复杂画面可以嵌套 group：背景单独模糊，人物保持清晰，图片按钮仍在最上方；再对整组使用遮罩或镜头变换。所有子节点的前后顺序由树结构决定。
自定义效果通过版本化 Effect 描述输入图像、参数、输出范围和后端实现。高级作者可以开发宿主扩展，常用效果随后以普通 Aside 函数组合复用。shader 支持属于后端能力，项目检查列出缺少的能力。可移植效果应同时提供简化实现，游戏通过能力查询选择适合的平台版本。
特效改变的像素与逻辑输入区域分别声明。普通仿射变换与几何裁剪自动映射命中；弯曲、扭曲和任意着色器的交互区域由作者提供逻辑形状或专用映射。为每次点击从 GPU 读回画面会增加延迟，因此基础接口使用可直接计算的区域。

## 九 选择与会变化的内容
choose(options) 接收具有稳定 key 和 label 的选项数组，更新 game.choice 并等待 select(key)。当前句与选择共用主剧情交互权限。作者把选择视图放在自己的画面里，位置和背景均可自定。
```aside
fn choice_view(model) {
    list(items: bind(model.options),
         key: |option| option.id,
         build: |option| {
             button(on_click: |event| model.select(option.key)) {
                 text(value: bind(option.label));
             }
         })
}
let answer = choose([
    #{key: "window", label: "走到窗边"},
    #{key: "door", label: "留在门口"}
]);
if answer == "window" { by_window(); }
else { by_door(); }
```
choice_view 挂载在包含对话 UI 的画面中，by_window 和 by_door 是作者的剧情函数。choose 把传入的普通记录复制成引擎拥有的只读可观察行，id、key 和 label 都可读取并绑定，输入记录保持原值。id 是本次 InteractionId 与 key 形成的不透明稳定标识，列表用 option.id 保持这一身份；下一次选择使用新的 id。game.choice.options 是这些行构成的只读可观察列表，默认为空；等待选择时填入选项，选择完成后清空。过期或重复 key 的点击按交互身份过滤。
普通 children 中的 for 适合构造当下画面。list 用于持续变化的列表：items 连接可观察集合，key 确定元素身份，build 为新 key 创建节点；同 key 的可观察 item 保持身份并更新字段，移除 key 才销毁对应子树。默认竖排，虚拟显示由 viewport 范围和可复用测量结果控制。重复 key 在对应列表位置报错。普通数组作为 items 时按固定内容构造。

## 十 存档 历史 路线与跳过
saves 是独立服务。save(slot_id) 返回 SaveRequest，quick_save() 使用快速槽；load(slot_id) 和 quick_load() 返回 LoadRequest。请求有只读 status、error 和 finished，可用 derive 或 bind 显示进度；finished 可等待，保存按钮通常只提交请求并观察模型。成功意味着槽位已提交到存储。请求自己的 finished 也可等待：捕获在调用后的可恢复边界进行，这份存档里的对应请求恢复为已成功结果，等待者继续执行后面的语句。保存调用始终只执行一次。其他在途外部写请求跨进程恢复时，以可处理的 OperationInterrupted 错误结算；当前存档对应的请求使用上述成功规则。载入请求成功会替换会话，旧会话拥有的回调任务结束，进程级服务继续发布结果。
saves.slots(page: 1, page_size: 12) 返回 SlotPage，可观察 items 包含稳定 id、occupied、chapter、saved_at、thumbnail 和 status。saves.delete(slot_id) 返回 DeleteRequest，成功后对应摘要变为空槽。每页页码从 1 开始，page_size 与页码决定固定普通槽位集合；快速槽独立。作者通过 grid、image、text、button 组成页面，翻页只更换显示的集合。
```aside
let slot_page = saves.slots(page: 1, page_size: 12);
let save_button = button("保存", on_click: |event| {
    saves.save(slot_page.items[0].id);
});
```
保存、读档和删除请求共用 Queued、Running、Succeeded、Failed 状态；保存额外公开 phase = Capturing 或 Writing。error 在失败时包含说明，成功时为 null。finished 返回请求结果；请求失败抛出对应错误。槽位摘要只在成功提交后替换，失败保留原摘要。
game.history 是只读可观察列表，每条包含 id、speaker_name、text、voice 与恢复点信息。用 scroll 包含 list 即可按需显示历史条目；读档或回滚时列表切换到恢复的会话状态。普通 UI 更换保持历史。
game.stories.entries 为章节索引，条目有 id、title、visited；game.stories.links 保存显式登记的 from/to 连接。编译器从 #[story] 收集入口，动态连接由 game.stories.link(from_id, to_id) 登记。作者用按钮代表章节、路径节点画连接，布局可手排或由自己的算法计算。
game.restart(story_id) 使用当前会话固定的主剧情模块集合，从命名章节入口重新执行剧情任务，保留当前剧情变量，结束旧主剧情任务及其交互；目标函数负责构造自己的画面。game.start(entry) 创建全新会话，适合新游戏。saves.load 恢复保存的位置、变量和画面。作者据此给路线按钮选择含义清楚的操作。
按住快进、自动播放、分段跳过使用统一的剧情推进服务。game.skip_to(story_id) 以正常控制流加速执行，抵达目标章节入口、选项或未读边界时停止；目标不可达时按正常剧情结束，并报告目标未抵达。game.set_auto(enabled) 控制自动播放。按住快进由输入层的 skip 动作保持状态驱动，作者可以将键盘或手柄绑定到该动作。
快进缩短可跳过的已读等待和有限演出，变量计算仍按顺序执行；松开、失焦、打开菜单、新游戏和读档清除按住状态。tween 完成到终值，有限等待结束，媒体按播放策略停止或继续；循环活动仍由 owner 管理。作者显示的控制提示跟随同一服务状态。
这些服务提供数据与操作，页面的外观由作者组合。本版列出的签名属于待实现规范；对应功能达到契约验收后，指南为该节标注已实现版本与运行命令。

## 十一 作者怎样扩展自己的积木
复用的最小单位是普通 fn。返回节点的函数用于画面，返回动作的函数用于演出，普通对象用于把需要操作的部分交给调用者。命名实参、children、闭包和模块导入共同组成这套方式。
```aside
fn arrive(node, destination_x, seconds = 0.5) {
    parallel(
        tween(node, x: destination_x, duration: seconds),
        tween(node, opacity: 1, duration: seconds)
    )
}
```
同一个 arrive 可以用于人物、标题按钮或整组文字。更换图片、布局和动作参数即可形成自己的组件库。自定义节点通过宿主注册参与相同的属性、输入、绘制与恢复协议。新增语言关键字只用于普通组合确实难以表达、且多个创作场景共同需要的规则。

## 十二 文档与代码同步
本指南由项目负责人维护。每个 API 标明设计中、已实现及适用版本。实现后的示例提取为随代码检查的源码片段：解析、类型与接口检查、必要的行为验证使用同一份示例。中文与英文引用相同样例文件；翻译更新随接口变更一起提交。
接口、行为、默认值或取消规则变更时，同一个 PR 修改实现、规范、两种语言说明和受影响示例。尚在规划的高级用法独立标注。用户可以从已实现部分开始创作，遇到组合困难时用最小源码和期望画面反馈，负责人据此修订通用接口。
保存此指南的同一个仓库提交是版本依据，功能状态按小节标注；接口改动同时更新中文、英文和共享示例。
本版相对上一草案的核心变化是：统一 fn 声明；按完整节点树组织画面；构造动作与启动活动分开；明确临时叠加与永久替换；把控件外观、派生绑定和持久模型补完整。引擎设计由这些可说明的作者行为反向约束。
