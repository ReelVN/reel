# Reel Game Creator Guide
Design version 1.1 · Development baseline. Implementation status is stated in the guide.

[中文](zh-CN.md) · [文档目录 / Documentation index](../README.md)

## Reading This Guide
This guide starts with the scene you want to create and explains how to use Aside to write stories, compose UI, choreograph scenes, and extend effects. All interfaces and examples are currently design specifications entering implementation. Each section will be marked as implemented, with its applicable version, once the code passes acceptance checks. The Chinese and English editions share the same examples and interface names. Reel Engine Architecture Design describes the runtime; Aside VM and Language Design defines the language rules.
Authors work with three basic concepts: functions organize reusable content, node trees describe scenes, and activities represent changes over time. Story code, button events, and utility functions all use the same fn declaration. Components are ordinary functions that return a node or a collection of named nodes.

Implementation boundary: the compiler provides lexing, parsing, semantic analysis and bytecode generation for a supported subset. VM storage, root ranges, bytecode validation and internal Rust stage/render contracts are also implemented. These foundations do not establish runnable creator APIs, CLI commands or complete story sessions. See the [current implementation status](../README.md#当前状态) for merged and pending work; each section's explicit status takes precedence over its design examples.

## 1 A Minimal Project and Shared Conventions
A project needs only game.toml, main.aside, and assets/ to get started. Split out directories such as ui/ and story/ as your project grows. The configuration specifies main::main as the entry point and a logical canvas of 1920×1080. reel dev runs source code and reports errors with their file locations; reel build produces bytecode and asset packages; a lightweight player runs the build output. These commands are implementation targets. build outputs .asb bytecode packages, .rpk asset packages, and release.json. Assets can be packaged by category, and the release player loads them according to the manifest. Story updates and asset updates are built separately.
game.toml uses entry and title in the [game] table, and width, height, and scale in the [display] table. Set entry to "main::main"; the defaults are width=1920, height=1080, and scale="contain". contain preserves the aspect ratio and leaves empty space at the edges; input coordinates are converted using the same scaling. game:: modules resolve .aside files relative to the directory containing game.toml. File and directory names must be valid module identifiers. External source modules are imported through [modules] mappings from aliases to relative directories; game and reel are reserved aliases.
use reel::prelude::* imports the names of common nodes, layouts, actions, and services. Authors still build their pages and visual style with their own functions. The game:: prefix resolves modules from the game's source root. All documents consistently use fn, let, let mut, |parameters| closures, and use imports. Except for complete module and function examples, standalone snippets belong inside a running function and reuse variables such as view and mio introduced earlier. Ordinary statements end with semicolons; the final expression in a function can serve as its return value. Object literals use {field: value}.
```aside
use reel::prelude::*;
use game::ui::title::title;

fn main() {
    stage.replace(title());
}
```
The ui/title.aside module used above can be built directly from the author's own nodes:
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
See section 3 for opening in story/opening.aside. The examples' relative file locations match their import paths; the root module main.aside handles startup only.
let fixes a variable's binding while leaving node properties mutable. Use let mut when the variable must be rebound to another value. Ordinary expressions are evaluated once when executed. bind(model.value) connects an observable property; derive(|| expression) connects a read-only value computed from other properties. See the second document for the complete language rules.

## 2 Describing the Whole Scene in Code
stack layers its children from back to front in the order they are written; row arranges them horizontally, and column vertically. Ordinary nodes share position, size, opacity, visibility, transforms, and clipping. Absolute positions use logical canvas coordinates, with the origin at the top left, x increasing to the right, and y increasing downward. Layout containers arrange their children; a container's own x and y determine its position within its parent. When content extends beyond a container, a clip or scroll container determines the visible region.
The following room places the background at the bottom, the character portrait above it, the CG above that, and the dialogue UI on top. The author's function returns named parts that the story needs to manipulate.
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

## 3 Story Code Is Ordinary Sequential Code
Define each character once in a module. Each line of dialogue calls the speaker object; narration calls narrator. A standalone string is still just a string. A character call updates the dialogue data and history, then waits for the current line to finish. Ordinary function calls continue along the same execution sequence.
```aside
use reel::prelude::*;
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
While text is appearing character by character, the first advance reveals the rest of the current text; the next advance finishes the line. Events consumed by controls such as buttons and sliders remain within those controls. Story advancement uses a new input sequence number, so a single click always causes only one advance.
A character describes identity and speaking parameters. The author's components draw the dialogue box. Here is the simplest text layout; background images, nameplates, and shortcut buttons can be added to the node tree as usual.
```aside
pub fn dialogue_view(model) {
    column(x: 160, y: 780, width: 1600, gap: 12) {
        text(value: bind(model.speaker_name),
             visible: derive(|| model.speaker_name != ""));
        text(value: bind(model.text), reveal: bind(model.reveal));
    }
}
```
game.dialogue.lines is the shared observable list of dialogue lines. speaker_name/text/reveal are read-only convenience projections of the first line in the currently active line group. ADV views commonly use these fields; NVL and speech-bubble views use the complete line list. model.text holds the complete text, while reveal controls how much is visible, keeping the layout stable during character-by-character display. Replacing, hiding, or showing the dialogue view preserves the session history. Character calls accept resource parameters such as voice; voice playback is associated with the current line. Multiple characters speaking at once also share a single advance interaction:
```aside
let eri = character("Eri");
say([mio.line("你听见了吗？"), eri.line("在窗外。")],
    mode: "bubbles");
```
Speech-bubble and NVL views use list to bind game.dialogue.lines, with current_line_ids distinguishing the current group from accumulated lines. The current group's convenience fields always refer to its first line. Choices also expose a data model. Authors use buttons to call game.choice.select(option_id), and choose returns the corresponding key.
When you need a route map or chapter restarts, use an ordinary API to register a stable ID and display title for a function. The function still follows the same calling and waiting rules.
```aside
fn opening() {
    // 剧情正文。
}
story.register("chapter01.opening", opening, title: "窗上的光");
```
An ordinary fn is sufficient for story code. A chapter entry registered through story.register must be a module-level function with no required parameters. game.start uses the function's own stable module symbol, so an ordinary new-game entry needs no additional registration. Only route and restart operations that address chapters by ID use the registry. Registration occurs within a module candidate; validation and module publication are committed together. game.start(opening) accepts the same kind of symbolic entry point and submits a new-game request from a UI event. The engine first validates the entry point against the latest published module set, then creates a new session and calls it, initializing global story variables, auto-play, and fast-forward state. Ordinary helper calls retain their original module bindings.

## 4 Sequencing Changes and Running Them Together
tween only describes target properties, final values, and duration. sequence combines actions in order, parallel combines them concurrently, and repeat repeats an action. These are all ordinary library functions that produce reusable action values. Only run starts an activity, returning an activity that can be awaited and controlled. await activity waits for it to end while input, audio, and other activities continue. By default, run(action) belongs to the current scope, or to the current task's owner when outside a scope. Specify an explicit owner when the activity needs to outlive that boundary.
An action reads its starting property values when it actually becomes active. In a sequence, a later step reads its starting values after the previous step finishes. Action descriptions are reusable, and every run creates independent progress. run atomically reserves every property that the entire action tree will write. These reservations last until the root activity ends, including future sequence segments, repeat iterations, and periods while paused. A sequence may write the same property in order; parallel branches must have disjoint write sets. A conflict is reported at the call site and leaves existing activities intact. To take over, first stop the old activity. Assigning directly to a reserved property first stops its root activity, then applies the assignment. This rule also applies to properties reserved for future segments. An activity releases all reservations when it ends.
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

## 5 Managing Temporary Scenes with Scopes
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
The startup flow is still an ordinary fn. Media play returns an activity handle. A normal click event can stop playback, and both natural completion and an explicit stop continue to the next line.
```aside
use reel::prelude::*;
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

## 8 Styling and Moving Elements Together
Reusable styles are ordinary data; component functions decide how to apply them to their nodes. Start with defaults, then override them with variants and explicit arguments. Defaults apply only to missing fields; false, 0, and null retain the author's explicit meaning. Temporary scope overrides take precedence over a node's underlying properties.
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
Node properties can be changed directly. Use bind/derive for appearance that should keep following application state or preference changes. Ordinary style records are used during construction. A shared runtime color palette can be stored in a model, with its properties bound to text and images. Authors can draw their components entirely themselves or reuse familiar layout and control behavior.
To move UI and character portraits together, place them in the same group and apply the action to that group.
```aside
let cast = group() {
    image("assets/mio.png");
    dialogue_view(game.dialogue);
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
Implementation status (R01 / Issue #25): real Masonry cases verify the internal Rust frame records and resource leases, groups/clips across fragments, and External paint order. Recording/null consumers and linear-alpha formulas are checked. Accepted notices retain candidate qualification and matching geometry, allowing acceptance to be processed after graphics completion; upload completion still leaves input geometry unchanged. Export work-budget failures also retain the actual painter location. The author-facing Aside examples remain design specifications; at the R01 milestone GPU rasterization, windows, and the player were not implemented (see R02 below), and simulated presentation does not establish device acceptance. See [verification scope and measurements](../../crates/reel-stage/R01-REPORT.md).

R02 implementation status: the optional wgpu adapter consumes RenderFrame directly. A thin winit shell provides a draggable, zoomable renderer diagnostic with an image, group opacity, clipping and optional real Parley text. This is not an Aside story session. Unsupported records fail explicitly; the full engine scope remains. See [launch command](../../crates/reel-player/README.md) and [device evidence and limitations](../../crates/reel-render/R02-REPORT.md).

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
choose(options) accepts an array of options, each with a stable key and a label, updates game.choice, and waits for select(option_id). The current line and choices share the right to control main-story interaction. Authors place the choice view within their own scenes and choose its position and background.
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
choice_view is mounted in the scene containing the dialogue UI; by_window and by_door are the author's story functions. choose copies the supplied ordinary records into engine-owned, read-only observable rows. id, key, and label can all be read and bound, while the input records remain unchanged. id is an opaque, stable identifier formed from the current InteractionId and key. The list uses option.id to preserve that identity; the next choice uses new IDs. game.choice.options is a read-only observable list of these rows, empty by default. It is populated while waiting for a choice and cleared when the choice is complete. select accepts option.id, which includes interaction identity, while the value returned to story code is still the application key. Stale or duplicate selections are filtered using that identity.
A for loop inside ordinary children is appropriate for constructing a scene as it is now. list is for continuously changing lists: items connects to an observable collection, key determines element identity, and build creates a node for a new key. An observable item with the same key retains its identity while its fields update; only removing that key destroys the corresponding subtree. Layout is vertical by default. Virtualized display is governed by the viewport range and reusable measurement results. Duplicate keys produce errors at the corresponding list location. Passing an ordinary array as items constructs fixed content.

## 11 Saves, History, Routes, and Skipping
saves is an independent service. save(slot_id) returns a SaveRequest, and quick_save() uses the quick-save slot; load(slot_id) and quick_load() return a LoadRequest. Requests expose read-only status and error members. Use derive or bind to display progress. The request itself can be awaited; save buttons usually only submit a request and observe its model. Success means the slot has been committed to storage. A caller can also directly await the save request it submitted: capture occurs at a restorable boundary after the call. The corresponding request in that saved image is restored as a successful result, so the waiting task continues with the following statements. The save call is executed only once. Other in-flight external write requests settle with a catchable OperationInterrupted error when restored across processes; the request corresponding to the current saved image follows the success rule above. A successful load request replaces the session, ending callback tasks owned by the old session while the process-level service continues publishing results.
saves.slots(page: 1, page_size: 12) returns a SlotPage whose observable items include stable id, occupied, chapter, saved_at, thumbnail, and status fields. saves.delete(slot_id) returns a DeleteRequest; on success, the corresponding summary becomes an empty slot. Page numbers start at 1; page_size and page number determine a fixed set of ordinary slots, while the quick-save slot is separate. Authors build pages with grid, image, text, and button; paging only changes the displayed set.
```aside
let slot_page = saves.slots(page: 1, page_size: 12);
let save_button = button("保存", on_click: |event| {
    saves.save(slot_page.items[0].id);
});
```
Save, load, and delete requests share the Queued, Running, Succeeded, and Failed states. Saves additionally expose phase = Capturing or Writing. error contains an explanation on failure and is null on success. await request returns the request result; request failure throws the corresponding error. Slot summaries are replaced only after a successful commit; failure preserves the previous summary.
game.history is a read-only observable list. Each entry is a HistoryGroup containing group_id, interaction_id, lines, and restore-point information. lines preserves the characters, content identities, parameters, displayed text, and voices from that moment. Single-line convenience fields project only the group's first line. Put a list inside scroll to display history entries as needed. Loading or rolling back switches the list to the restored session state. Ordinary UI replacements preserve history.
game.stories.entries is the chapter index, with id, title, and visited fields on each entry; game.stories.links holds explicitly registered from/to connections. Module publication collects entry points from story.register, while game.stories.link(from_id, to_id) registers dynamic connections. Authors represent chapters with buttons and draw connections with path nodes, arranging them manually or with their own layout algorithms.
game.restart(story_id) uses the current session's fixed main-story module set to rerun the story task from a named chapter entry point. It preserves current story variables and ends the previous main-story task and its interaction; the target function is responsible for constructing its own scene. game.start(entry) creates a fresh session, suitable for a new game. saves.load restores the saved position, variables, and scene. Authors use these distinctions to give route buttons clearly defined actions.
Hold-to-fast-forward, auto-play, and skipping to a chapter use a shared story-advancement service. game.skip_to(story_id) accelerates execution through normal control flow, stopping at the target chapter entry, a choice, or an unread-content boundary. If the target is unreachable, the story ends normally and the service reports that the target was not reached. game.set_auto(enabled) controls auto-play. Hold-to-fast-forward is driven by the held state of the input layer's skip action; authors can bind keyboard or gamepad input to that action.
Fast-forward shortens skippable waits for already-read content and finite scene activities, while variable calculations still execute in sequence. Releasing the input, losing focus, opening a menu, starting a new game, or loading a save clears the held state. A tween completes at its final value, finite waits end, and media stops or continues according to its playback policy. Looping activities remain managed by their owner. Author-provided control hints follow the same service state.
These services provide data and operations; authors compose the appearance of their pages. The signatures listed in this edition are specifications awaiting implementation. Once a feature passes its contract acceptance checks, the guide will mark that section with the implemented version and run commands.

## 12 Game Variables and Component State
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

Implementation status (M01 / PR #14): the Rust audio layer implements the Kira bridge, optional CPAL output, bounded reclamation and device-failure notification. Independent checks cover actual Kira with offline Linux output, default/output-feature-off contracts and Windows cross compilation. Physical devices, hardware clocks, hot-plug and native start/stop/suspend behavior are not accepted; the CPAL clock remains Unavailable. The author-facing audio/video APIs, story waits and session recovery below remain design targets; passing Rust probes does not establish their integration. See [audio verification scope](../../crates/reel-media/VERIFICATION.md).

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

