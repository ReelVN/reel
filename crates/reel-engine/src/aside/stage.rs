//! Stable Aside host nodes projected into Reel's one shared stage model.
use std::{borrow::Cow, collections::BTreeMap};

aside_host::native_documentation! {
    pub(super) fn documentation {
        /// Create a video image node. Its play method starts playback and returns an
        /// activity. Common image fit, clipping, transforms and masks apply to decoded
        /// frames; hiding the node leaves playback running.
        "reel::video";
        /// Create an input scope with `actions: {name: {bindings: ["Escape"],
        /// on_release: callback}}` and optional key, pointer or wheel callbacks.
        /// Each action chooses phase handlers or an exclusive `on_action` stream.
        /// Handlers accept zero parameters or one target/action/value/phase event.
        /// Missing handlers create no event or task; held values still update.
        /// An assigned actions table replaces bindings and handlers together at
        /// stage synchronization; construction installs immediately. Old held actions
        /// cancel through their old handlers. Internal edits after installation do
        /// not update the map: assign a new table. Input ownership and callback lifetime
        /// follow the node. on_key or keyboard/gamepad bindings enable focus and
        /// unfocused routing. Pointer-only scopes preserve keyboard focus.
        "reel::input";
        /// Create a text node from plain or rich text. Common font, reveal, outline,
        /// shadow and layout properties apply to the same paragraph. `on_link` receives
        /// activated rich link ids; text itself creates no dialogue interaction.
        "reel::text";
        /// Create an editable text control bound to value. `on_change` receives edits;
        /// `on_submit` handles submission. text_input is single-line; text_area is multiline.
        /// Use a writable bind reference to keep an author model synchronized.
        /// placeholder displays an empty-value hint. read_only preserves selection, copying
        /// and submission while preventing user edits. Common outline and shadow styles
        /// reuse the editor layout; selection_color and caret_color customize its marks.
        "reel::text_input" | "reel::text_area";
        /// Create an image node from an asset path or captured image bytes. Fit, slicing,
        /// repetition and corner colors configure its common image presentation.
        "reel::image";
        /// Lay out children horizontally with common gap, padding and alignment.
        "reel::row";
        /// Lay out children vertically with common gap, padding and alignment.
        "reel::column";
        /// Lay out equal-width columns in source order. Set columns to the column count;
        /// column_width and row_height choose fixed track sizes. Omitted row_height
        /// follows content, while an omitted column_width shares available width.
        /// column_gap and row_gap inherit gap. Common transforms follow layout.
        "reel::grid";
        /// Overlay children in declaration order. Later children paint above earlier ones.
        "reel::stack";
        /// Place authored pointer artwork in the cursor layer. It follows the accepted
        /// pointer position, shares node properties and does not intercept input.
        "reel::cursor";
        /// Create a button with optional label and on_click callback. Authored children
        /// can supply its appearance; decorated:false disables built-in decoration.
        /// focus_on_pointer:false preserves keyboard focus on pointer presses;
        /// Navigation and explicit focus participation follow focusable.
        /// focusable:false excludes Tab, directional and explicit focus while
        /// preserving pointer hover, press and click interaction.
        /// The callback receives an event with this node as target.
        "reel::button";
        /// Create a numeric control with min, max and step. Bind value to a writable
        /// property; on_change observes committed value changes.
        "reel::slider";
        /// Show determinate progress. Bind value or max to track live state. Set the
        /// track with background and the fill with color. Children overlay the bar.
        ///
        /// ```aside
        /// fn main() {
        ///     let state = model({loaded: 3});
        ///     stage.replace(progress_bar(value: bind(state.loaded), max: 8,
        ///         width: 240, height: 20, color: "#70c9ff"));
        /// }
        /// ```
        "reel::progress_bar";
        /// Create a boolean control with optional label. Bind value to a writable
        /// property, and use on_change for committed changes.
        "reel::toggle" | "reel::checkbox";
        /// Create an option control. Selecting it writes option to the shared value
        /// binding; controls with the same binding share the selected value.
        /// Arrow navigation also discovers matching options in other virtual lists
        /// through their existing measured row requests. An interval of K unknown
        /// rows can require K logical row probes plus ordinary window construction.
        /// Only current windows and interaction pins remain mounted. Selection
        /// waits for an accepted frame.
        "reel::radio";
        /// Create a scroll viewport. axis selects scroll directions; scroll_x/y are
        /// observable offsets. follow_end follows appended content while at the end.
        /// content_align positions content smaller than the viewport.
        "reel::scroll";
        /// Float ordinary children beside this node's logical parent, above the scene.
        /// The parent anchors placement without measuring the popup in its flow.
        /// The floating layer uses its own opacity, blend, mask and transforms;
        /// ancestor clipping and compositing stay in the underlying layer.
        /// Defaults to modal:true. on_dismiss receives {target, reason}, where reason
        /// is outside or back; removing target closes it. Hiding or disabling an
        /// ancestor withdraws the layer. A popup without a parent stays unpresented.
        /// Dismissal follows the active modal scope.
        /// Hover tooltips use modal:false so their anchor keeps receiving pointer
        /// interaction while the popup is visible.
        ///
        /// ```aside
        /// fn main() {
        ///     let trigger = button("Options", on_click: |event| {
        ///         event.target.add(popup(placement: "bottom-start", offset: 4,
        ///             on_dismiss: |event| event.target.remove()) {
        ///             text("Click outside to close", padding: 12, background: "#303848");
        ///         });
        ///     });
        ///     stage.replace(trigger);
        /// }
        /// ```
        "reel::popup";
        /// Float passive children beside their parent while it is hovered or visibly
        /// focused. Placement flips to fit the viewport. The parent keeps input.
        ///
        /// ```aside
        /// fn main() {
        ///     stage.replace(button("Music") {
        ///         tooltip(offset: 6) { text("Listen to the soundtrack"); };
        ///     });
        /// }
        /// ```
        "reel::tooltip";
        /// Create a clipped camera container. camera_x/y select the viewed world point;
        /// zoom and camera_rotation transform its ordinary children. Move camera_z
        /// toward its direct camera_plane children for perspective. Drawing and input
        /// share the transform; the common x/y properties place the viewport.
        "reel::camera";
        /// A container projected at a distance when it is a direct camera child.
        /// Plane distance and camera motion change scale without reordering children.
        /// Outside a camera the container behaves like an ordinary stack.
        "reel::camera_plane";
        /// Replace the mounted stage root with this node. Retired nodes cancel their
        /// owned work. Build the new tree before mounting it.
        "reel::stage::replace";
    }
}
use std::sync::Arc;
pub(super) mod appearance;
pub(super) mod camera;
pub(super) mod configure;
pub(super) mod controls;
pub(super) mod defaults;
pub(super) mod flow;
pub(in crate::aside) mod image_skin;
pub(super) mod input;
mod interaction;
pub(in crate::aside) mod interaction_appearance;
pub(super) mod overrides;
pub(super) mod popup;
pub(super) mod resources;
pub(super) mod scroll;
pub(super) mod scrollbar;
pub(super) mod snapshot;
pub(super) mod source;
pub(super) mod text_input;

