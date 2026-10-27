//! One variant inventory for the independently maintained GLSL and native Cg sources.

pub struct Shader {
    pub name: String,
    #[allow(dead_code)]
    pub vertex: bool,
    pub source: String,
}

pub fn sources(directory: &str, extension: &str, native: bool) -> Vec<Shader> {
    let mut shaders = Vec::new();
    let read = |name: &str| {
        let path = format!("{directory}/{name}.{extension}");
        println!("cargo:rerun-if-changed={path}");
        std::fs::read_to_string(path).expect("read shader source")
    };
    let mut add = |name: &str, vertex, source: &str, defines: &str| {
        let source = if native {
            format!("{defines}\n{source}")
        } else {
            source.replacen("#version 100", &format!("#version 100\n{defines}"), 1)
        };
        shaders.push(Shader {
            name: name.into(),
            vertex,
            source,
        });
    };
    for name in [
        "vertex",
        "composite_vertex",
        "effect_vertex",
        "effect_padded_vertex",
    ] {
        add(name, true, &read(name), "");
    }
    let composite = read("composite");
    for (name, define) in [
        ("composite", ""),
        ("source_over", "REEL_SOURCE_OVER_ONLY"),
        ("alpha_mask", "REEL_ALPHA_MASK_ONLY"),
        ("extended_blend", "REEL_EXTENDED_BLEND"),
    ] {
        add(
            name,
            false,
            &composite,
            &if define.is_empty() {
                String::new()
            } else {
                format!("#define {define}")
            },
        );
    }
    let mask = read("mask");
    add("mask", false, &mask, "");
    for capacity in [8, 16] {
        add(
            &format!("mask_{capacity}"),
            false,
            &mask,
            &format!("#define REEL_MASK_NODES {capacity}"),
        );
    }
    for capacity in [16, 32] {
        add(
            &format!("mask_{capacity}_compact"),
            false,
            &mask,
            &format!("#define REEL_MASK_NODES {capacity}\n#define REEL_MASK_SLOTS 8"),
        );
    }
    let split = read("mask_split");
    for (name, fetches, math, alpha) in [
        ("mask_split_1", 1, 8, false),
        ("mask_split_2", 2, 16, false),
        ("mask_split_2_alpha", 2, 16, true),
    ] {
        add(
            name,
            false,
            &split,
            &format!(
                "#define REEL_MASK_FETCHES {fetches}\n#define REEL_MASK_MATH {math}\n{}",
                if alpha {
                    "#define REEL_MASK_ALPHA_ONLY"
                } else {
                    ""
                }
            ),
        );
    }
    let paired = read("mask_paired");
    for (channels, defines) in [
        ("aa", ""),
        ("ag", "#define REEL_MASK_SECOND_GRAY"),
        ("ga", "#define REEL_MASK_FIRST_GRAY"),
        (
            "gg",
            "#define REEL_MASK_FIRST_GRAY\n#define REEL_MASK_SECOND_GRAY",
        ),
    ] {
        for composite in [false, true] {
            add(
                &format!(
                    "mask_paired_{channels}{}",
                    if composite { "_composite" } else { "" }
                ),
                false,
                &paired,
                &format!(
                    "{defines}\n{}",
                    if composite {
                        "#define REEL_MASK_COMPOSITE"
                    } else {
                        ""
                    }
                ),
            );
        }
    }
    let sample = read("mask_sample");
    for composite in [false, true] {
        for threshold in [false, true] {
            let name = if threshold {
                "mask_threshold"
            } else {
                "mask_sample_chain"
            };
            add(
                &format!("{name}{}", if composite { "_composite" } else { "" }),
                false,
                &sample,
                &format!(
                    "{}\n{}",
                    if composite {
                        "#define REEL_MASK_COMPOSITE"
                    } else {
                        ""
                    },
                    if threshold {
                        "#define REEL_MASK_THRESHOLD"
                    } else {
                        ""
                    }
                ),
            );
        }
    }
    let paint = read("paint");
    for (name, alpha, regional) in [
        ("paint_direct", 0, false),
        ("paint_direct_bc1", 1, false),
        ("paint_direct_bc3", 2, false),
        ("paint_regional", 0, true),
        ("paint_regional_bc1", 1, true),
        ("paint_regional_bc3", 2, true),
        ("paint_direct_coverage", 3, false),
        ("paint_solid", 4, false),
    ] {
        add(
            name,
            false,
            &paint,
            &format!(
                "#define REEL_DIRECT {alpha}\n{}",
                if regional {
                    "#define REEL_REGIONAL"
                } else {
                    ""
                }
            ),
        );
    }
    for (name, kind, style) in [
        ("paint_rgba", 0, 0),
        ("paint_coverage", 1, 0),
        ("paint_gradient", 0, 2),
        ("paint_glyph_gradient", 1, 2),
        ("paint_appearance", 0, 1),
        ("paint_packed", 3, 1),
        ("paint_video", 2, 0),
        ("paint_video_appearance", 2, 1),
    ] {
        add(
            name,
            false,
            &paint,
            &format!("#define REEL_SOURCE_KIND {kind}\n#define REEL_STYLE {style}"),
        );
    }
    add("present", false, &read("present"), "");
    if native {
        add("clear_vertex", true, &read("clear_vertex"), "");
        add("clear", false, &read("clear"), "");
    }
    shaders
}
