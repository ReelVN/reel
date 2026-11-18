//! A scrollbar borrows a viewport and owns only its stable visual content slots.
use super::*;
use reel_ui::{
    stage::{ControlValue, NodeChange},
    widgets::{ScrollAxis, ScrollBarView},
};

#[derive(aside_host::Properties)]
pub(in crate::aside) enum ScrollBarField {
    /// Existing scroll viewport. This reference is borrowed; retirement of the bar leaves it alive.
    #[aside(name="viewport",ty=Any,doc_type=aside_host::source::named("reel::types::ScrollNode"),readonly)]
    Viewport,
    /// Controlled axis: vertical or horizontal. A two-axis viewport may have one bar for each axis.
    #[aside(name="axis",ty=String,default="vertical",readonly,validate=validate_axis)]
    Axis,
    /// Minimum proportional thumb length, in logical pixels; constrained to the track.
    #[aside(name="min_thumb_size",ty=Any,doc_type=aside_host::source::number(),default=16,readonly,validate=validate_nonnegative)]
    MinThumbSize,
    /// Fixed thumb length, or null for the viewport/content ratio.
    #[aside(name="thumb_size",ty=Any,doc_type=aside_host::source::nullable(aside_host::source::number()),default=null,readonly,validate=validate_thumb)]
    ThumbSize,
    /// Keyboard/controller and accessibility increment in logical scroll pixels.
    #[aside(name="step",ty=Any,doc_type=aside_host::source::number(),default=40,readonly,validate=validate_positive)]
    Step,
    /// Space before the thumb's travel track, in logical pixels.
    #[aside(name="inset_start",ty=Any,doc_type=aside_host::source::number(),default=0,readonly,validate=validate_nonnegative)]
    InsetStart,
    /// Space after the thumb's travel track, in logical pixels.
    #[aside(name="inset_end",ty=Any,doc_type=aside_host::source::number(),default=0,readonly,validate=validate_nonnegative)]
    InsetEnd,
}
fn axis(h: &Heap, v: Value) -> Result<ScrollAxis, HeapError> {
    let word = super::super::text::read_string(h, v, 10).map_err(|_| HeapError::TypeMismatch)?;
    match word.as_str() {
        "vertical" => Ok(ScrollAxis::Vertical),
        "horizontal" => Ok(ScrollAxis::Horizontal),
        _ => Err(HeapError::TypeMismatch),
    }
}
fn validate_axis(h: &Heap, v: Value) -> Result<(), HeapError> {
    axis(h, v).map(|_| ())
}
fn validate_positive(_: &Heap, v: Value) -> Result<(), HeapError> {
    positive(v).map(|_| ()).map_err(|_| HeapError::TypeMismatch)
}
fn validate_thumb(_h: &Heap, v: Value) -> Result<(), HeapError> {
    if v.tag() == ValueTag::Null {
        Ok(())
    } else {
        validate_positive(_h, v)
    }
}
pub(super) fn parts_offset() -> u32 {
    (ScrollBarField::PROPERTIES.len()
        + NodeProperty::PROPERTIES.len()
        + interaction::Field::NAMES.len()) as u32
}
pub(super) fn validate(h: &Heap, i: u32, v: Value) -> Result<(), HeapError> {
    let prefix = ScrollBarField::PROPERTIES.len() as u32;
    if i < prefix {
        ScrollBarField::validate(h, i, v)
    } else if i >= parts_offset() {
        Err(HeapError::ReadOnly)
    } else {
        validate_layout_property(h, i - prefix, v)
    }
}
fn field(h: &Heap, fields: Value, index: usize) -> Result<Value, HeapError> {
    h.table_entry(fields, index as u32).map(|p| p.1)
}
pub(in crate::aside::stage) fn target(h: &Heap, fields: Value) -> Result<Value, HeapError> {
    field(h, fields, ScrollBarField::Viewport.index())
}
pub(super) fn view(h: &Heap, fields: Value) -> Result<ScrollBarView, AdapterError> {
    let value = |p: ScrollBarField| field(h, fields, p.index());
    let axis = axis(h, value(ScrollBarField::Axis)?)?;
    let fixed = value(ScrollBarField::ThumbSize)?;
    let mut view = ScrollBarView {
        axis,
        min_thumb_size: nonnegative(value(ScrollBarField::MinThumbSize)?)?,
        thumb_size: if fixed.tag() == ValueTag::Null {
            None
        } else {
            Some(positive(fixed)?)
        },
        step: positive(value(ScrollBarField::Step)?)?,
        inset_start: nonnegative(value(ScrollBarField::InsetStart)?)?,
        inset_end: nonnegative(value(ScrollBarField::InsetEnd)?)?,
        ..Default::default()
    };
    let viewport = target(h, fields)?;
    let target_fields = match h.host_fields(viewport) {
        Ok(v) => v,
        Err(HeapError::RetiredHost | HeapError::InvalidHandle) => return Ok(view),
        Err(e) => return Err(e.into()),
    };
    let horizontal = axis == ScrollAxis::Horizontal;
    if !horizontal {
        view.snap = scroll::snap(field(h, target_fields, scroll::ScrollField::SnapY.index())?)?;
    }
    let metrics = scroll::ScrollField::PROPERTIES.len()
        + NodeProperty::PROPERTIES.len()
        + interaction::Field::NAMES.len();
    view.viewport = nonnegative(field(h, target_fields, metrics + usize::from(!horizontal))?)?;
    view.content = nonnegative(field(
        h,
        target_fields,
        metrics + 2 + usize::from(!horizontal),
    )?)?;
    view.live = true;
    view.value = number(h.host_property(PropertyId {
        host: viewport,
        field: if horizontal {
            scroll::ScrollField::X.index()
        } else {
            scroll::ScrollField::Y.index()
        } as u32,
    })?)?
    .clamp(0., view.maximum());
    Ok(view)
}
pub(super) fn kind(h: &Heap, fields: Value) -> Result<NodeKind, AdapterError> {
    Ok(NodeKind::ScrollBar(view(h, fields)?))
}

