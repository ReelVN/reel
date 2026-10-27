# Reel 游戏开发指南

[English](en.md) · [文档目录](../README.md)

用 Aside 的函数、节点和活动表达画面、对白、交互与演出。一个创作单元可以同时包含 UI 和剧情；局部状态与行为放在一起，调用方决定组合和流程。

下载工具包，将包含 `reel`、`reel-lsp` 和 `reel-game` 的目录加入 `PATH`，重新打开终端。安装 Aside VS Code 扩展后，可使用补全、参数提示、F12 和保存时格式化。`aside.server.path` 指定语言服务，`aside.reel.path` 指定运行工具。

创建 `MyGame` 目录，使用 `src/main.aside` 入口和 `resources/` 资源目录。各作品共用工具安装，各自保存源码、资源和 `build/` 产物。脚本中的资源路径相对 `resources/`。

```sh
cd MyGame
reel check
reel dev
reel build
reel export --release
```

`check` 编译并定位错误；`dev` 运行源码。`build` 默认生成 `build/debug/game.asb`，`--release` 使用 `build/release/game.asb`。`reel run build/debug/game.asb` 运行已编译代码。

`dev` 将编译缓存保存在 `build/dev/`，工具与源码内容未变时直接复用；修改源码后重新编译。

交互终端中的源码命令显示当前阶段、模块进度和耗时；`dev` 复用缓存时显示缓存状态。

`export` 默认生成 `build/export/目标/debug/` 开发工程，`--release` 使用同一目标下的 `release/`。例如 Vita 发布目录为 `build/export/vita/release/`。发布目录包含播放器、代码、文本包和资源卷；整个目录交给玩家。默认输出重复执行时更新，显式 `--output` 通过 `--overwrite` 替换已有目标。替换导出目录前，将存档放在独立位置；`--save-directory` 指定运行数据目录。

平台素材放在 `resources/@目标/`，按相同路径覆盖公共素材。例如 `resources/@vita/bg/room.png` 覆盖 `resources/bg/room.png`，脚本仍引用 `bg/room.png`。目标内独有的文件也会选入，其余目标目录跳过；缺少覆盖时使用公共文件。默认资源目标为宿主系统名，如 `windows`、`linux` 或 `macos`。

```sh
reel dev --resource-target vita
reel export --release --resource-target vita
```

这两个命令使用当前平台播放器预览、导出选定素材。开发版复制所选资源，发布版打包为统一逻辑路径。

使用其他平台的运行时导出时，通过 `--runtime` 指定其目录。可执行文件、原生模块与平台元数据保留相对路径，与 `game.asb` 放在一起：

```sh
reel export --release --resource-target vita --runtime path/to/vita-runtime
```

使用对应平台的打包工具将该目录制成安装包。Vita 运行时读取准备好的 Opus 音频和块压缩纹理；硬解视频使用 H.264/AAC，尺寸为偶数且不超过 960 × 544。

安装 Reel 启动器后，每部作品可单独导出数据：

```sh
reel export --release --resource-target vita --data-only
```

将导出目录复制到 `ux0:/data/reel/作品名/`。启动器列出含 `game.asb` 的作品目录；进度保存在各作品的 `saves/` 目录。游戏中按 SELECT + START 返回作品列表。

在启动器中按 SELECT 打开设置，选择界面语言和主题；设置保存在 `ux0:/data/reel/launcher.tsv`。复制新作品后按 START 刷新列表。

在项目根添加 `resources.toml`，可以自动生成目标平台的素材副本：

```toml
[[targets.vita]]
match = "images/**/*.{png,jpg,jpeg}"
convert = "texture"
scale = 0.5
format = "auto"
power_of_two = true

[[targets.vita]]
match = "audio/**/*.{wav,ogg,mp3,flac}"
convert = "opus"
bitrate = 96000

[[targets.vita]]
match = "video/**/*.mp4"
convert = "video"
size = [960, 544]
bitrate = 4000000
```

规则按顺序取首个匹配，未匹配的文件直接使用原内容；`convert = "copy"` 可保留特定文件。先选取公共素材和平台覆盖，再应用转换。纹理缩放保留原有逻辑尺寸和 `source_rect` 坐标；原脚本中的图片、音频路径继续使用。BC 纹理与瓦片自动选择编码质量，保持所选格式的 GPU 存储大小。

给渐变素材单独配置 `format = "rgba8"` 与 `dither = "ordered"`，并把该规则放在通用纹理规则之前。抖动分散颜色量化误差，alpha 保持不变；RGBA8 的纹理内存是 BC1 的八倍。单个文件可运行 `reel prepare-texture image.png --output image.reel-texture --format rgba8 --dither ordered`。

长图和图集可在通用纹理规则前配置 `convert = "tiles"`。`tile_size = 510` 为 512 像素纹理留出边缘，`first_level = 1` 保存半尺寸及更小层级；同一规则也接受 `format` 和 `power_of_two`。原有逻辑路径与图集裁剪坐标继续使用，运行时按可见瓦片加载。

```sh
reel prepare-resources --resource-target vita
reel dev --resource-target vita
reel export --release --resource-target vita
```

`prepare-resources` 提前准备并报告转换结果；`dev`、`run` 和 `export` 也会按配置准备。缓存位于 `build/resources/`，素材、参数、对应转换实现或依赖变化时更新相关副本。离线转换逐个处理资源；`prepare-resources --memory-bytes` 可指定转换工作区限额。

视频转换调用 PATH 中的 `ffmpeg` 和 `ffprobe`，输出 H.264/AAC MP4，保持帧率并按 `size` 等比缩小。上下排列颜色与透明度的影片使用 `alpha_layout = "top_bottom"`。视频工作区按尺寸估算，编解码子进程自行分配内存。

