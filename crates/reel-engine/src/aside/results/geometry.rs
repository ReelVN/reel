//! Geometry requests are attachments on the single presentation-result queue.
use super::*;
pub(super) mod types;

aside_host::native_documentation! {
    pub(in crate::aside) fn documentation {
        /// Request geometry from a newly accepted frame. Await returns the local layout
        /// width and height plus bounds and transform in the requested reference space.
        /// text_end is the lower endpoint of the caret after the complete text, as
        /// {x,y} in the same reference space. It follows wrapping, alignment, rich text
        /// and ruby, independently of reveal progress. Empty text and non-text nodes
        /// return null.
        /// text_caret provides the same caret's transformed {x,y,width,height} bounds,
        /// with its extent taken from the actual final line. Center an overlay at
        /// x + width / 2, y + height / 2 instead of estimating from font size.
        /// Unavailable geometry rejects with {code: GeometryUnavailable}; retirement cancels
        /// it. Saved pending requests wait for a fresh presentation after restore.
        "reel::stage::geometry";
    }
}
use crate::aside::observable;
use reel_ui::stage::{NodeId, StageIdentity};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeometryQuery {
    pub sequence: u64,
    pub node: NodeId,
    pub relative_to: Option<NodeId>,
    pub reveal_margins: Option<[f64; 2]>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RevealProjection {
    pub requested: [f64; 2],
    pub rendered: [f64; 2],
    pub target: [f64; 2],
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NodeGeometry {
    pub width: f64,
    pub height: f64,
    /// x, y, width, height of the transformed local border box.
    pub bounds: [f64; 4],
    /// a, b, c, d, tx, ty; maps target-local points to reference-local points.
    pub transform: [f64; 6],
    /// Lower endpoint of the complete text's final caret, in reference coordinates.
    pub text_end: Option<[f64; 2]>,
    /// Transformed bounds of the complete text's final zero-width insertion caret.
    pub text_caret: Option<[f64; 4]>,
    pub reveal: Option<RevealProjection>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeometryReply {
    pub sequence: u64,
    /// None means the accepted frame cannot provide the requested geometry.
    pub geometry: Option<NodeGeometry>,
}

/// Charged numerical payload retained by exactly one recorded projection.
pub struct GeometryReplies {
    values: Vec<GeometryReply>,
    charge: HeapReservation,
}
impl std::ops::Deref for GeometryReplies {
    type Target = [GeometryReply];
    fn deref(&self) -> &Self::Target {
        &self.values
    }
}
impl std::fmt::Debug for GeometryReplies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.values.fmt(f)
    }
}
fn attachment(heap: &Heap, result: Value) -> Result<Value, HeapError> {
    heap.host_payload(result, Slot::Attachment as u32)
}
pub(super) fn retired(heap: &Heap, result: Value) -> Result<bool, HeapError> {
    let query = attachment(heap, result)?;
    if query.tag() != ValueTag::Null {
        for index in 0..2 {
            let node = heap.sequence_get(query, index)?;
            if node.tag() != ValueTag::Null {
                match heap.host_fields(node) {
                    Err(HeapError::RetiredHost) => return Ok(true),
                    value => {
                        value?;
                    }
                }
            }
        }
    }
    Ok(false)
}
fn value(
    heap: &mut Heap,
    host_types: &crate::aside::source::HostTypes,
    keys: &mut observable::SchemaKeys,
    geometry: NodeGeometry,
) -> Result<Value, AdapterError> {
    use types::{Bounds, Geometry, Point, Transform, record};
    let bounds = record::<Bounds, _>(heap, host_types, keys, geometry.bounds.map(Value::float))?;
    let transform =
        record::<Transform, _>(heap, host_types, keys, geometry.transform.map(Value::float))?;
    let text_end = match geometry.text_end {
        Some(point) => record::<Point, _>(heap, host_types, keys, point.map(Value::float))?,
        None => Value::NULL,
    };
    let text_caret = match geometry.text_caret {
        Some(bounds) => record::<Bounds, _>(heap, host_types, keys, bounds.map(Value::float))?,
        None => Value::NULL,
    };
    record::<Geometry, _>(
        heap,
        host_types,
        keys,
        [
            Value::float(geometry.width),
            Value::float(geometry.height),
            bounds,
            transform,
            text_end,
            text_caret,
        ],
    )
}

impl SessionVm {
    pub(in crate::aside) fn dispatch_geometry(
        &mut self,
        ticket: HostTicket,
    ) -> Result<(), AdapterError> {
        if ticket.context.no_wait || ticket.context.init || ticket.context.derived {
            return Err(AdapterError::Vm(aside_vm::executor::VmError::Effect));
        }
        let arguments = self.vm.host_arguments(ticket)?;
        let (target, relative, requested_owner) = (arguments[0], arguments[1], arguments[2]);
        self.stage.id(self.vm.heap(), target)?;
        if relative.tag() != ValueTag::Null {
            self.stage.id(self.vm.heap(), relative)?;
        }
        let owner = if requested_owner.tag() == ValueTag::Null {
            self.default_activity_owner(ticket.task)?
        } else {
            requested_owner
        };
        if owner.tag() != ValueTag::Null {
            crate::aside::scopes::validate_owner(self.vm.heap(), &self.stage, owner)?;
        }
        let result = self.request_geometry(target, relative, owner, None)?;
        self.vm.respond(ticket, HostResponse::Ready(result))?;
        self.issued = None;
        Ok(())
    }

    pub(in crate::aside) fn request_geometry(
        &mut self,
        target: Value,
        relative: Value,
        owner: Value,
        margins: Option<[f64; 2]>,
    ) -> Result<Value, AdapterError> {
        let ty = self.vm.heap().host_type("reel::types::GeometryRequest")?;
        let result =
            self.results
                .create_presentation(&mut self.vm, self.root_owner, owner, Some(ty))?;
        let query = if let Some([before, after]) = margins {
            self.vm.heap_mut().alloc_array(&[
                target,
                relative,
                Value::float(before),
                Value::float(after),
            ])?
        } else {
            self.vm.heap_mut().alloc_array(&[target, relative])?
        };
        self.vm
            .heap_mut()
            .set_host_payload(result, Slot::Attachment as u32, query)?;
        self.results
            .arm_presentation(&mut self.vm, result, Value::NULL)?;
        Ok(result)
    }

    /// Iterate the single producer queue without copying node or VM roots.
    pub fn geometry_queries(
        &self,
    ) -> impl Iterator<Item = Result<GeometryQuery, AdapterError>> + '_ {
        self.results.presentation_roots().filter_map(|root| {
            let query = (|| -> Result<Option<GeometryQuery>, AdapterError> {
                let heap = self.vm.heap();
                let result = heap.root_value(root)?;
                let query = attachment(heap, result)?;
                let barrier = heap.host_payload(result, Slot::Target as u32)?;
                if query.tag() == ValueTag::Null
                    || barrier.tag() == ValueTag::Null
                    || heap.sequence_get(barrier, 1)?.tag() != ValueTag::Null
                    || retired(heap, result)?
                {
                    return Ok(None);
                }
                let relative = heap.sequence_get(query, 1)?;
                Ok(Some(GeometryQuery {
                    sequence: heap.sequence_get(barrier, 0)?.as_int().unwrap() as u64,
                    node: self.stage.id(heap, heap.sequence_get(query, 0)?)?,
                    relative_to: if relative.tag() == ValueTag::Null {
                        None
                    } else {
                        Some(self.stage.id(heap, relative)?)
                    },
                    reveal_margins: if heap.sequence_len(query)? == 4 {
                        Some([
                            heap.sequence_get(query, 2)?.as_float().unwrap(),
                            heap.sequence_get(query, 3)?.as_float().unwrap(),
                        ])
                    } else {
                        None
                    },
                }))
            })();
            query.transpose()
        })
    }
    /// Capture only outstanding requests at or before this projection watermark.
    /// The frame owns its payload reservation until it is dropped, including any
    /// overlap with another in-flight/accepted frame and the Vec growth peak.
    pub fn record_geometry(
        &self,
        through: u64,
        mut read: impl FnMut(GeometryQuery) -> Option<NodeGeometry>,
    ) -> Result<GeometryReplies, AdapterError> {
        let mut replies = GeometryReplies {
            values: Vec::new(),
            charge: self.vm.heap().budget_usage().reserve(0)?,
        };
        for query in self.geometry_queries() {
            let query = query?;
            if query.sequence > through {
                continue;
            }
            if replies.values.len() == replies.values.capacity() {
                let capacity = replies
                    .values
                    .len()
                    .max(4)
                    .checked_mul(2)
                    .ok_or(AdapterError::Allocation)?;
                let bytes = capacity
                    .checked_mul(size_of::<GeometryReply>())
                    .ok_or(AdapterError::Allocation)? as u64;
                let mut charge = self.vm.heap().budget_usage().reserve(bytes)?;
                let mut next = Vec::new();
                next.try_reserve_exact(capacity)
                    .map_err(|_| AdapterError::Allocation)?;
                charge.resize((next.capacity() * size_of::<GeometryReply>()) as u64)?;
                next.append(&mut replies.values);
                replies.values = next;
                replies.charge = charge;
            }
            replies.values.push(GeometryReply {
                sequence: query.sequence,
                geometry: read(query),
            });
        }
        replies.values.sort_unstable_by_key(|reply| reply.sequence);
        Ok(replies)
    }

    /// The host supplies only replies frozen with the actually accepted candidate.
    /// Replies are sorted by sequence by record_geometry(). All outcomes are
    /// installed before advancing the common presentation barrier.
    pub fn accept_geometry(
        &mut self,
        identity: &StageIdentity,
        replies: &[GeometryReply],
    ) -> Result<(), AdapterError> {
        if replies.is_empty() || identity != self.stage.scene.identity() {
            return Ok(());
        }
        for root in self.results.presentations.iter().copied() {
            let heap = self.vm.heap_mut();
            let result = heap.root_value(root)?;
            if state(heap, result)? != State::Pending
                || attachment(heap, result)?.tag() == ValueTag::Null
            {
                continue;
            }
            let barrier = heap.host_payload(result, Slot::Target as u32)?;
            if barrier.tag() == ValueTag::Null
                || heap.sequence_get(barrier, 1)?.tag() != ValueTag::Null
            {
                continue;
            }
            let sequence = heap.sequence_get(barrier, 0)?.as_int().unwrap() as u64;
            let Ok(index) = replies.binary_search_by_key(&sequence, |reply| reply.sequence) else {
                continue;
            };
            let reply = &replies[index];
            match reply.geometry {
                Some(geometry) => {
                    let value = if let Some(reveal) = geometry.reveal {
                        let [request_x, request_y] = reveal.requested;
                        let [actual_x, actual_y] = reveal.rendered;
                        let [target_x, target_y] = reveal.target;
                        heap.alloc_array(
                            &[request_x, request_y, actual_x, actual_y, target_x, target_y]
                                .map(Value::float),
                        )?
                    } else {
                        value(
                            heap,
                            &self.stage.types,
                            &mut self.results.geometry_keys,
                            geometry,
                        )?
                    };
                    heap.set_field(barrier, 1, value)?;
                }
                None => {
                    let code = heap.alloc_string("GeometryUnavailable")?;
                    let error = observable::record(heap, &[("code", code)], &[])?;
                    settle(heap, result, State::Rejected, error)?;
                }
            }
        }
        self.results.changed();
        Ok(())
    }
}

pub(super) fn restore(heap: &mut Heap, result: Value) -> Result<(), AdapterError> {
    if attachment(heap, result)?.tag() != ValueTag::Null {
        let barrier = heap.host_payload(result, Slot::Target as u32)?;
        if barrier.tag() != ValueTag::Null {
            heap.set_field(barrier, 1, Value::NULL)?;
        }
    }
    Ok(())
}
