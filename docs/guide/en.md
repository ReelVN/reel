# Reel Game Creator Guide

Design version 1.1 · Development baseline. Implementation status is stated in the guide.

[中文](zh-CN.md) · [文档目录 / Documentation index](../README.md)

## Reading This Guide
Reel is currently a development prototype that runs simple interactive scenes; the full creation and distribution workflow is still being built. Start with the [basic scene](../../examples/author_stage.aside), [animation](../../examples/author_activities.aside), or [interactive buttons](../../examples/author_buttons.aside). The [capability table](../README.md#当前状态) states the actual boundaries, and [roadmap #94](https://github.com/jhq223/reel/issues/94) tracks implementation order.

This guide starts with the scene you want to create and explains how to use Aside to write stories, compose UI, choreograph scenes, and extend effects. Implemented features are ready to use; the remaining sections describe the target design and identify capabilities that are not yet connected. The Chinese and English editions share the same examples and interface names. Reel Engine Architecture Design describes the runtime; Aside VM and Language Design defines the language rules.
Authors work with three basic concepts: functions organize reusable content, node trees describe scenes, and activities represent changes over time. Ordinary functions, basic node composition and sequential stories now work together. Activities and advanced creator APIs retain their stated implementation status.

Currently runnable: text, image (PNG/JPEG), row, column, stack, stage.replace, trailing children blocks with if/for composition, updates to existing node properties, local subtree editing, finite animations with waiting and control, and narrator/choose waiting and resumption. Check/build and the player share the same registered Reel interfaces; dev runs source, and run executes .asb directly. See the complete [author-stage example](../../examples/author_stage.aside) and the commands in section 1.

Author button nodes and awaitable on_click callbacks now work; see the runnable example below. model, bind/unbind, derive, dynamic view and author dialogue/choice views are available; audio is connected to creator source; video and save/load remain planned. Script initialization and projects with multiple source modules are available; asset packaging and release manifests remain future work. Later examples using these capabilities describe the target design rather than currently runnable examples. See the [current implementation status](../README.md#当前状态) for the broader boundary.

Historical verification scope: the R01/R02 Rust renderer and diagnostic window passed independent checks on software Vulkan and Xvfb/X11. Those records establish behavior only for their recorded commits and environments. Source and ASB now connect to the real window entry point; the old diagnostic program's scope no longer describes the author commands. These checks do not establish physical-GPU, hot-plug or platform-lifecycle behavior. See [rendering verification scope](../../crates/reel-render/R02-REPORT.md).

## 1 A Minimal Project and Shared Conventions
Start the project from main in main.aside. check/build/dev accept a source file or project directory; run executes the built .asb. See the runnable multi-file [author_project](../../examples/author_project/main.aside). The title screen below still uses the planned game.start service.
Call game.configure(title: "窗边", width: 960, height: 540) in main to set the title and logical canvas using ordinary expressions. Omitted fields keep their values. Without a canvas, coordinates follow the viewport; a configured canvas uses contain scaling and the same transform for drawing and input. When first setting one dimension, the other uses the corresponding value from 1920×1080. The initial window is 960×540. fonts accepts a font file string or an ordered array, such as game.configure(fonts: ["fonts/body.ttf", "fonts/cjk.ttf"]). Paths are relative to the asset root; provide at least one font file before the first presentation. The font property on text and button selects a family or comma-separated fallback list, for example text("标题", font: "Noto Serif SC, system-ui"); font_size controls size. Font files share loaded data; system-ui denotes the configured default list. The player uses only fonts supplied by the work and reports missing font configuration.
Reel makes all currently registered basic nodes and story services available automatically: text, image, button, row, column, stack, stage, narrator and choose. Each source file can use them without an import. Your module declarations, local bindings and explicit imports take precedence over these defaults. If you define your own text function, `use reel::prelude::text as reel_text;` gives the Reel constructor a separate name. Explicit prelude imports remain ordinary valid imports. This default environment belongs to Reel; standalone Aside has no Reel names. Actions and complete application services retain the design status stated later. Authors build their pages and visual style with ordinary functions. All documents use fn, let, let mut, |parameters| closures and use imports. Except for complete module/function examples, standalone snippets belong inside a running function and reuse earlier variables. Ordinary statements end with semicolons; a function's final expression can be its return value. Object literals use {field: value}. The following imports use the implemented module layout; the title button's game.start call describes the planned game-entry service.
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

### Available Check, Build and Run Commands
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

Dev and build select main::main by default; `--entry main::name` selects another function. A missing or invalid entry is reported during the build. The entry may be private but cannot require parameters or capture an enclosing environment. Run loads the package initializer and entry directly without recompiling source. Source paths and positions are stored in the package, so runtime errors still point to the original .aside file. Existing output is replaced only with --overwrite.

With window enabled, source and built packages use the real window, text layout and presented-input path. Synchronous component construction continues within one execution time slice, updating the scene when it waits or the slice ends, without displaying a separate frame for every node. When waiting for dialogue or a choice, the player completes the current GC cycle and then sleeps until input arrives. Natural completion keeps the final scene in the window. Close the window to exit and finish cleanup; Escape sends Back to the current UI.

Projects read only modules imported by the entry. game::ui::scene resolves to ui/scene.aside beside the entry file; --module first=../library supplies an external source root. Aliases for the same directory share one environment. Function cycles can link; top-level reads of uninitialized bindings report ModuleInitializationCycle at the read. build stores the complete graph and entry in one movable .asb. The same main executes its initialization settings in source and ASB runs. Assets default to the source or ASB directory and can be selected with --assets; .rpk and release.json belong to the later distribution pipeline.

Source files, embedded source text and string constants share the actual package budget. Long text and concatenation advance in execution slices; slicing never truncates text or imposes a dialogue length limit.

Left-associated operations and member or call chains compile in source order without consuming nesting allowance for each link. Actual nesting through parentheses, blocks and subexpressions uses the host's configured compiler resource budget.

### Available Ranges, Collections and Interpolation
Inside a function, use ranges to build content and interpolation to display results. Ranges do not create an array first; `..` excludes the endpoint and `..=` includes it. Arrays support `append`, `pop` and `len`; tables support `has_own` and `len`. Interpolation currently accepts strings, numbers, booleans and null.
```aside
let values = [];
for value in 1..=3 { values.append(value); }
let latest = values.pop();
text(f"剩余 {values.len()} 项，最后一项 {latest}");
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

Registered functions can be stored and passed as values, for example `let say = narrator; say("你好");`; waiting behaves exactly as in a direct call. A function default can also be a constructor such as `text`. Use `(int,int)` for a coordinate pair and `(int,int)|null` for an optional pair. `is`, parameters, returns and variable assignments check these contracts; a failed assignment preserves the previous value. See the runnable [function_values.aside](../../examples/function_values.aside) example and use its path with the earlier `dev` or `build` commands.

Component calls with a trailing children block may omit the ending semicolon when used as standalone statements. A newline after the block ends that statement, so a parenthesized expression on the next line does not continue the previous call. Ordinary calls and property assignments still use semicolons. An explicit semicolon does not change child collection.

Implemented: text, image, row, column, stack and stage.replace use one shared node model. Ordinary functions can return nodes, composed through trailing children blocks, if and for over arrays/tuples. Properties such as value, x/y, width/height, color/background, opacity, visible and clip update the original node while its mounted identity stays unchanged. Replacing the stage retires the old tree. Retained old nodes cannot be read or written afterward, and do not keep the entire old tree alive. See the complete [author-stage example](../../examples/author_stage.aside).

Stack layers children from back to front in source order; row arranges them horizontally and column vertically. Omitted dimensions use content layout. x/y offset the layout position, with the logical origin at the top left, x increasing rightward and y downward. Defaults are font size 24, white text, a transparent background and zero gap/padding. Clip limits the visible area of a container. `scroll(height: 360) { ... }` creates a vertical scrolling region; axis can be "horizontal" or "both". Wheel input, touch dragging and keyboard/gamepad focus use the same input routing. Scrolling preserves text layout. With `follow_end: true`, an NVL page already at the end follows new content; scrolling back preserves the reading position.

### Editing components while running

`container.add(child)` appends a subtree to a row, column or stack and returns the original child. `index:` selects a zero-based insertion position, including the current child count; omitting it or passing null appends. The subtree must have no parent and must not be mounted as the stage root. Duplicate ownership, cycles, adding to text/image or an out-of-range position produce an error and preserve the original tree. Adding and removing preserve other nodes and their widget identities; unchanged text is not reshaped.

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

The complete [author_images.aside](../../examples/author_images.aside) includes these image files. Run it with the earlier dev command. Paths are relative to the source file for dev and the ASB file for run; use `--assets <directory>` to select a different resource root. For example, after building into target/reel-artifacts, run with `--assets examples`. Building an ASB does not copy image files.

Images and fonts use the same resource loading service. File reads and image decoding happen in the background, and image nodes using the same path share pixels. The previous complete scene remains visible until all images and configured fonts in the new scene are ready; story input pauses during that preparation. Changing source keeps the previous picture until the replacement is ready. A missing or invalid file reports its path and the image constructor's source location (game.configure for fonts) and ends the window run. Property type errors and an invalid fit are catchable at assignment. Images are loaded when mounted; unmounted component construction does not read files. In-session file hot reload is not implemented.

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
game.dialogue().lines is the shared observable list of dialogue lines. speaker_name/text/reveal are read-only convenience projections of the first line in the currently active line group. ADV views commonly use these fields; NVL and speech-bubble views use the complete line list. model.text holds the complete text, while reveal controls how much is visible, keeping the layout stable during character-by-character display. Replacing, hiding, or showing the dialogue view preserves the session history. Voice parameters are not connected yet. Multiple characters speaking at once also share a single advance interaction:
```aside
let eri = character("Eri");
say([mio.line("你听见了吗？"), eri.line("在窗外。")]);
```
`game.dialogue()` returns the stable session model. The nvl_view above composes accumulated lines; `model.clear()` clears the displayed page while preserving history. ADV uses the first-line shortcuts, and current_line_ids identifies the current group. Reveal changes update properties without rebuilding the NVL page; appending lines or clearing the page rebuilds the view content. Ordinary layouts determine placement, backgrounds, spacing and bubble shapes. In a custom dialogue view, taps not consumed by controls advance the current line; touch drags only scroll. See [author_scroll.aside](../../examples/author_scroll.aside) for a long menu.
When you need a route map or chapter restarts, use an ordinary API to register a stable ID and display title for a function. The function still follows the same calling and waiting rules.
```aside
fn opening() {
    // 剧情正文。
}
story.register("chapter01.opening", opening, title: "窗上的光");
```
An ordinary fn is sufficient for story code. A chapter entry registered through story.register must be a module-level function with no required parameters. game.start uses the function's own stable module symbol, so an ordinary new-game entry needs no additional registration. Only route and restart operations that address chapters by ID use the registry. Registration occurs within a module candidate; validation and module publication are committed together. game.start(opening) accepts the same kind of symbolic entry point and submits a new-game request from a UI event. The engine first validates the entry point against the latest published module set, then creates a new session and calls it, initializing global story variables, auto-play, and fast-forward state. Ordinary helper calls retain their original module bindings.

## 4 Sequencing Changes and Running Them Together

Animation and composed activities are available: `tween(node, x: 320, duration: 0.5, easing: "in_out")` describes a change and `run(action, owner: node)` starts it. Omitting owner uses the session in the main story and the event node in a button callback. Control an activity with `pause(activity)`, `resume(activity)`, `stop(activity)` and `finish(activity)`. Await returns the string `Completed` or `Stopped`; retiring a target or owner cancels the activity and throws at await. Duration is in nonnegative seconds; zero completes immediately. Easing accepts linear, in, out and in_out. Shared numeric properties can animate; width/height need explicit starting values, and null leaves a property out of the animation. Run [author_activities.aside](../../examples/author_activities.aside) with `reel dev examples/author_activities.aside --assets target/reel-artifacts/author-assets`. sequence/parallel/repeat are available; scope is not implemented yet.

The language runtime supports awaiting host-registered awaitable objects. An ordinary fn can use `await request` to suspend its call chain while the host handles other work. Even settled objects return on a later scheduling step; failures point to the await expression and retain the producer's cause. Engine activities now use this protocol; async fn and Future construction are not available yet. The runnable host integration check is [host_awaitables.rs](../../crates/aside-compiler/tests/host_awaitables.rs): `cargo test -p aside-compiler --test host_awaitables`.

Empty sequence/parallel and times: 0 complete immediately without property writes. times must be a nonnegative integer or null; omitting it or passing null creates a continuous loop, whose cycle must have a positive duration. A composition with an infinite branch rejects finish before changing the activity; use stop to end it. run checks the complete write sets of parallel branches before taking over existing activities. Pause, stop and cancellation affect the whole group; later segments acquire properties only when they start. A large clock step continues through every segment it crosses.

tween only describes target properties, final values, and duration. sequence combines actions in order, parallel combines them concurrently, and repeat repeats an action. These are all ordinary library functions that produce reusable action values. Only run starts an activity, returning an activity that can be awaited and controlled. await activity waits for it to end while input, audio, and other activities continue. Currently run(action) belongs to the session; specify a stage node with owner. The planned scope API will provide its own s.run entry point.
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

The creator scope, temporary overrides and restoration behavior in this section remain design targets.

scope executes an ordinary closure within the current story call chain. It manages temporary nodes, property overrides, offsets, and activities. Its body can deliver dialogue, wait, and call other functions normally. On exit, it releases only what that scope owns.
```aside
fn show_cg(view) {
    scope(body: |s| {
        s.override(view.hud, visible: false);
        s.add(view.picture, image("assets/window-cg.png"));
        await game.advance();
    });
}
```
Here the HUD is temporarily hidden, and the CG is added at the top of picture. When the player advances, the CG is removed and this visible override is released. The HUD's original bindings and underlying values are preserved throughout. On exit, the latest underlying values become visible, with any other active overrides still in place. Later-created overrides take precedence when nested; ending any layer removes only its own token.
s.add(parent, node) mounts a temporary subtree, s.override(node, props...) creates an animatable override layer, s.offset(node) creates an independent additive x/y offset, and s.run(action) starts an activity owned by this scope. Ordinary node.add continues to use the explicit parent node's lifetime; being inside a scope does not silently change its meaning.
Normal return, exceptions, and scene exit all trigger synchronous, idempotent host cleanup. The body explicitly waits for any exit animation. Scopes can be nested. A save records the current call position, activities, and overrides; after restoration, execution continues in the same body, then releases resources under the same rules. Ordinary writes to game variables retain their application meaning. scope manages only explicitly registered temporary effects.
By default, scope lives with the current task. Use scope(owner: node, body: ...) when it should end with a particular node. If that node ends first, only the scope region is cancelled, and ScopeCancelled is raised at the outer call site. Main-story code that is still alive can catch it outside the scope. Code involving invalid nodes within the region ends, and the host performs cleanup. If the current task itself is cancelled, the entire task ends.
visible=false hides the subtree and removes it from hit testing and focus; enabled=false keeps it visible but disables interaction; opacity changes only transparency. Use stage.replace(new_root) for permanent scene changes and a container's replace(new_child) for permanent local replacements.

## 6 Logos, Title Screens, and Local Interaction

Images, finite animation and button on_click callbacks are connected to creator source. Video and game entry switching in this section remain unimplemented; video events still describe the target design.

The startup flow is still an ordinary fn. Media play returns an activity handle. A normal click event can stop playback, and both natural completion and an explicit stop continue to the next line.
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
Video is an optional capability. A static-image logo uses image and run(tween(...)) to express the same flow. Media and animation share the activity control protocol. Creating a video node only creates its visual representation; play starts playback. Replacing the node ends its playback and event subscriptions.
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
Authors can use the same approach to wrap consistent fonts, button images, spacing, and animation, then import them as ordinary modules. Visual style is reused through components and parameters; shared property objects can also be organized using ordinary language rules.

Use `bind(node.x)` or `bind(node[key])` to capture a stable property location. The receiver and key are evaluated once. Passing the reference to a node constructor or assigning it to a node property establishes a connection; subsequent ordinary assignments write through while keeping it connected. `unbind(bind(node.x))` disconnects that location and preserves and returns its current value. Constraints on every connected control are checked together, and a failed write changes none of them. Explicitly destroying a source node disconnects surviving targets at their last value; existing references can no longer access the retired source. See section 12 and the verified scope for the implementation status of business `model` and `derive`.

Every node exposes read-only observable `hover`, `pressed`, `focused`, and `disabled` properties. Read them directly or connect them with `bind`; they cannot be assigned or used as writable slider/toggle values. Hover and focus include descendants; pressed includes pointer capture and the button’s own keyboard/controller confirmation hold. Disabled includes ancestor disabling. Cancellation, hiding, disabling, and window focus loss clear ongoing interaction. Set `decorated: false` on buttons and toggles to remove the default focus outline and toggle indicator/spacing, then compose the appearance from ordinary children. See [author_control_states.aside](../../examples/author_control_states.aside).

## 8 Styling and Moving Elements Together

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
To move UI and character portraits together, place them in the same group and apply the action to that group.
```aside
let cast = group() {
    image("assets/mio.png");
    dialogue_view(game.dialogue());
};
stage.replace(cast);
await run(tween(cast, x: 80, duration: 0.5));
```
To add shaking while an element is moving, give its base position and shake independent offsets. The following shake is an author-defined function that returns an ordinary action value. It works with dialogue boxes, character portraits, or any group.
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
Adding an offset only to hud shakes only the dialogue UI. It is independent of underlying x/y movement: multiple x/y offsets add together in the parent's coordinate system, leaving the base layout unchanged. Rendering and hit testing both use the composed result. When scope ends, it removes its own offset, leaving the moved base position correct. Rotation and scaling compose in an explicit transform-matrix order and are expressed through separate group nodes; additive x/y offsets do not imply additive rotation.

## 9 Masks, Effects, and Custom Scene Choreography
Historical verification (R01 / Issue #25): focused cases cover internal Rust frame records, resource leases, groups, clipping and opacity. R01 did not yet provide a GPU window, and recording consumers do not establish physical-device behavior. This section's author mask/blur effects remain design targets. The basic stage and real window now work together as described in sections 1 and 2. See the [historical verification scope](../../crates/reel-ui/R01-REPORT.md).

Historical verification (R02): the wgpu backend and winit diagnostic window included images, group opacity, clipping and Parley text. Those historical Rust diagnostics do not establish implementation of advanced-effect APIs. PNG/JPEG author images now use the shared stage and player resource loader. Current author source and ASB enter the real story window through dev/run. See [window documentation](../../crates/reel-player/README.md) and [device verification scope](../../crates/reel-render/R02-REPORT.md).

Effect values such as mask and blur belong to the same compositing system. An effect group first composites its children, then applies its effects, with group opacity applied once. Clipping defines a geometric region; an image mask defines degrees of visibility. mask.progress ranges from 0 for fully hidden to 1 for fully visible; softness controls edge width. Grayscale values in a black-and-white mask determine the order of revelation.
```aside
let wipe = mask("assets/ink-mask.png", progress: 0, softness: 0.06);
let reveal = group(effects: [wipe]) {
    image("assets/window-cg.png");
};
view.picture.add(reveal);
await run(tween(wipe, progress: 1, duration: 1.2), owner: reveal);
```
Effect parameters follow the same animation rules as node properties. More complex scenes can nest group nodes: blur the background separately, keep characters sharp, and leave image buttons on top, then apply a mask or camera transform to the whole group. The tree structure determines the front-to-back order of all children.
Custom effects use a versioned Effect description for input images, parameters, output bounds, and backend implementations. Advanced authors can develop host extensions, then compose and reuse common effects through ordinary Aside functions. Shader support is a backend capability; project checks list missing capabilities. Portable effects should also provide simplified implementations, letting the game select an appropriate platform version through capability queries.
Pixels changed by an effect and logical input regions are declared separately. Ordinary affine transforms and geometric clipping automatically map hit tests. For bends, distortions, and arbitrary shaders, authors provide logical shapes or specialized mappings for interactive regions. Reading the rendered image back from the GPU for every click would add latency, so the base interface uses regions that can be computed directly.

## 10 Choices and Changing Content

choose, custom choice_view and the read-only choice model are implemented. Call `game.configure(default_dialogue: false, default_choices: false)` to disable the corresponding default views, then mount author components on the stage. See [author_narrative.aside](../../examples/author_narrative.aside) for ADV/NVL switching and custom choices.

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
A for loop inside ordinary children constructs current content. `view(|| ...)` rebuilds its subtree when observable data actually read by the factory changes, retaining the outer container. Successful construction replaces the old content and cancels tasks and activities owned by retired nodes. Use bind/derive to update individual properties while retaining node identity. Use `list(|| data.items, key: |item| item.id, build: |row| text(value: derive(|| row.value.label)))` for collections that retain their controls. A stable key preserves its node, focus, and local state during insertion or sorting; removing it retires its owned tasks and activities. Keys are unique integers or strings. Each new key calls build once with a read-only observable row: key, value (the current item), and index (zero-based). Returning null creates no node for that key. Callbacks read row.value to use the current record; derive follows replacement records, while bind(row.value.label) fixes the original field location. Only reads in the source and key functions determine list reconciliation; use bind/derive/view inside each component for changing content. Duplicate keys or a failed component do not replace the old tree. Let the data source manage the list's children, and compose scroll around it. All items are currently instantiated; viewport virtualization is still pending. See [author_list.aside](../../examples/author_list.aside).

## 11 Saves, History, Routes, and Skipping

History data and auto-reading controls in this section are implemented. Save/load, route switching and chapter skipping remain design targets. Authors compose the UI; services do not generate fixed screens.

saves is an independent service. save(slot_id) returns a SaveRequest, and quick_save() uses the quick-save slot; load(slot_id) and quick_load() return a LoadRequest. Requests expose read-only status and error members. Use derive or bind to display progress. The request itself can be awaited; save buttons usually only submit a request and observe its model. Success means the slot has been committed to storage. A caller can also directly await the save request it submitted: capture occurs at a restorable boundary after the call. The corresponding request in that saved image is restored as a successful result, so the waiting task continues with the following statements. The save call is executed only once. Other in-flight external write requests settle with a catchable OperationInterrupted error when restored across processes; the request corresponding to the current saved image follows the success rule above. A successful load request replaces the session, ending callback tasks owned by the old session while the process-level service continues publishing results.
saves.slots(page: 1, page_size: 12) returns a SlotPage whose observable items include stable id, occupied, chapter, saved_at, thumbnail, and status fields. saves.delete(slot_id) returns a DeleteRequest; on success, the corresponding summary becomes an empty slot. Page numbers start at 1; page_size and page number determine a fixed set of ordinary slots, while the quick-save slot is separate. Authors build pages with grid, image, text, and button; paging only changes the displayed set.
```aside
let slot_page = saves.slots(page: 1, page_size: 12);
let save_button = button("保存", on_click: |event| {
    saves.save(slot_page.items[0].id);
});
```
Save, load, and delete requests share the Queued, Running, Succeeded, and Failed states. Saves additionally expose phase = Capturing or Writing. error contains an explanation on failure and is null on success. await request returns the request result; request failure throws the corresponding error. Slot summaries are replaced only after a successful commit; failure preserves the previous summary.
`game.history()` is implemented: it returns a stable read-only observable array for this session. Each group exposes `group_id/interaction_id/locale/lines/speaker_name/text`; each line exposes `id/speaker_id/speaker_name/content_id/text`, with null speaker_id for narration. Line IDs match game.dialogue().lines; convenience name/text fields project the first line. Full text is recorded once on submission and survives page clearing, reveal changes and UI replacement. Compose any interface with view, scroll and ordinary controls; see [author_reading.aside](../../examples/author_reading.aside). Restore points, voice replay and save/rollback restoration belong to their respective future services.
game.stories.entries is the chapter index, with id, title, and visited fields on each entry; game.stories.links holds explicitly registered from/to connections. Module publication collects entry points from story.register, while game.stories.link(from_id, to_id) registers dynamic connections. Authors represent chapters with buttons and draw connections with path nodes, arranging them manually or with their own layout algorithms.
game.restart(story_id) uses the current session's fixed main-story module set to rerun the story task from a named chapter entry point. It preserves current story variables and ends the previous main-story task and its interaction; the target function is responsible for constructing its own scene. game.start(entry) creates a fresh session, suitable for a new game. saves.load restores the saved position, variables, and scene. Authors use these distinctions to give route buttons clearly defined actions.
`game.reading()` returns stable read-only observable `auto/paused/delay` state. `game.set_auto(enabled, delay: null)` controls auto-reading; delay is a finite nonnegative number of seconds after the full text has actually been presented, defaults to 2 seconds, and is preserved when omitted. `game.pause_reading(paused)` pauses reveal, automatic advancement and dialogue/choice input; ordinary UI controls can still close a history page. Auto-reading waits for player input at choices and continues afterward. Focus loss, suspension, modal UI, pending resources and active input gestures stop timing; resuming starts a full delay again. State belongs to the session, survives UI replacement, and defaults to manual and unpaused in a new session. Finite scene activities and independent audio have separate controls. Authors choose every control, layout and history-page interaction.
`game.set_skip(enabled, unread: false)` is implemented. Observe `game.reading().skipping/skip_unread` for mode and policy, and `game.dialogue().read` for whether the entire group was read before submission. By default, unread dialogue or a choice turns skipping off. unread: true also skips unread dialogue; choices still require player input. Skipping follows normal control flow, bypasses reveal/dwell, and finishes running finite activities owned by the main story. Infinite, paused, node-owned activities and independent audio retain their controls. Auto-reading and skipping are mutually exclusive. Reading pause, focus loss, suspension and modal UI turn skipping off. Retiring the input node that started skipping also clears its held mode. Authors can use input callbacks to enable on pressed and disable on released/cancelled, or compose toggle controls. Content is marked read when dialogue advancement completes, not on cancellation or full reveal alone. Rust hosts reuse `ReadProgress` through `Session::new_with_read_progress` and connect `ids/from_ids` to their own persistent storage; new sessions default to empty progress. A changed source module version is conservatively unread.
Chapter skipping via `game.skip_to(story_id)` still depends on the future chapter registry.

These services provide data and operations; authors compose their pages. Save/load, route switching and chapter skipping above remain specifications. History, fast-forward and auto-reading are available through the APIs and runnable example linked above.

## 12 Game Variables and Component State

`model` and two-way field bindings are implemented. state, persistent, node.state and save restoration remain design targets.

Use model for application data independent of view nodes. Controls can share a field: input writes back to the data, and connected text, visibility and styling follow it. Run [author_models.aside](../../examples/author_models.aside): `cargo run -p reel-cli --features window -- dev examples/author_models.aside`.

```aside
let settings = model({volume: 0.8, music: true, palette: {accent: "#70d890"}});
let volume = slider(value: bind(settings.volume), min: 0, max: 1);
let music = toggle(value: bind(settings.music));
let indicator = text("Music", visible: bind(settings.music), color: bind(settings.palette.accent));
settings.palette.accent = "#ffd580";
```

Defaults are copied with their internal aliases and cycles preserved; the original objects remain independent. Data can contain tables, arrays, tuples, strings, byte strings and mutable buffers; closures and host resources such as nodes cannot be defaults. Objects assigned later retain shared identity. Aliases obtained through nested tables or array elements can still expose bindable fields. Ordinary table methods remain available. clear freezes existing connections at their last value and invalidates old references; bind again after adding a field with the same name. Copying advances within a work budget, and cancellation leaves no partial model.

In the language, let/let mut determines whether a binding can be reassigned, model determines whether properties are observable, and state ownership determines how state is saved and released. Each serves a separate purpose.
```aside
let route = state("route", {affection: 0, visited: set()});
let unlocks = persistent("unlocks", {cg: set()});

route.affection += 1;
route.visited.add("window");
unlocks.cg.add("window-cg");
```
state(id, defaults) returns named observable state in the current session; the same ID returns the same object. It is saved and restored through saving, loading, and rollback, and recreated from defaults for a new game. Function-local values and closure captures can also be saved with the complete call stack. Named state mainly serves shared game data and stable lookup during hot reload.
persistent(id, defaults) returns explicitly cross-playthrough observable data, written independently to global storage. Defaults fill only missing fields; repeated access preserves existing progress. Cross-playthrough state stores data values and types with persistent-data adapters. References to session nodes, tasks, or executable closures produce a lifetime-conflict diagnostic at the assignment. Idempotent operations, such as adding to sets, are recommended for unlocks. persistent.flush() can be awaited for a durable commit; the service normally coalesces updates into writes. Cross-session assignment copies data into the persistent graph, and reads return observable aliases within that graph. Any nested mutation marks it dirty. Snapshots record stable global-data identities for those aliases; loading reconnects them to current progress rather than writing back old content.
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
settings belongs to the engine's global preference service; restoring a story save reads the current preferences. Changes to application state follow ordinary control flow, and their results are preserved when temporary scope effects end.

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

Developers extending script interfaces from Rust can refer to the [Aside host SDK](../aside-host.md). Registered host functions use the same named arguments, defaults and source-location rules as ordinary Aside functions.

Rust extensions can also declare variadic positional arguments with `#[aside(rest)]`. Authors call them with ordinary arguments or `...items`; each call receives a fresh outer array while its elements retain their normal shared identity. Defaults and bound methods use the same rules. This facility is independent of Reel and does not by itself enable action combinators.

Basic text currently uses real text layout. The rich-text types, localization, media and custom drawing interfaces in this section remain future creator capabilities.

Rust custom drawing shares linear-premultiplied color conversion for solid colors, text and images. Both strokes and geometric clips support dashes, including curves, closed paths and dash offsets. An all-zero dash pattern draws a solid line; small miter limits use bevel joins. These capabilities belong to the existing Rust drawing interface; the Aside custom drawing entry point is still pending.

Author audio connects background resource loading, the real mixer and story waits; see [Author audio](#author-audio). Native-device and offline-mixing coverage are recorded separately in [audio verification](../../crates/reel-media/VERIFICATION.md). Video and precise audiovisual synchronization remain design contracts.

PlainText represents ordinary text, RichText represents explicit styled structure, and tr(id, fallback, params) creates LocalText. A translation template's structure is parsed first, then parameters are inserted as text; markup characters inside parameters remain literal text. The current dialogue group retains the text and voices from submission, and the next group uses the new language. An NVL page switches after it is cleared; UI and history use the current language immediately. reel loc extracts and updates catalogs and checks for missing translations and parameter issues.
video is an ordinary node; audio provides tracks, queues, loop regions, filters, and synchronization groups. play returns an awaitable activity, stop leaves it at its current state, and finish is for activities with an endpoint. Tracks and resources are managed by stable identity. Device reconstruction and seek preserve or rebind logical waits; errors have explicit terminal states.
custom_node can directly register state, measure, record, route, and event implemented in Aside. measure/record are read-only layout/rendering callbacks with execution budgets. route synchronously decides event consumption, capture, and focus, then event runs as an ordinary suspendable task. A minigame node can therefore prevent the same input from advancing the story while still being able to wait for resources in its application callback. New low-level graphics and platform capabilities are registered through native modules.
Camera, Mesh, Material, and Effect provide perspective stages and programmable effects, with state and resource identities still included in saves. Each backend declares its supported implementations and fallbacks in a capability list. Live2D, system text-to-speech, mobile/Web packaging, and external services are adapter capabilities within the full target scope. Their specific API documentation will provide examples in the same style alongside verified implementations.
External writes, such as network operations and purchases, use separate operation records and idempotency strategies. Unknown outcomes are queried and verified first; replay is read-only by default. Their lifetimes are explicitly distinguished from those of ordinary rollback-capable game variables. Author-defined components and script libraries can always extend scenes, layouts, choreography, and application logic.

### Finding and Handling Errors (Design Contract)
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

Entries must be ordinary module-level functions with no required parameters or captured environment; private functions are supported. Dialogue and choices use content layout and scrolling without a separate two-row or two-option cap. Both source and ASB window launch commands are implemented. Natural completion preserves the final scene; closing the window finishes cleanup. The headless example above focuses on dialogue and choices; use the author-stage example in section 1 for basic node composition. Finite animation and button events now work. Video, classes, delegates/hooks, property-bags and save/restore remain unimplemented.

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

A click passes the original button as event.target. A callback can await an animation. Synchronous code runs until it waits or ends before another script task executes, so engine budget slices do not interrupt ordinary read-modify-write operations. Multiple callbacks may await one activity; cancelling a consumer does not stop the shared producer.

The emitting button owns its callbacks and is the default owner of run inside them. Removing the button, clearing its parent or replacing the stage cancels those callbacks and activities. A callback that removes itself does not execute subsequent statements. narrator/choose currently belong to the main story; calling them from a callback raises a catchable context error. game.start and general event subscriptions remain unimplemented; direct node property bindings are available below.

Mounted button handlers and activities remain usable after the main entry returns; idle sessions do not repeatedly execute the VM. Once callbacks and activities have ended and no mounted handlers remain, the session finishes and preserves its final scene. A button click does not also advance dialogue. The built-in host supports mouse and touch activation, Tab focus navigation and Enter/Space activation. A gamepad D-pad or left stick navigates by the displayed positions of controls, and long choice lists scroll with focus; the south button confirms, while the east button and Escape send Back to the current UI. Confirmation activates on a paired release. Modal changes, focus loss and disconnection cancel old input; Back does not directly exit the application. Multiple contacts can operate separate controls at once. Releasing outside a button does not click it; system cancellation or suspension releases the interaction. Existing buttons work on touch screens without a separate implementation; run [author_buttons.aside](../../examples/author_buttons.aside). The built-in desktop host integrates the system IME; system accessibility adapters remain future work.

Use the dev/build/run commands in section 1 with this file. Nodes, input and scheduling do not depend on Winit/WGPU. The window feature selects the built-in desktop combination; other hosts can reuse the same session and presentation interfaces.

For ports and embedding, use PlatformHost, Presentation and StoryRuntime from reel-player, and RenderBackend from reel-render. Enabling reel-player/winit alone provides the Winit host without WGPU; reel-render/wgpu provides the independent WGPU backend. The built-in player uses these interfaces too: only an actually presented new frame updates input geometry, temporary resource pressure retries after submission completion, and permanent refusal preserves the previous frame. Story animations pause while the application is suspended and continue on resume; closing still completes task cleanup. Authors do not need to change their scripts or select a graphics API. Touch uses portable PointerEvent values; hosts enabling winit can reuse TouchInput to convert native contacts. Rust games and hosts can use reel_ui::input::ActionMap to bind keyboard, pointer and gamepad controls, read held values and receive press, change, release and cancellation events. NavigationInput supplies configurable thresholds, repeat timing and remapping. The independent gamepad feature uses Gilrs for desktop devices without requiring Winit/WGPU. The Winit adapter forwards system IME events and candidate-window positions; system accessibility remains unimplemented. See the [player documentation](../../crates/reel-player/README.md) for host integration and reproducible checks.

## Modal menus and focus

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

`slider(value: 0, min: 0, max: 1, step: null, on_change: null, ...)` provides a horizontal slider; `toggle(label: null, value: false, on_change: null, children: [], ...)` provides a switch. Both share stage position, size, colors, visibility, enabled state and modal scope; toggles also compose child nodes. The following [author_settings.aside](../../examples/author_settings.aside) runs after supplying fonts/body.ttf.

Sliders require finite increasing min/max and a finite value inside that range. step is null or finite and positive, anchored at min, with both endpoints reachable. Scripts may assign any in-range value regardless of input step. Assigning min/max must also leave the current value in range; invalid writes preserve the previous state. Mouse/touch dragging, arrow keys, Home/End, Shift acceleration, left/right controller navigation and accessibility actions share one control.

Accepted user changes update node.value before on_change runs; events contain target and that change’s value. Script assignments are silent. Callbacks may await and are cancelled when their node is removed. Value and layout updates preserve dragging; changing range/step ends the old drag, and interaction resumes after the new frame is accepted. Controls retain their value without a callback. Node values support bind/unbind; derive supplies read-only values for presentation and cannot be a slider/toggle writeback source.

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

```aside
let profile = model({name: ""});
text_input(value: bind(profile.name), width: 280, height: 40,
    color: "#202020", background: "#ffffff",
    on_submit: |event| { profile.name = event.value; });
```

Runnable example: [author_text_input.aside](../../examples/author_text_input.aside).

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

Use `audio.group_volume("music", value)` for music/voice/effects/ui, or master for overall volume. Sound, group and master gains multiply; group settings also apply to later sounds. `audio.pan(sound, value, duration: seconds)` moves stereo position from -1 (left) to 1 (right), with 0 centered. `audio.volume` and `audio.group_volume` also accept duration; omission applies the change immediately. `fade_in` starts when playback actually begins. `audio.stop(sound, fade: seconds)` resolves await with Stopped after fading out; loading or paused sounds stop immediately. Pause retains playback position while volume/pan envelopes continue; output suspension freezes all envelopes. Pausing during a fade-out stop finishes the stop immediately. Volume transitions interpolate decibels with a -60 dB silence endpoint. New commands continue from the current value; controls during loading update initial playback values.

By default, sound belongs to the current callback node, or to the session in the main story. Use `owner: node` to choose a node: removing it cancels loading and playback together. Cancelling a waiter does not stop shared playback. Playback, node cancellation and window shutdown use the same resource retirement path.

The Windows player submits stereo audio to the system, including when the default output device uses multiple channels.

WAV/PCM, Ogg/Vorbis, FLAC and MP3 support predecoded playback. PCM is charged to the host memory budget without a separate duration limit. Streaming long tracks, seek and save restoration remain part of the full design. Completed means mixer consumption, so it cannot establish precise hardware synchronization. Physical-device coverage is recorded in [audio verification](../../crates/reel-media/VERIFICATION.md).

Runnable example: [author_audio.aside](../../examples/author_audio.aside), using examples as the asset root and the same font configuration as other author examples. Its chime.wav is an original short sine tone supplied by this repository.

```powershell
./target/debug/reel.exe check examples/author_audio.aside
./target/debug/reel.exe dev examples/author_audio.aside --assets target/reel-artifacts/author-assets
```