/// Specific numeric fields share one registration and dispatch order.
pub(in crate::aside) fn animation_prefixes() -> [(&'static str, &'static [aside_host::Property]); 7]
{
    [
        ("camera", camera::CameraField::PROPERTIES),
        ("camera_plane", camera::PlaneField::PROPERTIES),
        ("scroll", scroll::ScrollField::PROPERTIES),
        ("flow", flow::FlowField::PROPERTIES),
        ("grid", flow::GridField::PROPERTIES),
        ("popup", popup::PopupField::PROPERTIES),
        ("particles", super::particles::ParticleField::PROPERTIES),
    ]
}

use aside_vm::{
    Heap, HeapError, HeapHandle, ObjectKind, PropertyId, RootKind, RootOwner, RootToken, Value,
    ValueTag,
};
use reel_ui::stage::{NodeId, NodeKind, NodeProps, Stage};

use super::{AdapterError, registry, text};
use aside_host::PropertySchema;
use aside_host::{CallContext, NativeBinding};
use aside_vm::bytecode::Effects;
use aside_vm::executor::Vm;

/// One host schema feeds construction, writes and animation; NodeProps remains in reel-ui.
#[derive(aside_host::Properties)]
pub(super) enum NodeProperty {
    /// Share the remaining row/column space by weight. Zero keeps natural or explicit
    /// sizing; a positive weight starts at zero on the flow axis and honors max size.
    #[aside(name = "flex", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_nonnegative, animatable)]
    Flex,
    /// Horizontal position in the parent's logical coordinate space.
    #[aside(name = "x", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_number, animatable)]
    X,
    /// Vertical position in the parent's logical coordinate space.
    #[aside(name = "y", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_number, animatable)]
    Y,
    /// Explicit logical width; null uses the node's measured width.
    #[aside(name = "width", ty = Any, doc_type = aside_host::source::nullable(aside_host::source::number()), default = null, validate = validate_dimension, animatable)]
    Width,
    /// Explicit logical height; null uses the node's measured height.
    #[aside(name = "height", ty = Any, doc_type = aside_host::source::nullable(aside_host::source::number()), default = null, validate = validate_dimension, animatable)]
    Height,
    /// Opacity of the node and its subtree, from 0 (transparent) to 1 (opaque).
    #[aside(name = "opacity", ty = Any, doc_type = aside_host::source::number(), default = 1, validate = validate_opacity, animatable)]
    Opacity,
    /// Color inversion strength, from 0 to 1.
    #[aside(name = "invert", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_invert, animatable)]
    Invert,
    /// Blend the completed subtree: normal, add, subtract, multiply, screen,
    /// overlay, hard_light, color_dodge, lighten, difference, soft_light, or linear_burn.
    /// The separable modes blend straight linear RGB, then compose source-over.
    /// Linear burn uses max(source + backdrop - 1, 0) per straight color channel.
    /// Subtract clamps linear premultiplied backdrop RGB minus source RGB to zero
    /// and preserves backdrop alpha. Opacity and masks scale the source once.
    #[aside(name = "blend", ty = String, default = "normal", validate = validate_blend)]
    Blend,
    /// Show this subtree and include it in input hit testing.
    #[aside(name = "visible", ty = Bool, default = true)]
    Visible,
    /// Allow this subtree to receive input while retaining its presentation.
    #[aside(name = "enabled", ty = Bool, default = true)]
    Enabled,
    /// Capture input above lower scene layers while the node is visible.
    #[aside(name = "modal", ty = Bool, default = false)]
    Modal,
    /// Clip child drawing and hit testing to this node's bounds.
    #[aside(name = "clip", ty = Bool, default = false)]
    Clip,
    /// Space between adjacent children in row and column layouts.
    #[aside(name = "gap", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_nonnegative, animatable)]
    Gap,
    /// Inset between this node's bounds and its content.
    #[aside(name = "padding", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_nonnegative, animatable)]
    Padding,
    /// Text size in logical pixels.
    #[aside(name = "font_size", ty = Any, doc_type = aside_host::source::number(), default = 24, validate = validate_nonnegative, animatable)]
    FontSize,
    /// Family name registered through game.configure fonts.
    #[aside(name = "font", ty = String, default = "system-ui")]
    Font,
    /// Foreground text color as a CSS hex color.
    #[aside(name = "color", ty = String, default = "#ffffff", validate = validate_color)]
    Color,
    /// Optional two-color paint for text and control captions. Explicit rich-text
    /// colors override it. Changing paint retains the shaped layout.
    #[aside(name = "text_gradient", ty = Any, doc_type = aside_host::source::nullable(aside_host::source::named("reel::types::LinearGradient")), default = null, validate = super::gradients::validate)]
    TextGradient,
    /// Node background color; null leaves the background transparent.
    #[aside(name = "background", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::String), default = null, validate = validate_background)]
    Background,
    /// Inside-border width in logical pixels. Finite, nonnegative and animatable; defaults to 0.
    /// Painting clamps the thickness to half the node's short edge. Layout and hit testing
    /// retain the node rectangle; use padding to inset content independently of the border.
    #[aside(name = "border_width", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_nonnegative, animatable)]
    BorderWidth,
    /// Solid border color as a CSS hex color; defaults to white (#ffffff).
    /// The border is drawn inside the node and is invisible when border_width is 0.
    #[aside(name = "border_color", ty = String, default = "#ffffff", validate = validate_color)]
    BorderColor,
    /// Background and border corner radius in logical pixels; defaults to 0.
    /// Finite, nonnegative and animatable. Changes repaint the same layout and hit rectangle.
    #[aside(name = "corner_radius", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_nonnegative, animatable)]
    CornerRadius,
    /// Rotation in degrees around the node's anchor.
    #[aside(name = "rotation", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_number, animatable)]
    Rotation,
    /// Horizontal scale around the node's anchor.
    #[aside(name = "scale_x", ty = Any, doc_type = aside_host::source::number(), default = 1, validate = validate_number, animatable)]
    ScaleX,
    /// Vertical scale around the node's anchor.
    #[aside(name = "scale_y", ty = Any, doc_type = aside_host::source::number(), default = 1, validate = validate_number, animatable)]
    ScaleY,
    /// Horizontal transform anchor as a fraction of the node width.
    #[aside(name = "anchor_x", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_number, animatable)]
    AnchorX,
    /// Vertical transform anchor as a fraction of the node height.
    #[aside(name = "anchor_y", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_number, animatable)]
    AnchorY,
    /// Draw the control's built-in decoration. Input and bindings stay active.
    #[aside(name = "decorated", ty = Bool, default = true)]
    Decorated,
    /// Visible UTF-8 byte prefix of a text node; null reveals the whole text.
    #[aside(name = "reveal", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::Int), default = null, validate = validate_reveal)]
    Reveal,
    /// Soft reveal progress. Grapheme k uses clamp(progress - k * step, 0, 1).
    /// Null disables soft reveal; the hard reveal prefix still applies.
    #[aside(name = "reveal_fade", ty = Any, doc_type = aside_host::source::nullable(aside_host::source::number()), default = null, validate = validate_optional_number, animatable)]
    RevealFade,
    /// UTF-8 byte offset where soft reveal begins. Earlier text stays opaque.
    #[aside(name = "reveal_fade_from", ty = Any, doc_type = aside_compiler::semantic::TypeHint::Int, default = 0, validate = validate_byte_index)]
    RevealFadeFrom,
    /// Nonnegative delay between successive graphemes in soft reveal progress units.
    #[aside(name = "reveal_fade_step", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_nonnegative, animatable)]
    RevealFadeStep,
    /// Horizontal text alignment: start, end, left, right, center or justify.
    #[aside(name = "text_align", ty = String, default = "start", validate = validate_text_align)]
    TextAlign,
    /// Soft-wrap text and text_area at the available content width, retaining explicit newlines.
    /// text_input remains single-line.
    #[aside(name = "text_wrap", ty = Bool, default = true)]
    TextWrap,
    /// Line-height multiplier relative to the font size.
    #[aside(name = "line_height", ty = Any, doc_type = aside_host::source::number(), default = 1.2, validate = validate_nonnegative, animatable)]
    LineHeight,
    /// Use shrink to fit text into fixed bounds, or none for the declared size.
    #[aside(name = "text_fit", ty = String, default = "none", validate = validate_text_fit)]
    TextFit,
    /// Smallest font size allowed by text_fit: shrink.
    #[aside(name = "min_font_size", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_nonnegative)]
    MinFontSize,
    /// Text outline width in logical pixels.
    #[aside(name = "text_outline", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_nonnegative, animatable)]
    TextOutline,
    /// Color of the text outline.
    #[aside(name = "text_outline_color", ty = String, default = "#000000", validate = validate_color)]
    TextOutlineColor,
    /// Image or composed mask applied to this subtree; null disables masking.
    #[aside(name = "mask", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::Union(vec![aside_compiler::semantic::TypeHint::String,aside_host::source::number(),aside_host::source::named("reel::types::Mask"),aside_host::source::named("reel::types::Effect")])), default = null, validate = validate_mask)]
    Mask,
    /// Reveal threshold for the subtree mask.
    #[aside(name = "mask_progress", ty = Any, doc_type = aside_host::source::number(), default = 1, validate = validate_mask_progress, animatable)]
    MaskProgress,
    /// Softness of the mask threshold transition.
    #[aside(name = "mask_softness", ty = Any, doc_type = aside_host::source::number(), default = 0.06, validate = validate_nonnegative, animatable)]
    MaskSoftness,
    /// Reverse the mask's coverage.
    #[aside(name = "mask_invert", ty = Bool, default = false)]
    MaskInvert,
    /// Horizontal mask sampling offset in local coordinates.
    #[aside(name = "mask_x", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_mask_offset, animatable)]
    MaskX,
    /// Vertical mask sampling offset in local coordinates.
    #[aside(name = "mask_y", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_mask_offset, animatable)]
    MaskY,
    /// Fill available layout space along none, x, y or both axes.
    #[aside(name = "stretch", ty = String, default = "none", validate = validate_stretch)]
    Stretch,
    /// Upper bound on measured logical width; null leaves it unconstrained.
    #[aside(name = "max_width", ty = Any, doc_type = aside_host::source::nullable(aside_host::source::number()), default = null, validate = validate_dimension, animatable)]
    MaxWidth,
    /// Upper bound on measured logical height; null leaves it unconstrained.
    #[aside(name = "max_height", ty = Any, doc_type = aside_host::source::nullable(aside_host::source::number()), default = null, validate = validate_dimension, animatable)]
    MaxHeight,
    /// Horizontal alignment within available space: 0 starts, 1 ends.
    #[aside(name = "align_x", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_opacity, animatable)]
    AlignX,
    /// Vertical alignment within available space: 0 starts, 1 ends.
    #[aside(name = "align_y", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_opacity, animatable)]
    AlignY,
    /// Callback for hover, press and focus interaction changes.
    /// State-driven appearance retains an ongoing press across callback scheduling
    /// and image loading. Replacing application content cancels the old press.
    #[aside(name = "on_interaction", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::Closure), default = null, validate = validate_callback)]
    OnInteraction,
    /// Requested font weight; 400 selects normal text.
    #[aside(name = "font_weight", ty = Any, doc_type = aside_host::source::number(), default = 400, validate = validate_font_weight)]
    FontWeight,
    /// Extra spacing between text glyphs in logical pixels.
    #[aside(name = "letter_spacing", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_text_spacing)]
    LetterSpacing,
    /// Extra spacing at word boundaries in logical pixels.
    #[aside(name = "word_spacing", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_text_spacing)]
    WordSpacing,
    /// Include this subtree in stage.capture images. Normal presentation is unchanged.
    #[aside(name = "capture_included", ty = Bool, default = true)]
    CaptureIncluded,
    /// Horizontal shadow offset in logical pixels. Two zero offsets disable the shadow.
    #[aside(name = "text_shadow_x", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_number, animatable)]
    TextShadowX,
    /// Vertical shadow offset in logical pixels. Reuses the text's shaped layout.
    #[aside(name = "text_shadow_y", ty = Any, doc_type = aside_host::source::number(), default = 0, validate = validate_number, animatable)]
    TextShadowY,
    /// Uniform shadow color, independent of rich span colors and subject to text reveal.
    #[aside(name = "text_shadow_color", ty = String, default = "#000000", validate = validate_color)]
    TextShadowColor,
    /// Paragraph flow: horizontal-tb (default), vertical-rl or vertical-lr.
    /// Vertical text wraps at height, uses line_height for column spacing and
    /// keeps source-order reveal, right-side ruby and link geometry. CJK glyphs
    /// stay upright; Latin words rotate clockwise. text_align follows the inline
    /// axis. text_input and text_area accept only horizontal-tb.
    #[aside(name = "writing_mode", ty = String, default = "horizontal-tb", validate = validate_writing_mode)]
    WritingMode,
    /// Apply a compiled drawing function to this node's completed subtree, before
    /// opacity, mask and blend. Null preserves the subtree's original colors.
    #[aside(name = "effect", ty = Any, doc_type = aside_host::source::nullable(aside_host::source::named("reel::types::Effect")), default = null, validate = super::effects::validate)]
    Effect,
    /// Immutable interaction paint created by appearance. Selection overlays ordinary
    /// values without writing bindings or replacing this node. Scope overrides win.
    #[aside(name = "appearance", ty = Any, doc_type = aside_host::source::nullable(aside_host::source::named("reel::types::Appearance")), default = null, validate = interaction_appearance::validate)]
    Appearance,
    /// Linear resolution multiplier for this node's effect output, in (0, 1].
    /// The reduced output is bilinearly enlarged; Fragment coordinates, the
    /// input subtree and independent mask keep their original resolution.
    #[aside(name = "effect_resolution_scale", ty = Any, doc_type = aside_host::source::number(), default = 1, validate = validate_effect_resolution_scale)]
    EffectResolutionScale,
}

