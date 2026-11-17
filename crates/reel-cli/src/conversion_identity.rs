//! Build-time converter identities used with source content and normalized recipes.
//!
//! CLI/player/UI changes leave these identities unchanged. Converter sources,
//! transitive codec dependencies and the build platform determine each identity. External
//! codec/interpreter identities are supplied separately by the relevant adapter.

/// Return the implementation identity for a normalized conversion recipe kind.
/// Unknown kinds indicate a missing converter integration and are rejected.
pub(crate) fn for_kind(kind: &str) -> &'static str {
    match kind {
        "webp" => env!("REEL_CONVERTER_WEBP"),
        "texture" => env!("REEL_CONVERTER_TEXTURE"),
        "atlas" => env!("REEL_CONVERTER_ATLAS"),
        "tiles" => env!("REEL_CONVERTER_TILES"),
        "opus" => env!("REEL_CONVERTER_OPUS"),
        "video" => env!("REEL_CONVERTER_VIDEO"),
        "font" => env!("REEL_CONVERTER_FONT"),
        _ => panic!("unknown resource converter kind: {kind}"),
    }
}