字体默认使用完整文件。需要裁剪时，显式选择 `convert = "font"`，用 `unicode = ["U+0020-024F", "U+3000-9FFF", "U+FF00-FFEF"]` 指定字符区间，用 `text = ["src/**/*.aside"]` 加入剧情与翻译用字。按作品支持的语言扩展区间，覆盖玩家输入；范围外字符会缺字。字体转换需要 Python 的 FontTools 包；`REEL_FONT_PYTHON` 可指定已安装该包的 Python。字体族、样式和字体集合中的各字体均保留。

`reel clean --resources` 清理转换缓存并保留导出包；下一次准备会重新转换。`reel clean` 清理整个 `build/`。

在项目根运行 `reel fmt` 整理源码，`reel fmt --check` 检查排版。格式化使用 4 空格缩进和 100 字符目标行宽，长调用紧凑排列参数。英文 API 文档由 Rust 声明生成，可在编辑器中悬停或跳转查看。

下文的完整项目包含 `main`；其余片段放在相应函数或组件中，沿用上下文中的变量。提供示例使用的字体和素材后运行。仓库共享素材位于 `examples/assets/`；通过 `--assets` 可另选资源根。

## 1 创建项目

把下面的代码保存到 `src/main.aside`，把字体放到 `resources/fonts/body.ttf`，然后在项目目录运行 `reel dev`。`game.configure` 设置作品名称、逻辑画布和字体；画面由 `stage.replace` 挂载。

```aside
fn main() {
    game.configure(title: "Window light", width: 960, height: 540,
        fonts: ["fonts/body.ttf"]);
    stage.replace(text("Hello", x: 24, y: 24));
}
```

内容增长后，按章节、菜单或小游戏拆分模块。入口配置作品、创建共享状态并开始流程。`game::ui::title` 对应 `src/ui/title.aside`：

```aside
use game::ui::title::title;

fn main() {
    game.configure(title: "窗边", width: 1920, height: 1080, fonts: ["fonts/body.ttf"]);
    stage.replace(title());
}
```

`src/ui/title.aside` 返回作者组合的标题页。普通页面通过函数调用衔接；`game.start` 创建新会话。后文给出 `src/story/opening.aside` 的剧情函数。

```aside
use game::story::opening::opening;

pub fn title() {
    column(x: 760, y: 480, gap: 24) {
        text("窗上的光");
        button("新游戏", on_click: |event| game.start(opening));
    }
}
```

## 2 用数据组织内容

用数组、表和普通循环生成内容。`..` 排除终点，`..=` 包含终点，整数区间按需迭代。`f"...{expression}..."` 把表达式的值插入文字。

```aside
let values = [];
for value in 1..=3 { values.append(value); }
let latest = values.pop();
stage.replace(text(f"剩余 {values.len()} 项，最后一项 {latest}"));
```

需要逐项计算内容时，用 `gen fn` 和 `yield`。调用创建生成器，`for` 每取一项才继续正文；提前退出循环会执行生成器的 `finally`。下面按需生成位置，避免建立中间数组。

```aside
gen fn positions(count: int) {
    for index in 0..count {
        yield (24 + index * 120, 40);
    }
}

fn main() {
    stage.replace(stack {
        for (x, y) in positions(4) {
            text("★", x: x, y: y);
        }
    });
}
```

手动推进使用 `let (done, value) = items.next()`，`done` 区分结束与产出的 `null`；结束时返回一次最终结果。`items.close()` 提前结束生成器。生成器负责按需产生数据，等待对白与动画的流程继续使用普通函数。

`get` 在键缺失时返回默认值，保留已存的 `null`、`false` 和 `0`。默认值按普通实参求值。

```aside
let settings = {volume: 0, wallpaper: null};
let volume = settings.get("volume", default: 0.7);
let wallpaper = settings.get("wallpaper", default: "default.png");
let language = settings.get("language", default: "zh-CN");
let removed = settings.remove("wallpaper");
settings.wallpaper = "new.png";
```

`sort` 稳定地原地排序。记录可以使用返回负数、零或正数的比较器；比较器同步执行，排序成功后更新数组。

```aside
let cuts = [180, 20, 90];
cuts.sort();
let captions = [{text: "后景", layer: 2}, {text: "前景", layer: 1}];
captions.sort(compare: |a, b| a.layer - b.layer);
```

展开可以组合数组和记录，后面的同名字段覆盖前面的字段。展开复制外层内容，内部对象继续共享。

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

tuple 适合固定形状的数据。`let` 和 `for` 支持固定长度解构，`_` 跳过一个位置；类型或长度不符会抛出 `PatternMismatch`。

```aside
let settings = {language: "zh-CN", volume: 0.8};
for (key, value) in settings.entries() {
    let pair = (key, value);
    let (name, _) = pair;
}
```

`match` 选择第一个匹配且条件成立的分支，分支末尾表达式提供结果。这里的 `mode` 由调用方传入。

```aside
let caption = match mode {
    "online" | "typing" => "在线",
    name is string if name != "" => name,
    _ => "离线",
};
```

容器的方法直接表达插入、删除、切片和反转。表的 `entries()` 返回 `(key, value)` 对。完整示例见 [collections](../../examples/collections/src/main.aside) 和 [collection_editing](../../examples/collection_editing/src/main.aside)。

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

二进制数据放在 `resources/` 中，用 `assets.read` 按需载入。读取结果是不可变 `Bytes`，支持长度和字节索引。

```aside
let cells = await assets.read("data/cells.bin");
let count = cells.len();
let first = cells[0];
```

连续数值用于索引前，使用 `std::math::to_int` 转为整数。它保留整数输入，并将浮点输入向零截断。