pub(super) struct StageBinding {
    #[cfg(test)]
    pub(super) sync_count: u64,
    #[cfg(test)]
    pub(super) projection_visits: u64,
    #[cfg(test)]
    pub(super) detached_root_checks: u64,
    last_sync: Option<(aside_vm::HeapId, aside_vm::SessionEpoch, u64, u64)>,
    checked_detached_roots: Option<(aside_vm::HeapId, aside_vm::SessionEpoch, u64)>,
    projection_index: projection::Index,
    pub(super) types: super::source::HostTypes,
    pub(super) text_filter_function: Option<aside_vm::bytecode::FunctionId>,
    pub(in crate::aside) page_methods: Option<[aside_vm::bytecode::FunctionId; 8]>,
    pub scene: Stage,
    pub(super) effect_products: Arc<super::source::EffectProducts>,
    effect_projection: super::effects::ProjectionCache,
    pub retirement_revision: u64,
    pub(super) virtual_scrolls: super::lists::virtualized::ScrollOwners,
    handles: BTreeMap<NodeId, NodeBinding>,
    restoring_resources: resources::Restoring,
    pub(in crate::aside) page_stacks: std::collections::BTreeSet<NodeId>,
    root: Option<RootToken>,
    // Schema names are immutable and shared by live nodes. Weak handles let
    // unused schemas disappear with their last node and require no save roots.
    property_keys: super::observable::SchemaKeys,
    pub(super) editing_limits: reel_ui::widgets::TextInputEditingLimits,
}
struct NodeBinding {
    shape: snapshot::Shape,
    handle: HeapHandle,
    interaction_offset: u32,
    interaction_listening: bool,
    fields_version: u64,
    projection_dirty: bool,
    overrides: Option<Box<overrides::Cache>>,
    input: Option<Box<input::State>>,
    text_history: Option<Box<text_input::History>>,
    text_source: Option<HeapHandle>,
    image_skin: Option<HeapHandle>,
    mask_source: Option<(HeapHandle, u64)>,
    effect_source: Option<(HeapHandle, u64)>,
    text_listening: bool,
    /// Weak identity stamp only: never dereferenced or treated as a GC root.
    radio_option: Option<Value>,
}

