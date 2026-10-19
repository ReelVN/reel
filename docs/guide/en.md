# Reel Game Creator Guide

For images exceeding a device texture dimension, prepare the original with `reel prepare-image panorama.png --output panorama.reel-image`, then use `image("panorama.reel-image")`. Distribute the manifest and its adjacent `.tiles` directory together. The player loads visible tiles and a budget-limited neighbor ring; panning and zooming reuse resident pixels. See the [format and preparation contract](../prepared-images.md). Run the [large-image example](../../examples/author_large_image.aside) with a wide original and the font setup below. Preparation is explicit; build does not copy or prepare images.

Design version 1.1 · Development baseline. Implementation status is stated in the guide.

[中文](zh-CN.md) · [文档目录 / Documentation index](../README.md)


Compilation and ASB loading do not impose default quotas on project size, aggregate records, modules or instruction counts. Format widths and available memory still apply; Rust embedders can set explicit Limits. Capacity diagnostics distinguish record counts from requested memory, so a longer story is not mistaken for invalid syntax.
## Reading This Guide
Reel aims to let you express the scenes and interactions you imagine in Aside, composing your own dialogue, UI, choreography and embedded 2D games. Multi-file projects, a shared stage, save/load and packaged exports are available; the full 2D authoring API, long-content costs and device coverage are still being developed. Start with the [basic scene](../../examples/author_stage.aside), [animation](../../examples/author_activities.aside), or [interactive buttons](../../examples/author_buttons.aside). The [capability table](../README.md#当前状态) states the actual boundaries, and [roadmap #94](https://github.com/jhq223/reel/issues/94) tracks implementation order.

This guide starts with the scene you want to create and explains how to use Aside to write stories, compose UI, choreograph scenes, and extend effects. Implemented features are ready to use; the remaining sections describe the target design and identify capabilities that are not yet connected. The Chinese and English editions share the same examples and interface names. Reel Engine Architecture Design describes the runtime; Aside VM and Language Design defines the language rules.
Authors work with three basic concepts: functions organize reusable content, node trees describe scenes, and activities represent changes over time. Ordinary functions, basic node composition and sequential stories now work together. Activities and advanced creator APIs retain their stated implementation status.

Currently runnable: text, image (PNG/JPEG), row, column, stack, stage.replace, trailing children blocks with if/for composition, updates to existing node properties, local subtree editing, finite animations with waiting and control, and narrator/choose waiting and resumption. Check/build and the player share the same registered Reel interfaces; dev runs source, and run executes .asb directly. See the complete [author-stage example](../../examples/author_stage.aside) and the commands in section 1.

Author button nodes and awaitable on_click callbacks now work; see the runnable example below. model, bind/unbind, derive, dynamic view and author dialogue/choice views are available; audio is connected to creator source; video and save/load are connected to the same stage and execution state. Script initialization and projects with multiple source modules are available; development and packaged release exports use the commands below. Each section identifies runnable examples and designs that are not yet implemented. See the [current implementation status](../README.md#当前状态) for the broader boundary.

Historical verification scope: the R01/R02 Rust renderer and diagnostic window passed independent checks on software Vulkan and Xvfb/X11. Those records establish behavior only for their recorded commits and environments. Source and ASB now connect to the real window entry point; the old diagnostic program's scope no longer describes the author commands. These checks do not establish physical-GPU, hot-plug or platform-lifecycle behavior. See [rendering verification scope](../../crates/reel-render/R02-REPORT.md).

## 1 A Minimal Project and Shared Conventions
`game.exit()` requests that the work close. Scripts stop once the request is accepted; the player cleans up tasks and resources before closing its window. You can await a fade first. Embedded hosts receive the same request and control their outer application. See [author_exit.aside](../../examples/author_exit.aside); provide fonts/body.ttf before running.

Start the project from main in main.aside. check/build/dev accept a source file or project directory; run executes the built .asb. See the runnable multi-file [author_project](../../examples/author_project/main.aside). The title screen can use game.start to enter a fresh story session.
Call game.configure(title: "窗边", width: 960, height: 540) in main to set the title and logical canvas using ordinary expressions. Omitted fields keep their values. Without a canvas, coordinates follow the viewport; a configured canvas uses contain scaling and the same transform for drawing and input. When first setting one dimension, the other uses the corresponding value from 1920×1080. The desktop window initially matches the first configured canvas, or uses 1280×720 without a canvas. Rust hosts can explicitly override the initial size with StoryWindowOptions.logical_size. After the first canvas is applied, window size stays under player control and continues to use contain scaling. fonts accepts a font file string or an ordered array, such as game.configure(fonts: ["fonts/body.ttf", "fonts/cjk.ttf"]). Paths are relative to the asset root; provide at least one font file before the first presentation. The font property on text and button selects a family or comma-separated fallback list, for example text("标题", font: "Noto Serif SC, system-ui"); font_size controls size. Font files share loaded data; system-ui denotes the configured default list. The player uses only fonts supplied by the work and reports missing font configuration.
Reel makes all currently registered basic nodes and story services available automatically: text, image, button, row, column, stack, stage, narrator and choose. Each source file can use them without an import. Your module declarations, local bindings and explicit imports take precedence over these defaults. If you define your own text function, `use reel::prelude::text as reel_text;` gives the Reel constructor a separate name. Explicit prelude imports remain ordinary valid imports. This default environment belongs to Reel; standalone Aside has no Reel names. Actions and complete application services retain the design status stated later. Authors build their pages and visual style with ordinary functions. All documents use fn, let, let mut, |parameters| closures and use imports. Except for complete module/function examples, standalone snippets belong inside a running function and reuse earlier variables. Ordinary statements end with semicolons; a function's final expression can be its return value. Object literals use {field: value}. The following imports use the implemented module layout; the title button's game.start call is implemented.
```aside
use game::ui::title::title;

fn main() {
    game.configure(title: "窗边", width: 1920, height: 1080, fonts: ["fonts/body.ttf"]);
    stage.replace(title());
}
```
The ui/title.aside module used above can be built directly from the author's own nodes:
```aside
use game::story::opening::opening;

pub fn title() {
    column(x: 760, y: 480, gap: 24) {
        text("窗上的光");
        button("新游戏", on_click: |event| game.start(opening));
    }
}
```
See section 3 for opening in story/opening.aside. The examples' relative file locations match their import paths; the root module main.aside handles startup only.
let fixes a variable's binding while leaving node properties mutable. Use let mut when the variable must be rebound to another value. Ordinary expressions are evaluated once when executed. bind(model.value) connects an observable property; derive(|| expression) connects a read-only value computed from other properties. See the second document for the complete language rules.

The desktop player rebuilds a lost graphics device while retaining the current story state. Diagnostics include the backend's device-loss reason. If rebuilding fails, the player reports the error and exits.

### Available Check, Build and Run Commands
The desktop player waits for the first mounted scene's fonts and images before presenting it. Platforms that support hidden windows show the window after the first frame is accepted. To provide a loading screen, mount ordinary stage nodes first, then start subsequent tasks. Startup errors retain their source locations.

The examples configure fonts/body.ttf in main. Run these Windows commands from the repository root to copy Microsoft YaHei into a temporary asset directory for local verification. Supply fonts with distribution rights when publishing your work. Close the window opened by dev or run before continuing.

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

Check compiles the source and reports errors without running the story. Add `--format json` for diagnostics with UTF-8 ranges and line/column positions. Build produces one .asb containing bytecode, module initialization and the selected entry. Both commands provide the same default Reel names, registered native functions and host default arguments as the player.

Dev and build select main::main by default; `--entry main::name` selects another function. A missing or invalid entry is reported during the build. The entry may be private but cannot require parameters or capture an enclosing environment. Run loads the package initializer and entry directly without recompiling source. Existing output is replaced only with --overwrite.

Build defaults to Debug, retaining complete source and mappings so runtime errors can point to the original .aside file. Add `--release` for publication: the ASB omits source, instruction mappings and declaration locations, reducing its size and resident debug data. Both modes use the same language behavior and runtime checks. Release still reports the error kind, function and instruction position, and retains runtime symbols and text constants. This option is separate from `cargo build --release`, which compiles the Rust player; it does not move story text into an external file. Execution saves belong to the actual ASB, so Debug and Release builds of the same source do not exchange saves.

```powershell
./target/debug/reel.exe build examples/author_stage.aside --release --output target/reel-artifacts/author-stage-release.asb --overwrite
```

With window enabled, source and built packages use the real window, text layout and presented-input path. Synchronous component construction continues within one execution time slice, updating the scene when it waits or the slice ends, without displaying a separate frame for every node. When waiting for dialogue or a choice, the player completes the current GC cycle and then sleeps until input arrives. Natural completion keeps the final scene in the window. Close the window to exit and finish cleanup; Escape sends Back to the current UI.

Projects read only modules imported by the entry. game::ui::scene resolves to ui/scene.aside beside the entry file; --module first=../library supplies an external source root. Aliases for the same directory share one environment. Function cycles can link; top-level reads of uninitialized bindings report ModuleInitializationCycle at the read. build stores the complete graph and entry in one movable .asb. The same main executes its initialization settings in source and ASB runs. Assets default to the source or ASB directory and can be selected with --assets; export additionally creates resource packages and a relocatable release.json manifest.

Source files, embedded source text and string constants share the actual package budget. Long text and concatenation advance in execution slices; slicing never truncates text or imposes a dialogue length limit.

Left-associated operations and member or call chains compile in source order without consuming nesting allowance for each link. Actual nesting through parentheses, blocks and subexpressions uses the host's configured compiler resource budget.

### Exporting Development and Release Directories

`reel export` builds code and organizes its resources together. It exports a development project by default; `--release` creates a release directory. Prepare the font as shown above, then run:

```powershell
./target/debug/reel.exe export examples/author_stage.aside --output target/reel-artifacts/development --resource assets=examples/assets --resource fonts=target/reel-artifacts/author-assets/fonts
./target/debug/reel.exe export examples/author_stage.aside --release --output target/reel-artifacts/release --resource assets=examples/assets --resource fonts=target/reel-artifacts/author-assets/fonts
./target/debug/reel.exe run target/reel-artifacts/release/release.json
./target/debug/reel.exe dev target/reel-artifacts/development/release.json
```

Development output contains editable `script/` files, loose resources, `game.asb` with source diagnostics, and `release.json`. Use run directly or dev to recompile the copied sources. Release output contains `game.asb`, `game.rtp`, `resources.rpk`, and `release.json`; distribute these with the player for the target platform. Moving the complete directory does not require paths from the build machine.

Each `--resource prefix=directory` maps a directory to its logical script path. Use `--text locales=build/locales` to put bundled translation catalogs in the text package; scripts still call `localization.load("en", "locales/en.ftl")`. Development copies these files; release packages them. Translators can maintain source catalogs beside their story and UI modules and use `reel loc bundle` to produce runtime catalogs first.

Resource packages split automatically at a target of 2 GiB including headers and indexes. Add `--pack-bytes 1073741824` to the release command for 1 GiB volumes. Splits occur between complete resources; an oversized resource occupies its own volume. Additional files are named `resources-002.rpk` and so on, and the manifest lists them automatically without changing script paths. Mounting reads indexes only; payloads are read by range.

Release export moves original string constants into `game.rtp` and excludes complete source from the ASB. The pool currently remains resident, with strings sharing its bytes when used; startup does not copy every passage into permanent VM objects. Translations can change without recompiling code, while the original text pool must match its ASB. Images, audio and video use indexed resource reads without extracting the entire archive.

Release disables loose-file overrides by default. Explicitly enable a mod or development overlay with `reel run release.json --assets overrides`. Later packages in the manifest override earlier ones, and loose files take precedence. `--overwrite` replaces the whole previous export; saves and additional files are not copied into it. Store runtime data separately. Output must be outside the input source and resource directories.

### Available Ranges, Collections and Interpolation
Inside a function, use ranges to build content and interpolation to display results. Ranges do not create an array first; `..` excludes the endpoint and `..=` includes it. Arrays support `append`, `pop` and `len`; tables support `has_own`, `get` and `len`. Interpolation currently accepts strings, numbers, booleans and null.
```aside
let values = [];
for value in 1..=3 { values.append(value); }
let latest = values.pop();
text(f"剩余 {values.len()} 项，最后一项 {latest}");
```

`table.get(key, default: null)` uses the default only for a missing key. Existing null, false and zero values are preserved, and no field is inserted. Returned objects retain their identity. The default is an ordinary argument and is evaluated normally. Use it in derive as well: later field insertion or modification updates the result. See [collections.aside](../../examples/collections.aside) for a complete example.

```aside
let settings = {volume: 0, wallpaper: null};
let volume = settings.get("volume", default: 0.7);
let wallpaper = settings.get("wallpaper", default: "default.png");
let language = settings.get("language", default: "zh-CN");
```

`array.sort(compare: null)` stably sorts in place and returns the same array. The default orders numbers (including mixed int/float values) or strings; use a comparator returning a negative, zero or positive int for records. Equal items keep their original order. Comparators cannot wait. Sorting advances within the VM execution budget and commits only after success; an exception, cancellation or modification of the source array leaves the working copy unpublished. See [collections.aside](../../examples/collections.aside).

```aside
let cuts = [180, 20, 90];
cuts.sort();
let captions = [{text: "后景", layer: 2}, {text: "前景", layer: 1}];
captions.sort(compare: |a, b| a.layer - b.layer);
```

Array spreading `[...items, last]` accepts arrays or tuples. Table spreading `{...base, color: value}` copies own fields, with later entries taking precedence. Each spread captures in source order: later changes to the source do not alter captured elements or fields, while inner objects remain shared references. Spreading null is an error. See the runnable [collections.aside](../../examples/collections.aside) example.

```aside
let base = {message: "窗上的光", x: 32, y: 48};
let opening = [base];
let captions = [...opening, {...base, message: "第二句", y: 90}];
for caption in captions {
    text(caption.message, x: caption.x, y: caption.y);
}
```

## 2 Describing the Whole Scene in Code
### Organizing Component Data

The story VM has no fixed default memory byte quota. Object and root storage grows on demand, without the former default thresholds of 16,384 objects or 1,024 roots; embedding hosts can set an explicit budget for their environment. Incremental GC releases empty object storage pages instead of retaining the peak number of full object slots; small identity indexes and work queues remain available for reuse. Long operations yield across execution slices to keep presentation responsive.

Fixed-length tuples can be destructured in let and for, including nested tuples and `_`. All values are prepared before names are bound; a shape mismatch raises PatternMismatch. Closures retain each iteration's values. Array/object/rest patterns and destructuring assignment remain pending. See [tuple_bindings.aside](../../examples/tuple_bindings.aside).

```aside
let settings = {language: "zh-CN", volume: 0.8};
for (key, value) in settings.entries() {
    let pair = (key, value);
    let (name, _) = pair;
}
```

Use `match` to select dialogue, UI or behavior by value. It supports literals, names, `_`, OR and type tests, with guards after `if`. Only the first matching arm with a true guard runs; its final expression supplies the result. No match raises MatchError. Structural destructuring is not implemented yet. See [match_values.aside](../../examples/match_values.aside) for a runnable example.

```aside
let caption = match mode {
    "online" | "typing" => "在线",
    name is string if name != "" => name,
    _ => "离线",
};
```

Arrays support insertion, removal, resizing, reversal, extension, clearing and swapping. Both tuples and arrays support range slices. Table keys/values/entries return new arrays of own fields; each entry is a `(key, value)` tuple. Bulk edits publish once on success and preserve the original array identity. New positions filled by resize share the supplied value; they do not deep-copy it.
See [collection_editing.aside](../../examples/collection_editing.aside) for a complete runnable example. insert/remove use nonnegative zero-based indices; slice accepts `begin..end` or `begin..=last` and rejects out-of-bounds ranges instead of truncating them. remove returns the removed item, top returns the last item, and other mutating methods return the original container.

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

Use byte_len for the UTF-8 byte length and scalar_count for the Unicode scalar count. slice_bytes and slice_scalars take explicit ranges and reject out-of-bounds endpoints or cuts inside a UTF-8 scalar. Combining sequences and emoji may contain multiple scalars; scalar slices are not grapheme slices. starts_with/ends_with compare exact prefixes/suffixes and always match an empty string. `separator.join(items)` joins an array or tuple of strings; an empty collection produces an empty string and other types are not converted automatically. Long text advances within execution budgets; changing the source array during a join is an error. See [text_operations.aside](../../examples/text_operations.aside) for a complete example.

`text.split(separator)` splits on the complete separator string and returns an array of strings, retaining leading, trailing and consecutive empty parts. An empty separator splits at Unicode scalar boundaries and retains leading/trailing empty parts. For example, `"/角色//纱世里/".split("/")` returns `["", "角色", "", "纱世里", ""]`. Scanning and fragment copying yield within execution budgets without first copying the entire input.

to_ascii_lowercase()/to_ascii_uppercase() convert ASCII letters and preserve all other text. For example, "Emily你好".to_ascii_lowercase() returns "emily你好"; use these methods for ASCII identifiers or account-name comparisons.

`trim()/trim_start()/trim_end()` remove Unicode whitespace from both ends, the beginning or the end, preserving spaces inside names or text. For example, `let player_name = "\u{3000} 空 山 \u{a0}".trim();` returns `"空 山"`; whitespace-only input becomes an empty string. See [text_operations.aside](../../examples/text_operations.aside).

Registered functions can be stored and passed as values, for example `let say = narrator; say("你好");`; waiting behaves exactly as in a direct call. A function default can also be a constructor such as `text`. Use `(int,int)` for a coordinate pair and `(int,int)|null` for an optional pair. `is`, parameters, returns and variable assignments check these contracts; a failed assignment preserves the previous value. See the runnable [function_values.aside](../../examples/function_values.aside) example and use its path with the earlier `dev` or `build` commands.

Component calls with a trailing children block may omit the ending semicolon when used as standalone statements. A newline after the block ends that statement, so a parenthesized expression on the next line does not continue the previous call. Ordinary calls and property assignments still use semicolons. An explicit semicolon does not change child collection.

Implemented: text, image, row, column, stack and stage.replace use one shared node model. Ordinary functions can return nodes, composed through trailing children blocks, if and for over arrays/tuples. Properties such as value, x/y, width/height, color/background, opacity, visible and clip update the original node while its mounted identity stays unchanged. Replacing the stage retires the old tree. Retained old nodes cannot be read or written afterward, and do not keep the entire old tree alive. See the complete [author-stage example](../../examples/author_stage.aside).

Stack layers children from back to front in source order; row arranges them horizontally and column vertically. Omitted dimensions use content layout. x/y offset the layout position, with the logical origin at the top left, x increasing rightward and y downward. Defaults are font size 24, white text, a transparent background and zero gap/padding. Clip limits the visible area of a container. `scroll(height: 360) { ... }` creates a vertical scrolling region; axis can be "horizontal" or "both". Wheel input, touch dragging and keyboard/gamepad focus use the same input routing. Scrolling preserves text layout. With `follow_end: true`, an NVL page already at the end follows new content; scrolling back preserves the reading position.

Use the common `stretch: "both"` property to fit a background image to a container sized by its content, without counting text lines or writing measured heights back into script state. `"x"` and `"y"` fill only that axis inside the parent's padding; the default is `"none"`. A stretched axis does not contribute to the parent's measurement and overrides width/height on that axis; x/y and anchors still apply. Stretching along a row/column's flow axis makes the child an overlay without consuming flow space or gaps. In scroll containers, stretching uses the viewport's inner box and does not increase that axis's scroll extent.

```aside
stack(max_width: 420) {
    image("images/panel.png", stretch: "both", slice: 12);
    column(padding: 24, gap: 16) {
        text("The frame follows wrapped text and buttons.", align_x: 0.5, text_align: "center");
        button("Continue", align_x: 0.5, on_click: |event| game.advance());
    }
}
```

Complete example: [author_stretch.aside](../../examples/author_stretch.aside). Supply `fonts/body.ttf` and `images/panel.png`, then run `reel dev examples/author_stretch.aside`. The button changes the content while the background retains its identity and follows the new height.

`max_width/max_height` cap a node’s natural or explicit size and default to null. Long text wraps within the available width; short text keeps its content width. Caps include padding; stretch still fills the parent’s inner box on its axes. `align_x/align_y` default to 0 and range from 0 to 1: 0 starts, 0.5 centers, and 1 ends within the parent’s remaining inner space. Both axes apply in a stack, only the cross axis in rows/columns, and only non-scrolling axes in a scroll. They preserve x/y and anchor transform semantics and support bindings and animation. [author_stretch.aside](../../examples/author_stretch.aside) shows a dialog whose dimensions follow its content while its background stretches and its button stays centered.

A view lays out its content as a stack, so both align_x and align_y apply. An input may be nested inside ordinary containers; without focus, the frontmost visible, enabled inner input receives keyboard and gamepad events. Modal panels and focused editors retain priority. Updating game.configure(text_speed:) does not interrupt an active drag. When the window, canvas or parent enlarges text, WGPU generates and caches higher-resolution glyphs for the display scale without changing logical layout.

### Layered images

An image's `color` multiplies its pixels: `image("assets/panels.png", color: "#ffaaaa")` gives the image a red tint; white preserves it. Both the color's alpha and opacity affect transparency. Ordinary images, tiled large images and individual layer descriptions share this behavior; a container's color does not affect its children. Use bind/derive for hover and other state colors without preparing or uploading separate images.

`image("assets/pattern.png", width: 1280, height: 720, repeat: "both", tile_scale: 1)` repeats a pattern at its original pixel size. repeat accepts none, x, y or both; repeating ignores fit and cannot be combined with slice or a prepared `.reel-image`. tile_scale must be positive. `corner_colors: ["#ffffff", "#ffffff", "#ffaaaa", "#ffaaaa"]` specifies top-left, top-right, bottom-right and bottom-left colors, interpolated bilinearly across the node's content box and multiplied by color. It works with ordinary, nine-slice and prepared images without copying textures.

Complete example: [author_image_patterns.aside](../../examples/author_image_patterns.aside). Provide `fonts/body.ttf` and `images/pattern.png`, then run `reel dev examples/author_image_patterns.aside`; changing the tile scale reuses the same image.

Images do not receive pointer input themselves, so track, thumb and other control skins do not block the underlying control. Place an image inside a button or input to make it clickable or draggable; the container handles mouse and touch input.

A Rust host can use `ImageSource::empty()` before a picture arrives, then update the same image node with `ImageSource::resource(lease)`. Direct resources retain ordinary image fit, clipping, transforms, color and opacity; same-sized updates repaint without layout. `ResourceRegistry::register_yuv420` accepts producer-owned 8-bit YUV420 planes with BT.601/709, range and transfer metadata, without copying them into RGBA. After `release(handle)` relinquishes registry ownership, captured pictures still retain their leases; the host owns decoding and the playback clock. Run the CPU contract check with `cargo test -p reel-player --lib decoded_frames_preserve`.

`layered_image` combines image descriptions into one ordinary stage node. The function runs when its model dependencies change. Array positions are stable slots, ordered from back to front; a null slot hides its previous image, and shortening the array retires trailing slots. Each description accepts image properties, including source, geometry, fit, color and opacity. Omitted properties reset to image defaults. bind/derive values in descriptions are read dependencies. Unchanged properties retain their values and image nodes are reused, so changing an expression does not rebuild the body.

For example, `layered_image(|| [body, faces[state.face]], width: 320, height: 180)` selects a foreground description using state.face while sharing the body description. Assign state.face to change the expression.

Animate and position the returned node as a group. The selected layers and dependencies use ordinary execution snapshots. Pose names and costume/expression combinations remain game data; there is no fixed number of layers or poses. No extra raster composition runs each frame. See [author_layered_image.aside](../../examples/author_layered_image.aside); provide fonts/body.ttf as in the other image examples.

### Random choices

`let rng = random();` creates an independent random stream. `rng.int(0, 10)` returns an integer from 0 through 9, `rng.float()` returns a number in [0, 1), and `rng.pick(items)` returns an original element from a nonempty array or tuple. `random(seed: 42)` reproduces the same call sequence; a saved `let pick = rng.pick` keeps using that stream. Draw in an event or story function and write the result to a model, rather than drawing inside derive. Failed draws do not advance the state. See [author_random.aside](../../examples/author_random.aside).

After `use std::math::{sin, cos};`, use `cos(angle) * speed` and `sin(angle) * speed` for a two-dimensional velocity; angle is in radians. The module also provides tan/asin/acos/atan, atan2(y,x), sqrt/hypot(x,y), pow(base,exponent), exp/ln/log2/log10, abs/floor/ceil/round, and radians/degrees. These pure float64 operations work in derive without allocating script objects. round breaks midpoint ties away from zero; invalid mathematical domains produce NaN and overflow may produce Infinity. See [author_math.aside](../../examples/author_math.aside).

`random` comes from the independent Aside standard library and is imported automatically by Reel; `use std::random;` is also supported. Create streams in main or an event handler; module initialization and derive are effect-restricted. Seeded sequences are reproducible within the same library version.

### Organizing data and behavior with classes

Use `class` to group character or word data with named fields, types and methods. A `let` field is initialized once; `let mut` permits writes. Field initializers run separately for each instance. Constructors use `fn init(self, ...)`, including named arguments and defaults. Parameter annotations can name local or imported classes; derived instances satisfy base-class annotations.

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

`caption()` retains its `word` receiver and can be passed as a callback. Write `class Child: Parent` for inheritance and use `super.method()` for the lexical base implementation. A derived class inherits `init` unless it declares its own; that constructor can explicitly call `super.init(...)` once. `static let` and `static fn` belong to the class. Construction must initialize every field; failure or cancellation makes escaped instances and bound methods report ConstructionFailed. Field layouts are fixed; derived classes extend behavior through methods. See [author_classes.aside](../../examples/author_classes.aside) for a runnable example.

`let state = model(MyState());` copies an already constructed instance's data, preserving its type, methods and internal shared references without rerunning construction. Connect controls with `bind(state.field)` and compose display values with `derive(|| state.caption())`; method-driven field changes update the corresponding views. Read-only fields provide one-way reads, while writable fields retain their type contracts on control writeback. Instances assigned later keep their shared identity. Closures and resources remain invalid as copied defaults, and static fields do not belong to instance state. See [author_class_models.aside](../../examples/author_class_models.aside).

### Editing components while running

To start an animation from a ready frame, mount the nodes, then `await stage.presented(owner: node)` before starting it. The wait includes derived layout and image/font preparation and settles only after actual frame acceptance; other tasks, music and animations continue. The returned object supports repeated waits and `.cancel()`; retiring its owner also cancels it. Omitted ownership follows the current callback/task node, or the session in the main story. See [author_scene_entry.aside](../../examples/author_scene_entry.aside):

```aside
stage.replace(scene);
await stage.presented(owner: scene);
await run(tween(scene, opacity: 1, duration: 0.5));
```

Inside a children block, use a local variable to create a control, add visuals that read its state, then explicitly provide the control itself. Descendants returned by add already belong to that control and are not mounted again by the outer container. Explicit children arrays follow the same rule. Do not provide the same node twice or borrow children from an omitted component or an already mounted stage.

Without ordinary arguments, write `column { ... }` and omit the empty `()`. Ordinary function components support the same shorthand.

```aside
column(gap: 12) {
    let control = button(width: 240, height: 48, decorated: false);
    control.add(text("Hover me", color: derive(|| if control.hover { "#ffd580" } else { "#ffffff" })));
    control;
    text("Another component");
}
```

Node identity, parentage and callback ownership are preserved. See [author_composition.aside](../../examples/author_composition.aside).

A scroll region exposes writable, bindable `scroll_x/scroll_y`; layout clamps them to the actual content range. Read-only observable `viewport_width/viewport_height/content_width/content_height` can drive custom thumb geometry through derive. `content_align: "end"` places short chat content at the bottom, `"center"` centers it, and the default is `"start"`. `input(on_pointer: ...)` receives `{target,x,y,phase}` in the presented node's local coordinates, with pressed/moved/released/cancelled phases. A primary mouse press or touch captures the drag; focus loss, hiding and disabling cancel it. The callback may wait and belongs to the input node. Consecutive moved events for the same node and handler coalesce to the latest coordinates until execution starts, avoiding an input backlog. Arguments stay stable after execution starts; press, release and cancellation events remain separate. See the [custom scrollbar](../../examples/author_scrollbar.aside).

`container.add(child)` appends a subtree to a row, column or stack and returns the original child. `index:` selects a zero-based insertion position, including the current child count; omitting it or passing null appends. The subtree must have no parent and must not be mounted as the stage root. Duplicate ownership, cycles, adding to text/image or an out-of-range position produce an error and preserve the original tree. Adding and removing preserve other nodes and their widget identities; unchanged text is not reshaped.

`container.move(child, index: ...)` reorders an existing direct child without removing it. The required index is its final zero-based position; for three children it is 0, 1 or 2. The method returns the same child, and moving it to its current position does nothing. Its properties, running animations, callbacks, focus and loaded resources stay attached. A stack uses the new order from back to front; rows and columns use it for layout. A different parent or an out-of-range index is an error and leaves the tree unchanged.

```aside
let back = image("assets/panels.png");
let front = image("assets/sun.jpg", x: 120);
let scene = stack { back; front; };
stage.replace(scene);
scene.move(back, index: 1);
```

See [author_reorder.aside](../../examples/author_reorder.aside) for overlapping cards that move to the front while their animation continues. Cross-parent moves are not supported.

`node.remove()` removes and retires that node and all descendants, including when called on the stage root. `container.clear()` retires only the children, preserving the container's properties and identity. Both return null. Retirement cancels associated animations and releases the stage's image requests; retired nodes cannot be remounted, read or written. Call your component function again to create another instance. Methods can be stored and passed while retaining their original receiver; a saved remove method can be called repeatedly.

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

The complete [author_dynamic_stage.aside](../../examples/author_dynamic_stage.aside) example combines component functions, local edits and a fade-in animation. Use the dev/build/run commands from section 1 with its path. The current methods insert and retire subtrees; moving between parents is not implemented yet.

Use `image(source, fit: "contain", ...)` to place PNG/JPEG files on the stage. Without width/height, an image uses its pixel dimensions as logical dimensions; setting just one preserves its aspect ratio. With both set, contain fits the whole image with empty space, cover fills and crops the box, and stretch fills the box without preserving the ratio. Images also support the common position, visibility, opacity and padding properties. `source` and `fit` can be changed on the original node.

For panels or button backgrounds, use `slice: 24` for nine-slice drawing, or `slice: [24, 16, 24, 16]` for separate left, top, right and bottom insets. Insets use source pixels; `slice_scale`, which defaults to 1, scales the destination border widths. All values must be finite and nonnegative. Nine-slice drawing fills the content box, preserving corner proportions while stretching edges and the center. Oversized source insets or a target too small for the borders shrink all four borders proportionally; zero-area slices are skipped. `slice: null` restores ordinary fit. Both properties support assignment, bindings and layered_image descriptors. Use whole PNG/JPEG images or encoded image Bytes; tiled .reel-image sources do not support nine-slice drawing. All slices share the image without generating or uploading extra pixels.

`slice_center: false` draws only the nine-slice border, leaving its center transparent; the default is true. For example: `image("assets/frame.png", width: 320, height: 180, slice: 10, slice_center: false)`.

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

The complete [author_images.aside](../../examples/author_images.aside) includes these image files. Run it with the earlier dev command. Paths are relative to the source file for dev and the ASB file for run; use `--assets <directory>` to select a different resource root. For example, after building into target/reel-artifacts, run with `--assets examples`. Building an ASB does not copy image files.

Images and fonts use the same resource loading service. File reads and image decoding happen in the background, and image nodes using the same path share pixels. The previous complete scene remains visible until all images and configured fonts in the new scene are ready; story input pauses during that preparation. Changing source keeps the previous picture until the replacement is ready. A missing or invalid file reports its path and the image constructor's source location (game.configure for fonts) and ends the window run. Property type errors and an invalid fit are catchable at assignment. Images load when mounted with visible ancestors; constructing unmounted components or a visible=false subtree does not read image files. Loaded hidden images may be reused while mounted, yielding idle cache storage when new images need loading. Unmounting releases that cache; the previous complete scene and submitted frames retain their own leases. opacity=0 still prepares images for a later fade-in. In-session file hot reload is not implemented.

Every basic node supports rotation, scale_x/scale_y and anchor_x/anchor_y; a container transforms the whole component. Rotation uses clockwise degrees and defaults to 0. Scale defaults to 1; negative values mirror, and zero collapses the picture and excludes it from hit testing. Anchors are fractions of the layout width and height: (0,0) is the default top-left corner and (0.5,0.5) is the center. They may also lie outside the component. The x/y values offset that anchor from the container's layout position. Scale around the anchor, rotate, then place it at that position; the parent's transform applies to the result. Width, height and row/column placement retain their size before transformation. Clips and presented geometry use the same transform. Edits preserve node identity; changing position, rotation, scale, anchor or opacity does not repeat layout.


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

See the runnable [author_transforms.aside](../../examples/author_transforms.aside); it uses the same assets and commands as the image example.

### Camera framing

Use `camera { ... }` to pan, zoom and rotate the view of a scene. Its `camera_x/camera_y` identify the world point shown at the center of the viewport; they default to (0,0), so specify your scene center to frame a conventional top-left scene. `zoom` defaults to 1 and must be positive. `camera_rotation` is the camera's clockwise angle in degrees; the scene turns oppositely. Normal `x/y`, `rotation`, scale and anchors place the viewport itself. Width and height set its size; omitted dimensions use content layout. Children overlap as in stack, and padding remains in viewport coordinates. `clip` defaults to true and can be disabled.

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

The four camera properties work with assignment, bind/derive and tween, including normal cancellation and ownership. Put HUD siblings outside the camera to keep them fixed; nested cameras are ordinary components. Motion preserves text layout and loaded images. Drawing and pointer coordinates share the accepted frame's transforms and clips. See the complete [author_camera.aside](../../examples/author_camera.aside), which needs only `fonts/body.ttf`; run it using section 1's dev/build/run commands. This is a two-dimensional camera; perspective projection is not implemented.



Scene nodes, drawing records and nesting grow with the actual content. Input paths grow before a new scene is published, keeping existing presses, drags and releases tied to the correct nodes. The host and device manage actual memory budgets and GPU texture dimensions.
The following room describes the full-scene design and is not directly runnable yet: it relies on game:: modules and the custom dialogue view that are not connected. The background, portrait, CG and author dialogue UI are layered in that order. The function returns named parts that the story needs to manipulate.
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
A trailing children block constructs the child list immediately. Expressions that return nodes add those nodes in order. A let declaration only stores a variable; writing portrait as a separate expression adds it to the current parent. Ordinary if and for constructs can organize this list; their conditions are evaluated once during construction. Once a parent receives a node, it owns that node. A node always has only one parent.
room_view only constructs the scene. stage.replace(view.root) attaches the whole scene to the stage and releases the previous scene. The stage continues to own it after the function returns. Author-defined components also accept children, with the same capabilities as built-in components.
```aside
fn card(children = []) {
    column(gap: 12, padding: 24) {
        for child in children { child; }
    }
}
```
Properties, layout, and rendering belong to nodes; interaction behavior belongs to controls. Authors can put a character portrait inside a button, place a button in a visual group, or rotate or fade out an entire settings page. The shared node tree determines hierarchy rules. Authors explicitly place overlays within the root node.

Keeping a reference to any node also keeps its unmounted parent component alive; GC does not invalidate a node still in use. Property types, colors and numeric ranges are checked at assignment, so the same try/catch can handle errors and a rejected assignment retains the old value. The configured font list supplies missing-glyph fallback in order, including Chinese.

## 3 Story Code Is Ordinary Sequential Code

narrator, character, say and choose support sequential waiting and resumption. After `let mio = character("Mio", id: "mio");`, call `mio("你好。");` directly. The default id is the name; explicit ids distinguish characters with the same name. Characters are immutable. `.line(text)` creates a reusable line descriptor; `say([mio.line("你好。"), eri.line("晚上好。")])` displays one ADV group with a shared reveal and advance interaction. History keeps each submitted identity, name and full text. Runnable example: [author_dialogue.aside](../../examples/author_dialogue.aside).

`game.configure(text_speed: 30)` sets Unicode graphemes per second, defaulting to 30; 0 displays the complete text immediately, and omission preserves the current setting. Advance first completes the text, then finishes the interaction. Suspension, resource waits and paired input pause reveal without accumulating a catch-up burst. Rust hosts and the window share playback logic.

Bindable dialogue, custom ADV/NVL and choice views are available in [author_narrative.aside](../../examples/author_narrative.aside). Voice and chapter startup remain planned. say submits a group sharing one advance interaction; author views determine its layout.

Dialogue lines, choices, interaction text and session history grow with the content. History preserves the whole session; reaching a display cache capacity does not interrupt the story or discard earlier records. Actual allocation failure is reported.

Define each character once in a module. Each line of dialogue calls the speaker object; narration calls narrator. A standalone string is still just a string. A character call updates the dialogue data and history, then waits for the current line to finish. Ordinary function calls continue along the same execution sequence.

The `history` argument controls recording: true (the default) keeps the displayed text, false omits the line, and a string or rich value supplies replacement history text. Character calls, narrator and .line share this option. Display, advancement and read tracking still use the original dialogue. This supports glitch effects, unrecorded thoughts and complete sentences in the log. Replacement text can use tr; its language is fixed at submission. Mixed groups keep only recorded lines; an entirely omitted group adds no empty history item. See [author_dialogue.aside](../../examples/author_dialogue.aside).
```aside
mio("……", history: "I remember the rain.");
narrator("A fleeting thought.", history: false);
say([mio.line("Noise.", history: false), eri.line("Stay.", history: rich("[b]Stay.[/b]"))]);
```

For background voice with a history entry only, call `game.record("The train is arriving.", speaker: mio)`; omitting speaker records narration. It returns immediately without changing dialogue or choices, waiting for a click, or marking text read. Ordinary tasks and control callbacks can use it too; audio.play controls the audio independently. These entries share the existing history and save data, so the same history UI displays them.
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
While text is appearing character by character, the first advance reveals the rest of the current text; the next advance finishes the line. Events consumed by controls such as buttons and sliders remain within those controls. Story advancement uses a new input sequence number, so a single click always causes only one advance.
A character describes identity and speaking parameters. The author's components draw the dialogue box. Here is the simplest text layout; background images, nameplates, and shortcut buttons can be added to the node tree as usual.
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
game.dialogue().lines is the shared observable list of dialogue lines. Each line exposes read-only id/speaker_id/speaker_name/content_id/text/reveal/read. speaker_id distinguishes characters with the same name and is null for narration; content_id identifies content in read progress. speaker_id/content_id/speaker_name/text/reveal project the first line in the displayed group. ADV views commonly use these fields; NVL and speech-bubble views use the complete line list. A line's read records whether it was read before submission; the top-level read records whether every line was previously read. These values survive interaction completion so an exit animation does not suddenly change colors. Use `color: derive(|| if model.read { "#aaaaaa" } else { "#ffffff" })` for read styling, and look up speaker_id in an author-defined palette for character colors. model.text holds the complete text, while reveal controls how much is visible, keeping the layout stable during character-by-character display. Replacing, hiding, or showing the dialogue view preserves the session history. Voice parameters are not connected yet. Multiple characters speaking at once also share a single advance interaction:
```aside
let eri = character("Eri");
say([mio.line("你听见了吗？"), eri.line("在窗外。")]);
```
`game.dialogue()` returns the stable session model. The nvl_view above composes accumulated lines; `model.clear()` clears the displayed page while preserving history. ADV uses the first-line shortcuts, and current_line_ids identifies the current group. Reveal changes update properties without rebuilding the NVL page; appending lines or clearing the page rebuilds the view content. Ordinary layouts determine placement, backgrounds, spacing and bubble shapes. In a custom dialogue view, taps not consumed by controls advance the current line; touch drags only scroll. See [author_scroll.aside](../../examples/author_scroll.aside) for a long menu.
Compose an upward exit in an ordinary dialogue function: wait for the character's dialogue, then `await run(tween(...))` to move the old text out before starting the next line. [author_dialogue_transition.aside](../../examples/author_dialogue_transition.aside) contains a complete component with clipping, character colors and read colors; supply fonts/body.ttf before running. Adjust typing speed with game.configure(text_speed:). Per-glyph fades, bounces and similar effects are not connected yet.

When you need a route map or chapter restarts, use an ordinary API to register a stable ID and display title for a function. The function still follows the same calling and waiting rules.
```aside
fn opening() {
    // 剧情正文。
}
story.register("chapter01.opening", opening, title: "窗上的光");
```
`game.start(entry)` accepts an ordinary function in the current ASB with no captures or required parameters. After acceptance, the old script does not continue. The new session reruns module initialization and then calls entry. Previous dialogue, UI callbacks and activities end; script globals, history, auto-play and skip state reset. Read progress, game.configure settings, host assets and save storage remain. The entry constructs its scene; do not clear the old stage after the call. Cross-ASB hot-reload entry resolution and persistent script globals are not implemented. See [author_game_start.aside](../../examples/author_game_start.aside).

## 4 Sequencing Changes and Running Them Together

`clock(interval: 60, owner: panel)` creates an observable periodic clock. Read-only ticks counts complete intervals; elapsed is their total duration in seconds, both initially zero. Use derive to compose clocks, countdowns or progress displays. Large time steps publish the latest value directly. pause/resume/stop and await use the activity contract; paused time is excluded and retiring the owner cancels the clock. An endless clock cannot finish. See the [clock example](../../examples/author_clock.aside).

Animation and composed activities are available: `tween(node, x: 320, duration: 0.5, easing: "in_out")` describes a change and `run(action, owner: node)` starts it. Omitting owner uses the current scope first; outside a scope, it uses the session in the main story and the event node in a button callback. Control an activity with `pause(activity)`, `resume(activity)`, `stop(activity)` and `finish(activity)`. Await returns the string `Completed` or `Stopped`; retiring a target or owner cancels the activity and throws at await. Duration is in nonnegative seconds; zero completes immediately. Easing accepts linear, cubic in/out/in_out, quadratic quad_in/quad_out/quad_in_out and sine_in/sine_out/sine_in_out. Shared numeric properties can animate; width/height need explicit starting values, and null leaves a property out of the animation. Run [author_activities.aside](../../examples/author_activities.aside) with `reel dev examples/author_activities.aside --assets target/reel-artifacts/author-assets`. sequence/parallel/repeat and scope ownership of activities and tasks are available.

`tween(viewport, scroll_y: 400, duration: 0.5, easing: "out")` smoothly moves a scroll viewport; `scroll_x` is also supported. The target must be a scroll node, and the content dimensions still constrain the scroll range.

For a sine-shaped movement, choose `sine_in` to accelerate, `sine_out` to decelerate, or `sine_in_out` to start and finish slowly. All reach the exact requested endpoint. These differ from the cubic in/out/in_out curves and the quadratic quad_in/quad_out/quad_in_out curves.

For wall-clock time, import `use std::time::{now, parts, format};`. now() returns integer UTC Unix seconds. parts(timestamp, timezone: null) returns year/month/day/hour/minute/second/weekday/offset_seconds; weekday is Monday 1 through Sunday 7. format(timestamp, pattern: "%Y-%m-%d %H:%M", timezone: null) formats text using strftime patterns. timezone accepts IANA names such as Asia/Shanghai and UTC; omission uses the system zone, or UTC when unavailable. saves.list's saved_at can be passed directly. These calls do not refresh automatically: update a model from a regular task and bind the text. See [author_wall_clock.aside](../../examples/author_wall_clock.aside).

The quintic curves `quint_in`, `quint_out` and `quint_in_out` give stronger acceleration and deceleration. `bounce_out` bounces on arrival, `bounce_in` reverses that motion, and `bounce_in_out` combines both ends. Bounce stays between the starting and ending values. Use the same tween API for a sliding menu or a falling logo; see [author_activities.aside](../../examples/author_activities.aside).

```aside
await run(tween(portrait, x: 720, duration: 0.8, easing: "sine_out"));
```

The complete [author_sine_easing.aside](../../examples/author_sine_easing.aside) compares all three sine curves over the same distance and duration; supply `fonts/body.ttf` before opening the window.


The language runtime supports awaiting host-registered awaitable objects. An ordinary fn can use `await request` to suspend its call chain while the host handles other work. Even settled objects return on a later scheduling step; failures point to the await expression and retain the producer's cause. Engine activities now use this protocol; async fn and Future construction are not available yet. The runnable host integration check is [host_awaitables.rs](../../crates/aside-compiler/tests/host_awaitables.rs): `cargo test -p aside-compiler --test host_awaitables`.

Empty sequence/parallel and times: 0 complete immediately without property writes. times must be a nonnegative integer or null; omitting it or passing null creates a continuous loop, whose cycle must have a positive duration. A composition with an infinite branch rejects finish before changing the activity; use stop to end it. run checks the complete write sets of parallel branches before taking over existing activities. Pause, stop and cancellation affect the whole group; later segments acquire properties only when they start. A large clock step continues through every segment it crosses.

tween only describes target properties, final values, and duration. sequence combines actions in order, parallel combines them concurrently, and repeat repeats an action. These are all ordinary library functions that produce reusable action values. Only run starts an activity, returning an activity that can be awaited and controlled. await activity waits for it to end while input, audio, and other activities continue. run(action) belongs to the current scope, otherwise it inherits its task resource owner; the main story uses the session lifetime. Specify a stage node with owner or a scope with s.run(action).
An action reads its starting property values when it actually becomes active. In a sequence, a later step reads its starting values after the previous step finishes. Action descriptions are reusable, and every run creates independent progress. An activity acquires property writes when a segment actually starts. A new segment stops any conflicting older root activity and takes over from the current values, including from paused activities. A sequence may write the same property in order; parallel branches must have disjoint write sets. Direct assignment, including assignment of the same value, stops the root activity writing that property. Assignments to other properties are unaffected. Ending an activity releases all of its writes.
```aside
let entrance = parallel(
    tween(view.portrait, x: 320, duration: 0.5),
    tween(view.hud, opacity: 1, duration: 0.3)
);
await run(entrance);
```
Every activity has a defined owner. Startup and main-story functions belong to the session; UI callbacks belong to the node that emitted the event. stage.replace ends the old scene and its callbacks, while the session-owned story code that called it continues. Action targets are only validated handles. If any node or effect target is destroyed, the entire root activity ends as Cancelled and releases all property reservations. When an owner is destroyed on the stage, its activities and callback tasks end together. pause suspends progress, resume continues it, and stop leaves the current value in place and causes a wait to return Stopped. finish brings finite actions to their final values and returns Completed; natural completion also returns Completed. Use stop to end an infinite loop, which has no final completion value of its own. Resource errors or activity cancellation raise the corresponding error in any waiting task that is still alive, where ordinary try/catch can handle it. If the waiting task's own owner has ended, that task ends immediately. The host releases its resources, and script cleanup code ends with the task.
parallel waits for all finite child actions. If any child action fails, it ends the remaining actions in the group and reports the error; sequence stops subsequent actions on failure. repeat(action, times: 3) repeats a finite number of times; omitting times creates a loop. Continuous loops, such as breathing or falling rain, run separately under scene ownership. Story code waits only for the finite activities it needs.
```aside
let breathing = repeat(sequence(
    tween(view.portrait, opacity: 0.92, duration: 1.4),
    tween(view.portrait, opacity: 1.0, duration: 1.4)
));
run(breathing, owner: view.root);
mio("听，雨又大了。");
```
The example above demonstrates the rules for composing loops. Actual character breathing can use subtle movement or scaling instead, with the same script structure. Authors wrap common actions in ordinary fn functions that take any node and return an action value. UI and character portraits can share these actions.

Use `delay(seconds)` to leave a pause in a scene without creating a display node. It shares the animation clock, composition, pause, stop, and owner cancellation rules; a player with only delays sleeps until the next deadline. Seconds must be finite and nonnegative; zero completes immediately. Runnable example: [author_timing.aside](../../examples/author_timing.aside).

```aside
await run(delay(1.5));
await run(sequence(delay(0.5), tween(panel, opacity: 0, duration: 0.3)));
```

## 5 Managing Temporary Scenes with Scopes

Scope ownership of tasks, activities, waits and temporary nodes is implemented. Property overrides and additive offsets remain planned.

scope executes an ordinary closure within the current story call chain. Its body can deliver dialogue, wait and call other functions. Exiting retires work owned by that scope. Temporary scenes join the same lifetime management as follows:
```aside
fn show_cg(view) {
    scope(body: |s| {
        s.add(view.picture, image("assets/window-cg.png"));
        await game.advance();
    });
}
```
The CG appears at the top of picture and is removed when the player advances and the scope exits. s.add(parent, node, index: 0) chooses an insertion position and returns the original node. Early page removal, scene replacement and exit after loading a save use the same retirement rules.
s.add(parent, node) mounts a temporary subtree, and s.run(action) starts an activity owned by this scope. The planned s.override(node, props...) will create an animatable override layer, while s.offset(node) will create an independent additive x/y offset. Ordinary node.add continues to use the explicit parent node's lifetime; being inside a scope does not silently change its meaning.
Normal return, exceptions, and scene exit all trigger synchronous, idempotent host cleanup. The body explicitly waits for any exit animation. Scopes can be nested. A save records the current call position, activities and temporary subtrees; after restoration, execution continues in the same body, then releases resources under the same rules. Ordinary writes to game variables retain their application meaning. scope manages only explicitly registered temporary effects.
By default, scope lives with the current task. Use scope(owner: node, body: ...) when it should end with a particular node. If that node ends first, only the scope region is cancelled, and ScopeCancelled is raised at the outer call site. Main-story code that is still alive can catch it outside the scope. Code involving invalid nodes within the region ends, and the host performs cleanup. If the current task itself is cancelled, the entire task ends.
visible=false hides the subtree and removes it from hit testing and focus; enabled=false keeps it visible but disables interaction; opacity changes only transparency. Use stage.replace(new_root) for permanent scene changes and a container's replace(new_child) for permanent local replacements.

## 6 Logos, Title Screens, and Local Interaction

Video is connected to the shared stage, media activities and execution saves. The default desktop backend supports non-fragmented MP4 with progressive 8-bit YUV420 H.264 Baseline video and optional mono/stereo AAC-LC. Custom hosts can supply other formats or hardware decoding through MediaBackend; playback reports a source-located error when video support is unavailable.

`video(source, ...image node properties)` creates only the visual node; `movie.play(looping:false, volume:1, start:0, end:null, owner:null)` returns an activity. start/end specify the segment start and exclusive end; null uses the movie end. Playing again stops the node's previous playback. Natural completion and stop both continue after await; failure and node/owner retirement raise catchable errors. See the complete [author_video.aside](../../examples/author_video.aside) example.

```aside
let movie = video("assets/logo.mp4", width: 1280, height: 720, fit: "contain");
let playing = movie.play();
stage.replace(button(width: 1280, height: 720, decorated: false,
    background: "#000000", on_click: || stop(playing)) {
    movie;
});
await playing;
```

`pause(playing)` / `resume(playing)` / `stop(playing)` share the activity protocol. `finish(playing)` completes a finite video; stop a looping video. `video.seek(playing, seconds)` seeks to absolute content time and `video.position(playing)` returns the current seconds. Its audio uses the Music volume bus. Hiding the node does not pause it; combine ordinary clipping, cameras, opacity and controls.

Saves record the path, segment, position and paused state and resume the same activity. A stopped/completed node's retained picture also restores without replaying audio. Pixels, decoder queues and GPU handles are not serialized. Decoding runs in the background; audible video follows consumed PCM and silent video follows session time. Both pause during host suspension.

To add a temporary button during a story, add an ordinary button to the desired parent node and remove it normally afterward. The author decides which content it covers, whether it blocks input underneath, and when it disappears. One-off buttons use the same event and ownership rules.
```aside
let hint = view.hud.add(button("查看照片",
    on_click: |event| photo_panel.open()
));
mio("桌上那张照片，你还记得吗？");
hint.remove();
```
photo_panel is an author-defined component control object. The example only demonstrates a button's lifetime interleaved with the story. Event callbacks are scheduled under their owning nodes. When a modal node opens, the engine consistently handles input isolation for underlying content, focus entry, and focus restoration after closing. A callback that needs to take over the main story submits an entry operation through game; ordinary scene changes can run directly.

## 7 Settings Pages and Image-Based Controls

button, slider, toggle and node bind/unbind are implemented; tabs, settings and slider appearance slots remain design targets.

The following example combines tabs, pages, and sliders into a settings page. switcher preserves each page's nodes and local values; select changes only the selected page. Hidden pages leave input handling and focus navigation.
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
The control handles pointer capture and value conversion during dragging. The same value determines both the thumb position and the volume. Authors supply rendering nodes for the track and thumb; the control lays them out according to its orientation, range, and slots. settings contains global settings, which retain their current values after a story save is loaded. Ordinary movement, scaling, and layout changes preserve dragging; input uses the geometry of the most recently presented frame. Changing the range, step, direction, or track-endpoint policy ends the current drag. Interaction resumes when the new configuration is shown, while external updates to the bound value still take effect immediately.
An image button is also the author's own ordinary function. The button provides interaction state, and the image selects a resource based on that state.
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
bind connects an observable property while preserving that property's existing read/write permissions. text can read a read-only property; slider requires a writable property when it writes a value back. derive computes read-only presentation values. A derived function establishes dependencies on the observable properties it reads and may also read immutable captured values. Internal changes to ordinary data retain ordinary language semantics. Observable property changes update only the relevant nodes. Writing a scalar to a writable bound property writes through to its binding source while preserving the binding. Animation also reserves and writes the ultimate source property, so two nodes bound to the same source reveal the actual write conflict. Explicitly changing the source stops the relevant root activities; passive projection updates are reads. Use scope overrides for temporary animation of read-only derived properties. Rebinding or unbind first ends activities that depend on the old binding; unbind preserves the current effective base value. During the brief save-capture period, script-derived computations and author-defined presentation updates pause, then resume using the latest state when capture finishes. Ordinary {...} data records are suitable for passing parameters; use model({...}) to create an observable model for application values that change over time. Cyclic dependencies, writes inside derived computations, and waits inside derived computations produce source-level diagnostics.
Repeated reads of the same model field in one derivation register one dependency. An internal index deduplicates large sets of model fields; nested components, cancellation and restoration during evaluation preserve the same dependencies and evaluation order without author cache or capacity settings. Virtual lists reduce live row nodes; the full data, key index and dependencies remain resident. Source changes still require indexing, and freshness checks before presentation still depend on the registered dependency graph.

Authors can use the same approach to wrap consistent fonts, button images, spacing, and animation, then import them as ordinary modules. Visual style is reused through components and parameters; shared property objects can also be organized using ordinary language rules.

Use `bind(node.x)` or `bind(node[key])` to capture a stable property location. The receiver and key are evaluated once. Passing the reference to a node constructor or assigning it to a node property establishes a connection; subsequent ordinary assignments write through while keeping it connected. `unbind(bind(node.x))` disconnects that location and preserves and returns its current value. Constraints on every connected control are checked together, and a failed write changes none of them. Explicitly destroying a source node disconnects surviving targets at their last value; existing references can no longer access the retired source. See section 12 and the verified scope for the implementation status of business `model` and `derive`.

Every node exposes read-only observable `hover`, `pressed`, `focused`, and `disabled` properties. Read them directly or connect them with `bind`; they cannot be assigned or used as writable slider/toggle values. Hover and focus include descendants; pressed includes pointer capture and the button’s own keyboard/controller confirmation hold. Disabled includes ancestor disabling. Cancellation, hiding, disabling, and window focus loss clear ongoing interaction. When a node retires, computations reading it stop updating and keep their last successful value. Removing the last consumer stops its computed appearance updates; shared consumers keep updating, and rebinding a retained reference resumes updates, but cannot restore a retired input. Set `decorated: false` on buttons and toggles to remove the default focus outline and toggle indicator/spacing, then compose the appearance from ordinary children. See [author_control_states.aside](../../examples/author_control_states.aside).

Use `on_interaction` when hover or focus should change application state, such as keeping a gallery item selected after the pointer leaves it. Every node accepts this callback or null, including assignment after construction. The immutable event contains `target`, `hover`, `pressed`, `focused`, `disabled`, and `focus_visible`; its booleans retain that transition's values even if the callback waits. Callbacks run only for actual state changes, after the whole event batch's read-only states are published. Assigning a callback does not send an initial event. Waiting callbacks belong to the node and are cancelled when it retires. Without a callback, no event object or task is allocated; use `bind`/`derive` for appearance alone.

```aside
let gallery = model({selected: null});
let card = button("Sketch", on_interaction: |event| {
    if event.hover || event.focus_visible { gallery.selected = "sketch"; }
});
```

[author_control_states.aside](../../examples/author_control_states.aside) combines transient appearance with a selection caption retained after leaving a control. Supply `fonts/body.ttf`, then run `reel dev examples/author_control_states.aside`.

## 8 Styling and Moving Elements Together

Images using the same resource are shared by the player. The wgpu backend reuses submitted GPU images, so changes to text or hover state do not require uploading the whole interface again. Cached images are released on resource retirement or when space is needed; authors do not manage GPU handles.

Ordinary functions, style spreading, node property updates, transforms and finite animation are available. Temporary scope overrides remain unimplemented.

Reusable styles are ordinary data; component functions decide how to apply them to their nodes. Start with defaults, then override them with variants and explicit arguments. Defaults apply only to missing fields; false, 0, and null retain the author's explicit meaning. Temporary scope overrides take precedence over a node's underlying properties.
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
`...style` passes own object fields as named arguments. Ordinary functions, node constructors and extracted methods share these rules. Merge overrides into one object first; supplying the same argument twice is an error. Positional arguments precede named arguments, including trailing children. See [author_buttons.aside](../../examples/author_buttons.aside) for initialization and button interaction.
Node properties can be changed directly. Use bind/derive for appearance that should keep following application state or preference changes. Ordinary style records are used during construction. A shared runtime color palette can be stored in a model, with its properties bound to text and images. Authors can draw their components entirely themselves or reuse familiar layout and control behavior.

Use `derive(|| control.hover || control.focus_visible)` for button highlights. Read-only `focus_visible` displays focus during keyboard/controller navigation and hides that indication after pointer use; `focused` retains the actual target, so Enter can still activate it. Both include descendants, and hiding, disabling or losing window focus removes the indication. Background animation does not invalidate unrelated buttons; replacement button content must be presented before accepting a new choice. See [author_fitted_text.aside](../../examples/author_fitted_text.aside).

To move UI and character portraits together, place them in the same group and apply the action to that group.
```aside
let cast = group() {
    image("assets/mio.png");
    dialogue_view(game.dialogue());
};
stage.replace(cast);
await run(tween(cast, x: 80, duration: 0.5));
```
`scope(body:, owner: null)` runs a piece of choreography in the current task and returns the body's value. By default, run, spawn, clock, audio, video, particles, completion and presentation waits belong to the current scope. Returning or throwing from the body cancels unfinished work. `s.run(action)` and `s.spawn(work)` explicitly attach work to that scope.

```aside
scope(body: |s| {
    s.run(repeat(sequence(
        tween(view.hud, opacity: 0.4, duration: 0.5),
        tween(view.hud, opacity: 1, duration: 0.5)
    )));
    narrator("The light keeps pulsing while this line is shown.");
});
```

Stopping an ordinary animation leaves its properties at the current values. Independent property overrides and additive offsets with automatic restoration remain planned. Removing an owner node cancels the scope's inner work; the outer catch receives "ScopeCancelled" and can continue. Inner script finally blocks are skipped, while outer finally blocks execute normally. Cancelling the outer task cleans up its scopes. Work with another explicit owner follows that owner's lifetime. Scopes, waits, activities and cancellation cleanup survive same-package saves.

[author_scope.aside](../../examples/author_scope.aside) uses one button to start animation and background work, and another to stop the whole effect through one task handle. Run `reel dev examples/author_scope.aside`.

## 9 Masks, Effects, and Custom Scene Choreography
Historical verification (R01 / Issue #25): focused cases cover internal Rust frame records, resource leases, groups, clipping and opacity. R01 did not yet provide a GPU window, and recording consumers do not establish physical-device behavior. The image-mask API below is implemented; blur and custom shaders remain design targets. The basic stage and real window now work together as described in sections 1 and 2. See the [historical verification scope](../../crates/reel-ui/R01-REPORT.md).

Historical verification (R02): the wgpu backend and winit diagnostic window included images, group opacity, clipping and Parley text. Those historical Rust diagnostics do not establish implementation of advanced-effect APIs. PNG/JPEG author images now use the shared stage and player resource loader. Current author source and ASB enter the real story window through dev/run. See [window documentation](../../crates/reel-player/README.md) and [device verification scope](../../crates/reel-render/R02-REPORT.md).

The mask node property applies a grayscale image mask to an entire subtree, sharing normal image loading and save state. mask_progress ranges from 0 for fully hidden to 1 for fully visible; mask_softness controls edge width, with 0 producing a hard edge. Dark regions appear first; mask_invert: true reverses their order. PNG/JPEG masks stretch to the node rectangle, so set width/height to define the covered area.
```aside
let reveal = stack(width: 1280, height: 720,
    mask: "assets/ink-mask.png", mask_progress: 0, mask_softness: 0.06) {
    image("assets/window-cg.png");
};
view.picture.add(reveal);
await run(tween(reveal, mask_progress: 1, duration: 1.2), owner: reveal);
reveal.mask = null;
```
mask_progress and mask_softness use ordinary derive/tween. See [author_image_mask.aside](../../examples/author_image_mask.aside). Progress 1 bypasses mask compositing; set mask to null when finished to release its resource reference. Masks do not change logical hit geometry; use enabled/visible to control interaction. Masks require whole PNG/JPEG images, not tiled .reel-image sources.

The `invert` node property inverts the composited subtree. It ranges from 0 (the default) to 1 for full inversion, with intermediate values blending in sRGB. Alpha is preserved. Put dialogue and menus in sibling nodes to keep them independent of the inverted scene. Direct assignment, derive, tween, and save restoration use the ordinary property protocol.

```aside
let scene = stack(width: 800, height: 450, invert: 0) {
    text("A different view", color: "#e08020", font_size: 48);
};
stage.replace(scene);
await run(tween(scene, invert: 1, duration: 0.4), owner: scene);
```

Runnable example: [author_invert.aside](../../examples/author_invert.aside). Inversion shares the group target with opacity/mask; set it to 0 when finished to remove the need for a separate inversion target.

Images, text, UI and effects share `blend`, which composites the completed subtree: `normal` uses source-over, `add` adds light, `multiply` darkens, and `screen` lightens. Opacity, masks and inversion apply before blending with the backdrop. Layout and input stay the same; color calculations use linear space. The light layer below can also contain images, particles or components. See [author_blend.aside](../../examples/author_blend.aside).

```aside
stack(width: 320, height: 180, background: "#800000", blend: "add", opacity: 0.5);
```

Effect parameters follow the same animation rules as node properties. More complex scenes can nest group nodes: blur the background separately, keep characters sharp, and leave image buttons on top, then apply a mask or camera transform to the whole group. The tree structure determines the front-to-back order of all children.
Custom effects use a versioned Effect description for input images, parameters, output bounds, and backend implementations. Advanced authors can develop host extensions, then compose and reuse common effects through ordinary Aside functions. Shader support is a backend capability; project checks list missing capabilities. Portable effects should also provide simplified implementations, letting the game select an appropriate platform version through capability queries.
Pixels changed by an effect and logical input regions are declared separately. Ordinary affine transforms and geometric clipping automatically map hit tests. For bends, distortions, and arbitrary shaders, authors provide logical shapes or specialized mappings for interactive regions. Reading the rendered image back from the GPU for every click would add latency, so the base interface uses regions that can be computed directly.

## 10 Choices and Changing Content

Text automatically wraps to the available width by default. Set `text_wrap: false` to retain each explicitly authored line; excess text remains visible unless clip trims it. The property supports direct assignment and bind/derive. This switch applies only to text: control labels such as buttons still do not wrap automatically, and text editors keep their own rules. See [author_text_wrap.aside](../../examples/author_text_wrap.aside) for a complete example.

```aside
text("A long authored line without automatic wrapping.\nA second explicit line.",
    width: 240, text_wrap: false, clip: true);
```

Use `text_fit: "shrink"` to fit a complete title or option into a fixed width; `min_font_size` sets its lower font-size bound. The default, `text_fit: "none"`, keeps normal layout. Shrink always fits the complete text without automatic wrapping, independently of text_wrap. Fitting only shrinks, preserves explicit line breaks and scales rich text and ruby together; typewriter reveal does not change the size. Use clip if text still overflows at the minimum. `text_outline` sets the outward glyph outline width and `text_outline_color` sets its color for text and control labels. For example: `text("Long option", width: 140, font_size: 30, text_fit: "shrink", min_font_size: 14, color: "#000000", text_outline: 2, text_outline_color: "#ffaaff")`. These properties accept derive, so hover or focus can change the text appearance.

Use `spawn(work, owner: root)` for independent scripted scenes or background game logic. work is a zero-argument function, closure or bound method and may loop and await animations/results. `await task` returns its value or raises its original error; repeated awaits share the terminal result. `task.cancel()` returns true for the first cancellation request and false afterwards; waiters receive Cancelled after cleanup. Cancellation does not run script finally blocks, so attach resources to their owning nodes. Removing a node cancels its tasks; hiding it does not. Cancelling a waiter leaves the producer running. An omitted owner inherits the callback/task node, or the session in the main story. Work starts after the caller waits or returns and has no main-story dialogue/choice permission. Initialization, pure derive and NoWait contexts cannot spawn tasks. See [author_tasks.aside](../../examples/author_tasks.aside). Use sequence/parallel/repeat for fixed animations; tween supports quadratic `quad_in`, `quad_out`, `quad_in_out` curves alongside the cubic in/out/in_out curves.

Task capacity grows with actual concurrency and is reused; authors do not reserve a task count. Rust hosts can set their own admission quota through `VmLimits.max_tasks`; allocations still count against the heap budget.

Ordinary functions run in the current task and return after their internal waits complete. If `ask()` already uses `await result` and returns bool, call it as `if ask() { ... }`; use another await only when a function returns an awaitable activity or completion. Awaiting a plain value such as bool reports `NotAwaitable` at that line.

See [author_fitted_text.aside](../../examples/author_fitted_text.aside) for a runnable example of resizing, switching text languages and hover outlines.

When an event handler needs no event data, use `on_click: || confirm()` or pass a zero-argument bound method. To read the event, use `on_click: |event| inspect(event.target)`. A handler declaring parameters or rest receives one event; a bound method’s `self` is not counted. This applies to all event handlers, which may await and retire with their owning node.

Forms, sign-in screens, settings and embedded games can return their own results. `completion(owner: root)` creates a one-shot awaitable. A callback submits any value with `.resolve(value)` (null when omitted), and the caller continues through `await` while other input and activities keep running. A component can return its node and result together:
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

`.reject(error)` throws that value at await; `.cancel()` throws Cancelled. All three settlement methods return true the first time and false afterwards, so repeated clicks cannot replace a result. Repeated awaits observe the same terminal state. Removing the owner cancels pending results; hiding it does not. Cancelling one waiter leaves other waiters unaffected. Without owner, the result belongs to the session in the main story or the event node in a callback. Resolve passes objects through without adopting other awaitables. Results do not occupy choose or dialogue and add no history. See the complete form and child-game composition in [author_results.aside](../../examples/author_results.aside); provide fonts/body.ttf before running.

[author_embedded_puzzle.aside](../../examples/author_embedded_puzzle.aside) embeds a 2D switch puzzle in custom dialogue. Operate graphical controls by pointer or keyboard/gamepad focus, pause both the component clock and hint animation, and return the move count to the story. Leaving the component cancels its pending result and retires its activities. The puzzle, dialogue and menu use ordinary stage nodes. This example uses existing events and activities; it does not provide physics or a fixed-step update loop. Supply fonts/body.ttf before running.

choose, custom choice_view and the read-only choice model are implemented. The engine adds no automatic dialogue or choice UI; mount your own components on the stage and use ordinary functions to package reusable appearances. See [author_narrative.aside](../../examples/author_narrative.aside) for ADV/NVL switching and custom choices.

choose(options) accepts an array of options, each with a stable key and a label, updates game.choice, and waits for select(option_id). The current line and choices share the right to control main-story interaction. Authors place the choice view within their own scenes and choose its position and background.
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
choice_view is mounted in the scene containing the dialogue UI; by_window and by_door are the author's story functions. choose copies the supplied ordinary records into engine-owned, read-only observable rows. id, key, and label can all be read and bound, while the input records remain unchanged. id is an opaque, stable identifier formed from the current InteractionId and key. Buttons capture option.id to preserve that interaction identity; the next choice uses new IDs. game.choice().options is a read-only observable list of these rows, empty by default. It is populated while waiting for a choice and cleared when the choice is complete. select accepts option.id, which includes interaction identity, while the value returned to story code is still the application key. Stale or duplicate select calls return false; accepted calls return true. choose always returns the original key.
A for loop inside ordinary children constructs current content. `view(|| ...)` rebuilds its subtree when observable data actually read by the factory changes, retaining the outer container. Successful construction replaces the old content and cancels tasks and activities owned by retired nodes. Use bind/derive to update individual properties while retaining node identity. Use `list(|| data.items, key: |item| item.id, build: |row| text(value: derive(|| row.value.label)))` for collections that retain their controls. A stable key preserves its node, focus, and local state during insertion or sorting; removing it retires its owned tasks and activities. Keys may be any value and must be unique within the list. Scalars retain their type, strings compare by content, and other objects compare by identity. Use choice IDs or stable object references directly; no surrogate numeric ID is needed. Each new key calls build once with a read-only observable row: key, value (the current item), and index (zero-based). Returning null creates no node for that key. Callbacks read row.value to use the current record; derive follows replacement records, while bind(row.value.label) fixes the original field location. Only reads in the source and key functions determine list reconciliation; use bind/derive/view inside each component for changing content. Duplicate keys or a failed component do not replace the old tree. Let the data source manage the list's children, and compose scroll around it. By default, all items remain instantiated. See [author_list.aside](../../examples/author_list.aside).

Long menus, history, and galleries can use `list(..., virtual:true)` as the only flow content of a vertical scroll with a finite viewport height. Only nearby rows remain mounted; leaving the window retires a component, and returning rebuilds it. Keep persistent state in model. Rows being edited or dragged remain until that interaction ends. Filter omitted items in the source and return a node from build; natural measurement requires a positive natural height. The list itself uses natural height: do not set height/max_height, vertical stretch, or its own position, rotation, scale, or anchor; transform the outer scroll instead. Rows can combine ordinary text, images, controls, and custom appearances. Layout measures varying heights automatically and refills the buffer before unmeasured rows enter the viewport. New windows use their measured heights for contiguous placement while preserving the reading anchor, with keyboard/controller navigation across windows. Unmeasured content uses the average of rows measured at the current width, so content_height used by a custom scrollbar is still refined over time; distant jumps and newly loaded images cannot have guaranteed sizes in advance. This reduces component and layout costs; full text and source data remain retained. For a gallery with several columns, group each row of pictures into an item with a stable key and return an ordinary row composition from build. The whole row is measured and retired together; keep persistent picture state in model. Changing the column count or grouping constructs rows under their new keys; this is not two-dimensional grid virtualization. The interactive [thousand-item menu](../../examples/author_list.aside) demonstrates varying descriptions, expanded state, filtering, reordering, and keyboard/controller selection. Run `reel dev examples/author_list.aside --assets target/reel-artifacts/author-assets` after supplying fonts/body.ttf as the example describes.

Runnable example: [an automatically measured gallery and custom scrollbar](../../examples/author_list_gallery.aside). Supply fonts/body.ttf and run `reel dev examples/author_list_gallery.aside --assets target/reel-artifacts/author-assets`. Express row content with ordinary layout; no separate row-height argument is needed. While a distant window is being built and measured, the player keeps the previous complete frame, then publishes the ready window. It does not show an empty or partly positioned intermediate frame.

Positioning by key: `let row = await items.scroll_to(key, align: "nearest")` constructs and measures the target window, then returns the row node after its frame is actually presented. Choose start, center, end, or nearest; the default scrolls only as needed. Reordering follows the same key. Removing the target, retiring the list, or a newer scroll or positioning request cancels the old request; handle it with try/catch. Returned nodes retain the ordinary offscreen retirement rules, so keep persistent state in model.
Resizing the viewport while waiting realigns the target; automatic scroll clamping does not cancel the request. When the source recomputes, the request waits for the new position to be presented before returning.

## 11 Saves, History, Routes, and Skipping

History, auto-reading and save/load interfaces are implemented; route switching and chapter jumping remain design targets. Authors compose the UI; services do not generate fixed screens.

Call `game.clear_history()` when earlier history is no longer needed, or `game.clear_history(keep_last: 37)` to retain the latest 37 recorded history lines. The count is in lines, not dialogue groups; lines with `history:false` do not count. The earliest retained group may keep only its trailing lines. Retained groups and lines keep their IDs, objects, contents and order without renumbering; the group's speaker_name/text update to its first remaining line. The observable history array and retained groups' lines arrays keep their identities. Omitting the argument or passing 0 clears everything; a count at least as large as the current line count changes nothing. Negative or non-integer counts are rejected. Current dialogue, NVL display, advancement, read progress, preferences and saved slots remain unchanged; loading a save restores its recorded history. New group IDs are not reused. Trimming performs synchronous work proportional to the record count, releases session references and requests incremental GC, so reclamation continues even if later interactions allocate little. References or UI caches held by your script still retain their contents: retire those views or refresh them from the history array as well. History has no automatic length limit and needs no cleanup timer. Rust hosts use `Session::clear_history(keep_last)`. See [author_reading.aside](../../examples/author_reading.aside).

```aside
game.dialogue().clear();
game.clear_history();
narrator("A new chapter begins.");
```

```aside
game.clear_history(keep_last: 37);
```

`saves.save(slot, title: null, thumbnail: null)`, `saves.load(slot)`, `saves.delete(slot)`, `saves.thumbnail(slot)` and `saves.list()` immediately return awaitable requests with read-only `status` (pending, succeeded, failed) and `error`. Awaiting a failed request throws its error; a successful save has atomically published the complete file. `await saves.list()` returns occupied slot summaries with string id, title, saved_at (UTC Unix seconds) and has_thumbnail, without loading images. Slot names are logical keys. Authors choose pages and empty slots and can use a fixed key such as `"quick"` for quick saves.

A caller may await its own save. Loading that snapshot restores the request as succeeded with read-only `restored` set to true (false after a normal write), allowing a menu to close, and continues after await without repeating the save call. Other unfinished external requests settle with OperationInterrupted. A successful load replaces the session; its caller does not resume in the old session.

Execution snapshot capture advances in work slices; encoding, compression and writing run on a worker. If business input needs to mutate state during snapshot capture, that save reports CaptureBusy, input proceeds, and the old slot remains intact. Desktop saves default to the user-data directory; Rust hosts can set `StoryWindowOptions.save_directory` and `save_bytes`. Custom hosts install `RuntimeSaves` with their own `SaveStorage` and audio backend. The directory adapter resolves the existing save directory, writes a temporary file in that same directory tree, syncs it and atomically replaces the old slot, including its thumbnail. The current VM snapshot format is 7 and the session snapshot format is 15. Saves require matching code and formats; older formats report an explicit version mismatch. System data in `persistent` is independent of story snapshots. Cross-version migration is not provided. Existing save restoration check: `cargo test -p reel-player --lib save_slot_survives`.

Runnable example: [author_saves.aside](../../examples/author_saves.aside).

Capture and preview example: [author_capture.aside](../../examples/author_capture.aside). `await stage.capture(width: 256, height: 144)` obtains PNG Bytes from the most recently presented frame; capture before opening the menu and pass the result as thumbnail. `await saves.thumbnail(slot)` reads only that slot's image and returns null when absent; display it with `image(bytes)`. Requests fail when no frame has been presented, the host does not support capture, or resources are insufficient; use try/catch to display errors.

For a portable desktop game, launch with `reel run game.asb --save-directory saves` from its directory. `dev` accepts the same option; relative paths use the launch working directory. A launcher can pass an absolute path next to the game. On platforms with read-only installation directories, keep saves in the platform's writable user-data directory. Scripts use the same slot API in either case.

Moving the mouse without holding a button during capture does not cancel the save. The window retains only the latest hover position and applies it after capture; clicks, dragging, keyboard input and focus changes still interrupt capture when they require session access.

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

game.restart(story_id) uses the current session's fixed main-story module set to rerun the story task from a named chapter entry point. It preserves current story variables and ends the previous main-story task and its interaction; the target function is responsible for constructing its own scene. game.start(entry) creates a fresh session, suitable for a new game. saves.load restores the saved position, variables, and scene. Authors use these distinctions to give route buttons clearly defined actions.
`game.reading()` returns stable read-only observable `auto/paused/delay` state. `game.set_auto(enabled, delay: null)` controls auto-reading; delay is a finite nonnegative number of seconds after the full text has actually been presented, defaults to 2 seconds, and is preserved when omitted. `game.pause_reading(paused)` pauses reveal, automatic advancement and dialogue/choice input; ordinary UI controls can still close a history page. Auto-reading waits for player input at choices and continues afterward. Focus loss, suspension, modal UI, pending resources and active input gestures stop timing; resuming starts a full delay again. State belongs to the session, survives UI replacement, and defaults to manual and unpaused in a new session. Finite scene activities and independent audio have separate controls. Authors choose every control, layout and history-page interaction.
`game.set_skip(enabled, unread: false, after_choices: false)` is implemented. Observe `game.reading().skipping/skip_unread/skip_after_choices` for mode and policy, and `game.dialogue().read` for whether the entire group was read before submission. By default, unread dialogue or a choice turns skipping off. unread: true also skips unread dialogue. after_choices: true preserves skip mode while waiting for the player to choose, then continues under the same read policy. Pause, focus loss and modal UI still cancel this mode. Skipping follows normal control flow, bypasses reveal/dwell, and finishes running finite activities owned by the main story. Infinite, paused, node-owned activities and independent audio retain their controls. Auto-reading and skipping are mutually exclusive. Reading pause, focus loss, suspension and modal UI turn skipping off. Retiring the input node that started skipping also clears its held mode. Authors can use input callbacks to enable on pressed and disable on released/cancelled, or compose toggle controls. Content is marked read when dialogue advancement completes, not on cancellation or full reveal alone. Rust hosts reuse `ReadProgress` through `Session::new_with_read_progress` and connect `ids/from_ids` to their own persistent storage; new sessions default to empty progress. A changed source module version is conservatively unread.

Use `game.clear_read_progress()` to reset read progress. The current waiting dialogue group and its lines immediately report `read: false`, and skipping that is limited to read dialogue stops. Future dialogue is unread until normal advancement records it again. History and completed NVL lines retain their presentation; automatic reading and skipping that includes unread dialogue keep their policies. Loading a save also evaluates its waiting dialogue against the current read records. This operation releases the old records and takes time proportional to their capacity. It performs no disk IO and does not delete game saves or `persistent` system data. Rust hosts have the corresponding `Session::clear_read_progress()` method. See the runnable [author_reading.aside](../../examples/author_reading.aside).

```aside
button("Reset read progress", on_click: |event| game.clear_read_progress());
```
Chapter skipping via `game.skip_to(story_id)` still depends on the future chapter registry.

These services provide data and operations; authors compose their pages. Manual dialogue confirmation reveals or advances once and disables auto-reading and fast-forward; rejected input does not change either mode. Saving, loading, history and reading controls are implemented; route and chapter registration remain pending.

## 12 Game Variables and Component State

`model`, two-way field bindings, save restoration and independent system data are implemented. state and node.state remain design targets.

Use model for application data independent of view nodes. Controls can share a field: input writes back to the data, and connected text, visibility and styling follow it. Run [author_models.aside](../../examples/author_models.aside): `cargo run -p reel-cli --features window -- dev examples/author_models.aside`.

```aside
let settings = model({volume: 0.8, music: true, palette: {accent: "#70d890"}});
let volume = slider(value: bind(settings.volume), min: 0, max: 1);
let music = toggle(value: bind(settings.music));
let indicator = text("Music", visible: bind(settings.music), color: bind(settings.palette.accent));
settings.palette.accent = "#ffd580";
```

Defaults are copied with their internal aliases and cycles preserved; the original objects remain independent. Data can contain fully constructed class instances, tables, arrays, tuples, strings, byte strings and mutable buffers; closures and host resources such as nodes cannot be defaults. Objects assigned later retain shared identity. Aliases obtained through nested instances, tables or array elements can still expose bindable fields. Ordinary table methods remain available. clear freezes existing connections at their last value and invalidates old references; bind again after adding a field with the same name. Copying advances within a work budget, and cancellation leaves no partial model.

In the language, let/let mut determines whether a binding can be reassigned, model determines whether properties are observable, and state ownership determines how state is saved and released. Each serves a separate purpose.
```aside
let route = state("route", {affection: 0, visited: set()});

route.affection += 1;
route.visited.add("window");
```
state(id, defaults) returns named observable state in the current session; the same ID returns the same object. It is saved and restored through saving, loading, and rollback, and recreated from defaults for a new game. Function-local values and closure captures can also be saved with the complete call stack. Named state mainly serves shared game data and stable lookup during hot reload.
Use `persistent.read(key, default: null)`, `persistent.write(key, value)` and `persistent.delete(key)` for system data. Operations return awaitable requests; catch handles failure, and successful writing means the file has been published. Only a missing record uses default; stored null/false/0 remain intact. Supported data includes null/bool/int/float, string/Bytes, array/table/tuple, preserving aliases and cycles. Tables store own fields only. Convert class instances, nodes, tasks and closures to data first. Records are independent of story slots and ASB versions, live in the same save-directory, and do not appear in saves.list. See [author_persistent.aside](../../examples/author_persistent.aside).

```aside
let preferences = await persistent.read("settings", default: {volume: 0.8});
preferences.volume = 0.6;
await persistent.write("settings", preferences);
```

Reads return ordinary copies; mutations require an explicit write. Loading a story does not overwrite system files, but local copies restore with the snapshot. Reread and apply current settings in the `restored` branch of a `saves.save` request and in new-game entries. Writes already handed to the background service continue across session replacement; unsubmitted requests are not replayed after loading. Games manage data upgrades using their own stored version fields.
model/state copies the default data graph and creates observable properties, preserving internal aliases and cycles in the copy. Nested mutable arrays, table values, Set values, buffers, and registered observable data types all notify through shared mutation hooks; changes made through an internal alias also notify. Session objects assigned later retain their ordinary shared identity and are incorporated into the observable graph. model objects are kept alive by actual references. Temporary component state can be stored in the node's own state slots.
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
node.state(key, defaults) belongs to that component instance. The same local key is independent across different component instances. After node destruction, the state is reclaimed normally according to reachability. Loading restores the component state; hot reload reuses it through stable node keys and state-slot keys. During ordinary execution, a call checks once and atomically adds missing fields, which the next statement can read. During candidate initialization or reconstruction, reads through the candidate view can see fields pending addition, while the live session continues to see the old values. Additions become externally visible only when the candidate transaction commits; failure preserves the old values. Data-structure upgrades use explicit, versioned migrations.
Store global preferences with persistent and read/apply current settings at startup and after restoring a story save. Changes to application state follow ordinary control flow, and their results are preserved when temporary scope effects end.

## 13 Extending Your Own Building Blocks

Ordinary node functions, named arguments, defaults, children and closures are available. The arrive example below uses the implemented finite animation interface.

The smallest unit of reuse is an ordinary fn. Functions returning nodes build scenes; functions returning actions choreograph them; ordinary objects expose parts that callers need to manipulate. Named arguments, children, closures, and module imports work together to support this approach.
Default parameter expressions run in parameter order on each call, after every actual argument has been evaluated. They may refer to earlier parameters and use the expression or block's final value as their result. An explicit return belongs in a function body; it is invalid inside that function's parameter default, including conditional or short-circuit branches. A nested function defined within a default returns through its own body. break and continue may target only loops established inside the default, and throw propagates a parameter-preparation failure.
```aside
fn arrive(node, destination_x, seconds = 0.5) {
    parallel(
        tween(node, x: destination_x, duration: seconds),
        tween(node, opacity: 1, duration: seconds)
    )
}
```
The same arrive function can be used for characters, title-screen buttons, or entire groups of text. Changing images, layouts, and action parameters lets authors build their own component libraries. Custom nodes are registered through the host and participate in the same property, input, rendering, and restoration protocols. New language keywords are reserved for rules that ordinary composition genuinely struggles to express and that multiple creative scenarios need.

## 14 Text, Media, and Extension Entry Points

Use explicit `rich(markup)` for inline styles: `narrator(rich("仰望 [ruby=ほしぞら]星空[/ruby]，[color=#ffd580][b]记住这一刻[/b][/color]。"));`. text, character calls, character.line and narrator accept plain strings or rich text; dialogue models and history retain all styles. Plain strings remain literal. A rich value's read-only `.plain` property returns its body without tags; `.markup` returns the original markup string. Neither read copies the text. Use `rich("[b]姓名：[/b] " + line.text.markup)` to compose a history entry while retaining its styles; escape ordinary strings with `rich_escape`. `model({text: rich("[b]初始内容[/b]")})` works directly as observable state; copying model defaults shares immutable rich values. Runnable example: [author_rich_text.aside](../../examples/author_rich_text.aside).

`game.dialogue().ready` means the current dialogue group has finished its text and all timing cues, including trailing pauses; it is false without an active dialogue. Each row in `lines` also exposes its own `ready`, which stays true for completed rows. Auto, skip and pause policies do not change this state. For example, a cue can use `visible: derive(|| dialogue.ready && !reading.auto && !reading.skipping && !reading.paused)`, where reading is `game.reading()`. Use ordinary text/image nodes, layout and animation to style the cue without comparing text lengths. See the [sentence timing example](../../examples/author_dialogue_timing.aside).

Write sentence timing in rich text too: `[speed=15]慢慢说[/speed]` reveals 15 graphemes per second, `[speed=2x]快一些[/speed]` uses twice the player's speed, `[speed=0]立即出现[/speed]` reveals an immediate segment, and `[pause=0.5]` pauses for half a second at that position. Speed tags can nest; closing one restores its enclosing speed. `[next]` continues after this line finishes and is presented, without changing global auto reading; grouped dialogue requires the marker on every line. Trailing pauses run first. Manual completion, skip, or global text_speed: 0 consumes remaining timing; menus and suspension preserve remaining pauses. Tags are absent from the body, history display and character count; ordinary text controls display the complete body. See [author_dialogue_timing.aside](../../examples/author_dialogue_timing.aside).

Paired, nested tags include `[b]`, `[i]`, `[u]`, `[s]` (bold, italic, underline, strike), `[size=36]`, `[color=#RRGGBB]` / `[color=#RRGGBBAA]`, `[em]` (emphasis dots), `[ruby=注音]基文[/ruby]`, and `[link=词条ID]正文[/link]`. `[[` represents a literal opening bracket; use rich_escape before inserting arbitrary player text. Invalid markup reports the rich call's source location and includes the tag's UTF-8 byte offset in the error value. Ruby cannot nest. Its base and half-size annotation wrap as one unit; the base reveals progressively and the annotation appears when the complete base is visible. Games supply fonts; bold and italic appearance depends on those fonts.

text wraps to its available width by default. width, x/y, padding and scroll determine NVL page placement and bounds; list gap controls paragraph spacing. `text_align` accepts start/end/left/right/center/justify and defaults to start. `line_height` is a font-size multiplier, defaulting to 1.2. Mixed sizes, ruby and emphasis dots participate in line height; reveal changes painting without reshaping. Node color supplies the base color, while explicit inline colors override their spans. Omit inline color to inherit speaker or read-state coloring.

`text(value: rich("查看 [link=star-map][u]星图[/u][/link]"), on_link: |event| show_glossary(event.id))` passes the link ID to an ordinary callback; event.target is the text node. Fully revealed links accept mouse and touch clicks. With text focused, left/right selects a link and confirm activates it. Link input is consumed without advancing dialogue or opening a browser. Removing the node cancels callbacks under its ownership. Runnable example: [author_rich_text.aside](../../examples/author_rich_text.aside); supply fonts/body.ttf first. Use `derive(|| rich(tr(...)))` for reactive language switching and rich_escape for player text passed into translated markup. Submitted dialogue and history preserve their original language and styles. Accessibility currently exposes the body label, without separate semantic link children.

Developers extending script interfaces from Rust can refer to the [Aside host SDK](../aside-host.md). Registered host functions use the same named arguments, defaults and source-location rules as ordinary Aside functions.

Rust extensions can also declare variadic positional arguments with `#[aside(rest)]`. Authors call them with ordinary arguments or `...items`; each call receives a fresh outer array while its elements retain their normal shared identity. Defaults and bound methods use the same rules. This facility is independent of Reel and does not by itself enable action combinators.

Basic text uses real text layout; external catalogs and reactive language switching are implemented. Inline rich text and video nodes are implemented; Aside custom drawing remains a future capability.

Rust custom drawing shares linear-premultiplied color conversion for solid colors, text and images. Both strokes and geometric clips support dashes, including curves, closed paths and dash offsets. An all-zero dash pattern draws a solid line; small miter limits use bevel joins. These capabilities belong to the existing Rust drawing interface; the Aside custom drawing entry point is still pending.

Author audio connects background resource loading, the real mixer and story waits; see [Author audio](#author-audio). Native-device and offline-mixing coverage are recorded separately in [audio verification](../../crates/reel-media/VERIFICATION.md). Video and precise audiovisual synchronization remain design contracts.

`localization.load(locale, source)` reads a UTF-8 Fluent catalog through the asset service. The calling task waits for background IO and parsing. Success replaces that locale's catalog; IO, syntax and duplicate-ID errors can be caught with try/catch and preserve the old catalog. Cancelling the task cancels the load. `localization.set_locale(locale)` selects a language; `localization.locale()` reads the observable language tag.

`tr(source, id: null, params: null, locale: null)` returns an ordinary string, searching the exact locale, its base language, then the source fallback. Without params, the fallback is literal text; with a parameter table, it is a Fluent pattern. Translations always use Fluent. Named string/int/float parameters and plural selectors are supported. Parameters are never reparsed as templates; missing parameters and other formatting errors are catchable. Fluent preserves bidirectional isolation marks so names in another writing direction cannot reorder surrounding text.

A per-call `locale` lets one view retrieve several languages without changing the global selection. For bilingual dialogue, include both lines in your view and bind reveal independently on the main line. Missing catalogs or translations use the same source fallback. Both calls share loaded catalogs rather than copying a language pack.
```aside
column(gap: 6) {
    text(tr("你好。", id: "greeting", locale: "zh-CN"), font_size: 28);
    text(tr("你好。", id: "greeting", locale: "en"), font_size: 18);
}
```

Use `derive(|| tr(...))` for live UI: changing language reevaluates subscribed expressions. Ordinary tr calls, submitted dialogue/choices, input drafts and stored history strings retain their evaluated content. For live chat history or choice presentation, retain IDs, source text and parameters and call tr in the display binding; branch keys remain independent of labels. No special LocalText type is required.

This fragment comes from [localization.aside](../../examples/localization.aside), with English translations in [locales/en](../../examples/locales/en/). Bundle them with the commands below, use target/reel-artifacts/localization as the asset root, and supply fonts through the host or game.configure.

```aside
localization.load("en", "en.ftl");
localization.set_locale("zh-CN");
let data = model({name: "Mio", count: 2});
text(value: derive(|| tr("你好，{ $name }。", id: "welcome", params: {name: data.name})));
button("English", on_click: |event| localization.set_locale("en"));
```

Usually write `tr("雨停了。")` directly. Extraction gives identical source text the same ID; moving code or changing indentation preserves it, while editing the text creates a new ID. Use `tr("雨停了。", id: "opening")` to distinguish meanings or retain an original content ID across edits. Omit the parameter table when the source has no parameters.

```sh
reel loc extract examples/localization.aside --output source.ftl
reel loc extract examples/localization.aside --output-dir source-catalog
reel loc bundle examples/locales/en --output target/reel-artifacts/localization/en.ftl
reel loc check target/reel-artifacts/localization/en.ftl --locale en
```

Extraction follows imported modules and includes source locations and function context. `--output-dir` mirrors module paths: story/day_one.aside becomes story/day_one.ftl. Identical IDs appear once in the first module by sorted relative path, with all call sites retained. Literal text is escaped automatically; source text with params is checked as a Fluent template. Dynamic text or IDs are reported with their locations and need a catalog supplied by the content generator. `--overwrite` explicitly replaces the selected outputs. Directory mode leaves other files in place and reports old FTL files with no current messages; review and remove them before bundling, or extract to a fresh directory. Keep translations separate from generated source catalogs; extraction does not implicitly merge or modify them.

Keep each language's translations in a separate directory, organized like the story and UI: locales/en/story/day_one.ftl and locales/en/ui/settings.ftl. Shared terms can live in common.ftl. `loc bundle` merges those files in relative-path order into one ordinary FTL outside the source directory; only maintain the module files, and generate the distribution catalog during the build. Cross-file references work normally. Duplicate message or term IDs and syntax errors report the original file, line and column; failed builds preserve the old output. Replacing an existing bundle requires `--overwrite`. Moving a translation between files does not change its identity. Runtime code continues to load one catalog per language; only load languages the game uses. Parsing and static text preparation happen during loading, with no per-translation directory scan. Localized asset substitution remains a future capability.
video is an ordinary node; audio provides tracks, queues, loop regions, filters, and synchronization groups. play returns an awaitable activity, stop leaves it at its current state, and finish is for activities with an endpoint. Tracks and resources are managed by stable identity. Device reconstruction and seek preserve or rebind logical waits; errors have explicit terminal states.
custom_node can directly register state, measure, record, route, and event implemented in Aside. measure/record are read-only layout/rendering callbacks with execution budgets. route synchronously decides event consumption, capture, and focus, then event runs as an ordinary suspendable task. A minigame node can therefore prevent the same input from advancing the story while still being able to wait for resources in its application callback. New low-level graphics and platform capabilities are registered through native modules.
Camera, Mesh, Material, and Effect provide perspective stages and programmable effects, with state and resource identities still included in saves. Each backend declares its supported implementations and fallbacks in a capability list. Live2D, system text-to-speech, mobile/Web packaging, and external services are adapter capabilities within the full target scope. Their specific API documentation will provide examples in the same style alongside verified implementations.
External writes, such as network operations and purchases, use separate operation records and idempotency strategies. Unknown outcomes are queried and verified first; replay is read-only by default. Their lifetimes are explicitly distinguished from those of ordinary rollback-capable game variables. Author-defined components and script libraries can always extend scenes, layouts, choreography, and application logic.

### Finding and Handling Errors (Design Contract)
The player reports the specific reason a host interface rejected a call, alongside its source file, line, column and error kind. In catch, use `f"{error}"` to display a CoreError: string details take precedence, otherwise it uses the fixed error message. Ordinary thrown values retain their original semantics.

Use try/catch for resource loading, media playback, or errors you throw yourself. catch receives the original value: a thrown string remains a string, and a thrown object is the original object. Use throw; to propagate the same fault. Writing throw error; throws again at the current location, retaining the previous fault as a cause.
core.error_info() provides the current catch's error code, original location, call stack, and causes. An ordinary helper called while handling the error can read the same information. Keeping the returned view keeps its fault record alive. finally is for synchronous cleanup; ordinary helpers may return synchronously within it. If cleanup raises another error, diagnostics show which return result or fault it replaced. The engine retains this information across suspension, host callbacks, and saves. See the [language specification: fault lifetimes](../aside-language.md#二十三-完整故障记录与生命周期).

## 15 Keeping Documentation and Code in Sync
The project lead maintains this guide. Each API is marked as designed or implemented, with its applicable version. Once implemented, examples are extracted as source snippets checked alongside the code: parsing, type and interface checks, and any necessary behavior verification use the same examples. The Chinese and English editions reference the same example files; translation updates are committed with interface changes.
When an interface, behavior, default, or cancellation rule changes, the same PR updates the implementation, specification, both language editions, and affected examples. Advanced uses still in planning are labeled separately. Users can begin creating with the implemented sections. When composition proves difficult, they can report minimal source code and the intended scene so the project lead can refine the shared interfaces.
The repository commit containing this guide is its version reference. Feature status is marked by section; interface changes update the Chinese edition, English edition, and shared examples together.
The core composition rules are ordinary functions, unified nodes, action values, and scopes. The style, temporary-interface, offset-animation, and variable examples constrain the implementation together. Once the capabilities are complete, the same source examples serve as acceptance tests.

## Verified Source Execution Scope

The compiler, VM and session pass native Windows execution checks covering collections/members, closures/default arguments, exceptions/finally, children/for, wait/resume, GC and cancellation. The author-stage example also passes actual check/build, ASB loading and SessionVm execution: existing scene nodes retain identity while text and appearance change between two dialogue lines. The window entry point uses the same program and presented-input path. The Chinese author scene also completed offscreen GPU readback on Windows with an AMD Radeon RX 7900 GRE using Vulkan. Native window lifecycles and hot-plug behavior still require separate verification.

Run the [minimal Aside story](../../crates/reel-engine/examples/story_contract.aside) with its [headless host](../../crates/reel-engine/examples/story_contract.rs):

```sh
cargo run --locked -p reel-engine --example story_contract
```

The host advances dialogue and selects the first option through actual compilation, module initialization, VM waiting/resumption and owner retirement. Symbol and name-use links to the same native coalesce; conflicting registrations still reject. The host linkage package uses the requested content version independently of native ABI v1.

Entries must be ordinary module-level functions with no required parameters or captured environment; private functions are supported. Dialogue and choices use content layout and scrolling without a separate two-row or two-option cap. Both source and ASB window launch commands are implemented. Natural completion preserves the final scene; closing the window finishes cleanup. The headless example above focuses on dialogue and choices; use the author-stage example in section 1 for basic node composition. Finite animation and button events now work. Video, delegates/hooks, property-bags and save/restore remain unimplemented.

## Runnable Buttons and Callbacks

button shares the stage tree with every other node. It accepts label, on_click, children and common appearance properties. Omit label to compose images, text or other components through children, laid out like a stack. enabled defaults to true; disabling a parent disables its interactive subtree. Assign a new closure or null to btn.on_click to replace or clear its handler. This code matches [author_buttons.aside](../../examples/author_buttons.aside).

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

A click passes the original button as event.target. A callback can await an animation. Synchronous code runs until an explicit await, an ordinary call actually waits, or the task ends. Awaiting an already completed object also gives other ready tasks a turn. Ordinary calls and engine budget slices do not interrupt read-modify-write operations that contain no await. Multiple callbacks may await one activity; cancelling a consumer does not stop the shared producer.

The emitting button owns its callbacks and is the default owner of run inside them. Removing the button, clearing its parent or replacing the stage cancels those callbacks and activities. A callback that removes itself does not execute subsequent statements. narrator/choose currently belong to the main story; calling them from a callback raises a catchable context error. game.start and direct node property bindings are implemented; general event subscriptions remain unimplemented.

Mounted button handlers and activities remain usable after the main entry returns; idle sessions do not repeatedly execute the VM. Once callbacks and activities have ended and no mounted handlers remain, the session finishes and preserves its final scene. A button click does not also advance dialogue. The built-in host supports mouse and touch activation, Tab focus navigation and Enter/Space activation. A gamepad D-pad or left stick navigates by the displayed positions of controls, and long choice lists scroll with focus; the south button confirms, while the east button and Escape send Back to the current UI. Confirmation activates on a paired release. Modal changes, focus loss and disconnection cancel old input; Back does not directly exit the application. Multiple contacts can operate separate controls at once. Releasing outside a button does not click it; system cancellation or suspension releases the interaction. Existing buttons work on touch screens without a separate implementation; run [author_buttons.aside](../../examples/author_buttons.aside). The built-in desktop host integrates the system IME; system accessibility adapters remain future work.

Use the dev/build/run commands in section 1 with this file. Nodes, input and scheduling do not depend on Winit/WGPU. The window feature selects the built-in desktop combination; other hosts can reuse the same session and presentation interfaces.

For ports and embedding, use PlatformHost, Presentation and StoryRuntime from reel-player, and RenderBackend from reel-render. Enabling reel-player/winit alone provides the Winit host without WGPU; reel-render/wgpu provides the independent WGPU backend. The built-in player uses these interfaces too: only an actually presented new frame updates input geometry, temporary resource pressure retries after submission completion, and permanent refusal preserves the previous frame. Story animations pause while the application is suspended and continue on resume; closing still completes task cleanup. Authors do not need to change their scripts or select a graphics API. Touch uses portable PointerEvent values; hosts enabling winit can reuse TouchInput to convert native contacts. Rust games and hosts can use reel_ui::input::ActionMap to bind keyboard, pointer and gamepad controls, read held values and receive press, change, release and cancellation events. NavigationInput supplies configurable thresholds, repeat timing and remapping. The independent gamepad feature uses Gilrs for desktop devices without requiring Winit/WGPU. The Winit adapter forwards system IME events and candidate-window positions; system accessibility remains unimplemented. See the [player documentation](../../crates/reel-player/README.md) for host integration and reproducible checks.

When images first appear or a save restores a cold cache, the backend batches uploads within available capacity, resumes large images by row, and reuses fully submitted textures. The previous complete view remains visible while waiting; upload completion does not advance input geometry. Rust hosts use `RenderEvent::ResourcesAvailable` to wake waiting presentation requests. Requests that genuinely exceed the host resource budget still report an error.

## Modal menus and focus

Before opening a save menu, use `let preview = await stage.capture(width: 256, height: 144);` to obtain PNG Bytes of the last presented frame. Display it with `image(preview)` and store it with the execution snapshot using `await saves.save("slot/1", thumbnail: preview)`. Listed slots expose `has_thumbnail`; load previews on demand with `await saves.thumbnail("slot/1")`. Capture accepts omitted dimensions or a single dimension, supports owner/cancellation, and requires no window or GPU access in author code.

Dialogue indicators, portraits and other controls can animate while players confirm with a pointer, touch, keyboard or gamepad. If the dialogue changes, revealing finishes, the stage root is replaced or the modal scope changes after a press, a new press is required so the old gesture cannot confirm new content.

Every shared node has a writable `modal` property, defaulting to false. A visible modal subtree owns input: clicks outside it and unhandled keyboard events cannot advance the story. With multiple modals, the last one in paint order takes input; node composition determines paint order. Hiding or removing the node releases the scope. Setting enabled to false still blocks underlying content.

Opening a menu focuses an eligible control inside it. Tab/Shift-Tab cycle within the menu; closing a nested menu restores the previous scope’s focus, and closing the last menu restores the original control if it remains eligible. The input scope changes when the new frame is actually presented and cancels dragging outside it. Run [author_modal.aside](../../examples/author_modal.aside) with the dev/build/run commands in the first section.

Keyboard activation requires pressing and releasing Enter or Space on the same button. Changing focus, hiding, disabling, or losing window focus cancels an unfinished key press.

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

## Named actions and input nodes

Focused controls handle input first; unclaimed input reaches the nearest matching ancestor input node. A menu can bind Escape/gamepad Back and a scene can bind skip without repeating those bindings on every child button. Focus changes cancel held actions. Input stays within the modal scope, and text editors retain keyboard and IME input.

`input` is a shared stage node with stack layout. A root input node receives unclaimed keyboard/gamepad input even before a control gains focus; focused child controls retain priority. It binds keyboard, gamepad and pointer controls to named actions. This example matches [author_input.aside](../../examples/author_input.aside); run it with the dev/build/run commands in the first section. Supply `fonts/body.ttf`, or change the path in main.

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

Each key in bindings names an action; its value is an array of binding descriptions. Controls use physical key names (KeyA, KeyD, Space, Escape), standard gamepad buttons (South, East, DpadLeft), axes (LeftX, LeftY, RightX, RightY, LeftZ, RightZ), or pointer buttons (PointerPrimary, PointerSecondary, PointerAuxiliary, PointerX1, PointerX2). Axes range from -1 to 1, with positive Y pointing down; trigger buttons range from 0 to 1. scale defaults to 1 and dead_zone to 0. Values outside the dead zone are normalized again before scaling; contributions are added and clamped to -1 through 1. Releasing one device or touch contact does not release an action still held by another.

Events carry target, action, value and phase. Phases are pressed, changed, released and cancelled; keyboard repeat does not produce another pressed event. `node.value("move_x")` reads the latest held value and rejects unknown actions; event.value retains the value of that particular event. Callbacks may await and belong to their node. Assigning `node.bindings = new_table` cancels the old state and installs the new map; edits inside the original table do not implicitly update it. Assign a closure or null to node.on_action.

The input node receives keyboard and gamepad actions while it has focus. When a child control gains focus, it takes ownership and cancels the parent input node's held actions, so the parent cannot steal controller confirmation. Use modal: true to give an independent interaction scope focus immediately. Nonmodal nodes can receive focus through Tab; bound pointer presses also focus the node and capture that contact. Hiding, disabling, losing focus, changing modal scope or clearing the presented frame cancels held actions. Removing a node ends its tasks and activities. A consumed action does not also navigate or advance dialogue. Ordinary buttons and dialogue activate with the primary pointer button or touch; other mouse buttons remain available to author actions.

Rust hosts use `InputDeviceId::{Keyboard, Pointer, Gamepad}`, `InputBindings`, `NodeKind::Input` and `NodeAction`. Route native controls through TextEvent::Input first; its Handled result reports whether an action in the current scope claimed the control. Unbound controls can feed NavigationInput within that same UI; raw events cannot cross modal boundaries. The fallible AsideSessionUi::presented and clear_presented calls deliver routing cancellations to the VM after presentation acceptance or revocation. Wake StoryRuntime when those calls create tasks.

## Value controls and settings

`checkbox` takes the same arguments as `toggle`, with a check box appearance instead of a switch. `radio(option, label: null, value: null, on_change: null, ...)` represents one choice. Bind each item's value to the same model field to make an exclusive group, without maintaining separate booleans. option can be changed or bound; an unmatched value selects no item. Values follow list key equality. Arrow keys, controller directions and Tab move focus; confirm, mouse or touch selects. Selecting the current value does not call on_change. Programmatic changes are silent; user changes commit the binding before delivering `{target,value}`.

```aside
let settings = model({sound: true, language: "zh-CN"});
column(gap: 12) {
    checkbox("Sound", value: bind(settings.sound));
    radio("zh-CN", "简体中文", value: bind(settings.language));
    radio("en", "English", value: bind(settings.language));
}
```

Both compose children and accept `decorated: false` for a fully custom appearance. Check boxes read value; radio items can compare value with option in derive to select their own images or colors. Bindings maintain selection, without manual visual updates in on_change. See [author_selection.aside](../../examples/author_selection.aside); supply fonts/body.ttf before running.

`slider(value: 0, min: 0, max: 1, step: null, on_change: null, ...)` provides a horizontal slider; `toggle(label: null, value: false, on_change: null, children: [], ...)` provides a switch. Both share stage position, size, colors, visibility, enabled state and modal scope; toggles also compose child nodes. The following [author_settings.aside](../../examples/author_settings.aside) runs after supplying fonts/body.ttf.

Sliders require finite increasing min/max and a finite value inside that range. step is null or finite and positive, anchored at min, with both endpoints reachable. Scripts may assign any in-range value regardless of input step. Assigning min/max must also leave the current value in range; invalid writes preserve the previous state. Mouse/touch dragging, arrow keys, Home/End, Shift acceleration, left/right controller navigation and accessibility actions share one control.

Accepted user changes update node.value before on_change runs; events contain target and that change’s value. Script assignments are silent. Callbacks may await and are cancelled when their node is removed. Value and layout updates preserve dragging; changing range/step ends the old drag, and interaction resumes after the new frame is accepted. Controls retain their value without a callback. Node values support bind/unbind; derive supplies read-only values for presentation and cannot be a slider/toggle writeback source.

Sliders also accept child nodes. With `decorated: false`, paint the track, thumb and hover/focus feedback using images or ordinary stage nodes; slider retains input and accessibility. The track spans the full content width with no built-in thumb inset; use padding when the skin needs margins. See [author_slider_skin.aside](../../examples/author_slider_skin.aside): `reel dev examples/author_slider_skin.aside` (provide `fonts/body.ttf`).

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

Rust hosts submit AccessKit ActionRequest through `SessionUi::handle_access` / `AsideSessionUi::handle_access`, preserving accepted-frame and modal rules; the platform host supplies the native screen-reader connection.

## Connecting stage properties

`text_input` edits a single line. Its value can bind to a writable model field; on_change runs after committed text changes, and on_submit runs on Enter. Both events carry target and value. IME preedit neither writes to the model nor triggers these callbacks. It supports cursor movement, selection, deletion, copy, cut, and paste. The system IME owns conversion and candidate selection; embedding hosts forward portable IME events and handle candidate-window position signals. Assigned values must be single-line strings; newlines in user input or paste become spaces. Style it with common node properties and compose your own background and focus indication; it adds no default border.

With an explicit height, input text is vertically centered in the content area together with its caret, selection and IME position. A regular text node with an explicit width measures its automatic height using all wrapped lines at that width.

Inline event links show an underline on hover or keyboard focus without reshaping text. A custom scrollbar should clamp its drag fraction to 0 through 1 before writing scroll_y, so its visuals do not follow an out-of-range value awaiting layout feedback; see author_scrollbar.aside.

```aside
let profile = model({name: ""});
text_input(value: bind(profile.name), width: 280, height: 40,
    color: "#202020", background: "#ffffff",
    on_submit: |event| { profile.name = event.value; });
```

Runnable example: [author_text_input.aside](../../examples/author_text_input.aside).

For scripted typing or keyboard games, use `input(on_key: |event| { ... })`. Events provide code, key, phase and shift/ctrl/alt/meta; phase is pressed/repeated/released/cancelled. Keys are delivered separately, and focus loss cancels held keys. Focused buttons consume their confirmation keys; other unclaimed keys bubble to an ancestor on_key, so clicking a language button does not stop scripted typing. Focused text editors retain all keyboard and system IME events. Omit bindings when no action mapping is needed, or combine on_action for gamepad and pointer controls; action mappings still follow focus ownership. Example: [author_keyboard.aside](../../examples/author_keyboard.aside).

`bind` and `unbind` work with existing node properties. These sliders share a position value, and the text position and visibility connect directly to the controls, without callbacks that copy state. Run [author_bindings.aside](../../examples/author_bindings.aside) in a window using the font setup from section 1.

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

## Author audio

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

`audio.play` reads, decodes and plays in the background, returning an activity immediately. `await` returns `Completed` or `Stopped`; loading and device errors can be caught with `try/catch`. `volume` is a nonnegative linear gain: 1 preserves the source level and 0 mutes it. pause/resume retain position, stop does not restart playback, and repeated waits retain the same result.

`audio.queue(activity, source, start: 0, looping: false, loop_start: 0, loop_end: null)` appends a music or narration segment to the same activity and returns null. The activity retains its owner, group, volume, pan and controls. Prepared segments join at the mixer boundary; a late segment holds the content clock and outputs silence until ready. New audio prepares in the background and starts when its creating task begins an explicit await, an ordinary call actually waits, or the task returns, with the segments submitted in that synchronous execution segment prepared together. Awaiting an already completed object also commits this batch. Internal work slices do not start playback; an uncaught error or cancellation discards audio that has not been submitted yet. A queued successor exits the current loop at its next end; without a successor, looping continues. position and seek refer to the current source. Stop or owner retirement cancels the remaining queue; a loading error fails the original activity, and a completed activity cannot accept new segments. Saves preserve the current segment, position and remaining queue.
Runnable example: [author_audio_queue.aside](../../examples/author_audio_queue.aside) uses bundled clips for intro → loop → outro and continuous narration. Replace the clip paths with your own audio.

```aside
let music = audio.play("assets/intro.ogg", group: "music");
audio.queue(music, "assets/loop.ogg", looping: true);
narrator("Continue to leave the loop after this pass.");
audio.queue(music, "assets/outro.ogg");
await music;
```

Supply your own three audio files for this example. Queue capacity and PCM residency use the host's existing audio budgets. Exhausted capacity reports an error; the script does not configure additional limits.

Images, fonts and audio share the host's resource input capacity. When concurrent reads temporarily fill it, later reads wait for existing work to release its input. A single file larger than the configured input capacity still fails with its path and byte counts in the diagnostic.

Audio decoding accounts for overlapping old and new storage only during buffer growth and the final transfer to shared storage. Ordinary packet decoding reuses existing PCM capacity. Insufficient memory reports the bytes needed by that decoding stage.

`start` selects the starting position in seconds, and `audio.position(sound)` reads the current content position. Loading returns the requested start, pausing holds the position, and a finished activity retains its final position. With `looping: true`, use `loop_start` and `loop_end` to choose a loop; the default covers the whole track. A start before loop_start plays the introduction first, then wraps from loop_end to loop_start. A start beyond loop_end wraps into the loop. The end is exclusive, and the interval must fit within the source and contain at least one frame. Keep the default loop bounds when looping is disabled.

`audio.seek(sound, seconds)` seeks the same sound and returns null; do not await the call. The position is in finite nonnegative seconds. Paused sounds stay paused; playing sounds continue with their gain and loop settings unchanged. During loading, seeking changes the starting position. Non-looping positions beyond the end clamp to the end and complete naturally; paused sounds complete after resuming. Looping uses the same rules as start. Consecutive seeks keep the latest target, which `audio.position` and an immediate save observe until the mixer applies it and reports actual progress. Finished activities do not restart; `await sound` still waits for the original playback result.

Combine these calls to switch music at the same content position. Fade-in starts once the new track finishes loading and begins playback:
```aside
let next = audio.play("assets/music-next.ogg", group: "music", looping: true,
    start: audio.position(music), loop_start: 4, loop_end: 64, fade_in: 2);
audio.stop(music, fade: 2);
music = next;
```
Position is an explicit read and does not continuously refresh the UI. Saves retain the current item, its position and loop bounds, and the remaining queue for every audio group. Ended activities retain their result and do not replay. To replay a voice line from the beginning, explicitly seek or play it again.

Use `audio.group_volume("music", value)` for music/voice/effects/ui, or master for overall volume. Sound, group and master gains multiply; group settings also apply to later sounds. `audio.pan(sound, value, duration: seconds)` moves stereo position from -1 (left) to 1 (right), with 0 centered. `audio.volume` and `audio.group_volume` also accept duration; omission applies the change immediately. `fade_in` starts when playback actually begins. `audio.stop(sound, fade: seconds)` resolves await with Stopped after fading out; loading or paused sounds stop immediately. Pause retains playback position while volume/pan envelopes continue; output suspension freezes all envelopes. Pausing during a fade-out stop finishes the stop immediately. Volume transitions interpolate decibels with a -60 dB silence endpoint. New commands continue from the current value; controls during loading update initial playback values.

By default, sound belongs to the current callback node, or to the session in the main story. Use `owner: node` to choose a node: removing it cancels loading and playback together. Cancelling a waiter does not stop shared playback. Playback, node cancellation and window shutdown use the same resource retirement path.

The Windows player submits stereo audio to the system, including when the default output device uses multiple channels.

Provide an Aside check entry that makes its own assertions and finishes, then run the command below to check both source and ASB. The host uses the project resource root, isolated temporary saves, real CPU layout and accepted frames, with actual audio decoding and mixing enabled by default. It consumes 48 kHz stereo PCM offline according to monotonic elapsed time, without audible output. Each source and ASB run defaults to a 90-second limit. For longer scenarios, set `REEL_SCENARIO_TIMEOUT_SECS` to a positive u64 integer in seconds; invalid values fail immediately. This variable affects external project checks only; built-in tests keep their fixed 90-second limit. The host supplies no automatic clicks, choices or skipping. This can check audio waits, positions, queues and restoration; audiovisual synchronization still requires device testing.

```powershell
$env:REEL_SCENARIO = "path/to/project/check.aside"
$env:REEL_SCENARIO_TIMEOUT_SECS = "600"
cargo test -p reel-cli --test project_scenarios external_project_scenario -- --ignored --nocapture
```

WAV/PCM, Ogg/Vorbis, FLAC and MP3 support predecoded playback. PCM is charged to the host memory budget without a separate duration limit. Starting positions, loop regions, seeking during playback and save restoration are connected; streaming long tracks remains part of the full design. Completed means mixer consumption, so it cannot establish precise hardware synchronization. Physical-device coverage is recorded in [audio verification](../../crates/reel-media/VERIFICATION.md).

Runnable example: [author_audio.aside](../../examples/author_audio.aside), using examples as the asset root and the same font configuration as other author examples. Its chime.wav is an original short sine tone supplied by this repository.

```powershell
./target/debug/reel.exe check examples/author_audio.aside
./target/debug/reel.exe dev examples/author_audio.aside --assets target/reel-artifacts/author-assets
```

### Window and presentation settings

Run [author_window.aside](../../examples/author_window.aside) with your own `fonts/body.ttf`, using the same assets directory as the audio example above.

`window.info()` returns the current client area's logical width/height, scale_factor, fullscreen, vsync, windowed_width/windowed_height, capabilities and resolutions. resolutions lists the current display's available sizes converted to logical pixels; these are separate from the stage canvas. Check capabilities.resize/fullscreen/vsync before showing corresponding controls. A host without a window reports all capabilities as false.

```aside
let mut display = window.info();
if display.capabilities.resize {
    display = await window.configure(width: 1280, height: 720);
}
if display.capabilities.fullscreen {
    display = await window.configure(fullscreen: true);
}
```

configure preserves omitted fields and returns an activity; await receives the actual state after platform application. fullscreen means borderless fullscreen. Sizes configured during fullscreen are retained for the window after leaving it, without changing the monitor mode. A game can disable window-size selection while fullscreen. `await window.configure(vsync: false)` changes presentation synchronization; idle rendering remains demand-driven. Story saves do not restore window state; store player preferences with persistent. A second change while one is pending reports Busy; disable settings controls during application. game.configure continues to configure the canvas independently.

### Sprite particles

`particles(source, seed:0, ...node properties)` creates one stage node with a shared sprite and coordinate space. Its source must be a whole PNG/JPEG sprite; prepared `.reel-image` sources are supported by `image` nodes only. Each `emit` appends a batch and returns its own activity. The example uses your own `assets/petal.png` and `fonts/body.ttf`; see [author_particles.aside](../../examples/author_particles.aside).

```aside
let petals = particles("assets/petal.png", width: 1280, height: 20, seed: 42);
stage.replace(petals);
let falling = emit(petals, burst: 0, rate: 80, duration: 10,
    lifetime: 8, velocity_y: 90, velocity_x: 15, spread_x: 30,
    size: 18, size_spread: 6, spin_spread: 60);
await falling;
```

`emit(node, burst:1, rate:0, duration:null, lifetime:5, lifetime_spread:0, velocity_x:0, velocity_y:80, spread_x:20, spread_y:20, acceleration_x:0, acceleration_y:0, size:12, size_spread:4, size_end:null, opacity_start:1, opacity_end:0, spin:0, spin_spread:30, x:0, y:0, owner:null)` describes this batch. The default emits one particle immediately. Set burst to zero for a stream whose first particle is born after 1/rate seconds. duration null keeps a positive-rate stream emitting; a finite duration includes births at its endpoint. rate zero emits only the burst, even with duration null. duration zero still emits the burst. The activity completes when its last particle actually dies.

Spreads are uniform half-ranges around the base; size is the sprite width and preserves image aspect ratio. Size spread cannot exceed size; lifetime spread cannot exceed the positive lifetime. Each particle has its own seeded lifetime. size_end null keeps its randomly chosen birth width; a nonnegative final width interpolates over its lifetime. Birth positions are uniform within the width/height captured from the node by each emit (omitted dimensions are zero). The batch x/y is the rectangle origin in node-local coordinates; both accept positive or negative finite numbers and are fixed at emission. Moving the node still moves every batch; use emit x/y to add effects at different positions on the same node. Velocity uses logical pixels/second, acceleration pixels/second² and spin degrees/second. Source and seed are read-only; the node's position, rotation, scale, visibility, opacity and clipping still affect every batch.

`pause(falling)` freezes only that batch's births and motion, `resume(falling)` continues it, and `stop(falling)` clears that batch and returns Stopped from await. `finish(falling)` clears a finite batch and returns Completed; it rejects continuous emission. Calling emit again leaves previous batches running. The node seed, successful emission order and elapsed time reproduce the same particles regardless of frame timing; successive emissions use different samples. Retiring the node cancels all its batches; retiring an owner or scope cancels its batches. Hiding does not pause. Cancelling a waiter leaves emission running. Terminal results can be awaited again. Save/restore retains each batch's configuration, time, pause and owner, plus the next emission sequence. No script runs for each particle.

This overlapping spray emits twice on the same node. Common styles can be ordinary functions around emit:

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

### Querying presented node geometry

`stage.geometry(node, relative_to: overlay, owner: root)` waits for a frame actually presented after the request and returns one read-only geometry snapshot. width/height describe the node itself; bounds supplies x/y/width/height in the reference space, and transform's a/b/c/d/tx/ty maps local points into that space. Omitting relative_to uses logical stage coordinates. A sibling overlay or ancestor may be the reference; scrolling, cameras, rotation and mirroring are included, without clipping. The query uses the published layout recorded with that frame; property edits that have not been presented cannot be attached to an older frame. A retained picture shown while the stage is preparing cannot complete a new query.

```aside
overlay.visible = true;
overlay.opacity = 0;
let geometry = await stage.geometry(thumbnail, relative_to: overlay, owner: root);
indicator.x = geometry.bounds.x + geometry.bounds.width / 2;
indicator.y = geometry.bounds.y;
overlay.opacity = 1;
```

Hidden nodes have no current layout to query. If the accepted frame still hides or omits either node, or the reference transform is not invertible, catch receives a value whose code is GeometryUnavailable. Transparent nodes still participate in layout. Retiring the target, reference or owner cancels the request; inspect `core.error_info().code` for cancellation rather than assuming the caught value has a code field. Results do not update every frame. For virtual rows, await list.scroll_to(key) before querying the row node. Complete example: [author_geometry.aside](../../examples/author_geometry.aside).