```aside
use std::math::to_int;
let frame = to_int(1.25 * 20);
```

显示数值时，用 `std::number::format` 选择小数位数或科学计数法的有效数字位数。格式化只生成文本，业务数值继续保留原精度；分隔符和显示量级由作品决定。舍入规则与范围见[标准库声明](../../crates/aside-std/src/number.rs)。

```aside
use std::number::format;
let price = format(12.25, precision: 1);
let total = format(123456789, precision: 4, mode: "scientific");
```

## 3 组合画面与布局

函数返回节点，尾随子节点块组合它们。块内可使用条件、循环和局部变量；返回 `null` 表示这一处省略节点。位置、背景与交互都沿用同一棵舞台树。

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

`row`、`column` 和 `grid` 使用 Flex/Grid 布局。`grow` 分配剩余空间，`gap` 控制间距；固定位置的画面可以使用 `stack`。逻辑画布按窗口等比缩放，绘制与输入共享坐标转换。

```aside
column(gap: 24) {
    row(width: 640, gap: 12) {
        text("Mio", width: 120);
        text("The rest of this row belongs to the dialogue.", flex: 1);
    }
    grid(columns: 4, column_width: 140, row_height: 80, gap: 12) {
        for slot in 0..12 { button(f"{slot + 1}"); }
    }
}
```

内容决定面板尺寸，背景使用 `stretch` 跟随面板。九宫格 `slice` 保留边角，`max_width` 控制长文本的换行宽度。图片路径相对 `resources/`。

```aside
stack(max_width: 420) {
    image("images/panel.png", stretch: "both", slice: 12);
    column(padding: 24, gap: 16) {
        text("The frame follows wrapped text and buttons.", align_x: 0.5, text_align: "center");
        button("Dismiss", align_x: 0.5, on_click: |event| event.target.remove());
    }
}
```

自定义鼠标光标也由普通节点组成。把 `cursor` 加入所属面板，面板结束时光标一起释放；动画、绑定和图片属性照常使用。

```aside
let pointer = cursor {
    image("images/pointer.png", anchor_x: 0.5, anchor_y: 0.5);
};
panel.add(pointer);
```

## 4 组件、状态与交互

只需展示时，函数返回节点即可。组件需要持续操作或明确数据结构时，用类表达字段和方法。类型注解帮助检查、补全与定义跳转。

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

构造参数、属性赋值和 `.with(...)` 都能配置节点。按当前代码的可读性选择写法；重复使用的外观可以提取为普通函数或记录。

```aside
let caption = text("窗外下起了雨。")
    .with(font_size: 28, color: "#eeeeee")
    .with(x: 48, y: 420, max_width: 720);
caption.with(opacity: 0.5);
```

需要让玩家先看到当前画面再开始演出时，等待 `stage.presented()`。资源准备期间播放器保留上一幅已显示画面。

```aside
stage.replace(scene);
await stage.presented(owner: scene);
await run(tween(scene, opacity: 1, duration: 0.5));
```

用 `appearance` 复用控件的状态外观。背景、文字颜色、边框与变换直接作用于原节点；`focus_visible` 提示键盘和手柄焦点。各状态依次覆盖已指定的属性，作用域 `override` 保持最高优先级。图片等复合外观仍可由子节点和 `derive` 组合。

```aside
let look = appearance(base: {background: "#304060", corner_radius: 6},
    hover: {color: "#ffd580"}, pressed: {background: "#203040"},
    focus_visible: {border_width: 2}, disabled: {opacity: 0.4});
column(gap: 12) {
    button("Hover me", width: 240, height: 48, decorated: false, appearance: look);
    button("Another component", width: 240, height: 48, decorated: false, appearance: look);
}
```

同一个父节点内，`move` 调整绘制顺序并保留节点身份、回调和正在进行的动画。

```aside
let back = image("assets/panels.png");
let front = image("assets/sun.jpg", x: 120);
let scene = stack { back; front; };
stage.replace(scene);
scene.move(back, index: 1);
```

`add` 挂载新内容，`remove` 结束节点及其拥有的工作。局部更新适合通知、菜单项和临时控件。示例见 [author_dynamic_stage](../../examples/author_dynamic_stage/src/main.aside)。

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

图片与文字都可以参与布局。更改图片源后，引擎准备新资源并在就绪时更新画面；同一路径的图片共享资源。

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

父节点的平移、旋转、缩放和透明度作用于整个组件。负缩放用于镜像，`anchor_x`、`anchor_y` 决定变换锚点。

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

`camera` 把世界中的一个区域映射到视口。HUD 放在相机外的兄弟节点中保持固定。大图可以预先准备为分块图片，运行时只加载视口所需的层级和瓦片：

```sh
reel prepare-image original.png --output resources/panorama.reel-image
```

用 `image("panorama.reel-image")` 显示。将生成的清单及相邻 `.tiles` 目录一起放在资源目录，构建和导出会收集它们。`--tile-size` 默认 512；加上两像素边缘后须在设备纹理尺寸限制内。准备过程需要解码原图，运行时内存随可见区域分配。完整用法见 [author_large_image](../../examples/author_large_image/src/main.aside)。

为较低分辨率准备背景或立绘时，可以缩小资源并编码为 GPU 压缩纹理，保留原图的布局尺寸和 `source_rect` 坐标：

```sh
reel prepare-texture original.png --output resources/background.reel-texture --size 960x544
```

用 `image("background.reel-texture")` 显示，相同资源可用于粒子和图片遮罩。工具按比例缩小，不放大原图；默认给不透明图片使用 BC1，给含透明度的图片使用 BC3。`--format rgba8` 使用未压缩存储，适合需要保留细节的 UI 与遮罩；`--power-of-two` 为需要二次幂尺寸的设备填充边缘。`prepare-image` 默认生成压缩纹理瓦片，`--tile-encoding png` 选择 PNG 瓦片。BC 格式需要渲染设备支持，运行时直接上传压缩块。转换接口见 [prepare_texture](../../crates/reel-cli/src/prepare_texture.rs)。

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

