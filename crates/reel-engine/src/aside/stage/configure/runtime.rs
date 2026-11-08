//! Native configuration retains original arguments and roots only its proposal.
use super::{FieldIndex, Schema};
use crate::aside::{
    AdapterError, NativeBudget,
    stage::{NodeProperty, StageBinding, snapshot::Shape},
};
use aside_host::PropertySchema;
use aside_vm::executor::FaultKind;
use aside_vm::{
    HeapError, PropertyBatch, PropertyBatchProgress, PropertyId, PropertyWrites, Value,
    executor::{
        Continuation, FaultRetention, HostResponse, HostTicket, NativeInput, ProvidedArguments, Vm,
    },
};
use std::cell::Cell;

pub(in crate::aside) enum Progress {
    Complete { work: u32 },
    Continue { work: u32, required_work: u32 },
}

struct Writes<'a> {
    provided: ProvidedArguments<'a>,
    receiver: Value,
    receiver_argument: Option<usize>,
    common_offset: u32,
    schema: &'a Schema,
}
impl PropertyWrites for Writes<'_> {
    fn len(&self) -> usize {
        self.provided.len() - usize::from(self.receiver_argument.is_some())
    }
    fn get(&self, index: usize) -> Option<(PropertyId, Value)> {
        let index = index
            + usize::from(
                self.receiver_argument
                    .is_some_and(|receiver| index >= receiver),
            );
        let (parameter, value) = self.provided.get(index)?;
        let field = self.schema.fields.get(parameter.checked_sub(1)? as usize)?;
        debug_assert_eq!(field.parameter, parameter);
        let field = match field.index {
            FieldIndex::Specific(index) => index,
            FieldIndex::Common(index) => self.common_offset + index,
        };
        Some((
            PropertyId {
                host: self.receiver,
                field,
            },
            value,
        ))
    }
}

fn accepts(schema: &Schema, shape: Shape) -> bool {
    match schema.type_name {
        "reel::types::Node" => true,
        "reel::types::FlowNode" => matches!(shape, Shape::Row | Shape::Column),
        "reel::types::GridNode" => shape == Shape::Grid,
        "reel::types::TextNode" => shape == Shape::Text,
        "reel::types::TextInputNode" => matches!(shape, Shape::TextInput | Shape::TextArea),
        "reel::types::ImageNode" => shape == Shape::Image,
        "reel::types::VideoNode" => shape == Shape::Video,
        "reel::types::ParticlesNode" => shape == Shape::Particles,
        "reel::types::ButtonNode" => shape == Shape::Button,
        "reel::types::InputNode" => shape == Shape::Input,
        "reel::types::SliderNode" => shape == Shape::Slider,
        "reel::types::ScrollBarNode" => shape == Shape::ScrollBar,
        "reel::types::ToggleNode" => matches!(shape, Shape::Switch | Shape::Checkbox),
        "reel::types::RadioNode" => shape == Shape::Radio,
        "reel::types::ScrollNode" => shape == Shape::Scroll,
        "reel::types::PopupNode" => shape == Shape::Popup,
        "reel::types::CameraNode" => shape == Shape::Camera,
        _ => false,
    }
}

fn reject(
    vm: &mut Vm,
    ticket: HostTicket,
    error: HeapError,
    work: u64,
) -> Result<Progress, AdapterError> {
    let kind = match error {
        HeapError::ReadOnly => FaultKind::ReadOnly,
        HeapError::TypeMismatch | HeapError::WrongKind | HeapError::PropertyConflict => {
            FaultKind::Type
        }
        HeapError::InvalidHandle | HeapError::RetiredHost | HeapError::BindingCycle => {
            FaultKind::InvalidState
        }
        _ => FaultKind::Resource,
    };
    vm.respond_language_error(ticket, kind, Value::NULL)?;
    Ok(Progress::Complete {
        work: u32::try_from(work).map_err(|_| {
            AdapterError::Input("configuration work exceeds the native budget range")
        })?,
    })
}

