# Aside author scene inputs

Executable inputs shared by the Reel 0.4.5 creator guides. Parse entry points
and enclosing bindings are in ../support/guide_scenes.rs.

```aside
let particle_count = if build.target == "vita" { 80 } else { 320 };
```

```aside
fn main() {
    game.configure(title: "Window light", width: 960, height: 540,
        fonts: ["fonts/body.ttf"]);
    stage.replace(text("Hello", x: 24, y: 24));
}
```

```aside
use ui::title::title;

fn main() {
    game.configure(title: "窗边", width: 1920, height: 1080, fonts: ["fonts/body.ttf"]);
    stage.replace(title());
}
```

```aside
use story::opening::opening;

pub fn title() {
    column(x: 760, y: 480, gap: 24) {
        text("窗上的光");
        button("新游戏", on_click: |event| game.start(opening));
    }
}
```

```aside
let values: array<int> = [];
for value in 1..=3 { values.append(value); }
let latest = values.pop();
stage.replace(text(f"剩余 {values.len()} 项，最后一项 {latest}"));
```

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

```aside
let settings = {volume: 0, wallpaper: null};
let volume = settings.get("volume", default: 0.7);
let wallpaper = settings.get("wallpaper", default: "default.png");
let language = settings.get("language", default: "zh-CN");
let removed = settings.remove("wallpaper");
settings.wallpaper = "new.png";
```

```aside
let cuts = [180, 20, 90];
cuts.sort();
let captions = [{text: "后景", layer: 2}, {text: "前景", layer: 1}];
captions.sort(compare: |a, b| a.layer - b.layer);
```

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

```aside
type CaptionStyle = {message: string, color?: string};

fn caption_color(style: CaptionStyle) -> string {
    style.color ?? "#FFFFFF"
}
```

```aside
let settings = {language: "zh-CN", volume: 0.8};
for (key, value) in settings.entries() {
    let pair = (key, value);
    let (name, _) = pair;
}
```

```aside
let caption = match mode {
    "online" | "typing" => "在线",
    name is string if name != "" => name,
    _ => "离线",
};
```

```aside
enum MenuResult {
    Back,
    Load(slot: int, preview: bool),
}

fn selected(result: MenuResult) -> string {
    match result {
        MenuResult.Back => "返回",
        MenuResult.Load { slot, .. } => f"读取第 {slot} 槽",
    }
}

fn slot_to_load(result: MenuResult) -> int | null {
    if let MenuResult.Load { slot, .. } = result { slot } else { null }
}
```

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
let divider = caption.find(" · ");
let chapters = rows.filter(|row| row != "序章");
let labels = chapters.map(|row| f"阅读 {row}");
let numbered = [];
for index, label in labels {
    numbered.append(f"{index + 1}. {label}");
}
```

```aside
let cells = await assets.read("data/cells.bin");
let count = cells.len();
let first = cells[0];
```

```aside
use std::math::to_int;
let frame = to_int(1.25 * 20);
```

```aside
use std::number::format;
let price = format(12.25, precision: 1);
let total = format(123456789, precision: 4, mode: "scientific");
```

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

```aside
stack(max_width: 420) {
    image("images/panel.png", stretch: "both", slice: 12);
    column(padding: 24, gap: 16) {
        text("The frame follows wrapped text and buttons.", align_x: 0.5, text_align: "center");
        button("Dismiss", align_x: 0.5, on_click: |event| event.target.remove());
    }
}
```

```aside
let pointer = cursor {
    image("images/pointer.png", anchor_x: 0.5, anchor_y: 0.5);
};
panel.add(pointer);
```

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

```aside
let caption = text("窗外下起了雨。")
    .with(font_size: 28, color: "#eeeeee")
    .with(x: 48, y: 420, max_width: 720);
