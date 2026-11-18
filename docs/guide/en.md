# Reel Creator Guide

[简体中文](zh-CN.md) · [Documentation](../README.md)

Express scenes, dialogue, interaction and choreography with Aside functions, nodes and activities. A creative unit can contain both UI and story; keep its local state and behavior together, and let callers choose composition and flow.

Download the tool bundle, add the directory containing `reel`, `reel-lsp` and `reel-game` to `PATH`, and reopen your terminal. Install the Aside VS Code extension for completion, signature help, F12 and formatting on save. `aside.server.path` selects the language server and `aside.reel.path` selects the runtime tool.

Create a `MyGame` directory with `src/main.aside` as its entry and `resources/` for assets. Projects share an installed toolset and keep their own source, resources, local build products in `build/`, and distributable packages in `dist/`. Script asset paths are relative to `resources/`.

```sh
cd MyGame
reel check
reel dev
reel build
reel export --release
```

`check` compiles and locates errors; `dev` runs source. `build` produces `build/debug/game.asb`, or `build/release/game.asb` with `--release`. Use `reel run build/debug/game.asb` to run compiled code.

`dev` stores its compilation cache in `build/dev/` and reuses it while the tool and source contents remain unchanged. Editing source triggers recompilation. After editing assets, restart the game to load them.

