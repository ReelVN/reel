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
}

impl Shape {
    pub(super) fn from_symbol(symbol: &str) -> Self {
        match symbol {
            "reel::text" => Self::Text,
            "reel::text_input" => Self::TextInput,
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
            "reel::row" => Self::Row,
            "reel::column" => Self::Column,
            "reel::stack" => Self::Stack,
            _ => unreachable!("registered stage constructor"),
        }
    }
    fn prefix(self) -> usize {
        match self {
            Self::Text | Self::Button => 2,
            Self::Image | Self::Video => registry::IMAGE_PREFIX.len(),
            Self::TextInput => text_input::TextInputField::PROPERTIES.len(),
            Self::Slider => controls::SliderField::PROPERTIES.len(),
            Self::Switch | Self::Checkbox => controls::ToggleField::PROPERTIES.len(),
            Self::Radio => controls::RadioField::PROPERTIES.len(),
            Self::Input => input::InputField::PROPERTIES.len(),
            Self::Particles => super::super::particles::PREFIX.len(),
            Self::Camera => camera::CameraField::PROPERTIES.len(),
            Self::Scroll => scroll::ScrollField::PROPERTIES.len(),
            Self::Row | Self::Column | Self::Stack | Self::VirtualList => 0,
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
            Self::TextInput => text_input::kind(heap, fields, limit)?,
            Self::Button => button_kind(heap, get(0)?, get(1)?, limit)?,
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
            Self::Input => {
                for index in 1..=3 {
                    input::validate(heap, index, get(index)?)?;
                }
                NodeKind::Input {
                    bindings: input::read_bindings(heap, get(0)?)?,
                    listening: get(1)?.tag() != ValueTag::Null,
                    keyboard: get(2)?.tag() != ValueTag::Null,
                    pointer: get(3)?.tag() != ValueTag::Null,
                }
            }
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
            Self::Row => NodeKind::Row,
            Self::Column => NodeKind::Column,
            Self::Stack => NodeKind::Stack,
            Self::VirtualList => unreachable!("virtual list projects its VM-owned state"),
        })
    }
    pub(super) fn validator(self) -> aside_vm::HostPropertyValidator {
        match self {
            Self::Text => |h, _, i, v| validate_text_property(h, i, v),
            Self::TextInput => |h, _, i, v| text_input::validate(h, i, v),
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
            Self::Particles => super::super::particles::validate,
            Self::Camera => |h, _, i, v| camera::validate(h, i, v),
            Self::Scroll => |h, _, i, v| scroll::validate(h, i, v),
            Self::Row | Self::Column | Self::Stack | Self::VirtualList => {
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
            ("reel.layout", Shape::Stack),
        ] {
            let version = match shape {
                Shape::Video => 4,
                Shape::Image => 8,
                _ => 6,
            };
            registry.register_validator(name, version, shape.validator())?;
        }
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
            origin: node.props.mask_origin.as_deref().map(str::to_owned),
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
        let mask_origin = saved.origin.as_deref().map(Arc::from);
        let kind = if saved.shape == Shape::VirtualList {
            NodeKind::VirtualList(super::super::lists::virtualized::project(heap, value)?)
        } else {
            saved.shape.project(heap, fields, saved.origin, limit)?
        };
        let props = read_props(heap, fields, saved.shape.prefix(), limit, None, mask_origin)?;
        let mask_source = mask_handle(heap, fields, saved.shape.prefix())?;
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
        let input_values = match &kind {
            NodeKind::Input { bindings, .. } => {
                bindings.iter().map(|a| (a.name.clone(), 0.)).collect()
            }
            _ => BTreeMap::new(),
        };
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
                mask_source,
                input_source: if matches!(saved.shape, Shape::Input) {
                    input::source_handle(heap, heap.table_entry(fields, 0)?.1)?
                } else {
                    None
                },
                text_source: if matches!(
                    saved.shape,
                    Shape::Text | Shape::TextInput | Shape::Image | Shape::Video
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
                input_values,
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