caption.with(opacity: 0.5);
```

```aside
let pose = model({ face: null });
let portrait = layered_image([{ source: "assets/body.png" }, bind(pose.face)]);
pose.face = { source: "assets/smile.png" };
```

```aside
fn portrait(expression: string) {
    let face = match expression {
        "smile" => "assets/smile.png",
        _ => "assets/neutral.png",
    };
    layered_image([{ source: "assets/body.png" }, { source: face }])
}

narrator("The door opened.");
let arrival = portrait("smile");
stage.replace(arrival);
```

```aside
stage.replace(scene);
await stage.presented(owner: scene);
await tween(scene, opacity: 1, duration: 0.5).run();
```

```aside
let look = appearance(base: {background: "#304060", corner_radius: 6},
    hover: {color: "#ffd580"}, pressed: {background: "#203040"},
    focus_visible: {border_width: 2}, disabled: {opacity: 0.4});
column(gap: 12) {
    button("Hover me", width: 240, height: 48, decorated: false, appearance: look);
    button("Another component", width: 240, height: 48, decorated: false, appearance: look);
}
```

```aside
let back = image("assets/panels.png");
let front = image("assets/sun.jpg", x: 120);
let scene = stack { back; front; };
stage.replace(scene);
scene.move(back, index: 1);
```

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

```aside
let view = camera(width: 960, height: 540, camera_x: 480, camera_y: 270) {
    image("assets/panels.png", width: 960, height: 540);
    text("Look here", x: 620, y: 160);
};
stage.replace(view);
await stage.presented(owner: view);
await tween(view, camera_x: 660, camera_y: 210, zoom: 1.8,
                camera_rotation: 8, duration: 1.2, easing: "in_out").run();
```

```aside
let room = camera(width: 960, height: 540, camera_x: 480, camera_y: 270) {
    camera_plane(distance: 900) { image("assets/room.png", width: 960, height: 540); };
    camera_plane(distance: 600) { image("assets/mio.png", x: 370, y: 85); };
};
stage.replace(room);
await tween(room, camera_z: 120, duration: 0.8).run();
```

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

```aside
fn card(title, children = []) {
    column(gap: 12, padding: 24) {
        text(title);
        ...children;
    }
}
```

```aside
overlay.visible = true;
overlay.opacity = 0;
let geometry = await stage.geometry(thumbnail, relative_to: overlay, owner: root);
indicator.x = geometry.bounds.x + geometry.bounds.width / 2;
indicator.y = geometry.bounds.y;
overlay.opacity = 1;
```

```aside
mio(rich("Listen.[pause=0.5]"), reveal_skippable: false);
narrator("The light is moving.", reveal_skippable: false);
say([mio.line("One."), eri.line("Two.")], reveal_skippable: false);
```

```aside
mio("……", history: "I remember the rain.");
narrator("A fleeting thought.", history: false);
say([mio.line("Noise.", history: false), eri.line("Stay.", history: rich("[b]Stay.[/b]"))]);
```

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

```aside
fn main() {
    let viewport = scroll(width: 280, height: 120) {
        column() { for i in 0 .. 20 { text(f"Line {i}"); } }
    };
    let bar = scrollbar(viewport, width: 16, height: 120,
        visible: derive(|| viewport.max_scroll_y > 0));
    bar.track.background = "#28303a";
    bar.thumb.background = "#9daabe";
    stage.replace(row(gap: 8) { viewport; bar; });
}
```

```aside
let eri = character("Eri");
say([mio.line("你听见了吗？"), eri.line("在窗外。")]);
```

```aside
let line = mio("Starting", wait: false);
await delay(0.5).run();
line.append(" ...", wait: false);
audio.play("assets/chime.wav");
line.append(" ready.");
line.append("\nWelcome back.");
```

```aside
let entrance = tween(portrait, opacity: 1, duration: 0.3);
let move = tween(portrait, x: 640, duration: 1);
await sequence(entrance, move, delay(0.5)).run();
```

```aside
use std::math::curve;

