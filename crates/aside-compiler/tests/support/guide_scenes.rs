use aside_compiler::EntryPoint as E;
pub const SCENES: &[(&str, E)] = &[
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
    ("protected dialogue reveal", E::Block),
    ("selective history", E::Block),
    ("opening module", E::Module),
    ("dialogue component", E::Module),
    ("simultaneous dialogue in story", E::Block),
    ("story registration", E::Module),
    ("paused scene entrance", E::Block),
    ("entrance in story", E::Block),
    ("parallel entrance", E::Block),
    ("reusable entrance action", E::Block),
    ("breathing in story", E::Block),
    ("timed delay in story", E::Block),
    ("show_cg helper", E::Module),
    ("scoped property override", E::Block),
    ("scoped additive offset", E::Block),
    ("video startup", E::Block),
    ("temporary hint in story", E::Block),
    ("preferences component", E::Module),
    ("picture button component", E::Module),
    ("interaction state callback", E::Block),
    ("menu style and construction", E::Module),
    ("moving group in story", E::Block),
    ("scoped choreography in story", E::Block),
    ("stage transition", E::Block),
    ("mask in story", E::Block),
    ("composed mask", E::Block),
    ("subtree inversion", E::Block),
    ("subtree blending", E::Block),
    ("authored line wrapping", E::Block),
    ("component result in story", E::Block),
    ("choice component and branch in story", E::Block),
    ("clear dialogue presentation", E::Block),
    ("retain recent history", E::Block),
    ("save button construction", E::Module),
    ("clear read progress", E::Block),
    ("shared model controls", E::Block),
    ("game variables in story", E::Block),
    ("persistent settings", E::Block),
    ("settings component", E::Module),
    ("arrive helper", E::Module),
    ("bilingual dialogue", E::Block),
    ("external translations in story", E::Block),
    ("runnable button module", E::Module),
    ("modal menu module", E::Module),
    ("named input actions module", E::Module),
    ("shared selection controls", E::Block),
    ("reading settings module", E::Module),
    ("editable text in story", E::Block),
    ("shared property module", E::Module),
    ("author audio", E::Module),
    ("audio continuation", E::Block),
    ("queued audio continuation", E::Block),
    ("window configuration", E::Block),
    ("continuous particles", E::Block),
    ("burst particles", E::Block),
    ("accepted node geometry", E::Block),
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
        "opening module" => &["show_cg"],
        "value match expressions" => &["mode"],
        "mask in story" => &["view"],
        "stage transition" => &["scene"],
        "presentation wait" => &["scene"],
        "custom mouse cursor" | "paused scene entrance" => &["panel"],
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
