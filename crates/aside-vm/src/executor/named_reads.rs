//! Named point reads reuse pure admission metadata and existing resumable steps.
use super::*;

/// Current table metadata for one literal MemberGet admission only. The caller's
/// receiver register retains the table; neither values nor borrowed storage are
/// cached, and budget suspension discards this plan.
pub(super) struct NamedRead {
    table: Value,
    version: u64,
    host: bool,
}

#[derive(Clone, Copy)]
enum Phase {
    Validate,
    Scan,
    Equality {
        dest: Reg,
        negated: bool,
    },
    Cleanup {
        dest: Reg,
        negated: bool,
        result: Result<bool, FaultKind>,
    },
}

impl Phase {
    fn capture(phase: &ScriptPhase) -> Option<Self> {
        Some(match *phase {
            ScriptPhase::TableValidate => Self::Validate,
            ScriptPhase::TableScan => Self::Scan,
            ScriptPhase::Equality { dest, negated } => Self::Equality { dest, negated },
            ScriptPhase::EqualityCleanup {
                dest,
                negated,
                result,
            } => Self::Cleanup {
                dest,
                negated,
                result,
            },
            _ => return None,
        })
    }

    fn publish(self, phase: &mut ScriptPhase) {
        // Reconstruct only the active small variant. Copying ScriptPhase by
        // value would also copy storage for unrelated large continuation arms.
        *phase = match self {
            Self::Validate => ScriptPhase::TableValidate,
            Self::Scan => ScriptPhase::TableScan,
            Self::Equality { dest, negated } => ScriptPhase::Equality { dest, negated },
            Self::Cleanup {
                dest,
                negated,
                result,
            } => ScriptPhase::EqualityCleanup {
                dest,
                negated,
                result,
            },
        };
    }

    #[cfg(feature = "profiling")]
    fn point(self, frame: &ScriptFrame) -> Option<step_costs::StepPoint> {
        Some(step_costs::StepPoint {
            function: frame.code.function.0,
            pc: frame.pc.0,
            native: None,
            mode: match self {
                Self::Validate | Self::Scan => 19,
                _ => 23,
            },
        })
    }
}

#[inline]
pub(super) fn eligible(frame: &ScriptFrame, meter: &Meter) -> bool {
    // A one-step remainder retains the ordinary single-step dispatch.
    frame
        .table_work
        .as_ref()
        .is_some_and(|work| work.named_read(frame.member_symbol))
        && meter.budget.work - meter.usage.work >= 2
        && Phase::capture(&frame.phase).is_some()
}

impl Vm {
    pub(super) fn prepare_named_read(&self, object: Value, info: ObjectInfo) -> Option<NamedRead> {
        #[cfg(test)]
        if self.work_probe.single_named_read_step {
            return None;
        }
        let (table, info, host) = match info.kind {
            ObjectKind::Table => (object, info, false),
            ObjectKind::Host => {
                let table = self.heap.host_fields(object).ok()?;
                (table, self.heap.info(table).ok()?, true)
            }
            _ => return None,
        };
        if info.kind != ObjectKind::Table {
            return None;
        }
        Some(NamedRead {
            table,
            version: info.structure_version,
            host,
        })
    }

    pub(super) fn execute_named_read(
        &mut self,
        frame: &mut ScriptFrame,
        plan: NamedRead,
        dest: Reg,
        object: Reg,
        symbol: SymbolId,
    ) -> Result<bool, FaultKind> {
        let key = self.value(self.constants, symbol.0).map_err(vm_fault)?;
        let action = if plan.host {
            collections::TableAction::GetHost { object, dest }
        } else {
            collections::TableAction::MemberGet { dest, symbol }
        };
        self.start_table_at_version(frame, plan.table, key, action, plan.version)?;
        Ok(false)
    }

    /// The existing scalar/preparation classifier routes only eligible cursors
    /// here. Publish completed logical steps before another fallible operation.
    pub(super) fn named_read_step(&mut self, meter: &mut Meter) -> Result<bool, VmError> {
        #[cfg(feature = "profiling")]
        let driver = self.step_costs.start_driver(step_costs::Phase::Script);
        let result = self.named_read_block(meter);
        #[cfg(feature = "profiling")]
        self.step_costs.finish_driver(driver);
        result
    }

    fn named_read_block(&mut self, meter: &mut Meter) -> Result<bool, VmError> {
        let index = self.active.frames.data.len() - 1;
        let depth = self.active.frames.data.len();
        let Frame::Script(mut frame) = self.active.frames.data[index] else {
            unreachable!()
        };
        for step in 0..32 {
            let Some(phase) = Phase::capture(&frame.phase) else {
                break;
            };
            if !frame
                .table_work
                .as_ref()
                .is_some_and(|work| work.named_read(frame.member_symbol))
            {
                break;
            }
            // The first step consumed its clock boundary in the outer driver.
            if step != 0 && !meter.next_short_step() {
                break;
            }
            #[cfg(feature = "execution-counts")]
            self.execution_counts.step(&frame.phase);
            let progressed = step_cost!(
                self.step_costs,
                Script,
                phase.point(&frame),
                match phase {
                    Phase::Validate | Phase::Scan => self.table_step(&mut frame, meter),
                    Phase::Equality { .. } | Phase::Cleanup { .. } =>
                        self.equality_phase_step(&mut frame, meter),
                }
            )?;
            if !progressed {
                return Ok(false);
            }
            let Frame::Script(stored) = &mut self.active.frames.data[index] else {
                unreachable!()
            };
            if let Some(phase) = Phase::capture(&frame.phase) {
                phase.publish(&mut stored.phase);
                match (&frame.table_work, &mut stored.table_work) {
                    (Some(current), Some(stored)) => current.publish_named_cursor(stored),
                    _ => stored.table_work = frame.table_work,
                }
            } else {
                *stored = frame;
            }
            // A reactive refresh publishes this canonical caller before pushing
            // its ordinary frame. Stop before that callback or any possible GC.
            if self.active.frames.data.len() != depth {
                break;
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