let acceleration = curve([
    {time: 0, value: 0, mode: "cubic", out_tangent: 0.5},
    {time: 1, value: 1, in_tangent: 1.5},
]);
await tween(portrait, x: 640, duration: 2, easing: acceleration).run();
```

```aside
panel.opacity = 0;
let music = audio.play("assets/chime.wav", owner: panel, paused: true);
let entrance = tween(panel, opacity: 1, duration: 0.25).run(owner: panel, paused: true);
stage.replace(panel);
await stage.presented(owner: panel);
music.resume();
entrance.resume();
```

```aside
let frames = ["assets/frame-a.png", "assets/frame-b.png"];
let count = frames.len();
let timer = clock(interval: 0.125, paused: true, owner: root);
root.add(image(source: derive(|| frames[timer.ticks % count]), width: 64, height: 64));
stage.replace(root);
await stage.presented(owner: root);
timer.resume();
```

```aside
await tween(portrait, x: 720, duration: 0.8, easing: "sine_out").run();
```

```aside
let entrance = parallel(
    tween(view.portrait, x: 320, duration: 0.5),
    tween(view.hud, opacity: 1, duration: 0.3),
);
await entrance.run();
```

```aside
fn fade_in(node, seconds = 0.5) {
    tween(node, from: {opacity: 0}, opacity: 1, duration: seconds)
}
await sequence(delay(0.5), fade_in(view.hud)).run();
```

```aside
let breathing = repeat(sequence(
    tween(view.portrait, opacity: 0.92, duration: 1.4),
    tween(view.portrait, opacity: 1.0, duration: 1.4),
));
breathing.run(owner: view.root);
mio("听，雨又大了。");
```

```aside
await delay(1.5).run();
await sequence(delay(0.5), tween(panel, opacity: 0, duration: 0.3)).run();
```

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
    timer.resume();
    await delay(3).run();
    timer.stop();
}
```

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

```aside
scope(body: |s| {
    let effect = s.override(panel, {opacity: 0.4, width: null});
    await tween(effect, opacity: 1, duration: 0.3).run(owner: s);
    let next = completion();
    s.add(panel, button("Continue", on_click: || next.resolve()));
    await next;
});
```

```aside
let moving = tween(panel, x: 500, duration: 4).run();
scope(body: |s| {
    let shake = s.offset(panel, y: -3);
    await repeat(sequence(
        tween(shake, x: 6, duration: 0.05),
        tween(shake, x: -6, duration: 0.05)
    ), times: 8).run();
});
await moving;
```

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

```aside
let movie = video("assets/logo.mp4", width: 1280, height: 720, fit: "contain");
let playing = movie.play();
stage.replace(button(width: 1280, height: 720, decorated: false,
    background: "#000000", on_click: || playing.stop()) {
    movie;
});
await playing;
```

```aside
let overlay = video("assets/overlay.mp4", alpha_layout: "top_bottom", width: 960, height: 512);
stage.replace(overlay);
await overlay.play();
```

```aside
let hint = view.hud.add(button("查看照片",
    on_click: |event| photo_panel.open()
));
mio("桌上那张照片，你还记得吗？");
hint.remove();
```

