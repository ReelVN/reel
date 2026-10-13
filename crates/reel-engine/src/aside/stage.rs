//! Stable Aside host nodes projected into Reel's one shared stage model.
use std::collections::BTreeMap;

use aside_vm::{
    Heap, HeapError, HeapHandle, ObjectKind, RootKind, RootOwner, RootToken, Value, ValueTag,
};
use reel_ui::stage::{NodeId, NodeKind, NodeProps, Stage};

use super::{AdapterError, registry, text};
use aside_host::PropertySchema;

/// One host schema feeds construction, writes and animation; NodeProps remains in reel-ui.
#[derive(aside_host::Properties)]
pub(super) enum NodeProperty {
    #[aside(name = "x", ty = Any, default = 0, validate = validate_number, animatable)]
    X,
    #[aside(name = "y", ty = Any, default = 0, validate = validate_number, animatable)]
    Y,
    #[aside(name = "width", ty = Any, default = null, validate = validate_dimension, animatable)]
    Width,
    #[aside(name = "height", ty = Any, default = null, validate = validate_dimension, animatable)]
    Height,
    #[aside(name = "opacity", ty = Any, default = 1, validate = validate_opacity, animatable)]
    Opacity,
    #[aside(name = "visible", ty = Bool, default = true)]
    Visible,
    #[aside(name = "clip", ty = Bool, default = false)]
    Clip,
    #[aside(name = "gap", ty = Any, default = 0, validate = validate_nonnegative, animatable)]
    Gap,
    #[aside(name = "padding", ty = Any, default = 0, validate = validate_nonnegative, animatable)]
    Padding,
    #[aside(name = "font_size", ty = Any, default = 24, validate = validate_nonnegative, animatable)]
    FontSize,
    #[aside(name = "color", ty = String, default = "#ffffff", validate = validate_color)]
    Color,
    #[aside(name = "background", ty = Any, default = null, validate = validate_background)]
    Background,
    #[aside(name = "rotation", ty = Any, default = 0, validate = validate_number, animatable)]
    Rotation,
    #[aside(name = "scale_x", ty = Any, default = 1, validate = validate_number, animatable)]
    ScaleX,
    #[aside(name = "scale_y", ty = Any, default = 1, validate = validate_number, animatable)]
    ScaleY,
    #[aside(name = "anchor_x", ty = Any, default = 0, validate = validate_number, animatable)]
    AnchorX,
    #[aside(name = "anchor_y", ty = Any, default = 0, validate = validate_number, animatable)]
    AnchorY,
}

#[derive(Default)]
pub(super) struct StageBinding {
    pub scene: Stage,
    handles: BTreeMap<NodeId, NodeBinding>,
    root: Option<RootToken>,
}
struct NodeBinding {
    handle: HeapHandle,
    fields_version: u64,
}

impl StageBinding {
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

    pub(super) fn property_offset(&self, heap: &Heap, value: Value) -> Result<u32, AdapterError> {
        let id = self.id(heap, value)?;
        Ok(match self.scene.node(id).unwrap().kind {
            NodeKind::Text(_) => 1,
            NodeKind::Image { .. } => 2,
            _ => 0,
        })
    }

