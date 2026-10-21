use aside_compiler::EntryPoint as E;
pub const SCENES: &[(&str, E)] = &[
    ("minimal single-file project", E::Module),
    ("main module", E::Module),
    ("title module", E::Module),
    ("ranges, collections and interpolation", E::Block),
    ("table lookup defaults", E::Block),
    ("stable array sorting", E::Block),
    ("ordered array and table spreads", E::Block),
    ("tuple bindings", E::Block),
    ("value match expressions", E::Block),
    ("collection editing and own fields", E::Block),
    ("component returns and optional children", E::Block),
    ("intrinsic panel layout", E::Block),
    ("custom mouse cursor", E::Block),
    ("typed author classes", E::Module),
    ("presentation wait", E::Block),
    ("locally composed control", E::Block),
    ("layer ordering", E::Block),
    ("dynamic stage module", E::Module),
    ("image module", E::Module),
    ("transform module", E::Module),
    ("camera framing", E::Block),
    ("room module", E::Module),
    ("card component", E::Module),
    ("accepted node geometry", E::Block),
    ("protected dialogue reveal", E::Block),
    ("selective history", E::Block),
    ("opening module", E::Module),
    ("dialogue component", E::Module),
    ("simultaneous dialogue in story", E::Block),
    ("segmented dialogue in story", E::Block),
    ("accelerating scalar curve", E::Block),
    ("paused scene entrance", E::Block),
    ("clock frame animation", E::Block),
    ("entrance in story", E::Block),
    ("parallel entrance", E::Block),
    ("reusable entrance action", E::Block),
    ("breathing in story", E::Block),
    ("timed delay in story", E::Block),
    ("sampled scalar curves", E::Module),
    ("show_cg helper", E::Module),
    ("scoped property override", E::Block),
    ("scoped additive offset", E::Block),
    ("cancellable scene module", E::Module),
    ("video startup", E::Block),
    ("temporary hint in story", E::Block),
    ("runnable button module", E::Module),
    ("modal menu module", E::Module),
    ("named input actions module", E::Module),
    ("picture button component", E::Module),
    ("interaction state callback", E::Block),
    ("shared selection controls", E::Block),
    ("reading settings module", E::Module),
    ("window configuration", E::Block),
    ("editable text in story", E::Block),
    ("shared property module", E::Module),
    ("menu style and construction", E::Module),
    ("moving group in story", E::Block),
    ("scoped choreography in story", E::Block),
    ("stage transition", E::Block),
    ("mask in story", E::Block),
    ("composed mask", E::Block),
    ("subtree inversion", E::Block),
    ("subtree blending", E::Block),
    ("continuous particles", E::Block),
    ("burst particles", E::Block),
    ("progressive glyph opacity", E::Block),
    ("authored line wrapping", E::Block),
    ("component result in story", E::Block),
    ("scoped prompt in story", E::Block),
    ("choice component and branch in story", E::Block),
    ("conditional component options", E::Block),
    ("clear dialogue presentation", E::Block),
    ("retain recent history", E::Block),
    ("save button construction", E::Module),
    ("clear read progress", E::Block),
    ("shared model controls", E::Block),
    ("persistent settings", E::Block),
    ("arrive helper", E::Module),
    ("text spacing", E::Block),
    ("bilingual dialogue", E::Block),
    ("external translations in story", E::Block),
    ("author audio", E::Module),
    ("audio continuation", E::Block),
    ("queued audio continuation", E::Block),
];
pub fn snippets(doc: &str) -> Vec<(usize, String)> {
    let mut result = Vec::new();
    let mut start = None;
    let mut text = String::new();
    for (i, line) in doc.lines().enumerate() {
        if line == "```aside" {
            assert!(start.is_none());
            start = Some(i + 2);
            text.clear();
        } else if line == "```" && start.is_some() {
            result.push((start.take().unwrap(), text.clone()));
        } else if start.is_some() {
            text.push_str(line);
            text.push('\n');
        }
    }
    assert!(start.is_none());
    result
}

#[allow(dead_code)]
pub fn enclosing(scene: &str) -> &[&str] {
    match scene {
        "segmented dialogue in story" => &["mio"],
        "accelerating scalar curve" => &["portrait"],
        "opening module" => &["show_cg"],
        "value match expressions" => &["mode"],
        "mask in story" => &["view"],
        "stage transition" => &["scene"],
        "presentation wait" => &["scene"],
        "custom mouse cursor" | "paused scene entrance" => &["panel"],
        "clock frame animation" => &["root"],
        "selective history" | "protected dialogue reveal" => &["mio", "eri"],
        "simultaneous dialogue in story" => &["mio"],
        "entrance in story" => &["portrait"],
        "parallel entrance" | "reusable entrance action" => &["view"],
        "breathing in story" => &["view", "mio"],
        "timed delay in story" | "scoped property override" | "scoped additive offset" => {
            &["panel"]
        }
        "temporary hint in story" => &["view", "photo_panel", "mio"],
        "moving group in story" => &["dialogue_view"],
        "scoped choreography in story" => &["view"],
        "scoped prompt in story" => &["root"],
        "choice component and branch in story" => &["by_window", "by_door"],
        "queued audio continuation" => &["music"],
        "accepted node geometry" => &["overlay", "thumbnail", "root", "indicator"],
        _ => &[],
    }
}

#[allow(dead_code)]
pub fn mutable(scene: &str, name: &str) -> bool {
    scene == "queued audio continuation" && name == "music"
}

// Kept as syntax/recovery inputs; these are not implemented author interfaces.
#[allow(dead_code)]
pub const PLANNED_SOURCES: &[(&str, &str, E)] = &[
    (
        "story registration",
        include_str!("../fixtures/guide-planned/story_registration.aside"),
        E::Module,
    ),
    (
        "preferences component",
        include_str!("../fixtures/guide-planned/preferences_component.aside"),
        E::Module,
    ),
    (
        "game variables in story",
        include_str!("../fixtures/guide-planned/game_variables_in_story.aside"),
        E::Block,
    ),
    (
        "settings component",
        include_str!("../fixtures/guide-planned/settings_component.aside"),
        E::Module,
    ),
];