```aside
fn main() {
    let caption = text("Ready", width: 320, height: 48);
    let controls = row(gap: 16) {
        button("Move", width: 180, height: 64, background: "#284860",
            on_click: |event| {
                event.target.enabled = false;
                await tween(caption, x: 120, duration: 0.4).run();
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
                    await sequence(
                        tween(marker, y: 16, duration: 0.15),
                        tween(marker, y: 64, duration: 0.15)
                    ).run();
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

```aside
fn picture_button(normal, hovered, pressed, clicked) {
    let control = button(on_click: clicked, decorated: false);
    control.add(image(image_skin(control, normal, highlight: hovered, pressed: pressed)));
    return control;
}
```

```aside
let gallery = model({selected: null});
let card = button("Sketch", on_interaction: |event| {
    if event.hover || event.focus_visible { gallery.selected = "sketch"; }
});
```

```aside
let settings = model({sound: true, language: "zh-CN"});
column(gap: 12) {
    checkbox("Sound", value: bind(settings.sound));
    radio("zh-CN", "简体中文", value: bind(settings.language));
    radio("en", "English", value: bind(settings.language));
}
```

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

```aside
let mut display = window.info();
if display.capabilities.resize {
    display = await window.configure(width: 1280, height: 720);
}
if display.capabilities.fullscreen {
    display = await window.configure(fullscreen: true);
}
```

```aside
let next = button("Next", visible: false);
stage.replace(column { button("Skip"); next; });
await stage.presented();
next.visible = true;
let focused = await stage.focus(next);
```

```aside
let focused = await stage.focus(next, visibility: "preserve");
```

```aside
let profile = model({name: ""});
stage.replace(text_input(value: bind(profile.name), width: 280, height: 40,
    color: "#202020", background: "#ffffff",
    on_submit: |event| { profile.name = event.value; }));
```

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

```aside
let cast = stack() {
    image("assets/mio.png");
    dialogue_view(game.dialogue());
};
stage.replace(cast);
await tween(cast, x: 80, duration: 0.5).run();
```

```aside
scope(body: |s| {
    repeat(sequence(
        tween(view.hud, opacity: 0.4, duration: 0.5),
        tween(view.hud, opacity: 1, duration: 0.5)
    )).run(owner: s);
    narrator("The light keeps pulsing while this line is shown.");
});
```

```aside
stage.transition(target: scene, duration: 0.6, change: || {
    scene.clear();
    scene.add(image("assets/room-evening.png"));
});
```

```aside
let reveal = stack(width: 1280, height: 720,
    mask: "assets/ink-mask.png", mask_progress: 0, mask_softness: 0.06) {
    image("assets/window-cg.png");
};
view.picture.add(reveal);
await tween(reveal, mask_progress: 1, duration: 1.2).run(owner: reveal);
reveal.mask = null;
```

```aside
let edge = mask.alpha("assets/mask-waves.png", x: mask.param("x"), repeat: "x");
let reveal = stack(width: 640, height: 360, mask: edge, mask_x: 0) {
    stack(width: 640, height: 360, background: "#2d8aab");
};
stage.replace(reveal);
await tween(reveal, mask_x: 1, duration: 3).run(owner: reveal);
```

```aside
let scene = stack(width: 800, height: 450, invert: 0) {
    text("A different view", color: "#e08020", font_size: 48);
};
stage.replace(scene);
await tween(scene, invert: 1, duration: 0.4).run(owner: scene);
```

```aside
stage.replace(stack(width: 320, height: 180, background: "#204060") {
    stack(width: 320, height: 180, background: "#800000", blend: "add", opacity: 0.5);
});
```

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
    await tween(scene, mask_progress: 1, duration: 1.2).run(owner: scene);
    scene.mask = null;
}
```

```aside
let petals = particles("assets/petal.png", width: 1280, height: 20, seed: 42);
stage.replace(petals);
let falling = emit(petals, burst: 0, rate: 80, duration: 10,
    lifetime: 8, velocity_y: 90, velocity_x: 15, spread_x: 30,
    size: 18, size_spread: 6, spin_spread: 60);
await falling;
```

```aside
let spray = particles("assets/petal.png", x: 640, y: 360, seed: 42);
stage.replace(spray);
fn burst() {
    emit(spray, burst: 48, lifetime: 1, lifetime_spread: 0.4,
        velocity_y: -160, spread_x: 180, spread_y: 100,
        acceleration_y: 240, size: 18, size_spread: 6, size_end: 0)
}
let first = burst();
await delay(0.2).run();
let second = burst();
await first;
await second;
spray.remove();
```

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

```aside
await tween(glow, size_scale: 0, duration: 0.2).run();
```