impl StageBinding {
    pub(super) fn create_scrollbar(
        &mut self,
        vm: &mut Vm,
        owner: RootOwner,
        args: &[Value],
        limit: usize,
        origin: Option<String>,
    ) -> Result<Value, AdapterError> {
        let viewport = self.id(vm.heap(), args[0])?;
        if !matches!(
            self.scene.node(viewport).map(|n| &n.kind),
            Some(NodeKind::Scroll { .. })
        ) {
            return Err(AdapterError::Input(
                "scrollbar viewport must be a ScrollNode",
            ));
        }
        let root = self.create(
            vm,
            ("reel::scrollbar", "reel::types::ScrollBarNode"),
            args,
            limit,
            origin,
        )?;
        let built = (|| {
            for (index, name, color) in [(0, "track", "#30343b"), (1, "thumb", "#a0a8b4")] {
                let wrapper = self.scrollbar_part(vm, owner, root, None, limit)?;
                let content = self.scrollbar_part(vm, owner, wrapper, Some(color), limit)?;
                let key = self.property_key(vm.heap_mut(), name)?;
                let fields = vm.heap().host_fields(root)?;
                vm.heap_mut()
                    .table_store(fields, parts_offset() + index, key, content)?;
            }
            let id = self.id(vm.heap(), root)?;
            self.sync_tree(vm.heap_mut(), id, limit)?;
            Ok(root)
        })();
        if built.is_err() {
            self.discard_unmounted(vm.heap_mut(), root)?;
        }
        built
    }
    fn scrollbar_part(
        &mut self,
        vm: &mut Vm,
        owner: RootOwner,
        parent: Value,
        color: Option<&str>,
        limit: usize,
    ) -> Result<Value, AdapterError> {
        let mut args = defaults::layout(vm.heap_mut())?;
        args[NodeProperty::Stretch.index()] = vm.heap_mut().alloc_string("both")?;
        if let Some(color) = color {
            args[NodeProperty::Background.index()] = vm.heap_mut().alloc_string(color)?;
        }
        args.push(Value::NULL);
        let node = self.dispatch(vm, owner, "reel::stack", &args, limit, None)?;
        if let Err(error) = self.add_node(vm.heap_mut(), parent, node, None) {
            self.discard_unmounted(vm.heap_mut(), node)?;
            return Err(error);
        }
        Ok(node)
    }
    pub(in crate::aside) fn scrollbar_offset(
        &mut self,
        h: &mut Heap,
        bar: Value,
        amount: f64,
        relative: bool,
    ) -> Result<Value, AdapterError> {
        if !amount.is_finite() {
            return Err(HeapError::TypeMismatch.into());
        }
        let id = self.id(h, bar)?;
        if self.handles[&id].shape != snapshot::Shape::ScrollBar {
            return Err(AdapterError::Input("expected ScrollBarNode"));
        }
        let fields = h.host_fields(bar)?;
        let view = view(h, fields)?;
        if !view.live {
            return Err(HeapError::RetiredHost.into());
        }
        let offset = view.clamp(if relative {
            view.shifted(amount)
        } else {
            amount
        });
        let property = PropertyId {
            host: target(h, fields)?,
            field: if view.axis == ScrollAxis::Horizontal {
                scroll::ScrollField::X.index()
            } else {
                scroll::ScrollField::Y.index()
            } as u32,
        };
        h.write_host_property(property, Value::float(offset), true)?;
        Ok(Value::float(offset))
    }
    pub(in crate::aside) fn change_scrollbar(
        &mut self,
        h: &mut Heap,
        change: &NodeChange,
    ) -> Result<Option<(Value, Value, Value)>, AdapterError> {
        let Some(node) = self.scene.node(change.node) else {
            return Ok(None);
        };
        let NodeKind::ScrollBar(shown) = node.kind else {
            return Ok(None);
        };
        if !self.mounted_in_scope(h, change.node, self.modal_scope(h)?)? {
            return Ok(None);
        }
        let bar = self.value(h, change.node)?;
        let current = view(h, h.host_fields(bar)?)?;
        let ControlValue::Number(value) = change.value else {
            return Ok(None);
        };
        if !shown.same_geometry(current)
            || current.range() != change.range
            || !current.range().is_some_and(|r| r.contains(value))
        {
            return Ok(None);
        }
        let value = self.scrollbar_offset(h, bar, value, false)?;
        Ok(Some((bar, Value::NULL, value)))
    }
}

