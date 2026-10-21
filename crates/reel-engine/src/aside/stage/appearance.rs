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
fn colors() -> TypeHint {
    nullable(TypeHint::Union(vec![
        TypeHint::Array,
        TypeHint::Tuple(vec![TypeHint::String; 4]),
    ]))
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
}