```aside
let caption = text("The light is moving.", reveal_fade: 0,
    reveal_fade_from: 0, reveal_fade_step: 20.0 / 255.0);
stage.replace(caption);
await tween(caption, reveal_fade: 3, duration: 3.0 * 255.0 / 1400.0).run(owner: caption);
caption.reveal_fade = null;
```

```aside
stage.replace(text("A long authored line without automatic wrapping.\nA second explicit line.",
    width: 240, text_wrap: false, clip: true));
```

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

```aside
scope(owner: root, body: |s| {
    let result = completion();
    s.add(root, button("Confirm", enabled: bind(result.pending),
        on_click: || result.resolve(true)));
    await result
});
```

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

```aside
let flag = true;
let actions = if flag {
    {accept: [binding("Enter")]}
} else {
    {back: [binding("Escape")]}
};
```

```aside
game.dialogue().clear();
game.reading().clear_history();
narrator("A new chapter begins.");
```

```aside
game.reading().clear_history(keep_last: 37);
```

```aside
use std::resume::{root, flow};

class GameState {
    let mut score: int = 0;
}
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

```aside
stage.replace(button("Reset read progress", on_click: |event| game.clear_read_progress()));
```

```aside
let settings = model({volume: 0.8, music: true, palette: {accent: "#70d890"}});
let volume = slider(value: bind(settings.volume), min: 0, max: 1);
let music = toggle(value: bind(settings.music));
let indicator = text("Music", visible: bind(settings.music), color: bind(settings.palette.accent));
settings.palette.accent = "#ffd580";
```

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

```aside
fn arrive(node, destination_x, seconds = 0.5) {
    parallel(
        tween(node, x: destination_x, duration: seconds),
        tween(node, opacity: 1, duration: seconds)
    )
}
```

```aside
stage.replace(column(gap: 12) {
    text("CHAPTER ONE", font_size: 32, letter_spacing: 2);
    text("made with love", font_size: 28, word_spacing: -2);
});
```

```aside
let sidebar = text(rich("[ruby=ほし]星[/ruby]を見上げる。"),
    writing_mode: "vertical-rl", height: 690,
    font_size: 26, line_height: 40 / 26.0);
stage.replace(sidebar);
```

```aside
localization.set_locale("zh-CN");
let data = model({name: "Mio"});
stage.replace(column(gap: 12) {
    text(derive(|| tr(f"你好，{data.name}。", {en: f"Hello, {data.name}."})));
    button("English", on_click: |event| localization.set_locale("en"));
});
```

```aside
localization.load("en");
column(gap: 6) {
    text(tr("你好。", {en: "Hello."}, locale: "zh-CN"), font_size: 28);
    text(tr("你好。", {en: "Hello."}, locale: "en"), font_size: 18);
}
```

```aside
narrator("Wait for me.", voice: "assets/wait-for-me.opus");
```

```aside
fn main() {
    audio.group_volume("music", 0.7);
    let music = audio.play("assets/chime.wav", group: "music", looping: true, volume: 0.4, fade_in: 0.5);
    music.pause();
    music.resume();
    audio.volume(music, 0.2, duration: 0.5);
    let sound = audio.play("assets/chime.wav", pan: -0.5, rate: 0.8);
    audio.rate(sound, 1.2, duration: 0.15);
    await sound;
    audio.stop(music, fade: 0.5);
    await music;
}
```

```aside
let mut music = audio.play("assets/intro.ogg", group: "music");
audio.queue(music, "assets/loop.ogg", looping: true);
narrator("Continue to leave the loop after this pass.");
audio.queue(music, "assets/outro.ogg");
await music;
```

```aside
let next = audio.play("assets/music-next.ogg", group: "music", looping: true,
    start: music.position(), loop_start: 4, loop_end: 64, fade_in: 2);
audio.stop(music, fade: 2);
music = next;
```
