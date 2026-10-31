//! Native Cg builtin shader variants compiled offline to GXP.

pub struct Shader {
    pub name: String,
    pub vertex: bool,
    pub source: String,
}

pub fn sources() -> Vec<Shader> {
    let mut shaders = Vec::new();
    let read = |name: &str| {
        let path = format!("src/gxm/shaders/{name}.cg");
        println!("cargo:rerun-if-changed={path}");
        std::fs::read_to_string(path).expect("read shader source")
    };
    let mut add = |name: &str, vertex, source: &str, defines: &str| {
        let source = format!("{defines}\n{source}");
        shaders.push(Shader {
            name: name.into(),
            vertex,
            source,
        });
    };
    for name in [
        "vertex",
        "vertex_instances",
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
        ("tile_composite", "REEL_TILE_DESTINATION"),
        (
            "tile_extended_blend",
            "REEL_TILE_DESTINATION\n#define REEL_EXTENDED_BLEND",
        ),
        ("extended_blend", "REEL_EXTENDED_BLEND"),
        ("transition", "REEL_TRANSITION_ONLY"),
        ("masked_transition", "REEL_MASKED_TRANSITION_ONLY"),
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
    // Specialize ordinary non-inverted blends; inversion and custom modes keep
    // the generic programs and their existing arithmetic.
    for blend in [1, 2, 3, 5, 6, 7, 8, 9, 10] {
        add(
            &format!("tile_blend_{blend}"),
            false,
            &composite,
            &format!(
                "#define REEL_TILE_DESTINATION\n#define REEL_STATIC_BLEND {blend}.0\n{}",
                if blend > 4 {
                    "#define REEL_EXTENDED_BLEND"
                } else {
                    ""
                },
            ),
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
    add(
        "vertex_bounded",
        true,
        &read("vertex"),
        "#define REEL_BOUNDED",
    );
    add(
        "vertex_instances_bounded",
        true,
        &read("vertex_instances"),
        "#define REEL_BOUNDED",
    );
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
        let defines = format!(
            "#define REEL_DIRECT {alpha}\n{}",
            if regional {
                "#define REEL_REGIONAL"
            } else {
                ""
            }
        );
        if alpha != 3 {
            for clipped in [false, true] {
                add(
                    &format!("{name}_bounded{}", if clipped { "" } else { "_unclipped" }),
                    false,
                    &paint,
                    &format!(
                        "{defines}\n#define REEL_BOUNDED\n{}",
                        if clipped {
                            ""
                        } else {
                            "#define REEL_UNCLIPPED"
                        }
                    ),
                );
            }
        }
        add(name, false, &paint, &defines);
        add(
            &format!("{name}_unclipped"),
            false,
            &paint,
            &format!("{defines}\n#define REEL_UNCLIPPED"),
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
        ("paint_appearance_constant", 0, 3),
        ("paint_packed_constant", 3, 3),
        ("paint_video_constant", 2, 3),
    ] {
        let defines = format!("#define REEL_SOURCE_KIND {kind}\n#define REEL_STYLE {style}");
        add(name, false, &paint, &defines);
        add(
            &format!("{name}_unclipped"),
            false,
            &paint,
            &format!("{defines}\n#define REEL_UNCLIPPED"),
        );
    }
    for clipped in [false, true] {
        for (name, define) in [
            ("paint_alpha_solid", "#define REEL_OPAQUE_SOURCE"),
            ("paint_alpha_direct", ""),
            ("paint_alpha_region", "#define REEL_REGIONAL"),
        ] {
            add(
                &format!("{name}{}", if clipped { "" } else { "_unclipped" }),
                false,
                &read("paint_alpha"),
                &format!(
                    "{define}\n{}",
                    if clipped {
                        ""
                    } else {
                        "#define REEL_UNCLIPPED"
                    }
                ),
            );
        }
        for (name, sampling, style, plain_adjust) in [
            ("paint_appearance_direct", 1, 1, false),
            ("paint_appearance_region", 2, 1, false),
            ("paint_constant_direct", 1, 3, false),
            ("paint_constant_region", 2, 3, false),
            ("paint_adjust_direct", 1, 1, true),
            ("paint_adjust_region", 2, 1, true),
            ("paint_constant_plain_direct", 1, 3, true),
            ("paint_constant_plain_region", 2, 3, true),
        ] {
            add(
                &format!("{name}{}", if clipped { "" } else { "_unclipped" }),
                false,
                &paint,
                &format!(
                    "#define REEL_SOURCE_KIND 0\n#define REEL_STYLE {style}\n#define REEL_SIMPLE_SAMPLING {sampling}\n{}\n{}",
                    if plain_adjust {
                        "#define REEL_PLAIN_ADJUST"
                    } else {
                        ""
                    },
                    if clipped {
                        ""
                    } else {
                        "#define REEL_UNCLIPPED"
                    }
                ),
            );
        }
        for (name, sampling, opaque_white) in [
            ("paint_monochrome_direct", 1, false),
            ("paint_monochrome_region", 2, false),
            ("paint_monochrome_opaque_direct", 1, true),
            ("paint_monochrome_opaque_region", 2, true),
        ] {
            add(
                &format!("{name}{}", if clipped { "" } else { "_unclipped" }),
                false,
                &paint,
                &format!(
                    "#define REEL_SOURCE_KIND 0\n#define REEL_STYLE 1\n#define REEL_SIMPLE_SAMPLING {sampling}\n#define REEL_PLAIN_ADJUST\n#define REEL_MONOCHROME\n{}\n{}",
                    if opaque_white {
                        "#define REEL_MONOCHROME_OPAQUE_WHITE"
                    } else {
                        ""
                    },
                    if clipped {
                        ""
                    } else {
                        "#define REEL_UNCLIPPED"
                    }
                ),
            );
        }
        add(
            &format!(
                "paint_video_direct_nv12{}",
                if clipped { "" } else { "_unclipped" }
            ),
            false,
            &paint,
            &format!(
                "#define REEL_NV12_DIRECT\n{}",
                if clipped {
                    ""
                } else {
                    "#define REEL_UNCLIPPED"
                }
            ),
        );
    }
    add("present", false, &read("present"), "");
    add("clear_vertex", true, &read("clear_vertex"), "");
    add("clear", false, &read("clear"), "");
    shaders
}