    pub fn dispatch(
        &mut self,
        heap: &mut Heap,
        owner: RootOwner,
        symbol: &str,
        args: &[Value],
        read_limit: usize,
        origin: Option<String>,
    ) -> Result<Value, AdapterError> {
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

        let prefix = registry::node_prefix(symbol);
        let fields = heap.alloc_table()?;
        let field_count = prefix.len() + NodeProperty::PROPERTIES.len();
        for (index, argument) in args.iter().take(field_count).enumerate() {
            let name = if index < prefix.len() {
                prefix[index]
            } else {
                NodeProperty::PROPERTIES[index - prefix.len()].name
            };
            let key = heap.alloc_string(name)?;
            heap.table_store(fields, index as u32, key, *argument)?;
        }
        let kind = match symbol {
            "reel::text" => NodeKind::Text(read_text(heap, args[0], read_limit)?),
            "reel::image" => NodeKind::Image {
                source: read_source(heap, args[0], read_limit)?,
                fit: read_fit(heap, args[1])?,
                origin,
            },
            "reel::row" => NodeKind::Row,
            "reel::column" => NodeKind::Column,
            "reel::stack" => NodeKind::Stack,
            _ => unreachable!("registered stage constructor"),
        };
        let props = read_props(heap, fields, prefix.len())?;
        let mut children = Vec::new();
        let references = heap.alloc_array(&[])?;
        if prefix.is_empty() {
            let input = args[args.len() - 1];
            if input.tag() != ValueTag::Null {
                for index in 0..heap.sequence_len(input)? {
                    let child = heap.sequence_get(input, index)?;
                    children.push(self.id(heap, child)?);
                    heap.array_push(references, child)?;
                }
            }
        }
        let id = self.scene.next_identity()?;
        let value = heap.alloc_composable_host(
            fields,
            &[
                Value::int(id.0 as i64),
                references,
                Value::NULL,
                Value::NULL,
            ],
            Some(match symbol {
                "reel::text" => validate_text_property,
                "reel::image" => validate_image_property,
                _ => validate_layout_property,
            }),
        )?;
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
                handle: heap.handle(value)?,
                fields_version: heap.info(fields)?.content_version,
            },
        );
        Ok(value)
    }

    fn sync_tree(
        &mut self,
        heap: &Heap,
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
            let binding = &self.handles[&id];
            let value = heap.import_handle(binding.handle)?;
            let fields = heap.host_fields(value)?;
            let version = heap.info(fields)?.content_version;
            if version == binding.fields_version {
                continue;
            }
            let (kind, offset) = match &node.kind {
                NodeKind::Text(_) => (
                    NodeKind::Text(read_text(heap, heap.table_entry(fields, 0)?.1, read_limit)?),
                    1,
                ),
                NodeKind::Image { origin, .. } => (
                    NodeKind::Image {
                        source: read_source(heap, heap.table_entry(fields, 0)?.1, read_limit)?,
                        fit: read_fit(heap, heap.table_entry(fields, 1)?.1)?,
                        origin: origin.clone(),
                    },
                    2,
                ),
                kind => (kind.clone(), 0),
            };
            let props = read_props(heap, fields, offset)?;
            updates.push((id, kind, props, version));
        }
        for (id, kind, props, version) in updates {
            self.scene.update(id, kind, props)?;
            self.handles.get_mut(&id).unwrap().fields_version = version;
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
        for id in nodes {
            if let Some(binding) = self.handles.remove(&id)
                && let Ok(value) = heap.import_handle(binding.handle)
            {
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
}

fn read_text(heap: &Heap, value: Value, read_limit: usize) -> Result<String, AdapterError> {
    text::read_string(heap, value, read_limit)
}

fn read_source(heap: &Heap, value: Value, read_limit: usize) -> Result<String, AdapterError> {
    let source = read_text(heap, value, read_limit)?;
    if source.is_empty() {
        return Err(AdapterError::Input("image source must not be empty"));
    }
    Ok(source)
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

fn validate_image_property(heap: &Heap, index: u32, value: Value) -> Result<(), HeapError> {
    match index {
        0 => {
            let info = heap.info(value)?;
            if info.kind == ObjectKind::String && info.len != 0 {
                Ok(())
            } else {
                Err(HeapError::TypeMismatch)
            }
        }
        1 => read_fit(heap, value)
            .map(|_| ())
            .map_err(|_| HeapError::TypeMismatch),
        _ => validate_layout_property(heap, index - 2, value),
    }
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
fn nonnegative(value: Value) -> Result<f64, AdapterError> {
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
fn color(heap: &Heap, value: Value) -> Result<[u8; 4], AdapterError> {
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
        return if heap.info(value)?.kind == ObjectKind::String {
            Ok(())
        } else {
            Err(HeapError::TypeMismatch)
        };
    }
    validate_layout_property(heap, index - 1, value)
}
pub(super) fn validate_layout_property(
    heap: &Heap,
    index: u32,
    value: Value,
) -> Result<(), HeapError> {
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
fn validate_opacity(_: &Heap, value: Value) -> Result<(), HeapError> {
    validation(opacity(value))
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
fn read_props(heap: &Heap, fields: Value, offset: usize) -> Result<NodeProps, AdapterError> {
    let get = |property: NodeProperty| -> Result<Value, AdapterError> {
        Ok(heap
            .table_entry(fields, (property.index() + offset) as u32)?
            .1)
    };
    let background = get(NodeProperty::Background)?;
    Ok(NodeProps {
        x: number(get(NodeProperty::X)?)?,
        y: number(get(NodeProperty::Y)?)?,
        width: dimension(get(NodeProperty::Width)?)?,
        height: dimension(get(NodeProperty::Height)?)?,
        opacity: opacity(get(NodeProperty::Opacity)?)?,
        visible: boolean(get(NodeProperty::Visible)?)?,
        clip: boolean(get(NodeProperty::Clip)?)?,
        gap: nonnegative(get(NodeProperty::Gap)?)?,
        padding: nonnegative(get(NodeProperty::Padding)?)?,
        font_size: nonnegative(get(NodeProperty::FontSize)?)?,
        color: color(heap, get(NodeProperty::Color)?)?,
        background: if background.tag() == ValueTag::Null {
            None
        } else {
            Some(color(heap, background)?)
        },
        rotation: number(get(NodeProperty::Rotation)?)?,
        scale_x: number(get(NodeProperty::ScaleX)?)?,
        scale_y: number(get(NodeProperty::ScaleY)?)?,
        anchor_x: number(get(NodeProperty::AnchorX)?)?,
        anchor_y: number(get(NodeProperty::AnchorY)?)?,
    })
}