一个场景可以返回根节点以及剧情需要操作的部分。将下面的组件放在 `src/ui/room.aside`，由剧情函数控制人物、CG 和对白区域。

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

可复用容器接收 `children`，调用方决定内容。同一容器可以装对白、设置或小游戏。

```aside
fn card(children = []) {
    column(gap: 12, padding: 24) {
        for child in children { child; }
    }
}
```

为已经显示的节点定位浮层时，等待 `stage.geometry`。结果包含转换到参考节点坐标的边界；透明节点仍参与布局。文本的 `text_caret` 给出末尾插入光标的边界，可用 `y + height / 2` 居中放置继续标记。目标隐藏、未挂载或参考变换不可逆时，请求报告 `GeometryUnavailable`。

```aside
overlay.visible = true;
overlay.opacity = 0;
let geometry = await stage.geometry(thumbnail, relative_to: overlay, owner: root);
indicator.x = geometry.bounds.x + geometry.bounds.width / 2;
indicator.y = geometry.bounds.y;
overlay.opacity = 1;
```

## 5 对白与剧情

角色调用按顺序等待玩家推进。需要保留一段逐字节奏时，设置 `reveal_skippable: false`。普通函数也可穿插 UI、音效与小游戏。

```aside
mio(rich("Listen.[pause=0.5]"), reveal_skippable: false);
narrator("The light is moving.", reveal_skippable: false);
say([mio.line("One."), eri.line("Two.")], reveal_skippable: false);
```

每句对白可决定进入历史的文字。`history: false` 省略这一行，字符串或富文本提供单独的历史内容。

```aside
mio("……", history: "I remember the rain.");
narrator("A fleeting thought.", history: false);
say([mio.line("Noise.", history: false), eri.line("Stay.", history: rich("[b]Stay.[/b]"))]);
```

`src/story/opening.aside` 用普通控制流组织剧情。`choose` 返回稳定的分支 key，调用对应章节即可组成共同线和个人线。

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

把对白视图放在 `src/ui/dialogue.aside`。`game.dialogue()` 提供文字、姓名和显现位置，作者决定布局、字体、背景与提示光标。`ready` 表示整组正文与句内停顿完成。

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

同时发言用 `say` 组合多个角色行。每行保留自己的姓名、样式与显现状态。

```aside
let eri = character("Eri");
say([mio.line("你听见了吗？"), eri.line("在窗外。")]);
```

`wait: false` 返回可继续追加的对白句柄。追加文字、播放音效或等待动画后，再设置推进点；新对白或选择会结束旧句柄。

```aside
let line = mio("Starting", wait: false);
await run(delay(0.5));
line.append(" ...", wait: false);
audio.play("assets/chime.wav");
line.append(" ready.");
line.append("\nWelcome back.");
```

## 6 串行、并行与时间

`tween`、`delay` 和组合函数描述动作，`run` 启动动作，`await` 等待完成。`sequence` 与 `.then` 表达串行，`parallel` 与 `.together` 表达并行。

```aside
let entrance = tween(portrait, opacity: 1, duration: 0.3);
let move = tween(portrait, x: 640, duration: 1);
await entrance.then(move).then(delay(0.5)).run();
```

内置缓动适合常见运动；自定义曲线用关键点表达节奏，并可供多个动作复用。

```aside
use std::math::curve;

let acceleration = curve([
    {time: 0, value: 0, mode: "cubic", out_tangent: 0.5},
    {time: 1, value: 1, in_tangent: 1.5},
]);
await tween(portrait, x: 640, duration: 2, easing: acceleration).run();
```

需要同步起点时，先创建暂停的活动并准备画面，首帧显示后再恢复。音频与动画分别保留自己的播放状态。

```aside
panel.opacity = 0;
let music = audio.play("assets/chime.wav", owner: panel, paused: true);
let entrance = tween(panel, opacity: 1, duration: 0.25).run(owner: panel, paused: true);
stage.replace(panel);
await stage.presented(owner: panel);
music.resume();
entrance.resume();
```

帧动画用 `clock` 的时间驱动图片选择。时钟属于场景，暂停和退场沿同一生命周期处理。可运行示例见 [author_frame_animation](../../examples/author_frame_animation/src/main.aside)。

```aside
let frames = ["assets/frame-a.png", "assets/frame-b.png"];
let count = frames.len();
let timer = clock(interval: 0.125, paused: true, owner: root);
root.add(image(source: derive(|| frames[timer.ticks % count]), width: 64, height: 64));
stage.replace(root);
await stage.presented(owner: root);
timer.resume();
```

单个动作可以直接启动并等待，缓动参数决定运动节奏。

```aside
await tween(portrait, x: 720, duration: 0.8, easing: "sine_out").run();
```

并行动作分别控制人物和界面，整个组合在两者完成后继续剧情。

```aside
let entrance = tween(view.portrait, x: 320, duration: 0.5)
    .together(tween(view.hud, opacity: 1, duration: 0.3));
await entrance.run();
```

返回动作的普通函数可以复用演出。`from` 显式指定起始属性。

```aside
fn fade_in(node, seconds = 0.5) {
    tween(node, from: {opacity: 0}, opacity: 1, duration: seconds)
}
await delay(0.5).then(fade_in(view.hud)).run();
```

循环动作适合呼吸、闪烁和等待提示。保存活动句柄，场景需要时暂停、恢复或停止。

