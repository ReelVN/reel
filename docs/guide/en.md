# Reel Creator Guide

[简体中文](zh-CN.md) · [Documentation](../README.md)

Express scenes, dialogue, interaction and choreography with Aside functions, nodes and activities. A creative unit can contain both UI and story; keep its local state and behavior together, and let callers choose composition and flow.

Download the tool bundle, add the directory containing `reel`, `reel-lsp` and `reel-game` to `PATH`, and reopen your terminal. Install the Aside VS Code extension for completion, signature help, F12 and formatting on save. `aside.server.path` selects the language server and `aside.reel.path` selects the runtime tool.

Create a `MyGame` directory with `src/main.aside` as its entry and `resources/` for assets. Projects share an installed toolset while keeping their own source, resources and `build/` outputs. Script asset paths are relative to `resources/`.

```sh
cd MyGame
reel check
reel dev
reel build
reel export --release
```

`check` compiles and locates errors; `dev` runs source. `build` produces `build/debug/game.asb`, or `build/release/game.asb` with `--release`. Use `reel run build/debug/game.asb` to run compiled code.

`export` produces a development project at `build/export/debug/`, or a release at `build/export/release/` with `--release`. A release contains the player, code, text package and resource volumes; distribute the whole directory. Default outputs update on repeated runs; explicit `--output` destinations use `--overwrite` to replace existing output. Keep saves separately before replacing an export directory; `--save-directory` selects runtime storage.

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

As the work grows, split modules by chapter, menu or minigame. The entry configures the work, creates shared state and begins the flow. `game::ui::title` resolves to `src/ui/title.aside`:

```aside
use game::ui::title::title;

fn main() {
    game.configure(title: "窗边", width: 1920, height: 1080, fonts: ["fonts/body.ttf"]);
    stage.replace(title());
}
```

`src/ui/title.aside` returns an authored title screen. Connect ordinary pages with function calls; use `game.start` to create a new session. The story function for `src/story/opening.aside` appears below.

```aside
use game::story::opening::opening;

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

```aside
stage.replace(scene);
await stage.presented(owner: scene);
await run(tween(scene, opacity: 1, duration: 0.5));
```

The control handles interaction while its children define appearance. Connect hover, press or focus state to colors and images with `derive`; use `focus_visible` for keyboard and gamepad focus cues.

```aside
column(gap: 12) {
    let control = button(width: 240, height: 48, decorated: false);
    control.add(text("Hover me", color: derive(|| if control.hover { "#ffd580" } else { "#ffffff" })));
    control;
    text("Another component");
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

A reusable container accepts `children`, leaving content to its caller. The same container can hold dialogue, settings or a minigame.

```aside
fn card(children = []) {
    column(gap: 12, padding: 24) {
        for child in children { child; }
    }
}
```

Await `stage.geometry` to position an overlay relative to a presented node. The result includes bounds in the reference node coordinates; transparent nodes still participate in layout. A hidden or unmounted target, or a noninvertible reference transform, reports `GeometryUnavailable`.

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
    scroll(height: 600, follow_end: true) {
        view(|| column(gap: 24) {
            for line in model.lines { dialogue_view(line); }
        });
    }
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

`popup` follows its trigger node and suits dropdowns or hints. `on_dismiss` handles outside clicks or the back action.

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

Read `window.info().capabilities` before changing settings, then await `window.configure` for the applied state. Window size and logical canvas are configured separately.

```aside
let mut display = window.info();
if display.capabilities.resize {
    display = await window.configure(width: 1280, height: 720);
}
if display.capabilities.fullscreen {
    display = await window.configure(fullscreen: true);
}
```

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

`blend` controls a subtree against its background with `normal`, `add`, `subtract`, `multiply` or `screen`. The subtree is composed before blending.

```aside
stage.replace(stack(width: 320, height: 180, background: "#204060") {
    stack(width: 320, height: 180, background: "#800000", blend: "add", opacity: 0.5);
});
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

`saves` captures execution position, state and presentation. Input requiring a session change during capture fails that save with `CaptureBusy` and preserves the previous slot. Obtain a thumbnail with `stage.capture` before supplying it to a save request. The save list provides data for an authored slot UI.

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

Use `persistent` for global preferences and unlocks. Reload and apply current preferences at startup and after restoring story execution. Stored data is independent of the execution snapshot.

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
    let sound = audio.play("assets/chime.wav", pan: -0.5);
    await sound;
    audio.stop(music, fade: 0.5);
    await music;
}
```

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
