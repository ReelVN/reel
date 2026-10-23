//! Stable Aside host nodes projected into Reel's one shared stage model.
use std::collections::BTreeMap;

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
        /// Tab, controller navigation and focused confirmation remain available.
        /// The callback receives an event with this node as target.
        "reel::button";
        /// Create a numeric control with min, max and step. Bind value to a writable
        /// property; on_change observes committed value changes.
        "reel::slider";
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
        /// Create a clipped camera container. camera_x/y select the viewed world point;
        /// zoom and camera_rotation transform its ordinary children. Drawing and input
        /// use the same transform; the common x/y properties place the viewport.
        "reel::camera";
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
pub(super) mod flow;
pub(super) mod input;
mod interaction;
pub(super) mod overrides;
pub(super) mod popup;
pub(super) mod scroll;
pub(super) mod snapshot;
pub(super) mod source;
pub(super) mod text_input;

/// Specific numeric fields share one registration and dispatch order.
pub(in crate::aside) fn animation_prefixes() -> [(&'static str, &'static [aside_host::Property]); 5]
{
    [
        ("camera", camera::CameraField::PROPERTIES),
        ("scroll", scroll::ScrollField::PROPERTIES),
        ("flow", flow::FlowField::PROPERTIES),
        ("grid", flow::GridField::PROPERTIES),
        ("popup", popup::PopupField::PROPERTIES),
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
    /// Blend the completed subtree: normal, add, subtract, multiply, or screen.
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
    #[aside(name = "mask", ty = Any, doc_type = aside_host::source::nullable(aside_compiler::semantic::TypeHint::Union(vec![aside_compiler::semantic::TypeHint::String,aside_host::source::number(),aside_host::source::named("reel::types::Mask")])), default = null, validate = validate_mask)]
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
}

pub(super) struct StageBinding {
    pub(super) types: super::source::HostTypes,
    pub(super) text_filter_function: Option<aside_vm::bytecode::FunctionId>,
    pub scene: Stage,
    pub retirement_revision: u64,
    handles: BTreeMap<NodeId, NodeBinding>,
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
    mask_source: Option<HeapHandle>,
    text_listening: bool,
    /// Weak identity stamp only: never dereferenced or treated as a GC root.
    radio_option: Option<Value>,
}

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
            types: super::source::HostTypes::new(heap)?,
            text_filter_function: None,
            scene: Stage::default(),
            retirement_revision: 0,
            handles: BTreeMap::new(),
            root: None,
            property_keys: Default::default(),
            editing_limits: Default::default(),
        })
    }

    fn property_key(&mut self, heap: &mut Heap, name: &'static str) -> Result<Value, HeapError> {
        self.property_keys.key(heap, name)
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
        for i in 0..heap.sequence_len(references)? {
            heap.set_host_payload(heap.sequence_get(references, i)?, 2, container)?;
        }
        heap.set_host_payload(container, 1, references)?;
        let retired = self.scene.reconcile_children(parent, children)?;
        self.retire(heap, retired)
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
        fn visit(stage: &StageBinding, heap: &Heap, id: NodeId) -> Result<bool, AdapterError> {
            let binding = &stage.handles[&id];
            let value = heap.import_handle(binding.handle)?;
            if heap.info(heap.host_fields(value)?)?.content_version != binding.fields_version
                || binding.projection_dirty
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
        Ok(match self.scene.node(id).unwrap().kind {
            NodeKind::Row(_) | NodeKind::Column(_) => flow::FlowField::PROPERTIES.len() as u32,
            NodeKind::Grid(_) => flow::GridField::PROPERTIES.len() as u32,
            NodeKind::Camera(_) => camera::CameraField::PROPERTIES.len() as u32,
            NodeKind::Particles(_) => super::particles::PREFIX.len() as u32,
            NodeKind::Text(_) => 2,
            NodeKind::TextInput { .. } => text_input::TextInputField::PROPERTIES.len() as u32,
            NodeKind::Slider { .. } => controls::SliderField::PROPERTIES.len() as u32,
            NodeKind::Toggle { .. } => controls::ToggleField::PROPERTIES.len() as u32,
            NodeKind::Radio { .. } => controls::RadioField::PROPERTIES.len() as u32,
            NodeKind::Input { .. } => input::InputField::PROPERTIES.len() as u32,
            NodeKind::Scroll { .. } => scroll::ScrollField::PROPERTIES.len() as u32,
            NodeKind::Popup(_) => popup::PopupField::PROPERTIES.len() as u32,
            NodeKind::Image { .. } => registry::IMAGE_PREFIX.len() as u32,
            NodeKind::Button { .. } => appearance::ButtonField::PROPERTIES.len() as u32,
            _ => 0,
        })
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

        let raw_args = args;
        let resolved = args
            .iter()
            .map(|&value| heap.property_argument(value))
            .collect::<Result<Vec<_>, _>>()?;
        let args = resolved.as_slice();
        let prefix = registry::node_prefix(symbol);
        let fields = heap.alloc_table()?;
        let field_count = prefix.len() + NodeProperty::PROPERTIES.len();
        for (index, argument) in args.iter().take(field_count).enumerate() {
            let name = if index < prefix.len() {
                prefix[index]
            } else {
                NodeProperty::PROPERTIES[index - prefix.len()].name
            };
            let key = self.property_key(heap, name)?;
            heap.table_store(fields, index as u32, key, *argument)?;
        }
        let shape = snapshot::Shape::from_symbol(symbol);
        let mask_origin = origin.as_deref().map(Arc::from);
        let (mut kind, input_declaration) = if shape == snapshot::Shape::Input {
            let (kind, declaration) = input::prepare(heap, fields)?;
            (kind, Some(declaration))
        } else {
            (shape.project(heap, fields, origin, read_limit)?, None)
        };
        let props = read_props(
            heap,
            fields,
            prefix.len(),
            Value::NULL,
            read_limit,
            None,
            mask_origin,
        )?;
        validate_text_fade(&kind, &props)?;
        let mask_source = mask_handle(heap, fields, prefix.len(), Value::NULL)?;
        let mut children = Vec::new();
        let references = heap.alloc_array(&[])?;
        if matches!(
            kind,
            NodeKind::Camera(_)
                | NodeKind::Row(_)
                | NodeKind::Column(_)
                | NodeKind::Grid(_)
                | NodeKind::Scroll { .. }
                | NodeKind::Popup(_)
                | NodeKind::Stack
                | NodeKind::Cursor
                | NodeKind::Button { .. }
                | NodeKind::Slider { .. }
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
                children = self.scene.composition_roots(&children)?;
                for &child in &children {
                    heap.array_push(references, self.value(heap, child)?)?;
                }
            }
        }
        let id = self.scene.next_identity()?;
        let payload = [
            Value::int(id.0 as i64),
            references,
            Value::NULL,
            Value::NULL,
            Value::int(0),
            Value::NULL,
        ];
        let value = heap.alloc_composable_host(fields, &payload, Some(shape.validator()))?;
        self.types.mark(
            heap,
            value,
            registry::node_type(symbol).expect("node constructor type"),
        )?;
        for (offset, name) in interaction::FIELDS.into_iter().enumerate() {
            let key = self.property_key(heap, name)?;
            let state = name == "disabled" && !props.enabled;
            heap.table_store(
                fields,
                (field_count + offset) as u32,
                key,
                Value::bool(state),
            )?;
        }
        heap.observe_host_properties(value, field_count as u32, interaction::FIELDS.len() as u32)?;
        let extra_fields = interaction::FIELDS.len();
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
            for (i, name) in scroll::METRICS.into_iter().enumerate() {
                let key = self.property_key(heap, name)?;
                heap.table_store(
                    fields,
                    (field_count + extra_fields + i) as u32,
                    key,
                    Value::float(0.),
                )?;
            }
            heap.observe_host_properties(
                value,
                (field_count + extra_fields) as u32,
                scroll::METRICS.len() as u32,
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
            Some(input::initialize(heap, value, args[0], declaration)?)
        } else {
            None
        };
        controls::project_group(heap, value, &mut kind)?;
        text_input::project(heap, value, &mut kind)?;
        self.scene.create(kind, props, children)?;
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
                interaction_listening: args[prefix.len() + NodeProperty::OnInteraction.index()]
                    .tag()
                    != ValueTag::Null,
                fields_version: heap.info(fields)?.content_version,
                projection_dirty: false,
                overrides: None,
                input,
                text_history: None,
                text_source: if matches!(
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
                text_listening: symbol == "reel::text" && args[1].tag() != ValueTag::Null,
                mask_source,
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
        Ok(())
    }

    fn sync_tree(
        &mut self,
        heap: &mut Heap,
        root: NodeId,
        read_limit: usize,
    ) -> Result<(), AdapterError> {
        let mut pending = vec![root];
        let mut updates = Vec::new();
        while let Some(id) = pending.pop() {
            let node = self
                .scene
                .node(id)
                .ok_or(AdapterError::Input("retired stage node"))?;
            pending.extend(node.children.iter().copied());
            let binding = self.handles.get_mut(&id).unwrap();
            let value = heap.import_handle(binding.handle)?;
            if matches!(node.kind, NodeKind::VirtualList(_))
                && let Some(parent) = node.parent
            {
                let parent = self.scene.node(parent).unwrap();
                if !matches!(
                    parent.kind,
                    NodeKind::Scroll {
                        axis: reel_ui::widgets::ScrollAxis::Vertical,
                        ..
                    }
                ) || parent
                    .children
                    .iter()
                    .filter(|id| {
                        !self
                            .scene
                            .node(**id)
                            .unwrap()
                            .props
                            .stretch
                            .axis(reel_ui::kurbo::Axis::Vertical)
                    })
                    .count()
                    != 1
                {
                    return Err(AdapterError::Input(
                        "virtual list must be the only flow child of a vertical scroll",
                    ));
                }
            }
            let fields = heap.host_fields(value)?;
            let version = heap.info(fields)?.content_version;
            let selected = if binding.overrides.is_some() {
                overrides::selection(heap, value)?
            } else {
                Value::NULL
            };
            if version == binding.fields_version
                && !binding.projection_dirty
                && !binding
                    .overrides
                    .as_ref()
                    .map(|cache| cache.changed(heap, value))
                    .transpose()?
                    .unwrap_or(false)
            {
                continue;
            }
            let (mut kind, offset) = match &node.kind {
                NodeKind::Row(_) | NodeKind::Column(_) => (
                    flow::flow_kind(heap, fields, matches!(node.kind, NodeKind::Row(_)))?,
                    flow::FlowField::PROPERTIES.len(),
                ),
                NodeKind::Grid(_) => (
                    flow::grid_kind(heap, fields)?,
                    flow::GridField::PROPERTIES.len(),
                ),
                NodeKind::VirtualList(_) => (
                    NodeKind::VirtualList(super::lists::virtualized::project(heap, value)?),
                    0,
                ),
                NodeKind::Input { bindings, .. } => {
                    let source = heap.table_entry(fields, 0)?.1;
                    let state = binding.input.as_mut().unwrap();
                    let bindings = input::replace(heap, value, source, bindings, state)?;
                    (
                        input::kind(heap, fields, bindings)?,
                        input::InputField::PROPERTIES.len(),
                    )
                }
                NodeKind::Camera(_) => (
                    camera::kind(heap, fields)?,
                    camera::CameraField::PROPERTIES.len(),
                ),
                NodeKind::Scroll { .. } => (
                    scroll::kind(heap, fields)?,
                    scroll::ScrollField::PROPERTIES.len(),
                ),
                NodeKind::Popup(_) => (
                    popup::kind(heap, fields)?,
                    popup::PopupField::PROPERTIES.len(),
                ),
                NodeKind::Slider { .. } => (
                    controls::slider_kind(heap, fields)?,
                    controls::SliderField::PROPERTIES.len(),
                ),
                NodeKind::Toggle { appearance, .. } => (
                    controls::toggle_kind(heap, fields, read_limit, *appearance)?,
                    controls::ToggleField::PROPERTIES.len(),
                ),
                NodeKind::Radio { revision, .. } => {
                    let option = heap
                        .table_entry(fields, controls::RadioField::Option.index() as u32)?
                        .1;
                    let changed = !binding
                        .radio_option
                        .is_some_and(|old| old.same_bits(option));
                    (
                        controls::radio_kind(
                            heap,
                            fields,
                            revision.wrapping_add(u64::from(changed)),
                            read_limit,
                        )?,
                        controls::RadioField::PROPERTIES.len(),
                    )
                }
                NodeKind::Button { .. } => (
                    button_kind(
                        heap,
                        heap.table_entry(fields, 0)?.1,
                        heap.table_entry(fields, 1)?.1,
                        heap.table_entry(fields, 2)?.1,
                        read_limit,
                    )?,
                    appearance::ButtonField::PROPERTIES.len(),
                ),
                NodeKind::Text(text) => {
                    let source = heap.table_entry(fields, 0)?.1;
                    (
                        NodeKind::Text(if binding.text_source == Some(heap.handle(source)?) {
                            text.clone()
                        } else {
                            super::rich_text::read(heap, source, read_limit)?
                        }),
                        2,
                    )
                }
                NodeKind::TextInput {
                    value,
                    multiline,
                    options,
                } => {
                    let source = heap.table_entry(fields, 0)?.1;
                    let projected = if binding.text_source == Some(heap.handle(source)?)
                        || binding.text_source.is_none()
                            && text_input::matches_text(heap, source, value)?
                    {
                        value.clone()
                    } else {
                        Arc::from(read_text(heap, source, read_limit)?)
                    };
                    if binding.text_history.as_ref().is_some_and(|h| {
                        h.state == text_input::HistoryState::Invalidated
                            || Some(h.expected) != heap.handle(source).ok()
                    }) || binding.text_source != Some(heap.handle(source)?)
                        && binding.text_history.is_none()
                    {
                        let host = heap.import_handle(binding.handle)?;
                        let text = &projected;
                        let mut selected = text_input::selection(heap, host)?;
                        let boundary = |mut offset: usize| {
                            offset = offset.min(text.len());
                            while !text.is_char_boundary(offset) {
                                offset -= 1;
                            }
                            offset
                        };
                        selected.anchor = boundary(selected.anchor);
                        selected.focus = boundary(selected.focus);
                        let revision = text_input::revision(heap, host)?
                            .checked_add(1)
                            .ok_or(HeapError::IdentityExhausted)?;
                        text_input::publish(heap, host, None, selected, false, false)?;
                        heap.set_host_payload(host, 4, Value::int(revision as i64))?;
                        binding.text_history = None;
                    } else if binding
                        .text_history
                        .as_ref()
                        .is_some_and(|h| h.state == text_input::HistoryState::ResetPublished)
                    {
                        binding.text_history = None;
                    }
                    (
                        NodeKind::TextInput {
                            multiline: *multiline,
                            options: text_input::options(heap, fields, Some(options), read_limit)?,
                            value: projected,
                        },
                        text_input::TextInputField::PROPERTIES.len(),
                    )
                }
                NodeKind::Particles(batch) => (
                    NodeKind::Particles(batch.clone()),
                    super::particles::PREFIX.len(),
                ),
                NodeKind::Image { source, origin, .. } => (
                    NodeKind::Image {
                        source: if binding.shape == snapshot::Shape::Video
                            || binding.text_source
                                == Some(heap.handle(heap.table_entry(fields, 0)?.1)?)
                        {
                            source.clone()
                        } else {
                            read_source(heap, heap.table_entry(fields, 0)?.1, read_limit)?
                        },
                        fit: read_fit(heap, heap.table_entry(fields, 1)?.1)?,
                        slice: read_slice(heap, heap.table_entry(fields, 2)?.1)?,
                        slice_scale: nonnegative(heap.table_entry(fields, 3)?.1)?,
                        repeat: read_image_repeat(
                            heap,
                            heap.table_entry(fields, 4)?.1,
                            heap.table_entry(fields, 2)?.1,
                        )?,
                        tile_scale: positive(heap.table_entry(fields, 5)?.1)?,
                        corner_colors: read_corner_colors(heap, heap.table_entry(fields, 6)?.1)?,
                        slice_center: boolean(heap.table_entry(fields, 7)?.1)?,
                        adjustment: read_image_adjustment(heap, fields)?,
                        source_rect: read_source_rect(heap, heap.table_entry(fields, 11)?.1)?,
                        origin: origin.clone(),
                    },
                    registry::IMAGE_PREFIX.len(),
                ),
                kind => (kind.clone(), 0),
            };
            controls::project_group(heap, value, &mut kind)?;
            let mut props = read_props(
                heap,
                fields,
                offset,
                selected,
                read_limit,
                Some((binding.mask_source, &node.props)),
                node.props.mask_origin.clone(),
            )?;
            overrides::apply_offsets(heap, value, &mut props)?;
            validate_text_fade(&kind, &props)?;
            if matches!(kind, NodeKind::VirtualList(_)) {
                validate_virtual_props(&props)?;
            }
            let mask_source = mask_handle(heap, fields, offset, selected)?;
            let version = if matches!(kind, NodeKind::TextInput { .. }) {
                heap.info(fields)?.content_version
            } else {
                version
            };
            updates.push((id, kind, props, version, mask_source));
        }
        for (id, kind, props, version, mask_source) in updates {
            let mut kind = kind;
            let host = heap.import_handle(self.handles[&id].handle)?;
            text_input::project(heap, host, &mut kind)?;
            text_input::project_queue(self.handles[&id].text_history.as_deref(), &mut kind);
            if matches!(kind, NodeKind::Radio { .. }) {
                let value = heap.import_handle(self.handles[&id].handle)?;
                self.handles.get_mut(&id).unwrap().radio_option = Some(
                    heap.table_entry(
                        heap.host_fields(value)?,
                        controls::RadioField::Option.index() as u32,
                    )?
                    .1,
                );
            }
            if matches!(
                kind,
                NodeKind::Text(_) | NodeKind::TextInput { .. } | NodeKind::Image { .. }
            ) {
                let value = heap.import_handle(self.handles[&id].handle)?;
                let source = heap.table_entry(heap.host_fields(value)?, 0)?.1;
                self.handles.get_mut(&id).unwrap().text_source = Some(heap.handle(source)?);
                if matches!(kind, NodeKind::Text(_)) {
                    self.handles.get_mut(&id).unwrap().text_listening =
                        heap.table_entry(heap.host_fields(value)?, 1)?.1.tag() != ValueTag::Null;
                }
            }
            self.scene.update(id, kind, props)?;
            let binding = self.handles.get_mut(&id).unwrap();
            binding.fields_version = version;
            binding.projection_dirty = false;
            if let Some(cache) = &mut binding.overrides {
                cache.accept(heap, heap.import_handle(binding.handle)?)?;
            }
            binding.mask_source = mask_source;
            let fields = heap.host_fields(heap.import_handle(binding.handle)?)?;
            binding.interaction_listening = heap
                .table_entry(
                    fields,
                    interaction::callback_field(binding.interaction_offset),
                )?
                .1
                .tag()
                != ValueTag::Null;
        }
        Ok(())
    }

    pub fn sync(&mut self, heap: &mut Heap, read_limit: usize) -> Result<(), AdapterError> {
        if let Some(root) = self.scene.root() {
            self.sync_tree(heap, root, read_limit)?;
        }
        // Every node keeps its complete component alive. A collected detached
        // root therefore means the whole component has lost script ownership.
        let lost = self
            .scene
            .nodes()
            .filter(|node| node.parent.is_none() && Some(node.id) != self.scene.root())
            .filter(|node| heap.import_handle(self.handles[&node.id].handle).is_err())
            .map(|node| node.id)
            .collect::<Vec<_>>();
        for root in lost {
            let retired = self.scene.discard(root)?;
            self.retire(heap, retired)?;
        }
        Ok(())
    }

    fn retire(&mut self, heap: &mut Heap, nodes: Vec<NodeId>) -> Result<(), AdapterError> {
        if !nodes.is_empty() {
            self.retirement_revision = self.retirement_revision.wrapping_add(1);
        }
        for id in nodes {
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

    fn mounted_in_scope(
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
    fn modal_scope(&self, heap: &Heap) -> Result<Option<NodeId>, AdapterError> {
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
                    | NodeKind::Toggle { .. }
                    | NodeKind::Radio { .. }
                    | NodeKind::TextInput { .. } => true,
                    NodeKind::Button { listening, .. } => *listening,
                    NodeKind::Popup(popup) => popup.listening,
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
        let node = self.scene.node(parent_id).unwrap();
        let index = match index {
            Some(i) => usize::try_from(i)
                .map_err(|_| AdapterError::Input("child index must be nonnegative"))?,
            None => node.children.len(),
        };
        // Prepare replacement traced edges before committing either tree.
        let previous = heap.host_payload(receiver, 1)?;
        let next = heap.alloc_array(&[])?;
        for i in 0..heap.sequence_len(previous)? {
            if i as usize == index {
                heap.array_push(next, child)?;
            }
            heap.array_push(next, heap.sequence_get(previous, i)?)?;
        }
        if index == node.children.len() {
            heap.array_push(next, child)?;
        }
        self.scene.add(parent_id, child_id, index)?;
        heap.set_host_payload(receiver, 1, next)?;
        heap.set_host_payload(child, 2, receiver)?;
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

fn read_text(heap: &Heap, value: Value, read_limit: usize) -> Result<String, AdapterError> {
    text::read_string(heap, value, read_limit)
}

fn read_source(
    heap: &Heap,
    value: Value,
    read_limit: usize,
) -> Result<reel_ui::stage::ImageSource, AdapterError> {
    if heap.info(value)?.kind == ObjectKind::Bytes {
        let length = heap.info(value)?.len;
        if length == 0 {
            return Err(AdapterError::Input("image source must not be empty"));
        }
        let mut bytes = Vec::with_capacity(length as usize);
        while bytes.len() < length as usize {
            bytes.extend_from_slice(heap.byte_segment(value, bytes.len() as u32)?);
        }
        return Ok(reel_ui::stage::ImageSource::encoded(bytes));
    }
    let source = read_text(heap, value, read_limit)?;
    if source.is_empty() {
        return Err(AdapterError::Input("image source must not be empty"));
    }
    Ok(source.into())
}

fn read_fit(heap: &Heap, value: Value) -> Result<reel_ui::properties::ObjectFit, AdapterError> {
    use reel_ui::properties::ObjectFit;
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 7 {
        return Err(AdapterError::Input(
            "image fit requires contain, cover or stretch",
        ));
    }
    let mut bytes = [0; 7];
    for index in 0..info.len {
        heap.read_bytes(value, index, &mut bytes[index as usize..index as usize + 1])?;
    }
    match &bytes[..info.len as usize] {
        b"contain" => Ok(ObjectFit::Contain),
        b"cover" => Ok(ObjectFit::Cover),
        b"stretch" => Ok(ObjectFit::Stretch),
        _ => Err(AdapterError::Input(
            "image fit requires contain, cover or stretch",
        )),
    }
}

fn read_source_rect(
    heap: &Heap,
    value: Value,
) -> Result<Option<reel_ui::kurbo::Rect>, AdapterError> {
    if value.tag() == ValueTag::Null {
        return Ok(None);
    }
    let invalid = || {
        AdapterError::Input(
            "image source_rect requires null or a finite (x, y, width, height) tuple with positive dimensions and finite float32 endpoints",
        )
    };
    if heap.info(value)?.kind != ObjectKind::Tuple || heap.sequence_len(value)? != 4 {
        return Err(invalid());
    }
    let mut values = [0.; 4];
    for (index, output) in values.iter_mut().enumerate() {
        let value = heap.sequence_get(value, index as u32)?;
        *output = value
            .as_float()
            .or_else(|| value.as_int().map(|n| n as f64))
            .ok_or_else(invalid)?;
        if !(*output as f32).is_finite() {
            return Err(invalid());
        }
    }
    let [x, y, width, height] = values;
    let rect = reel_ui::kurbo::Rect::new(x, y, x + width, y + height);
    if width <= 0. || height <= 0. || !valid_source_rect(rect) {
        return Err(invalid());
    }
    Ok(Some(rect))
}

fn valid_source_rect(rect: reel_ui::kurbo::Rect) -> bool {
    (rect.width() as f32) > 0.
        && (rect.height() as f32) > 0.
        && [
            rect.x0,
            rect.y0,
            rect.x1,
            rect.y1,
            rect.width(),
            rect.height(),
        ]
        .iter()
        .all(|n| (*n as f32).is_finite())
}

fn read_slice(heap: &Heap, value: Value) -> Result<Option<reel_ui::kurbo::Insets>, AdapterError> {
    use reel_ui::kurbo::Insets;
    if value.tag() == ValueTag::Null {
        return Ok(None);
    }
    if matches!(value.tag(), ValueTag::Int | ValueTag::Float) {
        let border = nonnegative(value)?;
        return Ok(Some(Insets::uniform(border)));
    }
    if !matches!(
        heap.info(value)?.kind,
        ObjectKind::Array | ObjectKind::Tuple
    ) || heap.sequence_len(value)? != 4
    {
        return Err(AdapterError::Input(
            "image slice requires null, a nonnegative number, or [left, top, right, bottom]",
        ));
    }
    Ok(Some(Insets::new(
        nonnegative(heap.sequence_get(value, 0)?)?,
        nonnegative(heap.sequence_get(value, 1)?)?,
        nonnegative(heap.sequence_get(value, 2)?)?,
        nonnegative(heap.sequence_get(value, 3)?)?,
    )))
}

fn read_repeat(heap: &Heap, value: Value) -> Result<[bool; 2], AdapterError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 4 {
        return Err(AdapterError::Input(
            "image repeat requires none, x, y or both",
        ));
    }
    let mut bytes = [0; 4];
    heap.read_bytes(value, 0, &mut bytes[..info.len as usize])?;
    match &bytes[..info.len as usize] {
        b"none" => Ok([false, false]),
        b"x" => Ok([true, false]),
        b"y" => Ok([false, true]),
        b"both" => Ok([true, true]),
        _ => Err(AdapterError::Input(
            "image repeat requires none, x, y or both",
        )),
    }
}

fn read_image_repeat(heap: &Heap, value: Value, slice: Value) -> Result<[bool; 2], AdapterError> {
    let repeat = read_repeat(heap, value)?;
    if repeat != [false; 2] && slice.tag() != ValueTag::Null {
        return Err(AdapterError::Input(
            "image repeat and slice are mutually exclusive",
        ));
    }
    Ok(repeat)
}

fn positive(value: Value) -> Result<f64, AdapterError> {
    let value = nonnegative(value)?;
    if value == 0. {
        return Err(AdapterError::Input("image tile_scale must be positive"));
    }
    Ok(value)
}

fn read_corner_colors(
    heap: &Heap,
    value: Value,
) -> Result<Option<[reel_ui::peniko::Color; 4]>, AdapterError> {
    if value.tag() == ValueTag::Null {
        return Ok(None);
    }
    if !matches!(
        heap.info(value)?.kind,
        ObjectKind::Array | ObjectKind::Tuple
    ) || heap.sequence_len(value)? != 4
    {
        return Err(AdapterError::Input(
            "image corner_colors requires null or [top_left, top_right, bottom_right, bottom_left]",
        ));
    }
    let mut colors = [reel_ui::peniko::Color::WHITE; 4];
    for (index, output) in colors.iter_mut().enumerate() {
        let [r, g, b, a] = color(heap, heap.sequence_get(value, index as u32)?)?;
        *output = reel_ui::peniko::Color::from_rgba8(r, g, b, a);
    }
    Ok(Some(colors))
}

fn drawing_component(value: Value, nonnegative: bool) -> Result<f32, AdapterError> {
    let value = value
        .as_float()
        .or_else(|| value.as_int().map(|v| v as f64))
        .ok_or(AdapterError::Input(
            "image color adjustment requires numbers",
        ))?;
    let component = value as f32;
    if !component.is_finite() || nonnegative && value < 0. {
        return Err(AdapterError::Input(
            "image color adjustment requires finite float32 values; saturation and brightness must be nonnegative",
        ));
    }
    Ok(component)
}

fn drawing_rgb(heap: &Heap, value: Value, nonnegative: bool) -> Result<[f32; 3], AdapterError> {
    if matches!(value.tag(), ValueTag::Int | ValueTag::Float) {
        return Ok([drawing_component(value, nonnegative)?; 3]);
    }
    if !matches!(
        heap.info(value)?.kind,
        ObjectKind::Array | ObjectKind::Tuple
    ) || heap.sequence_len(value)? != 3
    {
        return Err(AdapterError::Input(
            "image color adjustment requires a number or [red, green, blue]",
        ));
    }
    let mut rgb = [0.; 3];
    for (index, component) in rgb.iter_mut().enumerate() {
        *component = drawing_component(heap.sequence_get(value, index as u32)?, nonnegative)?;
    }
    Ok(rgb)
}

fn read_image_adjustment(
    heap: &Heap,
    fields: Value,
) -> Result<reel_ui::frame::ImageColorAdjustment, AdapterError> {
    Ok(reel_ui::frame::ImageColorAdjustment {
        saturation: drawing_component(heap.table_entry(fields, 8)?.1, true)?,
        gain: drawing_rgb(heap, heap.table_entry(fields, 9)?.1, true)?,
        offset: drawing_rgb(heap, heap.table_entry(fields, 10)?.1, false)?,
    })
}

pub(super) fn validate_image_property(
    heap: &Heap,
    index: u32,
    value: Value,
) -> Result<(), HeapError> {
    match index {
        0 => {
            let info = heap.info(value)?;
            if matches!(info.kind, ObjectKind::String | ObjectKind::Bytes) && info.len != 0 {
                Ok(())
            } else {
                Err(HeapError::TypeMismatch)
            }
        }
        1 => read_fit(heap, value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        2 => read_slice(heap, value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        3 => validate_nonnegative(heap, value),
        4 => read_repeat(heap, value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        5 => positive(value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        6 => read_corner_colors(heap, value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        7 => boolean(value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        8 => drawing_component(value, true)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        9 | 10 => drawing_rgb(heap, value, index == 9)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        11 => read_source_rect(heap, value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        _ => validate_layout_property(heap, index - registry::IMAGE_PREFIX.len() as u32, value),
    }
}

fn button_kind(
    heap: &Heap,
    label: Value,
    callback: Value,
    focus_on_pointer: Value,
    read_limit: usize,
) -> Result<NodeKind, AdapterError> {
    validate_button_property(heap, 1, callback)?;
    Ok(NodeKind::Button {
        label: if label.tag() == ValueTag::Null {
            None
        } else {
            Some(read_text(heap, label, read_limit)?)
        },
        listening: callback.tag() != ValueTag::Null,
        focus_on_pointer: focus_on_pointer
            .as_bool()
            .ok_or(AdapterError::Input("focus_on_pointer must be boolean"))?,
    })
}
fn validate_button_property(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
    match index {
        0 | 1 if value.tag() == ValueTag::Null => Ok(()),
        0 if heap.accepts_builtin(aside_vm::bytecode::BuiltinType::String, value)? => Ok(()),
        1 if heap.accepts_builtin(aside_vm::bytecode::BuiltinType::Closure, value)? => Ok(()),
        0 | 1 => Err(HeapError::TypeMismatch),
        2 => value.as_bool().map(|_| ()).ok_or(HeapError::TypeMismatch),
        _ => validate_layout_property(
            heap,
            index - appearance::ButtonField::PROPERTIES.len() as u32,
            value,
        ),
    }
}

fn validate_callback(heap: &Heap, value: Value) -> Result<(), HeapError> {
    validate_button_property(heap, 1, value)
}

pub(super) fn number(value: Value) -> Result<f64, AdapterError> {
    let value = value
        .as_float()
        .or_else(|| value.as_int().map(|n| n as f64))
        .ok_or(AdapterError::Input("node property requires a number"))?;
    if !value.is_finite() {
        return Err(AdapterError::Input("node geometry must be finite"));
    }
    Ok(value)
}
pub(super) fn nonnegative(value: Value) -> Result<f64, AdapterError> {
    let value = number(value)?;
    if value < 0. {
        return Err(AdapterError::Input("node size must be nonnegative"));
    }
    Ok(value)
}
fn dimension(value: Value) -> Result<Option<f64>, AdapterError> {
    if value.tag() == ValueTag::Null {
        Ok(None)
    } else {
        nonnegative(value).map(Some)
    }
}
fn opacity(value: Value) -> Result<f64, AdapterError> {
    let value = number(value)?;
    if !(0. ..=1.).contains(&value) {
        return Err(AdapterError::Input("opacity must be between zero and one"));
    }
    Ok(value)
}
fn boolean(value: Value) -> Result<bool, AdapterError> {
    value
        .as_bool()
        .ok_or(AdapterError::Input("node property requires bool"))
}
pub(super) fn color(heap: &Heap, value: Value) -> Result<[u8; 4], AdapterError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || !matches!(info.len, 7 | 9) {
        return Err(AdapterError::Input("color requires #rrggbb or #rrggbbaa"));
    }
    let mut bytes = [0; 9];
    // A color has at most nine bytes. Single-byte reads also respect the Heap's
    // existing per-operation byte budget, without allocating or copying text.
    for index in 0..info.len {
        heap.read_bytes(value, index, &mut bytes[index as usize..index as usize + 1])?;
    }
    if bytes[0] != b'#' {
        return Err(AdapterError::Input("color requires #rrggbb or #rrggbbaa"));
    }
    let mut result = [255; 4];
    for (channel, pair) in result
        .iter_mut()
        .zip(bytes[1..info.len as usize].chunks_exact(2))
    {
        let hex = |byte: u8| match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            b'A'..=b'F' => Ok(byte - b'A' + 10),
            _ => Err(AdapterError::Input("color requires hexadecimal channels")),
        };
        *channel = hex(pair[0])? * 16 + hex(pair[1])?;
    }
    Ok(result)
}
fn validate_text_property(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
    if index == 0 {
        return super::rich_text::source(heap, value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch);
    }
    if index == 1 {
        return validate_button_property(heap, 1, value);
    }
    validate_layout_property(heap, index - 2, value)
}
pub(super) fn validate_layout_property(
    heap: &Heap,
    index: u32,
    value: Value,
) -> Result<(), HeapError> {
    if index as usize >= NodeProperty::PROPERTIES.len() {
        return if (index as usize) < NodeProperty::PROPERTIES.len() + interaction::FIELDS.len()
            && value.as_bool().is_some()
        {
            Ok(())
        } else {
            Err(HeapError::TypeMismatch)
        };
    }
    NodeProperty::validate(heap, index, value)
}
fn validation<T>(result: Result<T, AdapterError>) -> Result<(), HeapError> {
    result.map(|_| ()).map_err(|error| match error {
        AdapterError::Heap(error) => error,
        _ => HeapError::TypeMismatch,
    })
}
fn validate_number(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(number(value))
}
fn validate_dimension(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(dimension(value))
}
fn invert(value: Value) -> Result<f64, AdapterError> {
    let value = number(value)?;
    if !(0. ..=1.).contains(&value) {
        return Err(AdapterError::Input("invert must be between zero and one"));
    }
    Ok(value)
}
fn validate_invert(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(invert(value))
}
fn blend(heap: &Heap, value: Value) -> Result<reel_ui::frame::BlendMode, HeapError> {
    use reel_ui::frame::BlendMode;
    use reel_ui::peniko::{Compose, Mix};
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 8 {
        return Err(HeapError::TypeMismatch);
    }
    let mut bytes = [0; 8];
    heap.read_bytes(value, 0, &mut bytes[..info.len as usize])?;
    Ok(match &bytes[..info.len as usize] {
        b"normal" => BlendMode::default(),
        b"add" => reel_ui::peniko::BlendMode::new(Mix::Normal, Compose::PlusLighter).into(),
        b"subtract" => BlendMode::Subtract,
        b"multiply" => Mix::Multiply.into(),
        b"screen" => Mix::Screen.into(),
        _ => return Err(HeapError::TypeMismatch),
    })
}
fn validate_blend(heap: &Heap, value: Value) -> Result<(), HeapError> {
    blend(heap, value).map(|_| ())
}
fn validate_opacity(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(opacity(value))
}
fn mask_progress(value: Value) -> Result<f64, AdapterError> {
    let value = number(value)?;
    if !(0. ..=1.).contains(&value) {
        return Err(AdapterError::Input(
            "mask_progress must be between zero and one",
        ));
    }
    Ok(value)
}
pub(super) fn mask_offset(value: Value) -> Result<f64, AdapterError> {
    let value = number(value)?;
    if !(value as f32).is_finite() {
        return Err(AdapterError::Input(
            "mask offset must be representable as finite f32",
        ));
    }
    Ok(value)
}
fn validate_mask_offset(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(mask_offset(value))
}
fn validate_mask_progress(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(mask_progress(value))
}
pub(super) fn validate_mask(heap: &Heap, value: Value) -> Result<(), HeapError> {
    if value.tag() == ValueTag::Null {
        return Ok(());
    }
    let info = heap.info(value)?;
    if info.kind == ObjectKind::String && info.len != 0 {
        Ok(())
    } else {
        super::masks::validate(heap, value)
    }
}
fn mask_handle(
    heap: &Heap,
    fields: Value,
    offset: usize,
    selected: Value,
) -> Result<Option<HeapHandle>, HeapError> {
    let value = overrides::get(heap, fields, offset, selected, NodeProperty::Mask.index())?;
    if value.tag() == ValueTag::Null {
        Ok(None)
    } else {
        heap.handle(value).map(Some)
    }
}
fn font_weight(value: Value) -> Result<f64, AdapterError> {
    let weight = number(value)?;
    if (1. ..=1000.).contains(&weight) {
        Ok(weight)
    } else {
        Err(AdapterError::Input(
            "font_weight must be between 1 and 1000",
        ))
    }
}
fn validate_font_weight(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(font_weight(value))
}
fn text_spacing(value: Value) -> Result<f64, AdapterError> {
    let spacing = number(value)?;
    if (spacing as f32).is_finite() {
        Ok(spacing)
    } else {
        Err(AdapterError::Input(
            "text spacing must be representable as finite f32",
        ))
    }
}
fn validate_text_spacing(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(text_spacing(value))
}
fn validate_nonnegative(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(nonnegative(value))
}
fn validate_color(heap: &Heap, value: Value) -> Result<(), HeapError> {
    validation(color(heap, value))
}
fn validate_background(heap: &Heap, value: Value) -> Result<(), HeapError> {
    if value.tag() == ValueTag::Null {
        Ok(())
    } else {
        validate_color(heap, value)
    }
}
fn validate_text_fade(kind: &NodeKind, props: &NodeProps) -> Result<(), AdapterError> {
    if matches!(kind, NodeKind::TextInput { .. }) && props.text_gradient.is_some() {
        return Err(AdapterError::Input(
            "text_gradient applies to text and control captions",
        ));
    }
    let source = match kind {
        NodeKind::Text(text) => Some(text.as_ref()),
        NodeKind::Button { label, .. }
        | NodeKind::Toggle { label, .. }
        | NodeKind::Radio { label, .. } => label.as_deref(),
        _ => None,
    };
    if source.is_some_and(|text| {
        props.reveal_fade_from < text.len() && !text.is_char_boundary(props.reveal_fade_from)
    }) {
        return Err(AdapterError::Input(
            "reveal_fade_from must be a UTF-8 byte boundary",
        ));
    }
    Ok(())
}

fn validate_optional_number(heap: &Heap, value: Value) -> Result<(), HeapError> {
    if value.tag() == ValueTag::Null {
        Ok(())
    } else {
        validate_number(heap, value)
    }
}
fn validate_byte_index(_: &Heap, value: Value) -> Result<(), HeapError> {
    if value.as_int().is_some_and(|n| usize::try_from(n).is_ok()) {
        Ok(())
    } else {
        Err(HeapError::TypeMismatch)
    }
}
fn validate_reveal(_: &Heap, value: Value) -> Result<(), HeapError> {
    if value.tag() == ValueTag::Null || value.as_int().is_some_and(|n| usize::try_from(n).is_ok()) {
        Ok(())
    } else {
        Err(HeapError::TypeMismatch)
    }
}

fn text_align(heap: &Heap, value: Value) -> Result<reel_ui::TextAlign, HeapError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 7 {
        return Err(HeapError::TypeMismatch);
    }
    let mut bytes = [0; 7];
    for i in 0..info.len {
        heap.read_bytes(value, i, &mut bytes[i as usize..i as usize + 1])?;
    }
    Ok(match &bytes[..info.len as usize] {
        b"start" => reel_ui::TextAlign::Start,
        b"end" => reel_ui::TextAlign::End,
        b"left" => reel_ui::TextAlign::Left,
        b"right" => reel_ui::TextAlign::Right,
        b"center" => reel_ui::TextAlign::Center,
        b"justify" => reel_ui::TextAlign::Justify,
        _ => return Err(HeapError::TypeMismatch),
    })
}
fn validate_text_align(heap: &Heap, value: Value) -> Result<(), HeapError> {
    text_align(heap, value).map(|_| ())
}
fn shrink_text(heap: &Heap, value: Value) -> Result<bool, HeapError> {
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 6 {
        return Err(HeapError::TypeMismatch);
    }
    let mut bytes = [0; 6];
    heap.read_bytes(value, 0, &mut bytes[..info.len as usize])?;
    match &bytes[..info.len as usize] {
        b"none" => Ok(false),
        b"shrink" => Ok(true),
        _ => Err(HeapError::TypeMismatch),
    }
}
fn validate_text_fit(heap: &Heap, value: Value) -> Result<(), HeapError> {
    shrink_text(heap, value).map(|_| ())
}
fn stretch(heap: &Heap, value: Value) -> Result<reel_ui::stage::Stretch, HeapError> {
    use reel_ui::stage::Stretch;
    let info = heap.info(value)?;
    if info.kind != ObjectKind::String || info.len > 4 {
        return Err(HeapError::TypeMismatch);
    }
    let mut bytes = [0; 4];
    heap.read_bytes(value, 0, &mut bytes[..info.len as usize])?;
    match &bytes[..info.len as usize] {
        b"none" => Ok(Stretch::None),
        b"x" => Ok(Stretch::X),
        b"y" => Ok(Stretch::Y),
        b"both" => Ok(Stretch::Both),
        _ => Err(HeapError::TypeMismatch),
    }
}
fn validate_stretch(heap: &Heap, value: Value) -> Result<(), HeapError> {
    stretch(heap, value).map(|_| ())
}
fn read_props(
    heap: &Heap,
    fields: Value,
    offset: usize,
    selected: Value,
    read_limit: usize,
    previous: Option<(Option<HeapHandle>, &NodeProps)>,
    mask_origin: Option<Arc<str>>,
) -> Result<NodeProps, AdapterError> {
    let get = |property: NodeProperty| -> Result<Value, AdapterError> {
        Ok(overrides::get(
            heap,
            fields,
            offset,
            selected,
            property.index(),
        )?)
    };
    let background = get(NodeProperty::Background)?;
    validate_callback(heap, get(NodeProperty::OnInteraction)?)?;
    let mask = get(NodeProperty::Mask)?;
    validate_mask(heap, mask)?;
    let mask = if mask.tag() == ValueTag::Null {
        None
    } else if let Some((Some(handle), props)) = previous
        && handle == heap.handle(mask)?
    {
        props.mask.clone()
    } else {
        Some(super::masks::project(heap, mask, read_limit)?)
    };
    let default_font;
    let font = if let Some((_, props)) = previous {
        &props.font
    } else {
        default_font = NodeProps::default().font;
        &default_font
    };
    Ok(NodeProps {
        flex: nonnegative(get(NodeProperty::Flex)?)?,
        x: number(get(NodeProperty::X)?)?,
        y: number(get(NodeProperty::Y)?)?,
        width: dimension(get(NodeProperty::Width)?)?,
        height: dimension(get(NodeProperty::Height)?)?,
        stretch: stretch(heap, get(NodeProperty::Stretch)?)?,
        max_width: dimension(get(NodeProperty::MaxWidth)?)?,
        max_height: dimension(get(NodeProperty::MaxHeight)?)?,
        align_x: opacity(get(NodeProperty::AlignX)?)?,
        align_y: opacity(get(NodeProperty::AlignY)?)?,
        opacity: opacity(get(NodeProperty::Opacity)?)?,
        invert: invert(get(NodeProperty::Invert)?)?,
        blend: blend(heap, get(NodeProperty::Blend)?)?,
        visible: boolean(get(NodeProperty::Visible)?)?,
        capture_included: boolean(get(NodeProperty::CaptureIncluded)?)?,
        enabled: boolean(get(NodeProperty::Enabled)?)?,
        modal: boolean(get(NodeProperty::Modal)?)?,
        clip: boolean(get(NodeProperty::Clip)?)?,
        decorated: boolean(get(NodeProperty::Decorated)?)?,
        reveal: get(NodeProperty::Reveal)?.as_int().map(|v| v as usize),
        reveal_fade: if get(NodeProperty::RevealFade)?.tag() == ValueTag::Null {
            None
        } else {
            Some(number(get(NodeProperty::RevealFade)?)?)
        },
        reveal_fade_from: get(NodeProperty::RevealFadeFrom)?.as_int().unwrap() as usize,
        reveal_fade_step: nonnegative(get(NodeProperty::RevealFadeStep)?)?,
        gap: nonnegative(get(NodeProperty::Gap)?)?,
        padding: nonnegative(get(NodeProperty::Padding)?)?,
        font_size: nonnegative(get(NodeProperty::FontSize)?)?,
        font_weight: font_weight(get(NodeProperty::FontWeight)?)?,
        letter_spacing: text_spacing(get(NodeProperty::LetterSpacing)?)?,
        word_spacing: text_spacing(get(NodeProperty::WordSpacing)?)?,
        font: text::read_shared_string(heap, get(NodeProperty::Font)?, read_limit, font)?,
        text_align: text_align(heap, get(NodeProperty::TextAlign)?)?,
        text_wrap: boolean(get(NodeProperty::TextWrap)?)?,
        line_height: nonnegative(get(NodeProperty::LineHeight)?)?,
        text_fit: if shrink_text(heap, get(NodeProperty::TextFit)?)? {
            reel_ui::core::TextFit::Shrink {
                min_font_size: nonnegative(get(NodeProperty::MinFontSize)?)? as f32,
            }
        } else {
            reel_ui::core::TextFit::None
        },
        text_outline: nonnegative(get(NodeProperty::TextOutline)?)?,
        text_outline_color: color(heap, get(NodeProperty::TextOutlineColor)?)?,
        text_shadow_x: number(get(NodeProperty::TextShadowX)?)?,
        text_shadow_y: number(get(NodeProperty::TextShadowY)?)?,
        text_shadow_color: color(heap, get(NodeProperty::TextShadowColor)?)?,
        color: color(heap, get(NodeProperty::Color)?)?,
        text_gradient: super::gradients::project(heap, get(NodeProperty::TextGradient)?)?,
        background: if background.tag() == ValueTag::Null {
            None
        } else {
            Some(color(heap, background)?)
        },
        border_width: nonnegative(get(NodeProperty::BorderWidth)?)?,
        border_color: color(heap, get(NodeProperty::BorderColor)?)?,
        corner_radius: nonnegative(get(NodeProperty::CornerRadius)?)?,
        rotation: number(get(NodeProperty::Rotation)?)?,
        scale_x: number(get(NodeProperty::ScaleX)?)?,
        scale_y: number(get(NodeProperty::ScaleY)?)?,
        anchor_x: number(get(NodeProperty::AnchorX)?)?,
        anchor_y: number(get(NodeProperty::AnchorY)?)?,
        mask,
        mask_origin,
        mask_progress: mask_progress(get(NodeProperty::MaskProgress)?)?,
        mask_softness: nonnegative(get(NodeProperty::MaskSoftness)?)?,
        mask_invert: boolean(get(NodeProperty::MaskInvert)?)?,
        mask_x: mask_offset(get(NodeProperty::MaskX)?)?,
        mask_y: mask_offset(get(NodeProperty::MaskY)?)?,
    })
}