```aside
let breathing = tween(view.portrait, opacity: 0.92, duration: 1.4)
    .then(tween(view.portrait, opacity: 1.0, duration: 1.4))
    .repeat();
breathing.run(owner: view.root);
mio("听，雨又大了。");
```

`delay` 在剧情中留出时间，也可与其他动作组合。等待期间播放器按活动的截止时间调度。

```aside
await run(delay(1.5));
await run(sequence(delay(0.5), tween(panel, opacity: 0, duration: 0.3)));
```

曲线也可直接采样，用同一时间源控制多个属性。场景资源准备好后恢复时钟。

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

## 7 临时画面与生命周期

`reading` 为一段剧情组合局部对白、选择和控制状态。函数返回结果，调用方决定下一页。

```aside
fn story_page(root) {
    reading(body: |page| {
        let controls = game.reading();
        page.add(root, button("Title", on_click: || page.finish("title")));
        controls.set_auto(condition: true).pause(false);
        narrator("The story continues here.");
        "completed"
    })
}
```

`scope` 拥有临时节点和活动。正文结束时统一释放它们，适合全屏 CG、确认框和临时玩法。

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

`override` 为作用域临时覆盖属性。作用域结束后，节点恢复底层属性值。

```aside
scope(body: |s| {
    let effect = s.override(panel, {opacity: 0.4, width: null});
    await s.run(tween(effect, opacity: 1, duration: 0.3));
    let next = completion();
    s.add(panel, button("Continue", on_click: || next.resolve()));
    await next;
});
```

`offset` 给运动叠加位移，适合震动和漂浮。基础动画继续推进，临时位移随作用域结束。

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

`finish(result)` 从组件交回结果并结束其拥有的工作。保存或退场动画应在 `finish` 前等待，或放在作用域返回之后。

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

视频是普通舞台节点，`play` 返回可等待活动。跳过按钮可以停止播放；布局和点击区域由作者组合。

```aside
let movie = video("assets/logo.mp4", width: 1280, height: 720, fit: "contain");
let playing = movie.play();
stage.replace(button(width: 1280, height: 720, decorated: false,
    background: "#000000", on_click: || stop(playing)) {
    movie;
});
await playing;
```

资源将颜色放在上半帧、灰度透明度放在下半帧时，设置 `alpha_layout: "top_bottom"`。自然高度为编码高度的一半，裁剪坐标使用可见颜色区域的像素。默认 `"native"` 保留普通图片透明度和视频的不透明画面。图片和视频共用此属性。

```aside
let overlay = video("assets/overlay.mp4", alpha_layout: "top_bottom", width: 960, height: 512);
stage.replace(overlay);
await overlay.play();
```

剧情运行中可随时加入临时控件。保留节点引用，在这段交互结束后移除。

```aside
let hint = view.hud.add(button("查看照片",
    on_click: |event| photo_panel.open()
));
mio("桌上那张照片，你还记得吗？");
hint.remove();
```

## 8 菜单、输入与设置

按钮回调可以修改状态，也可以等待演出。回调属于按钮节点；按钮退出时结束它拥有的任务。

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

`popup` 跟随触发节点定位，默认以模态层接收输入。悬停提示设置 `modal: false`，使锚点在提示显示期间继续接收指针交互。`on_dismiss` 处理外部点击或返回动作。

```aside
trigger.add(popup(placement: "bottom-start", offset: 4,
    on_dismiss: |event| event.target.remove()) {
    column(width: 200, gap: 8, padding: 12) {
        text("Choose a reading speed");
        button("Slow", on_click: || { preferences.speed = 20; });
        button("Fast", on_click: || { preferences.speed = 50; });
    }
});
```

模态菜单限制当前可交互的范围。把菜单放到场景上层，并为关闭操作保留清晰入口。下面的例子可直接运行。

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

命名动作把键盘、鼠标和手柄映射到同一行为。连续移动用时钟更新；输入回调更新玩家意图。文本编辑时，聚焦编辑器处理自己的键盘和输入法事件。

用 `stage.input()` 的只读可观察字段 `source` 和 `layout` 选择输入提示。`source` 表示最后使用的输入方式，手柄的 `layout` 为 `xbox`、`playstation`、`nintendo` 或 `generic`；作品提供对应图标。

需要指针位置时，用 `input(on_pointer: |event| ...)` 读取节点内的 `event.x`、`event.y` 和 `event.phase`。悬停与拖动都会产生 `moved`；主按钮或触摸产生 `pressed`、`released` 和 `cancelled`。自定义拖动控件用 `event.dragging` 判断该节点是否持有拖动。

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

图片按钮使用同一个 `button`，按交互状态切换图片。`decorated: false` 可交由子节点绘制全部外观。

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

需要悬停音效或预览时，用 `on_interaction` 接收状态变化。

```aside
let gallery = model({selected: null});
let card = button("Sketch", on_interaction: |event| {
    if event.hover || event.focus_visible { gallery.selected = "sketch"; }
});
```

复选框和单选项通过 `bind` 共享业务字段。同组单选项共享一个值，方向键可在组内移动并选择。

```aside
let settings = model({sound: true, language: "zh-CN"});
column(gap: 12) {
    checkbox("Sound", value: bind(settings.sound));
    radio("zh-CN", "简体中文", value: bind(settings.language));
    radio("en", "English", value: bind(settings.language));
}
```

设置页可以直接连接阅读状态。自动阅读在文字呈现后等待设定时间，选择处等待玩家；快进按已读与选择策略推进。模态界面、暂停和失焦沿阅读状态处理。

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

先通过 `window.info().capabilities` 判断可用设置，再等待 `window.configure` 返回应用后的状态。窗口大小与逻辑画布分别设置。