Use `reel-dev debug` to open the source project and reproduce a stutter or script fault. Close the window to review the log and scene timings. [Development diagnostics](../../crates/reel-dev/README.md#inspect-a-development-session)

Source commands in an interactive terminal show the current phase, module progress and elapsed time. `dev` reports when it reuses cached code.

`export` produces a development project at `dist/TARGET/debug/`. Use `--release` for `dist/TARGET/release/`, such as `dist/vita/release/`. Distribute the whole release directory, including the code, text package, resource volumes and any bundled player. Default outputs update on repeated runs. An explicit `--output` destination uses `--overwrite` to replace existing output. Keep saves separately before replacing an export directory. `--save-directory` selects runtime storage.

Use `reel-dev perf verify` before publishing to check key scenes exercised by the distribution player. The policy specifies a target, environment, scene labels and submission interval or memory limits. The resulting `verification.json` records observations and failed checks. [Release verification](../../crates/reel-dev/README.md#verify-a-release)

Place platform assets in `resources/@TARGET/` to override common assets at matching paths. For example, `resources/@vita/bg/room.png` overrides `resources/bg/room.png`; scripts still reference `bg/room.png`. Windows, Linux and macOS select the exact OS overlay first, then `resources/@desktop/`, then common files. Target-only files are included. The resource target defaults to the host OS name.

```sh
reel dev --resource-target vita
reel export --release --resource-target vita
```

These commands preview and export the selected assets using the current platform player. Development exports copy selected files; release exports package them under their logical paths.

Use `build.target` to choose platform settings or effects. Set its value with `--resource-target` for `check`, `build`, `dev` and `export`. The compiler checks both branches and generates code for the selected branch and its nested functions.

```aside
let particle_count = if build.target == "vita" { 80 } else { 320 };
```

Keep the logical canvas when sharing a layout; use resource scaling and effect settings to adjust rendering cost. `window.info().platform` reports the player currently running the game.

To export with another platform's runtime, pass its directory with `--runtime`. Its executable, native modules and platform metadata keep their relative paths beside `game.asb`:

```sh
reel export --release --resource-target vita --runtime path/to/vita-runtime
```

Use the platform's packaging tool to turn that directory into an installable package. A Vita runtime consumes prepared Opus audio and block textures; hardware movies use H.264/AAC with even dimensions up to 960 × 544.

Vita exports compile effects to GXP with the official SDK host tool `psp2cgc`. Set `REEL_PSP2CGC` to the compiler executable or `SCE_PSP2_SDK_DIR` to the SDK directory. The runtime embeds its builtin programs and uses the system GXM API.

With the Reel launcher installed, export each game as data:

```sh
reel export --release --resource-target vita --data-only
```

Copy the exported directory to `ux0:/data/reel/GameName/`. The launcher lists directories containing `game.asb`; each game keeps its progress in its own `saves/` directory. Press SELECT + START during a game to return to the list.

In the launcher, Circle confirms and Cross goes back. Press SELECT to choose the language and theme, and START to refresh the library after copying a new game. Preferences are saved in `ux0:/data/reel/launcher.json`.

During desktop development, `reel dev` reads PNG, JPEG and static WebP directly. Add `resources.toml` at the project root to prepare assets for each release target:

```toml
[[targets.vita]]
match = "images/**/*.{png,jpg,jpeg,webp}"
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

The first matching rule applies; unmatched files retain their original content. Use `convert = "copy"` to preserve specific files. Selection resolves common assets and target overrides before conversion. Texture scaling preserves logical dimensions and `source_rect` coordinates. Scripts keep their existing image and audio paths. BC textures and tiles choose encoding quality automatically while retaining the selected format's GPU storage size.

Set `fallback = "source"` on a BC texture rule to keep the original image when the chosen encoding exceeds the display-error threshold against the resized image. A Vita export reports the asset if its original exceeds the device's decode budget.

`targets.desktop` supplies shared conversion rules for Windows, Linux and macOS. Rules for the exact OS match first, followed by the shared desktop rules. Use `--resource-target desktop` to preview the shared target on its own. These rules convert 8-bit PNG to lossless WebP and selected audio to Opus. Existing WebP and Opus assets pass through.

```toml
[[targets.desktop]]
match = "**/*.png"
convert = "webp"

[[targets.desktop]]
match = "**/*.{wav,ogg,mp3,flac}"
convert = "opus"
bitrate = 96000
```

For smooth gradients, place a texture rule with `format = "rgba8"` and `dither = "ordered"` before the general texture rule. Dithering distributes color quantization error and preserves alpha; RGBA8 uses eight times the texture memory of BC1. For a single file, run `reel prepare-texture image.png --output image.reel-texture --format rgba8 --dither ordered`.

To pack small images used together into an atlas, add this before the general texture rule:

```toml
[[targets.vita]]
match = "images/ui/**/*.{png,jpg,jpeg,webp}"
convert = "atlas"
page_size = [1024, 1024]
format = "auto"
```

Each rule produces one atlas page that loads and uploads as a whole; use separate rules for images needed at different times. Scripts keep the original image paths, logical dimensions and crop coordinates. The page stores one texture level, with images aligned to compression blocks and padded edges for filtering. If the images do not fit, preparation reports an error; increase `page_size` or split the group. The rule accepts `scale`, `format` and `dither`.

For long images or atlases, place a `convert = "tiles"` rule before the general texture rule. `tile_size = 510` leaves room for gutters in a 512-pixel texture; `first_level = 1` stores half density and smaller levels. The rule also accepts `format` and `power_of_two`. The original logical path and atlas crop coordinates remain usable, while loading follows the visible tiles.

```sh
reel prepare-resources --resource-target vita
reel dev --resource-target vita
reel export --release --resource-target vita
```

`prepare-resources` prepares assets and reports its results; `dev`, `run` and `export` also prepare configured resources. The cache lives under `build/resources/` and updates affected copies when assets, parameters, the corresponding converter implementation or its dependencies change. Offline conversion processes one resource or configured atlas group at a time; `prepare-resources --memory-bytes` selects its workspace limit.

Video conversion uses `ffmpeg` and `ffprobe` from PATH to produce H.264/AAC MP4, preserving the frame rate and fitting within `size`. Movies with color and alpha stacked vertically use `alpha_layout = "top_bottom"`. Video workspace is estimated from dimensions; codec subprocesses allocate their own memory.

Fonts use complete files by default. To subset a font, explicitly select `convert = "font"`, use `unicode = ["U+0020-024F", "U+3000-9FFF", "U+FF00-FFEF"]` for character ranges, and add story and translation text with `text = ["src/**/*.aside"]`. Extend ranges for the languages accepted by player input; characters outside the selected coverage will be missing. Font conversion requires Python with FontTools; `REEL_FONT_PYTHON` can select that interpreter. Font families, styles and every face in a collection are preserved.

`reel clean --resources` removes the conversion cache. The next preparation converts assets again. `reel clean` removes the entire `build/` directory and keeps packages in `dist/`.

Run `reel fmt` in the project root to format source or `reel fmt --check` to check formatting. The formatter uses four-space indentation, a 100-character target width and compact argument wrapping. English API documentation is generated from Rust declarations and appears in editor hover and definition navigation.

Complete projects below include `main`; other snippets belong inside the surrounding function or component and use its variables. Supply their fonts and assets before running. Shared repository assets live in `examples/assets/`; `--assets` selects an alternate resource root.

## 1 Create a project

Save this as `src/main.aside`, place a font at `resources/fonts/body.ttf`, and run `reel dev` in the project directory. `game.configure` sets the title, logical canvas and fonts; `stage.replace` mounts the scene.

```aside
fn main() {
    game.configure(title: "Window light", width: 960, height: 540,
        fonts: ["fonts/body.ttf"]);
    stage.replace(text("Hello", x: 24, y: 24));
}
```

As the work grows, split modules by chapter, menu or minigame. The entry configures the work, creates shared state and begins the flow. Project imports start at `src/`, so `ui::title` refers to `src/ui/title.aside`.

```aside
use ui::title::title;

fn main() {
    game.configure(title: "窗边", width: 1920, height: 1080, fonts: ["fonts/body.ttf"]);
    stage.replace(title());
}
```

`src/ui/title.aside` returns an authored title screen. Connect ordinary pages with function calls; use `game.start` to create a new session. The story function for `src/story/opening.aside` appears below.

```aside
use story::opening::opening;

pub fn title() {
    column(x: 760, y: 480, gap: 24) {
        text("窗上的光");
        button("新游戏", on_click: |event| game.start(opening));
    }
}
```

## 2 Organize content with data

Build content with arrays, tables and ordinary loops. `..` excludes the end, `..=` includes it, and integer ranges iterate lazily. `f"...{expression}..."` inserts expression values into text.

```aside
let values = [];
for value in 1..=3 { values.append(value); }
let latest = values.pop();
stage.replace(text(f"剩余 {values.len()} 项，最后一项 {latest}"));
```

Write a variable's name once when it matches the record field, as in `{width, height, fullscreen: false}`.

Use `progress?.volume ?? 0.8` to read the volume from an optional progress object and provide a default. The default is evaluated only when the left side is `null`, so an existing `0` is preserved. Inside `if progress != null`, the local variable `progress` keeps its concrete type.

Use `gen fn` and `yield` to calculate items on demand. Calling the function creates a generator; each item requested by `for` resumes its body. Leaving the loop early runs the generator's `finally` blocks. This example produces positions without an intermediate array.

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

Use `let (done, value) = items.next()` to advance manually. `done` distinguishes completion from a yielded `null`; completion delivers the final result once. `items.close()` ends the generator early. Generators produce data on demand; use ordinary functions for flows that wait for dialogue or animation.

`get` returns the default for a missing key and preserves stored `null`, `false` and `0`. The default is evaluated as an ordinary argument.

```aside
let settings = {volume: 0, wallpaper: null};
let volume = settings.get("volume", default: 0.7);
let wallpaper = settings.get("wallpaper", default: "default.png");
let language = settings.get("language", default: "zh-CN");
let removed = settings.remove("wallpaper");
settings.wallpaper = "new.png";
```

`sort` sorts an array in place and preserves the order of equal elements. Records can supply a comparator returning a negative, zero or positive integer. Comparators run synchronously; successful sorting updates the array.

```aside
let cuts = [180, 20, 90];
cuts.sort();
let captions = [{text: "后景", layer: 2}, {text: "前景", layer: 1}];
captions.sort(compare: |a, b| a.layer - b.layer);
```

Spreads combine arrays and records. Later fields override earlier fields with the same key. A spread copies the outer contents while nested objects remain shared.

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

Use tuples for fixed shapes. `let` and `for` support fixed-length destructuring, with `_` skipping a position. A type or length mismatch raises `PatternMismatch`.

Destructure objects by field name with `let {value: item, index} = row;`. A field without an alias binds its own name.

```aside
let settings = {language: "zh-CN", volume: 0.8};
for (key, value) in settings.entries() {
    let pair = (key, value);
    let (name, _) = pair;
}
```

`match` chooses the first matching arm whose condition holds; its final expression supplies the result. Here, the caller supplies `mode`.

```aside
let caption = match mode {
    "online" | "typing" => "在线",
    name is string if name != "" => name,
    _ => "离线",
};
```

Container methods express insertion, removal, slicing and reversal directly. Table `entries()` returns `(key, value)` pairs. See [collections](../../examples/collections/src/main.aside) and [collection_editing](../../examples/collection_editing/src/main.aside).

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

Keep binary data in `resources/` and load it on demand with `assets.read`. The result is immutable `Bytes`, with length and byte indexing.

```aside
let cells = await assets.read("data/cells.bin");
let count = cells.len();
let first = cells[0];
```

Use `std::math::to_int` to turn a continuous value into an integer index. It preserves integer input and truncates floating-point input toward zero.

```aside
use std::math::to_int;
let frame = to_int(1.25 * 20);
```

Use `std::number::format` to choose decimal places or significant digits in scientific notation for numeric text. Formatting produces text while the stored value retains its precision; the project chooses separators and display thresholds. See the [standard declaration](../../crates/aside-std/src/number.rs) for rounding and ranges.

```aside
use std::number::format;
let price = format(12.25, precision: 1);
let total = format(123456789, precision: 4, mode: "scientific");
```

## 3 Compose scenes and layout

Functions return nodes, and trailing child blocks compose them. Use conditions, loops and local variables inside a block; returning `null` omits a node at that position. Placement, backgrounds and interaction share the same stage tree.

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

`row`, `column` and `grid` use Flex/Grid layout. `grow` allocates remaining space and `gap` sets spacing; use `stack` for positioned scenes. The logical canvas scales uniformly to the window, with drawing and input sharing the coordinate transform.

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

Let content size the panel and use `stretch` for its background. Nine-slice `slice` preserves corners; `max_width` bounds text wrapping. Image paths are relative to `resources/`.

```aside
stack(max_width: 420) {
    image("images/panel.png", stretch: "both", slice: 12);
    column(padding: 24, gap: 16) {
        text("The frame follows wrapped text and buttons.", align_x: 0.5, text_align: "center");
        button("Dismiss", align_x: 0.5, on_click: |event| event.target.remove());
    }
}
```

A custom pointer is composed of ordinary nodes. Add `cursor` to its owning panel so it retires with the panel; animation, bindings and image properties work as usual.

```aside
let pointer = cursor {
    image("images/pointer.png", anchor_x: 0.5, anchor_y: 0.5);
};
panel.add(pointer);
```

## 4 Components, state and interaction

Return a node for a simple view. Use a class when a component needs ongoing operations or a defined data shape. Type annotations support checking, completion and navigation.

Reel types such as `Node`, `TextNode` and `Scope` are available in annotations.

Use `-> {node: Node, caption: TextNode}` when a view returns named parts. A return type such as `-> "back" | "title"` limits the values a menu can return.

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

Constructor arguments, property assignments and `.with(...)` configure the same node. Choose the form that reads clearly in context; extract repeated appearance into ordinary functions or records.

```aside
let caption = text("窗外下起了雨。")
    .with(font_size: 28, color: "#eeeeee")
    .with(x: 48, y: 420, max_width: 720);
caption.with(opacity: 0.5);
```

Await `stage.presented()` when choreography should start after the current scene is visible. The player retains the previous presented view while resources prepare.

The player uses layout motion to prepare fixed-size images entering the viewport. When the next background, pose or menu is already known, mount its hidden subtree with `preload: true` while the player reads the current line or watches the current animation. Descendants inherit the setting. The player uses spare capacity to read and decode their images and effects. If the route changes, remove the subtree to release its requests and resources.

```aside
let menu = stack(visible: false, preload: true) {
    image("assets/panels.png", width: 320, height: 180);
};
scene.add(button("Menu", on_click: |e| { menu.visible = true; }));
scene.add(menu);
```

```aside
stage.replace(scene);
await stage.presented(owner: scene);
await run(tween(scene, opacity: 1, duration: 0.5));
```

Use `appearance` to share a control's interaction paint. Background, text color, borders and transforms apply directly to the existing node; `focus_visible` marks keyboard and gamepad focus. Each state overrides its specified properties, with scope overrides taking precedence. Children and `derive` still compose image-based or other compound appearances.

```aside
let look = appearance(base: {background: "#304060", corner_radius: 6},
    hover: {color: "#ffd580"}, pressed: {background: "#203040"},
    focus_visible: {border_width: 2}, disabled: {opacity: 0.4});
column(gap: 12) {
    button("Hover me", width: 240, height: 48, decorated: false, appearance: look);
    button("Another component", width: 240, height: 48, decorated: false, appearance: look);
}
```

Within one parent, `move` changes drawing order while preserving node identity, callbacks and running animation.

```aside
let back = image("assets/panels.png");
let front = image("assets/sun.jpg", x: 120);
let scene = stack { back; front; };
stage.replace(scene);
scene.move(back, index: 1);
```

`add` mounts content; `remove` retires a node and its owned work. Local updates suit notices, menu entries and temporary controls. See [author_dynamic_stage](../../examples/author_dynamic_stage/src/main.aside).

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

Images participate in layout alongside text. After an image source changes, the engine prepares the new resource and updates the view when ready. Images at the same path share resources.

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

A parent translation, rotation, scale or opacity applies to the whole component. Negative scale mirrors it; `anchor_x` and `anchor_y` set the transform anchor.

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

`camera` maps a world region into a viewport. Keep a fixed HUD in a sibling outside the camera. Prepare large images as tiles so runtime loading follows the visible area and required pyramid level:

```sh
reel prepare-image original.png --output resources/panorama.reel-image
```

Display it with `image("panorama.reel-image")`. Keep the manifest and adjacent `.tiles` directory together in resources; build and export collect both. `--tile-size` defaults to 512; the tile plus two gutter pixels must fit the device's texture limit. Preparation decodes the original, while runtime memory follows the visible area. See [author_large_image](../../examples/author_large_image/src/main.aside) for a complete example.

For lower-resolution backgrounds or portraits, resize artwork and encode GPU-compressed textures while retaining the original layout dimensions and `source_rect` coordinates:

```sh
reel prepare-texture original.png --output resources/background.reel-texture --size 960x544
```

Display it with `image("background.reel-texture")` or use it for particles and image masks. The tool preserves aspect ratio and only reduces dimensions. Opaque images use BC1 by default; images with transparency use BC3. `--format rgba8` keeps uncompressed storage for detailed UI artwork and masks; `--power-of-two` pads edges for devices requiring power-of-two dimensions. `prepare-image` generates compressed texture tiles by default; `--tile-encoding png` selects PNG tiles. BC formats require device support and upload as compressed blocks at runtime. See [prepare_texture](../../crates/reel-cli/src/prepare_texture.rs) for the conversion API.

For standalone images often displayed at smaller sizes, add `--mipmaps` or set `mipmaps = true` in a texture conversion rule to generate the complete prefiltered chain. The default keeps one level. Extra levels increase package size and texture residency; linear filtering blends adjacent levels. Pre-packed sprite sheets converted as one texture generate levels for the whole sheet, so neighboring regions need sufficient gutters. Tiled images use their existing level pyramid.

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

A scene can return its root and the parts controlled by the story. Put this component in `src/ui/room.aside`, then let story functions control its portrait, CG and dialogue area.

```aside
use ui::dialogue::dialogue_view;

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

A reusable container accepts `children`, leaving content to its caller. The same container can hold dialogue, settings or a minigame.

```aside
fn card(title, children = []) {
    column(gap: 12, padding: 24) {
        text(title);
        ...children;
    }
}
```

Await `stage.geometry` to position an overlay relative to a presented node. The result includes bounds in the reference node coordinates; transparent nodes still participate in layout. For text, `text_caret` gives the final insertion caret's bounds; use `y + height / 2` to center a continuation marker. A hidden or unmounted target, or a noninvertible reference transform, reports `GeometryUnavailable`.

```aside
overlay.visible = true;
overlay.opacity = 0;
let geometry = await stage.geometry(thumbnail, relative_to: overlay, owner: root);
indicator.x = geometry.bounds.x + geometry.bounds.width / 2;
indicator.y = geometry.bounds.y;
overlay.opacity = 1;
```

## 5 Dialogue and story flow

Character calls wait for player advancement in sequence. Set `reveal_skippable: false` to preserve a line reveal rhythm. Ordinary functions can interleave UI, sounds and minigames with dialogue.

```aside
mio(rich("Listen.[pause=0.5]"), reveal_skippable: false);
narrator("The light is moving.", reveal_skippable: false);
say([mio.line("One."), eri.line("Two.")], reveal_skippable: false);
```

Each line can choose its history text. `history: false` omits the line; a string or rich text supplies separate history content.

```aside
mio("……", history: "I remember the rain.");
narrator("A fleeting thought.", history: false);
say([mio.line("Noise.", history: false), eri.line("Stay.", history: rich("[b]Stay.[/b]"))]);
```

`src/story/opening.aside` organizes the story with ordinary control flow. `choose` returns a stable branch key; call the corresponding chapter to build common and character routes.

```aside
use ui::room::room_view;

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

Put the dialogue view in `src/ui/dialogue.aside`. `game.dialogue()` supplies text, speaker names and reveal positions; the author controls layout, fonts, background and advance indicators. `ready` means the group text and inline pauses have completed.

```aside
pub fn dialogue_view(model) {
    column(gap: 12) {
        text(value: bind(model.speaker_name),
             visible: derive(|| model.speaker_name != ""));
        text(value: bind(model.text), reveal: bind(model.reveal));
    }
}
pub fn nvl_view(model) {
    scroll(height: 600, follow_end: true, overscroll: "bounce") {
        view(|| column(gap: 24) {
            for line in model.lines { dialogue_view(line); }
        });
    }
}
```

`scroll` clamps offsets to the content range by default. `overscroll: "bounce"` enables elastic edges for touch panning; scroll positions, scrollbars and virtual lists keep using bounded logical offsets. Native UI drives the rebound.

Use `scroll(snap_y: 48)` to align a fixed-height list to vertical row stops. The mouse wheel, touch release, and linked scrollbar snap to those stops, including the end of the content. Scripted `scroll_y` assignments retain precise offsets.

Place `scrollbar(viewport)` beside the same scroll viewport. Its stable `track` and `thumb` content slots accept backgrounds or images with `stretch: "both"`; omitting `thumb_size` makes the thumb follow the visible-content proportion. External buttons can call `scroll_by` or `scroll_to`. Track clicks, dragging, keyboard and controller input share the target scroll position. See the [scrollbar source API](../../crates/reel-engine/src/aside/stage/scrollbar.rs) for parameters and lifetime behavior.

```aside
fn main() {
    let viewport = scroll(width: 280, height: 120) {
        column() { for i in 0 .. 20 { text(f"Line {i}"); } }
    };
    let bar = scrollbar(viewport, width: 16, height: 120);
    bar.track.background = "#28303a";
    bar.thumb.background = "#9daabe";
    stage.replace(row(gap: 8) { viewport; bar; });
}
```

Use `say` to combine simultaneous character lines. Each line retains its speaker, styling and reveal state.

```aside
let eri = character("Eri");
say([mio.line("你听见了吗？"), eri.line("在窗外。")]);
```

`wait: false` returns a speech handle that can accept more text. Append text, play sound or await animation before the next advancement point. A new speech or choice retires the old handle.

```aside
let line = mio("Starting", wait: false);
await run(delay(0.5));
line.append(" ...", wait: false);
audio.play("assets/chime.wav");
line.append(" ready.");
line.append("\nWelcome back.");
```

## 6 Sequence, parallel work and time

`tween`, `delay` and composition functions describe actions; `run` starts them and `await` waits for completion. Use `sequence` or `.then` for ordering, and `parallel` or `.together` for parallel work.

```aside
let entrance = tween(portrait, opacity: 1, duration: 0.3);
let move = tween(portrait, x: 640, duration: 1);
await entrance.then(move).then(delay(0.5)).run();
```

Built-in easing covers common motion. Use keyframes for a custom curve and reuse it across actions.

```aside
use std::math::curve;

let acceleration = curve([
    {time: 0, value: 0, mode: "cubic", out_tangent: 0.5},
    {time: 1, value: 1, in_tangent: 1.5},
]);
await tween(portrait, x: 640, duration: 2, easing: acceleration).run();
```

For a coordinated start, create paused activities, prepare the scene and resume after its first presentation. Audio and animation retain their own playback states.

```aside
panel.opacity = 0;
let music = audio.play("assets/chime.wav", owner: panel, paused: true);
let entrance = tween(panel, opacity: 1, duration: 0.25).run(owner: panel, paused: true);
stage.replace(panel);
await stage.presented(owner: panel);
music.resume();
entrance.resume();
```

Drive frame selection from a `clock`. The scene owns its clock, so pause and retirement share the same lifetime. See [author_frame_animation](../../examples/author_frame_animation/src/main.aside).

```aside
let frames = ["assets/frame-a.png", "assets/frame-b.png"];
let count = frames.len();
let timer = clock(interval: 0.125, paused: true, owner: root);
root.add(image(source: derive(|| frames[timer.ticks % count]), width: 64, height: 64));
stage.replace(root);
await stage.presented(owner: root);
timer.resume();
```

Start and await a single action directly; its easing controls the motion rhythm.

```aside
await tween(portrait, x: 720, duration: 0.8, easing: "sine_out").run();
```

Parallel actions control the portrait and UI independently; the story continues after both complete.

```aside
let entrance = tween(view.portrait, x: 320, duration: 0.5)
    .together(tween(view.hud, opacity: 1, duration: 0.3));
await entrance.run();
```

An ordinary function returning an action makes choreography reusable. `from` supplies explicit starting properties.

```aside
fn fade_in(node, seconds = 0.5) {
    tween(node, from: {opacity: 0}, opacity: 1, duration: seconds)
}
await delay(0.5).then(fade_in(view.hud)).run();
```

Repeating actions suit breathing, blinking and waiting indicators. Keep the activity handle to pause, resume or stop it.

```aside
let breathing = tween(view.portrait, opacity: 0.92, duration: 1.4)
    .then(tween(view.portrait, opacity: 1.0, duration: 1.4))
    .repeat();
breathing.run(owner: view.root);
mio("听，雨又大了。");
```

`delay` leaves time in the story and composes with other actions. While waiting, the player schedules work at activity deadlines.

```aside
await run(delay(1.5));
await run(sequence(delay(0.5), tween(panel, opacity: 0, duration: 0.3)));
```

Curves can also be sampled directly, letting one time source control several properties. Resume the clock after scene resources are ready.

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

## 7 Temporary scenes and lifetimes

`reading` groups local dialogue, choices and control state for a story segment. The function returns a result, and its caller chooses the next page.

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

`scope` owns temporary nodes and activities and retires them when its body ends. Use it for fullscreen CGs, confirmations and temporary gameplay.

When a button or back action ends a page with `s.finish(result)`, call `s.wait();` at the end of the body to keep the page open.

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

`override` supplies temporary property values for a scope. When the scope ends, the node exposes its underlying values again.

```aside
scope(body: |s| {
    let effect = s.override(panel, {opacity: 0.4, width: null});
    await s.run(tween(effect, opacity: 1, duration: 0.3));
    let next = completion();
    s.add(panel, button("Continue", on_click: || next.resolve()));
    await next;
});
```

`offset` adds displacement for shaking or floating. The base animation continues, and the temporary displacement ends with the scope.

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

`finish(result)` returns a component result and ends its owned work. Await saving or exit animation before `finish`, or perform it after the scope returns.

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

Video is an ordinary stage node, and `play` returns an awaitable activity. A skip button can stop playback; the author composes its layout and hit area.

```aside
let movie = video("assets/logo.mp4", width: 1280, height: 720, fit: "contain");
let playing = movie.play();
stage.replace(button(width: 1280, height: 720, decorated: false,
    background: "#000000", on_click: || stop(playing)) {
    movie;
});
await playing;
```

For an asset with color in the top half and a grayscale alpha mask in the bottom half, select `alpha_layout: "top_bottom"`. Its natural height is half the encoded height; cropping uses pixels in the visible color half. The default `"native"` preserves ordinary image transparency and opaque video. Images and videos share this property.

```aside
let overlay = video("assets/overlay.mp4", alpha_layout: "top_bottom", width: 960, height: 512);
stage.replace(overlay);
await overlay.play();
```

Add temporary controls during a story. Keep their node reference and remove them when that interaction ends.

```aside
let hint = view.hud.add(button("查看照片",
    on_click: |event| photo_panel.open()
));
mio("桌上那张照片，你还记得吗？");
hint.remove();
```

## 8 Menus, input and settings

`input(on_back: close)` closes on Escape, right click or the controller's back button. Declare `actions.back` to customize these bindings.

Button callbacks can update state or await choreography. A callback belongs to its button node; retiring the button ends its owned tasks.

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

`popup` follows its trigger node and receives input as a modal layer by default. Hover tooltips set `modal: false` so the anchor keeps receiving pointer interaction while the hint is visible. `on_dismiss` handles outside clicks or the back action.

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

A modal menu limits the active interaction scope. Place it above the scene and provide a clear close action. This example runs as a complete project.

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

Named actions map keyboard, mouse and gamepad controls to one behavior. Update continuous motion from a clock while input callbacks update intent. A focused text editor handles its keyboard and IME events.

Use the read-only observable `source` and `layout` fields of `stage.input()` to select input hints. `source` identifies the last active input method. Gamepad `layout` is `xbox`, `playstation`, `nintendo` or `generic`. The `buttons` field provides display labels for `South`, `East`, `North`, `West`, `LeftShoulder`, `RightShoulder`, `LeftTrigger`, `RightTrigger`, `Select` and `Start`. Bindings use logical control names, and games can supply their own icons.

For pointer positions, use `input(on_pointer: |event| ...)` to read local `event.x`, `event.y`, and `event.phase`. Hover and drag movement produce `moved`; primary-button and touch gestures produce `pressed`, `released`, and `cancelled`. Custom drag controls use `event.dragging` to check whether the node owns an active drag.

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

An image button uses the same `button`, selecting images from interaction state. Use `decorated: false` when child nodes draw its complete appearance.

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

Use `on_interaction` for state changes such as hover sounds or previews.

```aside
let gallery = model({selected: null});
let card = button("Sketch", on_interaction: |event| {
    if event.hover || event.focus_visible { gallery.selected = "sketch"; }
});
```

Checkboxes and radio options share business fields through `bind`. Radio options in a group share one value; arrow keys move and select within the group.

```aside
let settings = model({sound: true, language: "zh-CN"});
column(gap: 12) {
    checkbox("Sound", value: bind(settings.sound));
    radio("zh-CN", "简体中文", value: bind(settings.language));
    radio("en", "English", value: bind(settings.language));
}
```

Share the selected value and a list of `{key, label, build}` records between `tab_bar` and `page_stack` to declare each page once. Pages are built on first use and retain their controls and edits when switched away. Customize tab content with the interaction feedback and `appearance` on `item.control`. In `manual` mode, arrows move focus and confirmation selects a page. The default `automatic` mode selects with focus.

```aside
fn main() {
    game.configure(fonts: ["fonts/body.ttf"]);
    let settings = model({page: "display", volume: 0.8});
    let items = [
        {key: "display", label: "Display", build: || text("Display settings")},
        {key: "sound", label: "Sound", build: || column(gap: 12) {
            text("Volume");
            slider(value: bind(settings.volume), min: 0, max: 1, width: 300);
        }}
    ];
    let pages = page_stack(value: bind(settings.page), width: 600, height: 180, items: items);
    let tabs = tab_bar(value: bind(settings.page), activation: "manual", items: items,
        build: |item| text(item.label,
            opacity: derive(|| if item.selected || item.control.focus_visible {1} else {0.5})));
    stage.replace(column(x: 24, y: 24, gap: 16) { tabs; pages; });
}
```

`pages.get(key)` retrieves a built page; `await pages.ensure(key)` builds and retrieves one in advance; `pages.evict(key)` releases an inactive page. Use scopes for each visit's previews, audio and tasks, with control retention and visit work following their respective lifetimes.

Set `on_select: |event| { await pages.select(event.value); }` on the tab bar to update the bound selection after the page is ready. Put `tooltip() { text("Open settings"); }` inside a control to show a hint on hover or visible focus.

Use `progress_bar(value: bind(state.loaded), max: 100, width: 240, height: 16)` to display progress. Set `background` for the track and `color` for the fill. Add a child to overlay a label or icon.

Settings pages can connect directly to reading state. Auto mode waits after text presentation and pauses for choices; skipping follows read and choice policies. Modal UI, pauses and focus loss use the reading state.

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

Read `window.info().capabilities` before changing settings, then await `window.configure` for the applied state. Client sizes and monitor resolutions use physical pixels. The canvas in `game.configure` uses logical coordinates.

Tweens, particles and transitions follow the display refresh rate by default, using 60 Hz when the host rate is unknown. Set their target update rate with `game.configure(frame_rate: 144)` and restore automatic selection with `frame_rate: 0`. To measure continuous animation throughput, use `frame_rate: "uncapped"` and disable window vsync separately. Idle pages still draw on demand. Read the current display rate from `window.info().refresh_rate`.

```aside
let mut display = window.info();
if display.capabilities.resize {
    display = await window.configure(width: 1280, height: 720);
}
if display.capabilities.fullscreen {
    display = await window.configure(fullscreen: true);
}
```

`stage.focus` transfers keyboard or controller focus after the frame containing current changes is accepted. Here Next becomes visible before awaiting the handoff. The returned `FocusOperation` extends `WindowOperation` and awaits to bool. That result is false when the target is hidden, disabled, retired or outside the modal scope, preserving the existing focus. `owner` defaults to the current scope; scope exit cancels an unfinished request.

```aside
let next = button("Next", visible: false);
stage.replace(column { button("Skip"); next; });
await stage.presented();
next.visible = true;
let focused = await stage.focus(next);
```

The default handoff displays navigation focus. Use `visibility: "preserve"` to keep the indicator state current when the handoff is applied and prepare the next confirmation target during pointer use. Direction or confirmation presses then display navigation focus.

```aside
let focused = await stage.focus(next, visibility: "preserve");
```

Use `button(focusable: false, focus_on_pointer: false)` for side actions that accept pointer hover and clicks while retaining focus on the current control. `focusable: false` excludes the button from directional navigation, Tab and `stage.focus`; explicit handoff returns false. The property supports bindings and updates with the current stage state.

Use `await stage.focus(null, owner: page)` to clear navigation focus and its indicator. The page must belong to the current input scope. Directional navigation or Tab selects a control again; confirmation is routed to the page.

`text_input` edits a single line and `text_area` edits multiple lines. Bind them to business fields so control writeback and other views share one state.

```aside
let profile = model({name: ""});
stage.replace(text_input(value: bind(profile.name), width: 280, height: 40,
    color: "#202020", background: "#ffffff",
    on_submit: |event| { profile.name = event.value; }));
```

Editors expose undo, redo, selection and replacement operations. Toolbar availability follows observable editor state.

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

`bind` connects existing properties and `unbind` detaches them. Use `derive` to combine several values. Connected controls share state directly.

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

## 9 Style and choreography

Appearance can be an ordinary record. A component spreads defaults and caller overrides during construction; order determines the final value of repeated properties.

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

Group related nodes under one parent to move, fade or scale them together.

```aside
let cast = stack() {
    image("assets/mio.png");
    dialogue_view(game.dialogue());
};
stage.replace(cast);
await run(tween(cast, x: 80, duration: 0.5));
```

Use a scope to combine repeating effects with finite choreography and end the repeat when the body completes.

```aside
scope(body: |s| {
    s.run(repeat(sequence(
        tween(view.hud, opacity: 0.4, duration: 0.5),
        tween(view.hud, opacity: 1, duration: 0.5)
    )));
    narrator("The light keeps pulsing while this line is shown.");
});
```

A transition retains the current presented view and builds its destination in `change`. It advances after destination resources are ready and returns when complete.

```aside
stage.transition(target: scene, duration: 0.6, change: || {
    scene.clear();
    scene.add(image("assets/room-evening.png"));
});
```

An image mask controls reveal through progress and softness, which ordinary animation can drive.

```aside
let reveal = stack(width: 1280, height: 720,
    mask: "assets/ink-mask.png", mask_progress: 0, mask_softness: 0.06) {
    image("assets/window-cg.png");
};
view.picture.add(reveal);
await run(tween(reveal, mask_progress: 1, duration: 1.2), owner: reveal);
reveal.mask = null;
```

Compose complex masks with `mask` operations. Supply changing parameters through node properties and reuse expressions across nodes.

```aside
let edge = mask.alpha("assets/mask-waves.png", x: mask.param("x"), repeat: "x");
let reveal = stack(width: 640, height: 360, mask: edge, mask_x: 0) {
    stack(width: 640, height: 360, background: "#2d8aab");
};
stage.replace(reveal);
await run(tween(reveal, mask_x: 1, duration: 3), owner: reveal);
```

`invert` inverts a subtree and composes with opacity, masks and animation.

```aside
let scene = stack(width: 800, height: 450, invert: 0) {
    text("A different view", color: "#e08020", font_size: 48);
};
stage.replace(scene);
await run(tween(scene, invert: 1, duration: 0.4), owner: scene);
```

`blend` controls a subtree against its background with `normal`, `add`, `subtract`, `multiply`, `screen`, `overlay`, `hard_light`, `color_dodge`, `lighten`, `difference`, `soft_light` or `linear_burn`. The subtree is composed before blending.

```aside
stage.replace(stack(width: 320, height: 180, background: "#204060") {
    stack(width: 320, height: 180, background: "#800000", blend: "add", opacity: 0.5);
});
```

Use `sampling: "nearest"` on an image to preserve pixel edges; the default `"linear"` smooths scaling. Switching filtering shares the same image resource. Atlas crops and tiled images retain their source windows.

Use `effect` to apply a drawing function to a node and its children. `Fragment.sample()` reads their composed color; return a color to replace it or a float to control coverage. Bind parameters to ordinary state so controls and animation can change the effect.

The node property `effect_resolution_scale` controls its effect output resolution and defaults to `1.0`. For example, `effect_resolution_scale: if build.target == "vita" { 0.5 } else { 1.0 }` halves both axes on Vita and bilinearly enlarges the result to the original region. The input subtree retains its resolution; keep text and buttons in separate nodes when reducing a background effect.

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

Pass images to a drawing function with `texture(path)`. This example reveals a scene according to a noise image: the float returned by `effect` supplies `mask` coverage, while animation updates the bound progress. Images use the project resource paths and platform overrides.

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

Continuous particles suit rain, snow or falling objects. One particle node shares the image, while an emission activity controls rate and lifetime.

```aside
let petals = particles("assets/petal.png", width: 1280, height: 20, seed: 42);
stage.replace(petals);
let falling = emit(petals, burst: 0, rate: 80, duration: 10,
    lifetime: 8, velocity_y: 90, velocity_x: 15, spread_x: 30,
    size: 18, size_spread: 6, spin_spread: 60);
await falling;
```

Burst particles can respond to clicks or story events. A fixed seed reproduces the same choreography.

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

Use `size_curve` for size changes over normalized age. Prewarming fills the first frame with particles of different ages; radial speed and damping control their motion.

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

Use the particle node's `size_scale` to resize all living and future sprites together. It multiplies each sprite's age-based size curve while preserving emission areas, positions and trajectories. Assign it directly, bind it, or animate it with `tween`.

```aside
await tween(glow, size_scale: 0, duration: 0.2).run();
```

`reveal_fade` controls progressive glyph opacity and `reveal_fade_step` sets the offset between graphemes. Text layout stays stable.

```aside
let caption = text("The light is moving.", reveal_fade: 0,
    reveal_fade_from: 0, reveal_fade_step: 20.0 / 255.0);
stage.replace(caption);
await run(tween(caption, reveal_fade: 3, duration: 3.0 * 255.0 / 1400.0), owner: caption);
caption.reveal_fade = null;
```

`text_wrap` controls automatic wrapping while preserving explicit line breaks. Use `text_fit: "shrink"` and `min_font_size` for constrained space.

```aside
stage.replace(text("A long authored line without automatic wrapping.\nA second explicit line.",
    width: 240, text_wrap: false, clip: true));
```

## 10 Choices and component results

Use `completion` to turn asynchronous interaction into an ordinary function result. Create a form, wait for confirmation and continue the story.

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

`result.pending` can drive button availability; repeated waits after completion receive the same result.

```aside
scope(owner: root, body: |s| {
    let result = completion();
    s.add(root, button("Confirm", enabled: bind(result.pending),
        on_click: || result.resolve(true)));
    await result
});
```

A choice view reads options from `game.choice()` and submits option identity. Display text can be translated while branch keys stay stable. Use `list` for long collections; see [author_list_gallery](../../examples/author_list_gallery/src/main.aside) for scrolling and virtualization.

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

Ordinary conditionals can produce component arguments. Tables hold data, and the condition selects which data to pass.

```aside
let flag = true;
let actions = if flag {
    {accept: [binding("Enter")]}
} else {
    {back: [binding("Escape")]}
};
```

## 11 History, saves and shared data

Dialogue presentation and history are managed separately. Clear current text at a chapter boundary and trim history according to the work.

```aside
game.dialogue().clear();
game.reading().clear_history();
narrator("A new chapter begins.");
```

`keep_last` retains recent history by actual line count, which suits long-running works.

```aside
game.reading().clear_history(keep_last: 37);
```

`saves` captures business state, story positions, the current dialogue and history. Declare persistent data with State and build the current interface in Root. `run_flow` enters the story, and `flow` names that part of the story.

```aside
use std::resume::{state, root, flow};

class GameState {
    let mut score: int = 0;
}
state(GameState, "game.state");
root(start_story, "game");

fn start_story(state: GameState = model(GameState())) {
    reading(|| {
        let dialogue = game.dialogue();
        stage.replace(stack() {
            text(value: derive(|| dialogue.text));
            button("Save", y: 80, on_click: || { await saves.save("slot1"); });
        });
        run_flow(chapter_one, state)
    })
}

fn chapter_one(state: GameState) {
    flow("chapter_one");
    narrator("The train has arrived.");
    state.score += 1;
}
```

A new game evaluates Root's ordinary default argument. Loading supplies the restored State to the current Root, rebuilds its interface and reconnects the saved point. Defaulted State fields may be added, functions moved and story suffixes changed while existing saves resolve stable keys against current code. Preserve a renamed field's original key through the `fields` argument of `state`. Keep published flow, point and Root keys stable.

The compiler generates resume positions for verified dialogue waits, choices and flow calls. Patches that preserve the structure of those waits and calls can reuse their positions. Use `point` to explicitly name key positions that must retain their identity after release.

To offer jumps from history, call `saves.track_history(true)` in Root. Use `saves.can_jump(entry.group_id)` to show a jump button and await `saves.jump(entry.group_id)` to restore that dialogue and its business state. The jump is available while the checkpoint remains in the recent history window.

Root loads current preferences and builds Reading and the interface. Story advancement, counters and branches belong in flows. Keep business data and native resources that must continue in State or live business bindings; the current Root creates view factories and callbacks.

When a flow needs a scene object, use `run_flow(chapter_one, state, context: scene)` with the ordinary two-argument entry `chapter_one(state, scene)`. Loading connects `scene` and its local aliases to the object built by the current Root. Name several services with `context: {scene: scene, selection: selection}`. Loading connects those objects by field name. Keep backgrounds, actor positions, and minigame progress in State so Root can build the interface and its wait results.

Call `saves.can_capture()` when opening a save menu to enable the button at a verified position. Obtain an optional thumbnail with `stage.capture` before supplying it to a save request. The save list provides data for an authored slot UI. Input that changes the session during capture fails the save with `CaptureBusy` and preserves the previous slot.

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

Read progress can be reset independently and is recorded again after normal advancement. History, preferences and saves retain their own lifetimes.

```aside
stage.replace(button("Reset read progress", on_click: |event| game.clear_read_progress()));
```

`model` holds observable business state; `let mut` controls whether a variable can be rebound. Default data is copied when creating the model, while later assigned objects keep shared identity.

```aside
let settings = model({volume: 0.8, music: true, palette: {accent: "#70d890"}});
let volume = slider(value: bind(settings.volume), min: 0, max: 1);
let music = toggle(value: bind(settings.music));
let indicator = text("Music", visible: bind(settings.music), color: bind(settings.palette.accent));
settings.palette.accent = "#ffd580";
```

Use `persistent` for global preferences and unlocks. Load and apply current preferences when Root starts. Stored data is independent of story checkpoints.

Call `window.configure` from ordinary startup or a settings menu on a presented view. The Root can read the current window through `window.info()` while preparing a load.

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

## 12 Extend your components

Extract repeated expressions into ordinary functions. Return nodes to compose UI, actions to compose choreography, or objects to expose component operations.

```aside
fn arrive(node, destination_x, seconds = 0.5) {
    parallel(
        tween(node, x: destination_x, duration: seconds),
        tween(node, opacity: 1, duration: seconds)
    )
}
```

## 13 Text and translation

The work supplies fonts; `font` selects a family and `font_weight` selects its weight. `letter_spacing` and `word_spacing` adjust grapheme and space spacing. Use `rich` for inline styles; see the [RichText interface](../../crates/reel-engine/src/aside/rich_text.rs) for markup.

```aside
stage.replace(column(gap: 12) {
    text("CHAPTER ONE", font_size: 32, letter_spacing: 2);
    text("made with love", font_size: 28, word_spacing: -2);
});
```

`vertical-rl` writes top to bottom with columns moving left; `vertical-lr` moves columns right. `height` bounds column length and `line_height` sets spacing relative to font size. Button labels support vertical text; editors use horizontal text.

```aside
let sidebar = text(rich("[ruby=ほし]星[/ruby]を見上げる。"),
    writing_mode: "vertical-rl", height: 690,
    font_size: 26, line_height: 40 / 26.0);
stage.replace(sidebar);
```

Keep source text and translations in one `tr` call. Checking, development and packaging generate language data from source. Use `derive` for UI that follows language changes; submitted dialogue and history retain their original text. Single-language works use ordinary strings.

```aside
localization.set_locale("zh-CN");
let data = model({name: "Mio"});
stage.replace(column(gap: 12) {
    text(derive(|| tr(f"你好，{data.name}。", {en: f"Hello, {data.name}."})));
    button("English", on_click: |event| localization.set_locale("en"));
});
```

Preload a language, then use a call-specific `locale` to display several languages together. Interpolations evaluate once in source-text order and translations reference those values. See [localization](../../examples/localization/src/main.aside).

```aside
localization.load("en");
column(gap: 6) {
    text(tr("你好。", {en: "Hello."}, locale: "zh-CN"), font_size: 28);
    text(tr("你好。", {en: "Hello."}, locale: "en"), font_size: 18);
}
```

## 14 Audio and video

`audio.play` returns a controllable, awaitable activity. Group volumes serve music, voices, effects and UI; `owner` binds playback to a scene lifetime. The host chooses loading appropriate to short sounds or long tracks.

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

`rate` changes speed and pitch together: `2` plays twice as fast and raises pitch by an octave. `audio.rate` can change it over active playback time; pausing also pauses the change. Positions remain source seconds.

`audio.queue` appends segments to one activity for introductions, loops and endings. Saves retain the current resource, position and queued segments.

```aside
let mut music = audio.play("assets/intro.ogg", group: "music");
audio.queue(music, "assets/loop.ogg", looping: true);
narrator("Continue to leave the loop after this pass.");
audio.queue(music, "assets/outro.ogg");
await music;
```

When changing music, read its content position and fade the next track in at a corresponding position. `audio.at` waits for a content position for lyrics or captions; `video(...).play()` synchronizes picture and sound.

```aside
let next = audio.play("assets/music-next.ogg", group: "music", looping: true,
    start: audio.position(music), loop_start: 4, loop_end: 64, fade_in: 2);
audio.stop(music, fade: 2);
music = next;
```

## Continue creating

See [author_project](../../examples/author_project/src/main.aside) for module organization, and [author_arcade](../../examples/author_arcade/src/main.aside) or [author_embedded_puzzle](../../examples/author_embedded_puzzle/src/main.aside) for story and gameplay composition.

Use `try/catch` for recoverable errors and `throw;` to propagate the current fault. Source execution and Debug builds provide file and line locations; Release provides function and instruction locations. Use `std::core::error_info()` when you need a call stack.

See the [language specification](../aside-language.md) for language rules and the [host interface](../aside-host.md) for Rust extensions. Parameters, defaults and boundary behavior are maintained in English source documentation beside each interface.
