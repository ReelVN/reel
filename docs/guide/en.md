# Reel Game Creator Guide
Design version 1.0 · Submitted for independent review. Implementation status is stated in the guide.

[中文](zh-CN.md) · [文档目录 / Documentation index](../README.md)

## Reading This Guide
This guide starts with the scene you want to create and explains how to use Aside to write stories, compose UI, choreograph scenes, and extend effects. All interfaces and examples are currently design specifications entering implementation. Each section will be marked as implemented, with its applicable version, once the code passes acceptance checks. The Chinese and English editions share the same examples and interface names. Reel Engine Architecture Design describes the runtime; Aside VM and Language Design defines the language rules.
Authors work with three basic concepts: functions organize reusable content, node trees describe scenes, and activities represent changes over time. Story code, button events, and utility functions all use the same fn declaration. Components are ordinary functions that return a node or a collection of named nodes.

## 1 A Minimal Project and Shared Conventions
A project needs only game.toml, main.aside, and assets/ to get started. Split out directories such as ui/ and story/ as your project grows. The configuration specifies main::main as the entry point and a logical canvas of 1920×1080. reel dev runs source code and reports errors with their file locations; reel build produces bytecode and asset packages; a lightweight player runs the build output. These commands are implementation targets.
game.toml uses entry and title in the [game] table, and width, height, and scale in the [display] table. Set entry to "main::main"; the defaults are width=1920, height=1080, and scale="contain". contain preserves the aspect ratio and leaves empty space at the edges; input coordinates are converted using the same scaling. game:: modules resolve .aside files relative to the directory containing game.toml. File and directory names must be valid module identifiers. External source modules are imported through [modules] mappings from aliases to relative directories; game and reel are reserved aliases.
use reel::prelude::* imports the names of common nodes, layouts, actions, and services. Authors still build their pages and visual style with their own functions. The game:: prefix resolves modules from the game's source root. All documents consistently use fn, let, let mut, |parameters| closures, and use imports. Except for complete module and function examples, standalone snippets belong inside a running function and reuse variables such as view and mio introduced earlier. Ordinary statements end with semicolons; the final expression in a function can serve as its return value. Object literals use #{field: value}.
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
    return #{root: root, picture: picture,
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
Define each character once in a module. Every line of dialogue explicitly names its speaker; use narrator.say for narration. A standalone string is still just a string. say updates the dialogue data and history, then waits for the current line to finish. Ordinary function calls continue along the same execution sequence.
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
model.text holds the complete text, while reveal controls how much is visible, keeping the layout stable during character-by-character display. Replacing, hiding, or showing the dialogue view preserves the session history. say accepts resource parameters such as voice; voice playback is associated with the current line. Choices also expose a data model. Authors use buttons to call game.choice.select(key), and choose returns the same key.
Add chapter metadata when you need a route index or an explicit restart point. It gives the function a stable name without changing its calling or waiting behavior.
```aside
#[story(id: "chapter01.opening", title: "窗上的光")]
fn opening() {
    // 剧情正文。
}
```
An ordinary fn is sufficient for story code. game.start(opening) submits a new-game request from a UI event. After the callback returns, a new session is created and opening is called, initializing auto-play and fast-forward state.