```aside
let mut display = window.info();
if display.capabilities.resize {
    display = await window.configure(width: 1280, height: 720);
}
if display.capabilities.fullscreen {
    display = await window.configure(fullscreen: true);
}
```

`stage.focus` 在包含当前变更的画面被接受后，把键盘或手柄焦点交给目标。下面的 Next 从隐藏切换为可见，再等待交接结果。返回的 `FocusOperation` 继承 `WindowOperation`，await 结果为 bool；目标隐藏、禁用、退休或处于模态范围外时为 false，原焦点继续保留。`owner` 默认当前作用域，作用域结束会取消未完成请求。

```aside
let next = button("Next", visible: false);
stage.replace(column { button("Skip"); next; });
await stage.presented();
next.visible = true;
let focused = await stage.focus(next);
```

默认交接会显示导航焦点。用 `visibility: "preserve"` 保留交接应用时的指示状态，可以在鼠标操作期间预置下一次确认目标；随后按方向键或确认键会显示导航焦点。

```aside
let focused = await stage.focus(next, visibility: "preserve");
```

侧边操作可以用 `button(focusable: false, focus_on_pointer: false)` 接收鼠标悬停与点击，同时保留当前控件的焦点。`focusable: false` 会让方向键、Tab 和 `stage.focus` 跳过该按钮；显式交接返回 false。属性可绑定到模型，修改后沿当前舞台状态更新。

用 `await stage.focus(null, owner: page)` 清除当前导航焦点和提示。`page` 须处于当前输入作用域；后续方向键或 Tab 重新进入控件导航，确认键交给页面处理。

`text_input` 编辑单行文本，`text_area` 编辑多行文本。绑定到业务字段后，控件写回与其他视图更新共用同一状态。

```aside
let profile = model({name: ""});
stage.replace(text_input(value: bind(profile.name), width: 280, height: 40,
    color: "#202020", background: "#ffffff",
    on_submit: |event| { profile.name = event.value; }));
```

编辑控件提供撤销、重做、选择和替换操作。工具栏通过可观察状态更新启用条件。

```aside
let notes = text_area(value: "Draft", width: 360, height: 180);
let toolbar = row {
    button("Undo", enabled: derive(|| notes.can_undo), on_click: || notes.undo());
    button("Redo", enabled: derive(|| notes.can_redo), on_click: || notes.redo());
    button("Select all", on_click: || notes.select_all());
};
stage.replace(column { toolbar; notes; });
notes.select_bytes(0, focus: 5);
```

`bind` 连接现有属性，`unbind` 断开连接。需要组合多个值时使用 `derive`。连接后的控件直接共享状态。

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

## 9 样式与演出

外观可以是普通记录，组件在构造时展开默认样式和调用方覆盖。顺序决定同名属性的最终值。

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

把相关节点放在同一父节点中，即可整体移动、淡入或缩放。

```aside
let cast = stack() {
    image("assets/mio.png");
    dialogue_view(game.dialogue());
};
stage.replace(cast);
await run(tween(cast, x: 80, duration: 0.5));
```

用作用域把循环特效与有限演出组合，正文完成时结束循环。

```aside
scope(body: |s| {
    s.run(repeat(sequence(
        tween(view.hud, opacity: 0.4, duration: 0.5),
        tween(view.hud, opacity: 1, duration: 0.5)
    )));
    narrator("The light keeps pulsing while this line is shown.");
});
```

转场保留当前已显示画面，在 `change` 中构建目标。目标资源就绪后推进转场；函数返回时转场完成。

```aside
stage.transition(target: scene, duration: 0.6, change: || {
    scene.clear();
    scene.add(image("assets/room-evening.png"));
});
```

图片遮罩通过进度和柔和度控制显现，普通动画可以驱动这些属性。

```aside
let reveal = stack(width: 1280, height: 720,
    mask: "assets/ink-mask.png", mask_progress: 0, mask_softness: 0.06) {
    image("assets/window-cg.png");
};
view.picture.add(reveal);
await run(tween(reveal, mask_progress: 1, duration: 1.2), owner: reveal);
reveal.mask = null;
```

复杂遮罩用 `mask` 操作组合。可变参数通过节点属性传入，表达式在多个节点间复用。

```aside
let edge = mask.alpha("assets/mask-waves.png", x: mask.param("x"), repeat: "x");
let reveal = stack(width: 640, height: 360, mask: edge, mask_x: 0) {
    stack(width: 640, height: 360, background: "#2d8aab");
};
stage.replace(reveal);
await run(tween(reveal, mask_x: 1, duration: 3), owner: reveal);
```

`invert` 对整个子树反色，可以与透明度、遮罩和动画组合。

```aside
let scene = stack(width: 800, height: 450, invert: 0) {
    text("A different view", color: "#e08020", font_size: 48);
};
stage.replace(scene);
await run(tween(scene, invert: 1, duration: 0.4), owner: scene);
```

`blend` 控制子树与背景的混合，支持 `normal`、`add`、`subtract`、`multiply`、`screen`、`overlay`、`hard_light`、`color_dodge`、`lighten`、`difference`、`soft_light` 和 `linear_burn`。子树先完成自身合成，再应用混合。

```aside
stage.replace(stack(width: 320, height: 180, background: "#204060") {
    stack(width: 320, height: 180, background: "#800000", blend: "add", opacity: 0.5);
});
```

图像的 `sampling: "nearest"` 保留像素边缘，默认 `"linear"` 平滑缩放。切换采样方式复用同一图片资源；图集裁切和分块图片沿用各自的源窗口。

用 `effect` 将绘制函数应用到节点及其子树。`Fragment.sample()` 读取子树合成后的颜色；返回颜色替换结果，或返回浮点数控制覆盖率。参数绑定普通状态后，控件和动画都可以改变效果。