mod projection;

impl StageBinding {
    pub(in crate::aside) fn discard_unmounted(
        &mut self,
        heap: &mut Heap,
        host: Value,
    ) -> Result<bool, AdapterError> {
        let id = self.id(heap, host)?;
        if self.scene.node(id).unwrap().parent.is_some() || self.scene.root() == Some(id) {
            return Ok(false);
        }
        let retired = self.scene.discard(id)?;
        self.retire(heap, retired)?;
        Ok(true)
    }
    pub(in crate::aside) fn make_virtual(
        &mut self,
        heap: &Heap,
        host: Value,
    ) -> Result<(), AdapterError> {
        let id = self.id(heap, host)?;
        self.handles.get_mut(&id).unwrap().shape = snapshot::Shape::VirtualList;
        self.publish_virtual(heap, host)
    }

    pub(in crate::aside) fn publish_virtual(
        &mut self,
        heap: &Heap,
        host: Value,
    ) -> Result<(), AdapterError> {
        let id = self.id(heap, host)?;
        let props = self.scene.node(id).unwrap().props.clone();
        validate_virtual_props(&props)?;
        self.scene.update(
            id,
            NodeKind::VirtualList(super::lists::virtualized::project(heap, host)?),
            props,
        )?;
        Ok(())
    }
    pub fn new(heap: &Heap) -> Result<Self, AdapterError> {
        Ok(Self {
            #[cfg(test)]
            sync_count: 0,
            #[cfg(test)]
            projection_visits: 0,
            #[cfg(test)]
            detached_root_checks: 0,
            last_sync: None,
            checked_detached_roots: None,
            projection_index: projection::Index::new(heap)?,
            types: super::source::HostTypes::new(heap)?,
            text_filter_function: None,
            page_methods: None,
            scene: Stage::default(),
            effect_products: Arc::default(),
            effect_projection: Default::default(),
            retirement_revision: 0,
            virtual_scrolls: Default::default(),
            handles: BTreeMap::new(),
            restoring_resources: Default::default(),
            page_stacks: Default::default(),
            root: None,
            property_keys: Default::default(),
            editing_limits: Default::default(),
        })
    }

    fn property_key(&mut self, heap: &mut Heap, name: &'static str) -> Result<Value, HeapError> {
        self.property_keys.key(heap, name)
    }
    pub(super) fn set_effect_products(
        &mut self,
        heap: &Heap,
        products: Arc<super::source::EffectProducts>,
    ) -> Result<(), AdapterError> {
        let projection = super::effects::ProjectionCache::admit(heap, &products)?;
        self.effect_products = products;
        self.effect_projection = projection;
        Ok(())
    }
    pub(super) fn replace_view(
        &mut self,
        heap: &mut Heap,
        container: Value,
        child: Value,
        read_limit: usize,
    ) -> Result<(), AdapterError> {
        let id = self.id(heap, container)?;
        let old = self.scene.node(id).unwrap().children.clone();
        let references = heap.alloc_array(&[])?;
        if child.tag() != ValueTag::Null {
            let child_id = self.id(heap, child)?;
            if old.as_slice() == [child_id] {
                return Ok(());
            }
            self.sync_tree(heap, child_id, read_limit)?;
            heap.array_push(references, child)?;
            // Admission precedes retirement: a failed mount keeps the old tree.
            self.scene.add(id, child_id, old.len())?;
            heap.set_host_payload(child, 2, container)?;
        }
        for child in old {
            let retired = self.scene.remove(child)?;
            self.retire(heap, retired)?;
        }
        heap.set_host_payload(container, 1, references)?;
        Ok(())
    }

    pub(super) fn reconcile_list(
        &mut self,
        heap: &mut Heap,
        container: Value,
        references: Value,
        read_limit: usize,
    ) -> Result<(), AdapterError> {
        let parent = self.id(heap, container)?;
        let mut children = Vec::new();
        children
            .try_reserve_exact(heap.sequence_len(references)? as usize)
            .map_err(|_| AdapterError::Allocation)?;
        for i in 0..heap.sequence_len(references)? {
            children.push(self.id(heap, heap.sequence_get(references, i)?)?);
        }
        self.scene.validate_children(parent, &children)?;
        for &child in &children {
            if self.scene.node(child).unwrap().parent.is_none() {
                self.sync_tree(heap, child, read_limit)?;
            }
        }
        let mut tooltip_added = false;
        for i in 0..heap.sequence_len(references)? {
            let child = heap.sequence_get(references, i)?;
            if !heap.host_payload(child, 2)?.same_bits(container) {
                let id = self.id(heap, child)?;
                match self.handles[&id].shape {
                    snapshot::Shape::Radio => {
                        self.handles.get_mut(&id).unwrap().projection_dirty = true;
                    }
                    snapshot::Shape::Tooltip => tooltip_added = true,
                    _ => {}
                }
                heap.set_host_payload(child, 2, container)?;
            }
        }
        heap.set_host_payload(container, 1, references)?;
        let retired = self.scene.reconcile_children(parent, children)?;
        self.retire(heap, retired)?;
        if tooltip_added {
            self.sync_tooltips(heap, parent)?;
        }
        Ok(())
    }