## 4 Sequencing Changes and Running Them Together
tween only describes target properties, final values, and duration. sequence combines actions in order, parallel combines them concurrently, and repeat repeats an action. These are all ordinary library functions that produce reusable action values. Only run starts an activity and returns a control handle. await activity.finished waits for that activity to end while input, audio, and other activities continue.
An action reads its starting property values when it actually becomes active. In a sequence, a later step reads its starting values after the previous step finishes. Action descriptions are reusable, and every run creates independent progress. Before starting, run checks for conflicting property writes at the same time. If a conflict exists, it reports the conflict at the call site and leaves existing activities intact. To take over, authors first stop the existing activity, then start the new action. Assigning directly to a property currently driven by a tween stops that tween and applies the explicit assignment. A stopped tween returns Stopped; its parent sequence stops subsequent actions, or its parent parallel stops the other actions in the group, and the overall result is Stopped.
```aside
let entrance = parallel(
    tween(view.portrait, x: 320, duration: 0.5),
    tween(view.hud, opacity: 1, duration: 0.3)
);
let activity = run(entrance, owner: view.root);
await activity.finished;
```
Each activity belongs to an explicit owner. When that owner is destroyed on the stage, its activities and callback tasks end together. pause suspends progress, resume continues it, and stop leaves the current value in place and causes a wait to return Stopped. Natural completion returns Completed. Resource errors or activity cancellation raise the corresponding error in any waiting task that is still alive, where ordinary try/catch can handle it. If the waiting task's own owner has ended, that task ends immediately. The host releases its resources, and script cleanup code ends with the task.
parallel waits for all finite child actions. If any child action fails, it ends the remaining actions in the group and reports the error; sequence stops subsequent actions on failure. repeat(action, times: 3) repeats a finite number of times; omitting times creates a loop. Continuous loops, such as breathing or falling rain, run separately under scene ownership. Story code waits only for the finite activities it needs.
```aside
let breathing = repeat(sequence(
    tween(view.portrait, opacity: 0.92, duration: 1.4),
    tween(view.portrait, opacity: 1.0, duration: 1.4)
));
run(breathing, owner: view.root);
mio.say("听，雨又大了。");
```
The example above demonstrates the rules for composing loops. Actual character breathing can use subtle movement or scaling instead, with the same script structure. Authors wrap common actions in ordinary fn functions that take any node and return an action value. UI and character portraits can share these actions.

## 5 Temporary Full-Screen CGs and Restoring the Scene
This example fades out the HUD, fades in a CG, waits for the player to view it, and then restores the original room and UI. The original room, character portrait, and component data are preserved throughout. show_cg is an ordinary helper function.
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
The example records the original values on entry. On normal return or exit through a handleable exception, finally ends any remaining activity and restores those values. finally performs only synchronous cleanup. If the entire scene's owner has ended, the host directly cleans up its tasks and nodes. Each HUD/CG pair runs only one show_cg at a time; the author's modal entry point enforces that scope.
visible=false hides the subtree and removes it from hit testing and focus; enabled=false keeps it visible but disables subtree interaction; opacity controls only visual transparency. Disabling the HUD at the start of the fade makes button behavior during the transition explicit. game.advance waits for new advance input that has not been consumed by a control and belongs to the current main-story interaction.
Use stage.replace(new_root) for a permanent scene change. For a permanent local replacement, use a container's replace(new_child). Once the old subtree is destroyed, its handles enter an ended state. Temporary overlays preserve the original nodes and suit CGs, maps, photographs, and in-scene control panels.

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
    await playback.finished;
    stage.replace(title());
}
```
Video is an optional capability. A static-image logo uses image and run(tween(...)) to express the same flow. Media and animation share the activity control protocol. Creating a video node only creates its visual representation; play starts playback. Replacing the node ends its playback and event subscriptions.
To add a temporary button during a story, add an ordinary button to the desired parent node and remove it normally afterward. The author decides which content it covers, whether it blocks input underneath, and when it disappears. One-off buttons use the same event and ownership rules.
```aside
let hint = view.hud.add(button("查看照片",
    on_click: |event| photo_panel.open()
));
mio.say("桌上那张照片，你还记得吗？");
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
The control handles pointer capture and value conversion during dragging. The same value determines both the thumb position and the volume. Authors supply rendering nodes for the track and thumb; the control lays them out according to its orientation, range, and slots. settings contains global settings, which retain their current values after a story save is loaded.
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
bind connects an observable property while preserving that property's existing read/write permissions. text can read a read-only property; slider requires a writable property when it writes a value back. derive computes read-only presentation values. A derived function establishes dependencies on the observable properties it reads and may also read immutable captured values. Internal changes to ordinary data retain ordinary language semantics. Observable property changes update only the relevant nodes. Ordinary #{...} data records are suitable for passing parameters; use model(#{...}) to create an observable model for application values that change over time. Cyclic dependencies, writes inside derived computations, and waits inside derived computations produce source-level diagnostics.
Authors can use the same approach to wrap consistent fonts, button images, spacing, and animation, then import them as ordinary modules. Visual style is reused through components and parameters; shared property objects can also be organized using ordinary language rules.