```aside
use reel::types::Fragment;

fn tint(pixel: Fragment, amount: float = 0.5) -> (float, float, float, float) {
    let (r, g, b, a) = pixel.sample();
    (r * amount, g, b, a)
}

fn main() {
    let state = model({amount: 0.5});
    let paint = effect(tint, params: {amount: bind(state.amount)});
    stage.replace(column(gap: 16) {
        slider(value: bind(state.amount), min: 0, max: 1, step: 0.01, width: 320);
        stack(width: 320, height: 180, background: "#ffffff", effect: paint) {
            text("After the rain", color: "#803020");
        };
    });
}
```

用 `texture(path)` 为绘制函数传入图片。下例按噪声图的灰度显露场景：`effect` 返回的浮点数用作 `mask`，动画更新绑定的进度。图片沿用项目资源路径和平台覆盖规则。

```aside
use reel::types::{Fragment, Texture};

fn dissolve(pixel: Fragment, noise: Texture, progress: float) -> float {
    if noise.gray(pixel.uv) <= progress { 1.0 } else { 0.0 }
}

fn main() {
    let scene = stack(width: 1280, height: 720, mask_progress: 0) {
        image("assets/room.png", width: 1280, height: 720);
    };
    scene.mask = effect(dissolve, params: {
        noise: texture("assets/noise.png"), progress: bind(scene.mask_progress)
    });
    stage.replace(scene);
    await run(tween(scene, mask_progress: 1, duration: 1.2), owner: scene);
    scene.mask = null;
}
```

连续粒子用于雨、雪或飘落物。一个粒子节点共享素材，由发射活动控制节奏与寿命。

```aside
let petals = particles("assets/petal.png", width: 1280, height: 20, seed: 42);
stage.replace(petals);
let falling = emit(petals, burst: 0, rate: 80, duration: 10,
    lifetime: 8, velocity_y: 90, velocity_x: 15, spread_x: 30,
    size: 18, size_spread: 6, spin_spread: 60);
await falling;
```

瞬发粒子可响应点击或剧情事件。固定种子便于复现同一演出。

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

按年龄变化的尺寸用 `size_curve` 表达。预热让首帧已有不同年龄的粒子，径向速度和阻尼控制散开后的运动。

```aside
use std::math::curve;
let glow = particles("assets/spark.png", width: 1280, height: 720, seed: 7);
stage.replace(glow);
let envelope = curve([{time: 0, value: 0}, {time: 0.5, value: 1}, {time: 1, value: 0}]);
emit(glow, burst: 0, rate: 40, lifetime: 2, prewarm: 2,
    velocity_y: 0, spread_x: 0, spread_y: 0,
    size: 12, size_spread: 0, size_curve: envelope,
    radial_speed: 40, radial_spread: 10, damping: 1,
    angle: 180, angle_spread: 180, spin_spread: 0);
```

用粒子节点的 `size_scale` 同时缩放所有存活和后续出生粒子的图形尺寸。它与每粒子的年龄尺寸曲线相乘，保留出生区域、位置和运动轨迹；可直接赋值、绑定或用 `tween` 变化。

```aside
await tween(glow, size_scale: 0, duration: 0.2).run();
```


逐字透明度由 `reveal_fade` 控制，`reveal_fade_step` 设置相邻字素的进度差。正文布局保持稳定。

```aside
let caption = text("The light is moving.", reveal_fade: 0,
    reveal_fade_from: 0, reveal_fade_step: 20.0 / 255.0);
stage.replace(caption);
await run(tween(caption, reveal_fade: 3, duration: 3.0 * 255.0 / 1400.0), owner: caption);
caption.reveal_fade = null;
```

`text_wrap` 控制自动换行，显式换行仍保留。受限空间可使用 `text_fit: "shrink"` 和 `min_font_size` 缩小文字。

```aside
stage.replace(text("A long authored line without automatic wrapping.\nA second explicit line.",
    width: 240, text_wrap: false, clip: true));
```

## 10 选择与组件结果

用 `completion` 把异步交互变成普通函数的结果。创建表单、等待玩家确认，再继续剧情。

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

`result.pending` 可以连接按钮的启用状态；完成后的重复等待取得同一结果。

```aside
scope(owner: root, body: |s| {
    let result = completion();
    s.add(root, button("Confirm", enabled: bind(result.pending),
        on_click: || result.resolve(true)));
    await result
});
```

选择页从 `game.choice()` 读取选项并提交选项身份。显示文字可以翻译，分支 key 保持稳定。长列表可用 `list`，滚动与虚拟化见 [author_list_gallery](../../examples/author_list_gallery/src/main.aside)。

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

组件参数可由普通条件表达式生成。表是数据，条件只决定传入哪一份数据。

```aside
let flag = true;
let actions = if flag {
    {accept: [binding("Enter")]}
} else {
    {back: [binding("Escape")]}
};
```

## 11 历史、存档与共享数据

对白画面和历史分别管理。换章时可清空当前文字，再按作品需要清理历史。

```aside
game.dialogue().clear();
game.reading().clear_history();
narrator("A new chapter begins.");
```

`keep_last` 按实际历史行保留最近内容，适合长期运行的作品。

```aside
game.reading().clear_history(keep_last: 37);
```

`saves` 保存和恢复执行位置、状态与画面。捕获期间需要修改会话的输入会使本次保存以 `CaptureBusy` 失败；旧槽位保留。先用 `stage.capture` 取得缩略图，再传给保存请求。存档列表提供数据，作者组合槽位界面。

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

已读进度可以单独重置。正常推进后重新登记；历史、设置和存档保持各自的生命周期。

```aside
stage.replace(button("Reset read progress", on_click: |event| game.clear_read_progress()));
```