pub(in crate::aside) fn contracts() -> Vec<registry::NativeContract> {
    use aside_vm::bytecode::{BuiltinType, Effects};
    ["scroll_by", "scroll_to"]
        .into_iter()
        .map(|name| registry::NativeContract {
            symbol: format!("reel::scrollbar::{name}"),
            abi_version: 1,
            parameters: vec![
                registry::NativeParameter {
                    name: "bar".into(),
                    ty: BuiltinType::Any,
                    default: None,
                },
                registry::NativeParameter {
                    name: if name == "scroll_by" {
                        "delta"
                    } else {
                        "offset"
                    }
                    .into(),
                    ty: BuiltinType::Any,
                    default: None,
                },
            ],
            result: BuiltinType::Any,
            effects: Effects(Effects::HOST | Effects::WRITE),
            required_capabilities: vec!["reel.stage".into()],
            rest: None,
            standard_derived_commit: false,
            read_only_leaf: None,
        })
        .collect()
}
pub(in crate::aside) fn describe(api: &mut aside_host::source::Api) {
    use aside_host::source::{Member, named, number};
    let name = "reel::types::ScrollBarNode";
    for field in ["track", "thumb"] {
        api.types
            .iter_mut()
            .find(|t| t.name == name)
            .unwrap()
            .members
            .push(Member::field(
                name,
                field,
                named("reel::types::Node"),
                None,
                "Stable authored content slot; its private wrapper controls layout.",
                None,
            ));
    }
    for method in ["scroll_by", "scroll_to"] {
        let symbol = format!("reel::scrollbar::{method}");
        api.refine(
            &symbol,
            number(),
            &[
                ("bar", named(name)),
                (
                    if method == "scroll_by" {
                        "delta"
                    } else {
                        "offset"
                    },
                    number(),
                ),
            ],
        );
        let member = Member::method(api.native(&symbol).clone().bound(method));
        api.types
            .iter_mut()
            .find(|t| t.name == name)
            .unwrap()
            .members
            .push(member);
    }
}
aside_host::native_documentation! {
    pub(in crate::aside) fn documentation {
        /// Create a native scrollbar linked to an existing ScrollNode. The bar owns
        /// stable track/thumb content slots; it borrows the viewport. Add artwork to
        /// either slot, using stretch: "both" to fill the available rectangle.
        /// Null thumb_size follows viewport/content proportion; a number fixes its
        /// logical length. Insets reserve space at the ends of its travel track.
        /// Track clicks center the thumb. Dragging follows accepted measurements as
        /// virtual rows refine the scroll range. Arrows,
        /// controller navigation, PageUp/PageDown, Home/End and accessibility range
        /// actions use the same target offset. No per-move script callback is started.
        ///
        /// ```aside
        /// fn main() {
        /// let viewport = scroll(width: 280, height: 120) {
        ///     column() { for i in 0 .. 20 { text(f"Line {i}"); } }
        /// };
        /// let bar = scrollbar(viewport, width: 16, height: 120);
        /// bar.track.background = "#28303a";
        /// bar.thumb.background = "#9daabe";
        /// stage.replace(row(gap: 8) { viewport; bar; });
        /// }
        /// ```
        "reel::scrollbar";
        /// Move the linked viewport by delta logical pixels on the bar's axis.
        /// Apply viewport scroll stops, clamp to the range, and return the offset.
        "reel::scrollbar::scroll_by";
        /// Set the linked viewport's offset on this axis, applying its scroll
        /// stops and measured range. Return the resulting offset.
        "reel::scrollbar::scroll_to";
    }
}