    pub(super) fn id(&self, heap: &Heap, value: Value) -> Result<NodeId, AdapterError> {
        heap.host_fields(value)?;
        let id = NodeId(
            heap.host_payload(value, 0)?
                .as_int()
                .ok_or(AdapterError::Input("expected stage node"))? as u64,
        );
        if self.handles.get(&id).map(|node| node.handle) != Some(heap.handle(value)?)
            || self.scene.node(id).is_none()
        {
            return Err(AdapterError::Input("node is not owned by this stage"));
        }
        Ok(id)
    }

    pub(super) fn mounted_properties_current(&self, heap: &Heap) -> Result<bool, AdapterError> {
        if self.last_sync.is_some()
            && self.scene.root().is_some()
            && let Some(current) = self.projection_index.is_current(heap, &self.scene)
        {
            return Ok(current);
        }
        fn visit(stage: &StageBinding, heap: &Heap, id: NodeId) -> Result<bool, AdapterError> {
            let binding = &stage.handles[&id];
            let value = heap.import_handle(binding.handle)?;
            if heap.info(heap.host_fields(value)?)?.content_version != binding.fields_version
                || binding.projection_dirty
                || binding.image_skin.is_some()
                || super::effects::changed(heap, binding.effect_source)?
                || mask_changed(heap, binding.mask_source)?
                || binding
                    .overrides
                    .as_ref()
                    .map(|c| c.changed(heap, value))
                    .transpose()?
                    .unwrap_or(false)
            {
                return Ok(false);
            }
            for child in &stage.scene.node(id).unwrap().children {
                if !visit(stage, heap, *child)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        self.scene
            .root()
            .map_or(Ok(false), |root| visit(self, heap, root))
    }

    pub(super) fn property_offset(&self, heap: &Heap, value: Value) -> Result<u32, AdapterError> {
        let id = self.id(heap, value)?;
        Ok(self.handles[&id].shape.prefix() as u32)
    }

    pub fn dispatch(
        &mut self,
        vm: &mut Vm,
        owner: RootOwner,
        symbol: &str,
        args: &[Value],
        read_limit: usize,
        origin: Option<String>,
    ) -> Result<Value, AdapterError> {
        let heap = vm.heap_mut();
        if symbol == "reel::stage::replace" {
            let node = args[0];
            let id = self.id(heap, node)?;
            self.sync_tree(heap, id, read_limit)?;
            let root = heap.add_root(owner, RootKind::Host, node)?;
            let retired = match self.scene.replace(id) {
                Ok(retired) => retired,
                Err(error) => {
                    heap.release_root(root)?;
                    return Err(error.into());
                }
            };
            if let Some(old) = self.root.replace(root) {
                heap.release_root(old)?;
            }
            self.retire(heap, retired)?;
            return Ok(Value::NULL);
        }

        if symbol == "reel::scrollbar" {
            return self.create_scrollbar(vm, owner, args, read_limit, origin);
        }

        self.create(
            vm,
            (
                symbol,
                registry::node_type(symbol).expect("node constructor type"),
            ),
            args,
            read_limit,
            origin,
        )
    }

    pub(super) fn create(
        &mut self,
        vm: &mut Vm,
        (symbol, type_name): (&str, &str),
        args: &[Value],
        read_limit: usize,
        origin: Option<String>,
    ) -> Result<Value, AdapterError> {
        let heap = vm.heap_mut();
        let raw_args = args;
        // Resolve values before creating the node. Keep the original references
        // for binding, and copy the arguments only when resolution changes one.
        let mut resolved = Cow::Borrowed(raw_args);
        for (index, &argument) in raw_args.iter().enumerate() {
            let value = heap.property_argument(argument)?;
            if !value.same_bits(argument) {
                resolved.to_mut()[index] = value;
            }
        }
        let args = resolved.as_ref();
        let prefix = registry::node_prefix(symbol);
        let field_count = prefix.len() + NodeProperty::PROPERTIES.len();
        let fields = {
            let count = args.len().min(field_count);
            let bytes = size_of::<(Value, Value)>();
            let mut charge = heap.budget_usage().reserve((count * bytes) as u64)?;
            let mut entries = Vec::new();
            entries
                .try_reserve_exact(count)
                .map_err(|_| AdapterError::Allocation)?;
            charge.resize((entries.capacity() * bytes) as u64)?;
            let names = prefix.iter().copied().chain(
                NodeProperty::PROPERTIES
                    .iter()
                    .map(|property| property.name),
            );
            for (name, &argument) in names.zip(args) {
                entries.push((self.property_key(heap, name)?, argument));
            }
            heap.alloc_ordered_table(&entries)?
        };
        let shape = snapshot::Shape::from_symbol(symbol);
        let appearance_origin = origin.as_deref().map(Arc::from);
        let (mut kind, input_declaration) = if shape == snapshot::Shape::Input {
            let (kind, declaration) = input::prepare(heap, fields)?;
            (kind, Some(declaration))
        } else {
            (shape.project(heap, fields, origin, read_limit)?, None)
        };
        let mut props = read_props(
            heap,
            fields,
            prefix.len(),
            Value::NULL,
            read_limit,
            property_values::PropertyContext {
                previous: None,
                appearance_origin,
                effects: &self.effect_products,
                effect_projection: &mut self.effect_projection,
            },
        )?;
        let image_skin = image_skin::binding(heap, shape, fields)?;
        super::pages::semantics(shape, &kind, &mut props);
        validate_text_props(&kind, &props)?;
        let mask_source = mask_handle(heap, fields, prefix.len(), Value::NULL)?;
        let effect_source = effect_stamp(heap, fields, prefix.len(), Value::NULL)?;
        let mut children = Vec::new();
        let references = heap.alloc_array(&[])?;
        let slider_art = if let NodeKind::Slider { art, .. } = &kind {
            for field in [controls::SliderField::Track, controls::SliderField::Thumb] {
                let part = args[field.index()];
                if part.tag() != ValueTag::Null {
                    children.push(self.id(heap, part)?);
                }
            }
            if art.track.is_some() && art.track == art.thumb {
                return Err(AdapterError::Input(
                    "slider track and thumb must be distinct nodes",
                ));
            }
            Some(*art)
        } else {
            None
        };
        if matches!(
            kind,
            NodeKind::Camera(_)
                | NodeKind::CameraPlane { .. }
                | NodeKind::Row(_)
                | NodeKind::Column(_)
                | NodeKind::Grid(_)
                | NodeKind::Scroll { .. }
                | NodeKind::Popup(_)
                | NodeKind::Stack
                | NodeKind::Cursor
                | NodeKind::Button { .. }
                | NodeKind::Slider { .. }
                | NodeKind::ProgressBar { .. }
                | NodeKind::Toggle { .. }
                | NodeKind::Radio { .. }
                | NodeKind::Input { .. }
        ) {
            let input = args[args.len() - 1];
            if input.tag() != ValueTag::Null {
                for index in 0..heap.sequence_len(input)? {
                    let child = heap.sequence_get(input, index)?;
                    children.push(self.id(heap, child)?);
                }
            }
            if !children.is_empty() {
                children = self.scene.composition_roots(&children)?;
            }
            if let Some(art) = slider_art
                && [art.track, art.thumb]
                    .into_iter()
                    .flatten()
                    .any(|part| !children.contains(&part))
            {
                return Err(AdapterError::Input(
                    "slider art must be a direct child of the slider",
                ));
            }
            for &child in &children {
                heap.array_push(references, self.value(heap, child)?)?;
            }
        }
        let (id, value) = self.allocate_node_shell(heap, fields, references, shape, type_name)?;
        for (offset, &name) in interaction::Field::NAMES.iter().enumerate() {
            let key = self.property_key(heap, name)?;
            let state = offset == interaction::Field::Disabled.index() && !props.enabled;
            heap.table_store(
                fields,
                (field_count + offset) as u32,
                key,
                Value::bool(state),
            )?;
        }
        heap.observe_host_properties(
            value,
            field_count as u32,
            interaction::Field::NAMES.len() as u32,
        )?;
        let extra_fields = interaction::Field::NAMES.len();
        if matches!(
            shape,
            snapshot::Shape::TextInput | snapshot::Shape::TextArea
        ) {
            for (i, name) in text_input::METRICS.into_iter().enumerate() {
                let key = self.property_key(heap, name)?;
                heap.table_store(
                    fields,
                    text_input::metrics() + i as u32,
                    key,
                    if i < 2 {
                        Value::int(0)
                    } else {
                        Value::bool(false)
                    },
                )?;
            }
            heap.observe_host_properties(
                value,
                text_input::metrics(),
                text_input::METRICS.len() as u32,
            )?;
        }
        if symbol == "reel::scroll" {
            self.initialize_scroll_metrics(
                heap,
                value,
                fields,
                (field_count + extra_fields) as u32,
            )?;
            for field in [scroll::ScrollField::X, scroll::ScrollField::Y] {
                heap.require_writable_property(PropertyId {
                    host: value,
                    field: field.index() as u32,
                })?;
            }
        }
        let control_field = match symbol {
            "reel::slider" => Some(controls::SliderField::Value.index()),
            "reel::toggle" | "reel::checkbox" => Some(controls::ToggleField::Value.index()),
            "reel::radio" => Some(controls::RadioField::Value.index()),
            "reel::text_input" | "reel::text_area" => {
                Some(text_input::TextInputField::Value.index())
            }
            _ => None,
        };
        if let Some(field) = control_field {
            heap.require_writable_property(PropertyId {
                host: value,
                field: field as u32,
            })?;
        }

        for (field, &argument) in raw_args.iter().take(field_count).enumerate() {
            if heap.is_property_ref(argument) {
                heap.bind_host_property(
                    PropertyId {
                        host: value,
                        field: field as u32,
                    },
                    argument,
                )?;
            }
        }
        let input = if let Some(declaration) = input_declaration {
            Some(input::initialize(heap, value, fields, declaration)?)
        } else {
            None
        };
        controls::project_group(heap, value, &mut kind)?;
        text_input::project(heap, value, &mut kind)?;
        self.scene.create(kind, props, children)?;
        if shape == snapshot::Shape::PageStack {
            self.page_stacks.insert(id);
        }
        // Keeping any part of a component keeps its ancestors and siblings
        // alive. Explicit retirement clears both directions of these edges.
        for index in 0..heap.sequence_len(references)? {
            let child = heap.sequence_get(references, index)?;
            heap.set_host_payload(child, 2, value)?;
        }
        self.handles.insert(
            id,
            NodeBinding {
                shape,
                handle: heap.handle(value)?,
                interaction_offset: field_count as u32,
                interaction_listening: interaction::listening(
                    heap,
                    fields,
                    field_count as u32,
                    Value::NULL,
                )?,
                fields_version: heap.info(fields)?.content_version,
                projection_dirty: false,
                overrides: None,
                input,
                text_history: None,
                text_source: if image_skin.is_none()
                    && matches!(
                        symbol,
                        "reel::text"
                            | "reel::text_input"
                            | "reel::text_area"
                            | "reel::image"
                            | "reel::video"
                    ) {
                    Some(heap.handle(args[0])?)
                } else {
                    None
                },
                image_skin,
                text_listening: symbol == "reel::text" && args[1].tag() != ValueTag::Null,
                mask_source,
                effect_source,
                radio_option: (symbol == "reel::radio").then_some(args[0]),
            },
        );
        Ok(value)
    }

    pub(super) fn video_id(&self, heap: &Heap, value: Value) -> Result<NodeId, AdapterError> {
        let id = self.id(heap, value)?;
        if self.handles[&id].shape != snapshot::Shape::Video {
            return Err(AdapterError::Input("expected video node"));
        }
        Ok(id)
    }
    pub(super) fn set_video_frame(
        &mut self,
        id: NodeId,
        frame: reel_ui::resources::ResourceLease,
    ) -> Result<(), AdapterError> {
        let node = self
            .scene
            .node(id)
            .ok_or(AdapterError::Input("video node retired"))?;
        let mut kind = node.kind.clone();
        if let NodeKind::Image { source, .. } = &mut kind {
            *source = reel_ui::stage::ImageSource::resource(frame);
        }
        let props = node.props.clone();
        self.scene.update(id, kind, props)?;
        reel_core::profile::count(reel_core::profile::Phase::VideoStage);
        Ok(())
    }

    fn retire(&mut self, heap: &mut Heap, nodes: Vec<NodeId>) -> Result<(), AdapterError> {
        if !nodes.is_empty() {
            self.retirement_revision = self.retirement_revision.wrapping_add(1);
        }
        for id in nodes {
            self.page_stacks.remove(&id);
            self.virtual_scrolls.retire(id);
            if let Some(binding) = self.handles.remove(&id)
                && let Ok(value) = heap.import_handle(binding.handle)
            {
                interaction::publish(heap, value, binding.interaction_offset, Default::default())?;
                let view = heap.host_payload(value, 3)?;
                if binding.input.is_some() {
                    heap.set_host_payload(value, 3, Value::NULL)?;
                } else if view.tag() != ValueTag::Null {
                    heap.disconnect_derived(view)?;
                }
                overrides::retire(heap, value)?;
                heap.retire_host(value)?;
            }
        }
        Ok(())
    }

    pub fn revoke(&mut self, heap: &mut Heap) -> Result<(), AdapterError> {
        if let Some(root) = self.root.take() {
            heap.release_root(root)?;
        }
        let nodes = self.handles.keys().copied().collect();
        self.retire(heap, nodes)?;
        self.scene = Stage::default();
        Ok(())
    }

    pub fn value(&self, heap: &Heap, id: NodeId) -> Result<Value, AdapterError> {
        let node = self
            .handles
            .get(&id)
            .ok_or(AdapterError::Input("retired stage node"))?;
        Ok(heap.import_handle(node.handle)?)
    }

    /// Input has already been routed through accepted geometry. Check current
    /// ownership and script fields before retaining a callback task.
    pub fn click(&self, heap: &Heap, id: NodeId) -> Result<Option<(Value, Value)>, AdapterError> {
        let Some(node) = self.scene.node(id) else {
            return Ok(None);
        };
        if !matches!(node.kind, NodeKind::Button { .. }) {
            return Ok(None);
        }
        let target = self.value(heap, id)?;
        let callback = heap.table_entry(heap.host_fields(target)?, 1)?.1;
        if callback.tag() == ValueTag::Null {
            return Ok(None);
        }
        Ok(self
            .mounted_in_scope(heap, id, self.modal_scope(heap)?)?
            .then_some((target, callback)))
    }
    pub fn text_link(
        &self,
        heap: &Heap,
        link: &reel_ui::stage::NodeLink,
    ) -> Result<Option<(Value, Value)>, AdapterError> {
        let Some(node) = self.scene.node(link.node) else {
            return Ok(None);
        };
        let NodeKind::Text(text) = &node.kind else {
            return Ok(None);
        };
        if text != &link.text
            || !text.links().iter().any(|l| {
                l.id == link.id
                    && node.props.reveal.is_none_or(|end| l.range.end <= end)
                    && node.props.reveal_fade.is_none_or(|progress| {
                        use unicode_segmentation::UnicodeSegmentation;
                        let graphemes: Vec<_> =
                            text.grapheme_indices(true).map(|(i, _)| i).collect();
                        reel_ui::core::RevealFade {
                            from: node.props.reveal_fade_from,
                            progress,
                            step: node.props.reveal_fade_step,
                        }
                        .alpha(&graphemes, l.range.clone())
                            > 0.
                    })
            })
        {
            return Ok(None);
        }
        let target = self.value(heap, link.node)?;
        let fields = heap.host_fields(target)?;
        let current = heap.table_entry(fields, 0)?.1;
        if self.handles[&link.node].text_source != Some(heap.handle(current)?) {
            return Ok(None);
        }
        let callback = heap.table_entry(fields, 1)?.1;
        if callback.tag() == ValueTag::Null {
            return Ok(None);
        }
        Ok(self
            .mounted_in_scope(heap, link.node, self.modal_scope(heap)?)?
            .then_some((target, callback)))
    }

    pub(super) fn permits_input(&self, heap: &Heap, id: NodeId) -> Result<bool, AdapterError> {
        self.mounted_in_scope(heap, id, self.modal_scope(heap)?)
    }

    pub(super) fn mounted_in_scope(
        &self,
        heap: &Heap,
        id: NodeId,
        modal: Option<NodeId>,
    ) -> Result<bool, AdapterError> {
        let mut in_scope = modal.is_none();
        let mut current = Some(id);
        let mut root = id;
        while let Some(id) = current {
            let node = self.scene.node(id).unwrap();
            if matches!(node.kind, NodeKind::Popup(_)) && node.parent.is_none() {
                return Ok(false);
            }
            in_scope |= modal == Some(id);
            let value = self.value(heap, id)?;
            let fields = heap.host_fields(value)?;
            let offset = self.property_offset(heap, value)?;
            for property in [NodeProperty::Visible, NodeProperty::Enabled] {
                if overrides::get(
                    heap,
                    fields,
                    offset as usize,
                    overrides::selection(heap, value)?,
                    property.index(),
                )?
                .as_bool()
                    != Some(true)
                {
                    return Ok(false);
                }
            }
            root = id;
            current = self.scene.node(id).unwrap().parent;
        }
        Ok(in_scope && self.scene.root() == Some(root))
    }

    /// Inspect current script flags, including writes not yet projected to the UI.
    /// Reverse paint order stops at the first visible modal; no strings or full
    /// node properties are copied on the activation path.
    pub(super) fn modal_scope(&self, heap: &Heap) -> Result<Option<NodeId>, AdapterError> {
        if self.scene.has_popups()
            && let Some(modal) = self.popup_modal_scope(heap)?
        {
            return Ok(Some(modal));
        }
        let mut pending: Vec<_> = self
            .scene
            .root()
            .map(|id| (id, false))
            .into_iter()
            .collect();
        while let Some((id, children_visited)) = pending.pop() {
            if matches!(self.scene.node(id).unwrap().kind, NodeKind::Popup(_)) {
                continue;
            }
            let value = self.value(heap, id)?;
            let fields = heap.host_fields(value)?;
            let offset = self.property_offset(heap, value)?;
            let flag = |property: NodeProperty| -> Result<bool, AdapterError> {
                Ok(overrides::get(
                    heap,
                    fields,
                    offset as usize,
                    overrides::selection(heap, value)?,
                    property.index(),
                )?
                .as_bool()
                    == Some(true))
            };
            if !flag(NodeProperty::Visible)? {
                continue;
            }
            if children_visited {
                if flag(NodeProperty::Modal)? {
                    return Ok(Some(id));
                }
            } else {
                pending.push((id, true));
                pending.extend(
                    self.scene
                        .node(id)
                        .unwrap()
                        .children
                        .iter()
                        .map(|&id| (id, false)),
                );
            }
        }
        Ok(None)
    }

    pub fn has_handlers(&self) -> bool {
        let mut pending: Vec<_> = self.scene.root().into_iter().collect();
        while let Some(id) = pending.pop() {
            let node = self.scene.node(id).unwrap();
            if self.handles[&id].interaction_listening
                || match &node.kind {
                    NodeKind::Text(_) => self.handles[&id].text_listening,
                    NodeKind::Slider { .. }
                    | NodeKind::ScrollBar(_)
                    | NodeKind::Toggle { .. }
                    | NodeKind::Radio { .. }
                    | NodeKind::TextInput { .. } => true,
                    NodeKind::Button { listening, .. } => *listening,
                    NodeKind::Popup(popup) => popup.listening || popup.hint.is_some(),
                    NodeKind::Input {
                        bindings,
                        keyboard,
                        pointer,
                        wheel,
                        ..
                    } => *keyboard || *pointer || *wheel || !bindings.is_empty(),
                    _ => false,
                }
            {
                return true;
            }
            pending.extend(node.children.iter().copied());
        }
        false
    }
}

fn validate_virtual_props(props: &NodeProps) -> Result<(), AdapterError> {
    if props.height.is_some()
        || props.max_height.is_some()
        || props.stretch.axis(reel_ui::kurbo::Axis::Vertical)
        || props.x != 0.
        || props.y != 0.
        || props.rotation != 0.
        || props.scale_x != 1.
        || props.scale_y != 1.
        || props.anchor_x != 0.
        || props.anchor_y != 0.
    {
        return Err(AdapterError::Input(
            "virtual list requires natural height and no local transform; transform its scroll instead",
        ));
    }
    Ok(())
}

pub(super) fn bindings() -> [NativeBinding<StageBinding, AdapterError>; 6] {
    [
        add::binding(),
        remove::binding(),
        clear::binding(),
        input::binding::binding(),
        input::value::binding(),
        move_child::binding(),
    ]
}

/// Insert an unmounted child at index, or append it when index is null.
/// Return the inserted node. The parent owns its child's lifetime.
///
/// ```aside
/// fn main() {
///     game.configure(fonts: ["fonts/body.ttf"]);
///     let panel = stack();
///     stage.replace(panel);
///     let child = panel.add(text("Added"));
///     child.remove();
/// }
/// ```
#[aside_host::native(symbol = "reel::node::add", effects = Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST), capabilities = ["reel.stage"])]
pub(super) fn add(
    #[aside(context)] context: &mut CallContext<'_, StageBinding>,
    receiver: Value,
    child: Value,
    #[aside(default = null)] index: Option<i64>,
) -> Result<Value, AdapterError> {
    context
        .host
        .add_node(context.vm.heap_mut(), receiver, child, index)
}

impl StageBinding {
    pub(super) fn add_node(
        &mut self,
        heap: &mut Heap,
        receiver: Value,
        child: Value,
        index: Option<i64>,
    ) -> Result<Value, AdapterError> {
        let parent_id = self.id(heap, receiver)?;
        let child_id = self.id(heap, child)?;
        let child_count = self.scene.node(parent_id).unwrap().children.len();
        let index = match index {
            Some(i) => usize::try_from(i)
                .map_err(|_| AdapterError::Input("child index must be nonnegative"))?,
            None => child_count,
        };
        let children = heap.host_payload(receiver, 1)?;
        self.scene
            .add_with::<AdapterError>(parent_id, child_id, index, || {
                if index == child_count {
                    heap.array_push(children, child)?;
                } else {
                    let next = heap.alloc_array(&[])?;
                    for i in 0..heap.sequence_len(children)? {
                        if i as usize == index {
                            heap.array_push(next, child)?;
                        }
                        heap.array_push(next, heap.sequence_get(children, i)?)?;
                    }
                    heap.set_host_payload(receiver, 1, next)?;
                }
                Ok(())
            })?;
        heap.set_host_payload(child, 2, receiver)?;
        if matches!(self.scene.node(child_id).unwrap().kind, NodeKind::Popup(view) if view.hint.is_some())
        {
            self.sync_tooltips(heap, parent_id)?;
        }
        Ok(child)
    }
}

/// Move a direct child to a zero-based index within this container.
/// Return the same child, preserving its state and references.
#[aside_host::native(symbol = "reel::node::move", effects = Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST), capabilities = ["reel.stage"])]
pub(super) fn move_child(
    #[aside(context)] context: &mut CallContext<'_, StageBinding>,
    receiver: Value,
    child: Value,
    index: i64,
) -> Result<Value, AdapterError> {
    let heap = context.vm.heap_mut();
    let parent = context.host.id(heap, receiver)?;
    let id = context.host.id(heap, child)?;
    let node = context.host.scene.node(parent).unwrap();
    if context.host.scene.node(id).unwrap().parent != Some(parent) {
        return Err(AdapterError::Input(
            "move requires a direct child of this container",
        ));
    }
    let index = usize::try_from(index)
        .map_err(|_| AdapterError::Input("child index must be nonnegative"))?;
    if index >= node.children.len() {
        return Err(AdapterError::Input(
            "child index is outside the existing children",
        ));
    }
    let previous = node.children.iter().position(|&value| value == id).unwrap();
    if previous == index {
        return Ok(child);
    }
    let mut children = node.children.clone();
    children.remove(previous);
    children.insert(index, id);
    let references = heap.alloc_array(&[])?;
    for &id in &children {
        heap.array_push(references, context.host.value(heap, id)?)?;
    }
    let retired = context.host.scene.reconcile_children(parent, children)?;
    debug_assert!(retired.is_empty());
    heap.set_host_payload(receiver, 1, references)?;
    Ok(child)
}

/// Retire this node and its descendants, cancelling their owned activities.
/// Removing an already retired node completes immediately.
#[aside_host::native(symbol = "reel::node::remove", effects = Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST), capabilities = ["reel.stage"])]
pub(super) fn remove(
    #[aside(context)] context: &mut CallContext<'_, StageBinding>,
    receiver: Value,
) -> Result<(), AdapterError> {
    context.host.remove_node(context.vm.heap_mut(), receiver)
}

impl StageBinding {
    pub(super) fn remove_node(
        &mut self,
        heap: &mut Heap,
        receiver: Value,
    ) -> Result<(), AdapterError> {
        if matches!(heap.host_fields(receiver), Err(HeapError::RetiredHost)) {
            return Ok(());
        }
        let id = self.id(heap, receiver)?;
        let parent = heap.host_payload(receiver, 2)?;
        if parent.tag() != ValueTag::Null {
            let edges = heap.host_payload(parent, 1)?;
            let len = heap.sequence_len(edges)?;
            let index = (0..len)
                .rev()
                .find(|&i| heap.sequence_get(edges, i).unwrap().same_bits(receiver))
                .expect("mounted child has a traced parent edge");
            // The parent edge array is private stage storage. Compact it in
            // place so retirement does not allocate a replacement child tree.
            for i in index..len - 1 {
                heap.set_field(edges, i, heap.sequence_get(edges, i + 1)?)?;
            }
            heap.array_pop(edges)?;
        }
        let was_root = self.scene.root() == Some(id);
        let retired = self.scene.remove(id)?;
        if was_root && let Some(root) = self.root.take() {
            heap.release_root(root)?;
        }
        self.retire(heap, retired)
    }
}

/// Retire all children while preserving this container and its properties.
#[aside_host::native(symbol = "reel::node::clear", effects = Effects(Effects::ALLOCATE | Effects::WRITE | Effects::HOST), capabilities = ["reel.stage"])]
pub(super) fn clear(
    #[aside(context)] context: &mut CallContext<'_, StageBinding>,
    receiver: Value,
) -> Result<(), AdapterError> {
    let heap = context.vm.heap_mut();
    let id = context.host.id(heap, receiver)?;
    let edges = heap.alloc_array(&[])?;
    let retired = context.host.scene.clear_children(id)?;
    heap.set_host_payload(receiver, 1, edges)?;
    context.host.retire(heap, retired)
}

pub(super) mod property_values;
use property_values::read_props;
pub(super) use property_values::{
    boolean, button_kind, color, effect_stamp, mask_changed, mask_handle, mask_offset, nonnegative,
    number, positive, read_alpha_layout, read_corner_colors, read_fit, read_image_adjustment,
    read_image_repeat, read_sampling, read_slice, read_source, read_source_rect, read_text,
    validate_background, validate_blend, validate_button_property, validate_byte_index,
    validate_callback, validate_color, validate_dimension, validate_effect_resolution_scale,
    validate_font_weight, validate_image_property, validate_invert, validate_layout_property,
    validate_mask, validate_mask_offset, validate_mask_progress, validate_nonnegative,
    validate_number, validate_opacity, validate_optional_number, validate_reveal, validate_stretch,
    validate_text_align, validate_text_fit, validate_text_property, validate_text_props,
    validate_text_spacing, validate_writing_mode, validation, writing_mode,
};