`model` 保存可观察业务状态，`let mut` 控制变量能否重新绑定。默认数据在创建模型时复制，后续赋入的对象保留共享身份。

```aside
let settings = model({volume: 0.8, music: true, palette: {accent: "#70d890"}});
let volume = slider(value: bind(settings.volume), min: 0, max: 1);
let music = toggle(value: bind(settings.music));
let indicator = text("Music", visible: bind(settings.music), color: bind(settings.palette.accent));
settings.palette.accent = "#ffd580";
```

全局设置和解锁进度用 `persistent` 独立读写。启动与剧情读档后重新读取并应用当前设置；存入的数据独立于剧情执行快照。

```aside
class Preferences {
    let mut volume: float = 0.8;
    fn init(self, record: table | null = null) {
        if record != null { self.volume = record.volume; }
    }
}
let preferences = model(Preferences(await persistent.read("settings")));
preferences.volume = 0.6;
await persistent.write("settings", preferences);
```

## 12 扩展自己的组件

先用普通函数复用实际重复的表达。返回节点可组合界面，返回动作可组合演出，返回对象可交出组件的操作入口。

```aside
fn arrive(node, destination_x, seconds = 0.5) {
    parallel(
        tween(node, x: destination_x, duration: seconds),
        tween(node, opacity: 1, duration: seconds)
    )
}
```

## 13 文字与翻译

字体由作品提供，`font` 选择字体族，`font_weight` 选择字重。`letter_spacing` 和 `word_spacing` 调整字簇和空格间距。行内样式使用 `rich`；完整标记见 [RichText 接口](../../crates/reel-engine/src/aside/rich_text.rs)。

```aside
stage.replace(column(gap: 12) {
    text("CHAPTER ONE", font_size: 32, letter_spacing: 2);
    text("made with love", font_size: 28, word_spacing: -2);
});
```

`vertical-rl` 从上到下书写并向左换列，`vertical-lr` 向右换列。`height` 控制列长，`line_height` 按字号比例控制列距。按钮标签也支持竖排；文本编辑控件使用横排。

```aside
let sidebar = text(rich("[ruby=ほし]星[/ruby]を見上げる。"),
    writing_mode: "vertical-rl", height: 690,
    font_size: 26, line_height: 40 / 26.0);
stage.replace(sidebar);
```

原文和译文写在同一个 `tr` 调用中。检查、开发运行和打包都从源码生成所需语言内容。界面用 `derive` 跟随语言切换，已提交对白和历史保留当时的文字。单语言作品直接写字符串。

```aside
localization.set_locale("zh-CN");
let data = model({name: "Mio"});
stage.replace(column(gap: 12) {
    text(derive(|| tr(f"你好，{data.name}。", {en: f"Hello, {data.name}."})));
    button("English", on_click: |event| localization.set_locale("en"));
});
```

先预热语言，再用单次调用的 `locale` 同时显示多种语言。插值表达式按原文顺序各求值一次，译文引用这些值。完整示例见 [localization](../../examples/localization/src/main.aside)。

```aside
localization.load("en");
column(gap: 6) {
    text(tr("你好。", {en: "Hello."}, locale: "zh-CN"), font_size: 28);
    text(tr("你好。", {en: "Hello."}, locale: "en"), font_size: 18);
}
```

## 14 音频与视频

`audio.play` 返回可控制、可等待的活动。分组音量用于音乐、语音、音效和 UI；`owner` 把播放绑定到场景生命周期。短音效和长音轨由宿主选择合适的加载方式。

```aside
fn main() {
    audio.group_volume("music", 0.7);
    let music = audio.play("assets/chime.wav", group: "music", looping: true, volume: 0.4, fade_in: 0.5);
    pause(music);
    resume(music);
    audio.volume(music, 0.2, duration: 0.5);
    let sound = audio.play("assets/chime.wav", pan: -0.5, rate: 0.8);
    audio.rate(sound, 1.2, duration: 0.15);
    await sound;
    audio.stop(music, fade: 0.5);
    await music;
}
```

`rate` 同时改变速度与音高：`2` 表示两倍速，音高提高一个八度。`audio.rate` 可在播放过程中渐变速率，暂停时渐变也暂停。位置始终以素材秒数表示。

`audio.queue` 在同一活动后追加片段，适合前奏、循环段和尾奏。存档保留当前素材、位置和待播队列。

```aside
let mut music = audio.play("assets/intro.ogg", group: "music");
audio.queue(music, "assets/loop.ogg", looping: true);
narrator("Continue to leave the loop after this pass.");
audio.queue(music, "assets/outro.ogg");
await music;
```

切换音乐时可读取当前内容位置，让新曲从对应位置淡入。`audio.at` 等待指定内容位置，适合歌词和字幕；视频用 `video(...).play()` 同步画面与声音。

```aside
let next = audio.play("assets/music-next.ogg", group: "music", looping: true,
    start: audio.position(music), loop_start: 4, loop_end: 64, fade_in: 2);
audio.stop(music, fade: 2);
music = next;
```

## 继续创作

用 [author_project](../../examples/author_project/src/main.aside) 查看多文件组织，用 [author_arcade](../../examples/author_arcade/src/main.aside) 和 [author_embedded_puzzle](../../examples/author_embedded_puzzle/src/main.aside) 查看剧情与玩法的组合。

用 `try/catch` 处理可恢复的错误；`throw;` 保留当前故障继续传播。源码运行和 Debug 构建提供文件与行列位置，Release 提供函数与指令位置。需要调用栈时使用 `std::core::error_info()`。

语言规则见[语言规范](../aside-language.md)，Rust 扩展见[宿主接口](../aside-host.md)。参数、默认值和边界行为随各接口的英文源码文档维护。
