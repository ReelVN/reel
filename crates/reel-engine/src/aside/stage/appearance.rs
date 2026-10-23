//! Image, text and button fields share constructor and source-type declarations.
use aside_compiler::semantic::TypeHint;
use aside_host::{
    Properties,
    source::{named, nullable, number},
};

fn image_source() -> TypeHint {
    TypeHint::Union(vec![TypeHint::String, named("Bytes")])
}
fn text_source() -> TypeHint {
    TypeHint::Union(vec![TypeHint::String, named("reel::types::RichText")])
}
fn callback() -> TypeHint {
    nullable(TypeHint::Closure)
}
fn slice() -> TypeHint {
    nullable(TypeHint::Union(vec![
        number(),
        TypeHint::Array,
        TypeHint::Tuple(vec![number(); 4]),
    ]))
}
fn source_rect() -> TypeHint {
    nullable(TypeHint::Tuple(vec![number(); 4]))
}
fn colors() -> TypeHint {
    nullable(TypeHint::Union(vec![
        TypeHint::Array,
        TypeHint::Tuple(vec![TypeHint::String; 4]),
    ]))
}
fn rgb() -> TypeHint {
    TypeHint::Union(vec![
        number(),
        TypeHint::Array,
        TypeHint::Tuple(vec![number(); 3]),
    ])
}

#[derive(Properties)]
#[expect(
    dead_code,
    reason = "The enum variants declare the generated ordered property schema."
)]
pub(in crate::aside) enum ImageField {
    /// An asset path or encoded image bytes.
    #[aside(name="source",ty=Any,doc_type=image_source())]
    Source,
    /// Size the source using contain, cover or stretch.
    #[aside(name="fit",ty=String,default="contain")]
    Fit,
    /// Nine-slice borders in source pixels; null uses ordinary image fitting.
    #[aside(name="slice",ty=Any,doc_type=slice(),default=null)]
    Slice,
    /// Logical border width per source pixel when slicing is enabled.
    #[aside(name="slice_scale",ty=Any,doc_type=number(),default=1)]
    SliceScale,
    /// Repeat the source along none, x, y or both axes.
    #[aside(name="repeat",ty=String,default="none")]
    Repeat,
    /// Scale of a repeated source tile.
    #[aside(name="tile_scale",ty=Any,doc_type=number(),default=1)]
    TileScale,
    /// Four corner colors for bilinear multiplication; null keeps uniform color.
    #[aside(name="corner_colors",ty=Any,doc_type=colors(),default=null)]
    CornerColors,
    /// Draw the center of the nine-slice image.
    #[aside(name="slice_center",ty=Bool,default=true)]
    SliceCenter,
    /// Nonnegative Rec.709 sRGB saturation after tint multiplication; 1 preserves color.
    /// The drawing value must be finite and representable as float32.
    #[aside(name="saturation",ty=Any,doc_type=number(),default=1)]
    Saturation,
    /// Nonnegative sRGB multiplier, as a number or [red, green, blue]; defaults to 1.
    /// Applied after saturation. Components must be finite float32 drawing values.
    #[aside(name="brightness",ty=Any,doc_type=rgb(),default=1)]
    Brightness,
    /// sRGB offset, as a number or [red, green, blue]; defaults to 0.
    /// Applied after brightness and clamped to 0..1. Alpha stays unchanged.
    /// Components must be finite float32 drawing values.
    #[aside(name="color_offset",ty=Any,doc_type=rgb(),default=0)]
    ColorOffset,
    /// Source-pixel window (x, y, width, height); null selects the whole source.
    /// Components and endpoints must be finite float32 drawing values, with positive dimensions.
    /// Out-of-texture coordinates use edge padding. Fit, slice and repeat use this window.
    /// Bilinear sampling stays inside the window without shrinking its pixel mapping.
    /// Image and video share this property, bindings and execution snapshots.
    /// Prepared .reel-image sources do not support source_rect.
    ///
    /// ```aside
    /// let frames = [(0, 0, 64, 64), (64, 0, 64, 64)];
    /// let picture = image("images/sheet.png", source_rect: frames[0], width: 64, height: 64);
    /// picture.source_rect = frames[1];
    /// ```
    #[aside(name="source_rect",ty=Any,doc_type=source_rect(),default=null)]
    SourceRect,
}
#[derive(Properties)]
#[expect(
    dead_code,
    reason = "The enum variants declare the generated ordered property schema."
)]
pub(in crate::aside) enum TextField {
    /// The plain or rich text displayed by this node.
    #[aside(name="value",ty=Any,doc_type=text_source())]
    Value,
    /// Callback for an activated rich-text link.
    #[aside(name="on_link",ty=Any,doc_type=callback(),default=null)]
    OnLink,
}
#[derive(Properties)]
#[expect(
    dead_code,
    reason = "The enum variants declare the generated ordered property schema."
)]
pub(in crate::aside) enum ButtonField {
    /// Optional built-in label; children provide an authored appearance.
    #[aside(name="label",ty=Any,doc_type=nullable(TypeHint::String),default=null)]
    Label,
    /// Callback invoked when an enabled button is confirmed.
    #[aside(name="on_click",ty=Any,doc_type=callback(),default=null)]
    OnClick,
    /// Request keyboard focus on a primary pointer press; defaults to true.
    /// Set false for HUD or toolbar clicks that preserve the current focus.
    /// Tab, controller navigation and focused confirmation remain available.
    #[aside(name="focus_on_pointer",ty=Bool,default=true)]
    FocusOnPointer,
}