## 8 Masks, Effects, and Custom Scene Choreography
Effect values such as mask and blur belong to the same compositing system. An effect group first composites its children, then applies its effects, with group opacity applied once. Clipping defines a geometric region; an image mask defines degrees of visibility. mask.progress ranges from 0 for fully hidden to 1 for fully visible; softness controls edge width. Grayscale values in a black-and-white mask determine the order of revelation.
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
Effect parameters follow the same animation rules as node properties. More complex scenes can nest group nodes: blur the background separately, keep characters sharp, and leave image buttons on top, then apply a mask or camera transform to the whole group. The tree structure determines the front-to-back order of all children.
Custom effects use a versioned Effect description for input images, parameters, output bounds, and backend implementations. Advanced authors can develop host extensions, then compose and reuse common effects through ordinary Aside functions. Shader support is a backend capability; project checks list missing capabilities. Portable effects should also provide simplified implementations, letting the game select an appropriate platform version through capability queries.
Pixels changed by an effect and logical input regions are declared separately. Ordinary affine transforms and geometric clipping automatically map hit tests. For bends, distortions, and arbitrary shaders, authors provide logical shapes or specialized mappings for interactive regions. Reading the rendered image back from the GPU for every click would add latency, so the base interface uses regions that can be computed directly.

## 9 Choices and Changing Content
choose(options) accepts an array of options, each with a stable key and a label, updates game.choice, and waits for select(key). The current line and choices share the right to control main-story interaction. Authors place the choice view within their own scenes and choose its position and background.
```aside
fn choice_view(model) {
    list(items: bind(model.options),
         key: |option| option.key,
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
choice_view is mounted in the scene containing the dialogue UI; by_window and by_door are the author's story functions. game.choice.options is a read-only observable list, empty by default. It is populated while waiting for a choice and cleared when the choice is complete. Clicks for stale or repeated keys are filtered by interaction identity.
A for loop inside ordinary children is appropriate for constructing a scene as it is now. list is for continuously changing lists: items connects to an observable collection, key determines element identity, and build creates a node for a new key. An observable item with the same key retains its identity while its fields update; only removing that key destroys the corresponding subtree. Layout is vertical by default. Virtualized display is governed by the viewport range and reusable measurement results. Duplicate keys produce errors at the corresponding list location. Passing an ordinary array as items constructs fixed content.

## 10 Saves, History, Routes, and Skipping
saves is an independent service. save(slot_id) returns a SaveRequest, and quick_save() uses the quick-save slot; load(slot_id) and quick_load() return a LoadRequest. Requests expose read-only status, error, and finished members. Use derive or bind to display progress. finished can be awaited; save buttons usually only submit a request and observe its model. Success means the slot has been committed to storage. A successful load request replaces the session, ending callback tasks owned by the old session while the process-level service continues publishing results.
saves.slots(page: 1, page_size: 12) returns a SlotPage whose observable items include stable id, occupied, chapter, saved_at, thumbnail, and status fields. saves.delete(slot_id) returns a DeleteRequest; on success, the corresponding summary becomes an empty slot. Page numbers start at 1; page_size and page number determine a fixed set of ordinary slots, while the quick-save slot is separate. Authors build pages with grid, image, text, and button; paging only changes the displayed set.
```aside
let slot_page = saves.slots(page: 1, page_size: 12);
let save_button = button("保存", on_click: |event| {
    saves.save(slot_page.items[0].id);
});
```
Save, load, and delete requests share the Queued, Running, Succeeded, and Failed states. Saves additionally expose phase = Capturing or Writing. error contains an explanation on failure and is null on success. finished returns the request result; request failure throws the corresponding error. Slot summaries are replaced only after a successful commit; failure preserves the previous summary.
game.history is a read-only observable list. Each entry contains id, speaker_name, text, voice, and restore-point information. Put a list inside scroll to display history entries as needed. Loading or rolling back switches the list to the restored session state. Ordinary UI replacements preserve history.
game.stories.entries is the chapter index, with id, title, and visited fields on each entry; game.stories.links holds explicitly registered from/to connections. The compiler collects entry points from #[story], while game.stories.link(from_id, to_id) registers dynamic connections. Authors represent chapters with buttons and draw connections with path nodes, arranging them manually or with their own layout algorithms.
game.restart(story_id) reruns the current session's story task from a named chapter entry point. It preserves current story variables and ends the previous main-story task and its interaction; the target function is responsible for constructing its own scene. game.start(entry) creates a fresh session, suitable for a new game. saves.load restores the saved position, variables, and scene. Authors use these distinctions to give route buttons clearly defined actions.
Hold-to-fast-forward, auto-play, and skipping to a chapter use a shared story-advancement service. game.skip_to(story_id) accelerates execution through normal control flow, stopping at the target chapter entry, a choice, or an unread-content boundary. If the target is unreachable, the story ends normally and the service reports that the target was not reached. game.set_auto(enabled) controls auto-play. Hold-to-fast-forward is driven by the held state of the input layer's skip action; authors can bind keyboard or gamepad input to that action.
Fast-forward shortens skippable waits for already-read content and finite scene activities, while variable calculations still execute in sequence. Releasing the input, losing focus, opening a menu, starting a new game, or loading a save clears the held state. A tween completes at its final value, finite waits end, and media stops or continues according to its playback policy. Looping activities remain managed by their owner. Author-provided control hints follow the same service state.
These services provide data and operations; authors compose the appearance of their pages. The signatures listed in this edition are specifications awaiting implementation. Once a feature passes its contract acceptance checks, the guide will mark that section with the implemented version and run commands.

## 11 Extending Your Own Building Blocks
The smallest unit of reuse is an ordinary fn. Functions returning nodes build scenes; functions returning actions choreograph them; ordinary objects expose parts that callers need to manipulate. Named arguments, children, closures, and module imports work together to support this approach.
```aside
fn arrive(node, destination_x, seconds = 0.5) {
    parallel(
        tween(node, x: destination_x, duration: seconds),
        tween(node, opacity: 1, duration: seconds)
    )
}
```
The same arrive function can be used for characters, title-screen buttons, or entire groups of text. Changing images, layouts, and action parameters lets authors build their own component libraries. Custom nodes are registered through the host and participate in the same property, input, rendering, and restoration protocols. New language keywords are reserved for rules that ordinary composition genuinely struggles to express and that multiple creative scenarios need.

## 12 Keeping Documentation and Code in Sync
The project lead maintains this guide. Each API is marked as designed or implemented, with its applicable version. Once implemented, examples are extracted as source snippets checked alongside the code: parsing, type and interface checks, and any necessary behavior verification use the same examples. The Chinese and English editions reference the same example files; translation updates are committed with interface changes.
When an interface, behavior, default, or cancellation rule changes, the same PR updates the implementation, specification, both language editions, and affected examples. Advanced uses still in planning are labeled separately. Users can begin creating with the implemented sections. When composition proves difficult, they can report minimal source code and the intended scene so the project lead can refine the shared interfaces.
The repository commit containing this guide is its version reference. Feature status is marked by section; interface changes update the Chinese edition, English edition, and shared examples together.
The core changes from the previous draft are a unified fn declaration, scenes organized as complete node trees, a separation between constructing actions and starting activities, an explicit distinction between temporary overlays and permanent replacements, and fuller definitions of control appearance, derived bindings, and persistent models. These explainable author-facing behaviors in turn constrain the engine design.