/// Prepare through the shared heap property protocol. Continuation values carry
/// each call's own plan; no task holds a session-wide configuration lock.
pub(in crate::aside) fn dispatch(
    stage: &StageBinding,
    vm: &mut Vm,
    ticket: HostTicket,
    schema: &Schema,
    budget: NativeBudget,
) -> Result<Progress, AdapterError> {
    vm.heap().begin_property_configuration_work();
    let receiver = vm.host_arguments(ticket)?[0];
    let id = match stage.id(vm.heap(), receiver) {
        Ok(id) => id,
        Err(AdapterError::Heap(error)) => return reject(vm, ticket, error, 1),
        Err(_) => return reject(vm, ticket, HeapError::WrongKind, 1),
    };
    let node = &stage.handles[&id];
    if !accepts(schema, node.shape) {
        return reject(vm, ticket, HeapError::WrongKind, 1);
    }
    let common_offset = node.interaction_offset - NodeProperty::PROPERTIES.len() as u32;
    let plan = match ticket.input {
        NativeInput::Entry => None,
        NativeInput::Continue {
            version: 1,
            stage: 0,
        } => vm
            .host_continuation(ticket)?
            .first()
            .copied()
            .map(PropertyBatch::from_value),
        _ => return Err(AdapterError::Input("invalid configuration continuation")),
    };
    let prefix_work = Cell::new(0u64);
    let admission =
        vm.with_host_provided_arguments(ticket, |heap, provided| -> Result<_, HeapError> {
            let receiver_argument = provided.iter().position(|(index, _)| index == 0);
            let writes = Writes {
                provided,
                receiver,
                receiver_argument,
                common_offset,
                schema,
            };
            if writes.is_empty() {
                return Ok((
                    None,
                    if budget.with_minimum(1) == 0 {
                        PropertyBatchProgress::Preparing {
                            work: 0,
                            required_work: 1,
                        }
                    } else {
                        PropertyBatchProgress::Committed { work: 1 }
                    },
                ));
            }
            let required = match plan {
                Some(plan) => heap.property_batch_required_work(plan, &writes)?,
                None => PropertyBatch::INITIAL_WORK,
            };
            let work_budget = u64::from(
                budget.with_minimum(u32::try_from(required).map_err(|_| HeapError::OutOfBounds)?),
            );
            let fast_work = if plan.is_none() && ticket.input == NativeInput::Entry {
                match heap.try_write_host_properties(&writes, true, work_budget)? {
                    done @ PropertyBatchProgress::Committed { .. } => return Ok((None, done)),
                    PropertyBatchProgress::Preparing { work, .. } => work,
                    _ => unreachable!("scratch configuration publishes or defers"),
                }
            } else {
                0
            };
            if plan.is_none() && work_budget.saturating_sub(fast_work) < PropertyBatch::INITIAL_WORK
            {
                return Ok((
                    None,
                    PropertyBatchProgress::Preparing {
                        work: fast_work,
                        required_work: PropertyBatch::INITIAL_WORK,
                    },
                ));
            }
            let initial_work = fast_work
                + if plan.is_none() {
                    PropertyBatch::INITIAL_WORK
                } else {
                    0
                };
            prefix_work.set(initial_work);
            heap.begin_property_configuration_work();
            let plan = match plan {
                Some(plan) => plan,
                None => heap.begin_property_batch(true)?,
            };
            let progress = heap.step_property_batch(plan, &writes, work_budget - initial_work)?;
            let progress = match progress {
                PropertyBatchProgress::Prepared {
                    work,
                    required_commit_work,
                } if work_budget.saturating_sub(work + initial_work) >= required_commit_work => {
                    prefix_work.set(initial_work + work);
                    match heap.commit_property_batch(
                        plan,
                        &writes,
                        work_budget - work - initial_work,
                    )? {
                        PropertyBatchProgress::Committed { work: commit } => {
                            PropertyBatchProgress::Committed {
                                work: work + commit,
                            }
                        }
                        PropertyBatchProgress::Preparing {
                            work: commit,
                            required_work,
                        } => PropertyBatchProgress::Preparing {
                            work: work + commit,
                            required_work,
                        },
                        PropertyBatchProgress::Prepared {
                            work: commit,
                            required_commit_work,
                        } => PropertyBatchProgress::Prepared {
                            work: work + commit,
                            required_commit_work,
                        },
                    }
                }
                other => other,
            };
            let progress = match progress {
                PropertyBatchProgress::Preparing {
                    work,
                    required_work,
                } => PropertyBatchProgress::Preparing {
                    work: work + initial_work,
                    required_work,
                },
                PropertyBatchProgress::Prepared {
                    work,
                    required_commit_work,
                } => PropertyBatchProgress::Prepared {
                    work: work + initial_work,
                    required_commit_work,
                },
                PropertyBatchProgress::Committed { work } => PropertyBatchProgress::Committed {
                    work: work + initial_work,
                },
            };
            Ok((Some(plan), progress))
        })?;
    let (plan, progress) = match admission {
        Ok(admission) => admission,
        Err(error) => {
            return reject(
                vm,
                ticket,
                error,
                prefix_work.get() + vm.heap().property_configuration_work(),
            );
        }
    };
    let narrow = |work: u64| {
        u32::try_from(work)
            .map_err(|_| AdapterError::Input("configuration work exceeds the native budget range"))
    };
    match progress {
        PropertyBatchProgress::Committed { work } => {
            vm.respond(ticket, HostResponse::Ready(receiver))?;
            Ok(Progress::Complete {
                work: narrow(work)?,
            })
        }
        PropertyBatchProgress::Preparing {
            work,
            required_work,
        }
        | PropertyBatchProgress::Prepared {
            work,
            required_commit_work: required_work,
        } => {
            let values = [plan.map_or(Value::NULL, PropertyBatch::value)];
            vm.respond(
                ticket,
                HostResponse::Continue(Continuation {
                    version: 1,
                    stage: 0,
                    values: &values[..usize::from(plan.is_some())],
                    faults: FaultRetention::Preserve,
                }),
            )?;
            Ok(Progress::Continue {
                work: narrow(work)?,
                required_work: narrow(required_work)?,
            })
        }
    }
}
