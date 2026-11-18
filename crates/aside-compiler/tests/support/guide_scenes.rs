use aside_compiler::EntryPoint as E;
pub const SCENES: &[(&str, E)] = &[
    ("build target selection", E::Block),
    ("minimal single-file project", E::Module),
    ("main module", E::Module),
    ("title module", E::Module),
    ("ranges, collections and interpolation", E::Block),
    ("generator positions", E::Module),
    ("table lookup defaults", E::Block),
    ("stable array sorting", E::Block),
    ("ordered array and table spreads", E::Block),
    ("tuple bindings", E::Block),
    ("value match expressions", E::Block),
    ("collection editing and own fields", E::Block),
    ("binary asset reads", E::Block),
    ("numeric integer conversion", E::Block),
    ("number formatting", E::Block),
    ("component returns and optional children", E::Block),
    ("remaining space and grid layout", E::Block),
    ("intrinsic panel layout", E::Block),
    ("custom mouse cursor", E::Block),
    ("typed author classes", E::Module),
    ("configure a component", E::Block),
    ("bind a portrait layer", E::Block),
    ("portrait selected by expression", E::Block),
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
    ("composable scrollbar", E::Module),
    ("simultaneous dialogue in story", E::Block),
    ("segmented dialogue in story", E::Block),
    ("chained entrance", E::Block),
    ("accelerating scalar curve", E::Block),
    ("paused scene entrance", E::Block),
    ("clock frame animation", E::Block),
    ("entrance in story", E::Block),
    ("parallel entrance", E::Block),
    ("reusable entrance action", E::Block),
    ("breathing in story", E::Block),
    ("timed delay in story", E::Block),
    ("sampled scalar curves", E::Module),
    ("scoped reading page", E::Module),
    ("show_cg helper", E::Module),
    ("scoped property override", E::Block),
    ("scoped additive offset", E::Block),
    ("cancellable scene module", E::Module),
    ("video startup", E::Block),
    ("packed alpha video", E::Block),
    ("temporary hint in story", E::Block),
    ("runnable button module", E::Module),
    ("anchored popup", E::Block),
    ("modal menu module", E::Module),
    ("named input actions module", E::Module),
    ("picture button component", E::Module),
    ("interaction state callback", E::Block),
    ("shared selection controls", E::Block),
    ("retained pages and tabs", E::Module),
    ("reading settings module", E::Module),
    ("window configuration", E::Block),
    ("accepted focus handoff", E::Block),
    ("focus indicator preservation", E::Block),
    ("editable text in story", E::Block),
    ("editing transactions", E::Block),
    ("shared property module", E::Module),
    ("menu style and construction", E::Module),
    ("moving group in story", E::Block),
    ("scoped choreography in story", E::Block),
    ("stage transition", E::Block),
    ("mask in story", E::Block),
    ("composed mask", E::Block),
    ("subtree inversion", E::Block),
    ("subtree blending", E::Block),
    ("subtree drawing effect", E::Module),
    ("texture drawing mask", E::Module),
    ("continuous particles", E::Block),
    ("burst particles", E::Block),
    ("particle lifetime curve", E::Block),
    ("particle size animation", E::Block),
    ("progressive glyph opacity", E::Block),
    ("authored line wrapping", E::Block),
    ("component result in story", E::Block),
    ("scoped prompt in story", E::Block),
    ("choice component and branch in story", E::Block),
    ("conditional component options", E::Block),
    ("clear dialogue presentation", E::Block),
    ("retain recent history", E::Block),
    ("logical story checkpoint", E::Module),
    ("save button construction", E::Module),
    ("clear read progress", E::Block),
    ("shared model controls", E::Block),
    ("persistent settings", E::Block),
    ("arrive helper", E::Module),
    ("text spacing", E::Block),
    ("vertical text layout", E::Block),
    ("bilingual dialogue", E::Block),
    ("inline translations in story", E::Block),
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
        "focus indicator preservation" => &["next"],
        "particle size animation" => &["glow"],
        "segmented dialogue in story" => &["mio"],
        "anchored popup" => &["trigger", "preferences"],
        "accelerating scalar curve" | "chained entrance" => &["portrait"],
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
