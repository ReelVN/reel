//! Save node identity and constructor semantics; derive presentation from VM fields.
use super::*;
use aside_vm::snapshot::{SnapshotError, SnapshotMap, SnapshotRegistry};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::aside) enum Shape {
    Text,
    TextInput,
    Button,
    Slider,
    Switch,
    Checkbox,
    Radio,
    Input,
    Image,
    Video,
    Particles,
    Camera,
    Scroll,
    Row,
    Column,
    Stack,
    VirtualList,
    Cursor,
    TextArea,
    Grid,
    Popup,
}

impl Shape {
    pub(super) fn from_symbol(symbol: &str) -> Self {
        match symbol {
            "reel::text" => Self::Text,
            "reel::text_input" => Self::TextInput,
            "reel::text_area" => Self::TextArea,
            "reel::button" => Self::Button,
            "reel::slider" => Self::Slider,
            "reel::toggle" => Self::Switch,
            "reel::checkbox" => Self::Checkbox,
            "reel::radio" => Self::Radio,
            "reel::input" => Self::Input,
            "reel::image" => Self::Image,
            "reel::video" => Self::Video,
            "reel::particles" => Self::Particles,
            "reel::camera" => Self::Camera,
            "reel::scroll" => Self::Scroll,
            "reel::popup" => Self::Popup,
            "reel::grid" => Self::Grid,
            "reel::row" => Self::Row,
            "reel::column" => Self::Column,
            "reel::stack" => Self::Stack,
            "reel::cursor" => Self::Cursor,
            _ => unreachable!("registered stage constructor"),
        }
    }
    fn prefix(self) -> usize {
        match self {
            Self::Text => 2,
            Self::Button => appearance::ButtonField::PROPERTIES.len(),
            Self::Image | Self::Video => registry::IMAGE_PREFIX.len(),
            Self::TextInput | Self::TextArea => text_input::TextInputField::PROPERTIES.len(),
            Self::Slider => controls::SliderField::PROPERTIES.len(),
            Self::Switch | Self::Checkbox => controls::ToggleField::PROPERTIES.len(),
            Self::Radio => controls::RadioField::PROPERTIES.len(),
            Self::Input => input::InputField::PROPERTIES.len(),
            Self::Particles => super::super::particles::PREFIX.len(),
            Self::Camera => camera::CameraField::PROPERTIES.len(),
            Self::Scroll => scroll::ScrollField::PROPERTIES.len(),
            Self::Popup => popup::PopupField::PROPERTIES.len(),
            Self::Row | Self::Column => flow::FlowField::PROPERTIES.len(),
            Self::Grid => flow::GridField::PROPERTIES.len(),
            Self::Stack | Self::VirtualList | Self::Cursor => 0,
        }
    }
    pub(super) fn project(
        self,
        heap: &Heap,
        fields: Value,
        origin: Option<String>,
        limit: usize,
    ) -> Result<NodeKind, AdapterError> {
        let get = |index| Ok::<_, HeapError>(heap.table_entry(fields, index)?.1);
        Ok(match self {
            Self::Text => NodeKind::Text(super::super::rich_text::read(heap, get(0)?, limit)?),
            Self::TextInput | Self::TextArea => {
                text_input::kind(heap, fields, limit, self == Self::TextArea)?
            }
            Self::Button => button_kind(heap, get(0)?, get(1)?, get(2)?, limit)?,
            Self::Slider => controls::slider_kind(heap, fields)?,
            Self::Switch | Self::Checkbox => controls::toggle_kind(
                heap,
                fields,
                limit,
                if matches!(self, Self::Checkbox) {
                    reel_ui::stage::ToggleAppearance::Checkbox
                } else {
                    reel_ui::stage::ToggleAppearance::Switch
                },
            )?,
            Self::Radio => controls::radio_kind(heap, fields, 0, limit)?,
            Self::Input => input::kind(heap, fields, input::read_bindings(heap, get(0)?)?)?,
            Self::Image | Self::Video => NodeKind::Image {
                source: if self == Self::Video {
                    let source = text::read_string(heap, get(0)?, limit)?;
                    if source.is_empty() {
                        return Err(AdapterError::Input("video source must not be empty"));
                    }
                    reel_ui::stage::ImageSource::empty()
                } else {
                    read_source(heap, get(0)?, limit)?
                },
                fit: read_fit(heap, get(1)?)?,
                slice: read_slice(heap, get(2)?)?,
                slice_scale: nonnegative(get(3)?)?,
                repeat: read_image_repeat(heap, get(4)?, get(2)?)?,
                tile_scale: positive(get(5)?)?,
                corner_colors: read_corner_colors(heap, get(6)?)?,
                slice_center: boolean(get(7)?)?,
                adjustment: read_image_adjustment(heap, fields)?,
                source_rect: read_source_rect(heap, get(11)?)?,
                alpha_layout: read_alpha_layout(heap, get(12)?)?,
                origin,
            },
            Self::Particles => {
                let args = (0..self.prefix() as u32)
                    .map(get)
                    .collect::<Result<Vec<_>, _>>()?;
                super::super::particles::kind(heap, &args, origin, limit)?
            }
            Self::Camera => camera::kind(heap, fields)?,
            Self::Scroll => scroll::kind(heap, fields)?,
            Self::Popup => popup::kind(heap, fields)?,
            Self::Row | Self::Column => flow::flow_kind(heap, fields, self == Self::Row)?,
            Self::Grid => flow::grid_kind(heap, fields)?,
            Self::Stack => NodeKind::Stack,
            Self::Cursor => NodeKind::Cursor,
            Self::VirtualList => unreachable!("virtual list projects its VM-owned state"),
        })
    }
    pub(super) fn validator(self) -> aside_vm::HostPropertyValidator {
        match self {
            Self::Text => |h, _, i, v| validate_text_property(h, i, v),
            Self::TextInput => |h, _, i, v| text_input::validate(h, i, v, false),
            Self::TextArea => |h, _, i, v| text_input::validate(h, i, v, true),
            Self::Button => |h, _, i, v| validate_button_property(h, i, v),
            Self::Slider => controls::validate_slider,
            Self::Switch | Self::Checkbox => |h, _, i, v| controls::validate_toggle(h, i, v),
            Self::Radio => |h, _, i, v| controls::validate_radio(h, i, v),
            Self::Input => |h, _, i, v| input::validate(h, i, v),
            Self::Image => |h, _, i, v| validate_image_property(h, i, v),
            Self::Video => |h, _, i, v| {
                if i == 0 {
                    Err(HeapError::ReadOnly)
                } else {
                    validate_image_property(h, i, v)
                }
            },
            Self::Particles => |h, host, i, v| super::super::particles::validate(h, host, i, v),
            Self::Camera => |h, _, i, v| camera::validate(h, i, v),
            Self::Scroll => |h, _, i, v| scroll::validate(h, i, v),
            Self::Popup => |h, _, i, v| popup::validate(h, i, v),
            Self::Row | Self::Column => |h, _, i, v| flow::validate_flow(h, i, v),
            Self::Grid => |h, _, i, v| flow::validate_grid(h, i, v),
            Self::Stack | Self::VirtualList | Self::Cursor => {
                |h, _, i, v| validate_layout_property(h, i, v)
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(in crate::aside) struct SavedNode {
    id: u64,
    handle: HeapHandle,
    shape: Shape,
    origin: Option<String>,
}

impl StageBinding {
    pub(in crate::aside) fn register_snapshots(
        registry: &mut SnapshotRegistry,
    ) -> Result<(), SnapshotError> {
        for (name, shape) in [
            ("reel.text", Shape::Text),
            ("reel.text_input", Shape::TextInput),
            ("reel.text_area", Shape::TextArea),
            ("reel.button", Shape::Button),
            ("reel.slider", Shape::Slider),
            ("reel.toggle", Shape::Switch),
            ("reel.radio", Shape::Radio),
            ("reel.input", Shape::Input),
            ("reel.image", Shape::Image),
            ("reel.video", Shape::Video),
            ("reel.particles", Shape::Particles),
            ("reel.camera", Shape::Camera),
            ("reel.scroll", Shape::Scroll),
            ("reel.popup", Shape::Popup),
            ("reel.flow", Shape::Row),
            ("reel.grid", Shape::Grid),
            ("reel.layout", Shape::Stack),
        ] {
            registry.register_validator(name, 1, shape.validator())?;
        }
        registry.register_validator("reel.override", 1, overrides::validate)?;
        Ok(())
    }
    pub(in crate::aside) fn snapshot_root(&self) -> Option<RootToken> {
        self.root
    }

    pub(in crate::aside) fn snapshot_nodes(&self) -> impl Iterator<Item = SavedNode> + '_ {
        self.scene.nodes().map(|node| SavedNode {
            id: node.id.0,
            handle: self.handles[&node.id].handle,
            shape: self.handles[&node.id].shape,
            origin: node.props.appearance_origin.as_deref().map(str::to_owned),
        })
    }
    pub(in crate::aside) fn restore_node(
        &mut self,
        heap: &mut Heap,
        map: &SnapshotMap,
        saved: SavedNode,
        limit: usize,
    ) -> Result<Option<NodeId>, AdapterError> {
        // Detached stage entries do not keep otherwise unreachable author nodes alive.
        let Ok(handle) = map.handle(saved.handle) else {
            return Ok(None);
        };
        let value = heap.import_handle(handle)?;
        let fields = heap.host_fields(value)?;
        let appearance_origin = saved.origin.as_deref().map(Arc::from);
        let restored_input = if saved.shape == Shape::Input {
            Some(input::restore(heap, value, fields)?)
        } else {
            None
        };
        let (mut kind, input) = if let Some((kind, state)) = restored_input {
            (kind, Some(state))
        } else {
            (
                if saved.shape == Shape::VirtualList {
                    NodeKind::VirtualList(super::super::lists::virtualized::project(heap, value)?)
                } else {
                    saved.shape.project(heap, fields, saved.origin, limit)?
                },
                None,
            )
        };
        controls::project_group(heap, value, &mut kind)?;
        text_input::project(heap, value, &mut kind)?;
        let selected = overrides::selection(heap, value)?;
        let mut props = read_props(
            heap,
            fields,
            saved.shape.prefix(),
            selected,
            limit,
            None,
            appearance_origin,
            &self.effect_products,
            &mut self.effect_projection,
        )?;
        overrides::apply_offsets(heap, value, &mut props)?;
        let mask_source = mask_handle(heap, fields, saved.shape.prefix(), selected)?;
        let effect_source = effect_stamp(heap, fields, saved.shape.prefix(), selected)?;
        let overrides = if selected.tag() == ValueTag::Null {
            None
        } else {
            let mut cache = overrides::Cache::new(heap, overrides::offset_count(heap, value)?)?;
            cache.accept(heap, value)?;
            Some(cache)
        };
        let interaction_offset = (saved.shape.prefix() + NodeProperty::PROPERTIES.len()) as u32;
        interaction::publish(
            heap,
            value,
            interaction_offset,
            reel_ui::stage::InteractionState {
                disabled: !props.enabled,
                ..Default::default()
            },
        )?;
        let id = NodeId(saved.id);
        if heap.host_payload(value, 0)?.as_int() != Some(saved.id as i64) {
            return Err(AdapterError::Input("saved node identity"));
        }
        self.scene.restore_node(id, kind, props)?;
        self.handles.insert(
            id,
            NodeBinding {
                shape: saved.shape,
                handle,
                interaction_offset,
                interaction_listening: heap
                    .table_entry(fields, interaction::callback_field(interaction_offset))?
                    .1
                    .tag()
                    != ValueTag::Null,
                fields_version: heap.info(fields)?.content_version,
                projection_dirty: false,
                overrides,
                mask_source,
                effect_source,
                input,
                text_history: None,
                text_source: if matches!(
                    saved.shape,
                    Shape::Text | Shape::TextInput | Shape::TextArea | Shape::Image | Shape::Video
                ) {
                    Some(heap.handle(heap.table_entry(fields, 0)?.1)?)
                } else {
                    None
                },
                text_listening: matches!(saved.shape, Shape::Text)
                    && heap.table_entry(fields, 1)?.1.tag() != ValueTag::Null,
                radio_option: if matches!(saved.shape, Shape::Radio) {
                    Some(
                        heap.table_entry(fields, controls::RadioField::Option.index() as u32)?
                            .1,
                    )
                } else {
                    None
                },
            },
        );
        Ok(Some(id))
    }
    pub(in crate::aside) fn restore_child(
        &mut self,
        heap: &Heap,
        parent: NodeId,
        index: u32,
    ) -> Result<bool, AdapterError> {
        let value = self.value(heap, parent)?;
        let children = heap.host_payload(value, 1)?;
        if index == heap.sequence_len(children)? {
            return Ok(false);
        }
        let child = self.id(heap, heap.sequence_get(children, index)?)?;
        self.scene.add(parent, child, index as usize)?;
        Ok(true)
    }
    pub(in crate::aside) fn restore_root(
        &mut self,
        heap: &Heap,
        root: Option<RootToken>,
    ) -> Result<(), AdapterError> {
        if let Some(root) = root {
            self.scene.replace(self.id(heap, heap.root_value(root)?)?)?;
        }
        self.root = root;
        Ok(())
    }
}
