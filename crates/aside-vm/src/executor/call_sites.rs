//! Borrow logical caller positions without materializing diagnostic stack nodes.
use super::*;

/// A logical position in the admitted program, independent of source maps and
/// runtime task, frame, owner or wait identities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostCallSite {
    pub code: CodeRef,
    pub pc: Pc,
}

/// Borrow the current native's caller positions from outermost to innermost.
/// Native callback bridges are transparent. A native started directly by the
/// host contributes its root position before any script callbacks.
///
/// Iteration visits each existing frame once and allocates nothing. Equal sites
/// remain separate entries, preserving recursive call paths. The borrow ends
/// before the VM can advance, consume the ticket or restore another snapshot.
pub struct HostCallSites<'a> {
    frames: &'a [Frame],
    next: usize,
}

impl HostCallSites<'_> {
    /// Upper bound on positions and exact number of raw frames inspected by a
    /// full iteration. Reading this bound does not traverse the caller chain.
    pub fn frame_bound(&self) -> usize {
        self.frames.len()
    }
}

impl Iterator for HostCallSites<'_> {
    type Item = HostCallSite;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(frame) = self.frames.get(self.next) {
            let index = self.next;
            self.next += 1;
            match *frame {
                Frame::Script(script) => {
                    let pc = self.frames.get(index + 1).map_or(script.pc, |child| {
                        let ret = match child {
                            Frame::Script(s) => s.ret,
                            Frame::Native(n) => n.ret,
                        };
                        let (caller, pc) = ret.caller().expect("nested frame has a caller");
                        debug_assert_eq!(caller, script.id);
                        pc.unwrap_or(script.pc)
                    });
                    return Some(HostCallSite {
                        code: script.code,
                        pc,
                    });
                }
                Frame::Native(native) if index == 0 => {
                    return Some(HostCallSite {
                        code: native.location.code,
                        pc: native.location.pc,
                    });
                }
                Frame::Native(_) => {}
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.frames.len() - self.next))
    }
}

impl Vm {
    /// Borrow the complete logical caller path of this live, issued native.
    /// Source locations remain available separately on the ticket for diagnostics.
    pub fn host_call_sites(&self, ticket: HostTicket) -> Result<HostCallSites<'_>, VmError> {
        let task = self.task(ticket.task).map_err(|_| VmError::InvalidTicket)?;
        self.ticket_frame(task, ticket)?;
        Ok(HostCallSites {
            frames: &task.frames.data,
            next: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issued() -> (Vm, HostTicket) {
        let mut vm = Vm::load(
            verify(fixtures::helper_native(), Limits::default()).unwrap(),
            super::super::review_tests::echo_registry(),
            VmLimits::default(),
            SessionEpoch(1),
        )
        .unwrap();
        let task = vm
            .start(fixtures::ENTRY, &[], RootOwner(1), Context::Ordinary)
            .unwrap();
        loop {
            match vm.run_slice(task, SliceBudget::default(), || 0).unwrap() {
                RunOutcome::HostCall(ticket) => return (vm, ticket),
                RunOutcome::BudgetYield(_) => {}
                other => panic!("expected native: {other:?}"),
            }
        }
    }

    #[test]
    fn live_ticket_borrows_call_pcs_from_the_explicit_task_without_allocating() {
        let (mut vm, ticket) = issued();
        let second = vm
            .start(fixtures::ENTRY, &[], RootOwner(2), Context::Ordinary)
            .unwrap();
        assert_ne!(ticket.task, second);
        let objects = vm.heap.stats().live_objects;
        let sites = vm.host_call_sites(ticket).unwrap();
        assert_eq!(sites.frame_bound(), 3);
        let sites: Vec<_> = sites.collect();
        assert_eq!(
            sites,
            [
                HostCallSite {
                    code: CodeRef {
                        function: FunctionId(0),
                        version: 1
                    },
                    pc: Pc(1)
                },
                HostCallSite {
                    code: CodeRef {
                        function: FunctionId(1),
                        version: 1
                    },
                    pc: Pc(0)
                },
            ]
        );
        assert_eq!(vm.heap.stats().live_objects, objects);
        vm.respond(ticket, HostResponse::Ready(Value::int(42)))
            .unwrap();
        assert!(matches!(
            vm.host_call_sites(ticket),
            Err(VmError::InvalidTicket)
        ));
    }

    #[test]
    fn every_return_edge_preserves_the_call_pc_instead_of_the_advanced_caller_pc() {
        let (vm, ticket) = issued();
        let original = &vm.task(ticket.task).unwrap().frames.data;
        let Frame::Script(parent) = original[0] else {
            panic!("script")
        };
        for ret in [
            ReturnTo::Register {
                frame: parent.id,
                reg: Reg(0),
                call: Pc(7),
            },
            ReturnTo::Default {
                frame: parent.id,
                parameter: 0,
                call: Pc(7),
            },
            ReturnTo::Class {
                frame: parent.id,
                call: Pc(7),
            },
            ReturnTo::Sort {
                frame: parent.id,
                call: Pc(7),
            },
            ReturnTo::ReactiveRead {
                frame: parent.id,
                call: Pc(7),
            },
        ] {
            let mut frames = original.clone();
            let Frame::Script(child) = &mut frames[1] else {
                panic!("script")
            };
            child.ret = ret;
            let mut sites = HostCallSites {
                frames: &frames,
                next: 0,
            };
            assert_eq!(sites.next().unwrap().pc, Pc(7));
        }
    }

    #[test]
    fn native_callbacks_are_transparent_and_a_native_root_keeps_its_position() {
        let (vm, ticket) = issued();
        let original = &vm.task(ticket.task).unwrap().frames.data;
        let Frame::Native(mut bridge) = original[2] else {
            panic!("native")
        };
        let Frame::Script(mut callback) = original[1] else {
            panic!("script")
        };
        let Frame::Script(parent) = original[0] else {
            panic!("script")
        };
        bridge.id = FrameId(90);
        bridge.ret = ReturnTo::Register {
            frame: parent.id,
            reg: Reg(0),
            call: Pc(3),
        };
        for ret in [
            ReturnTo::Native { frame: bridge.id },
            ReturnTo::NativeDefault {
                frame: bridge.id,
                parameter: 0,
            },
        ] {
            callback.ret = ret;
            let frames = [
                original[0],
                Frame::Native(bridge),
                Frame::Script(callback),
                original[2],
            ];
            let sites: Vec<_> = HostCallSites {
                frames: &frames,
                next: 0,
            }
            .collect();
            assert_eq!(sites.len(), 2);
            assert_eq!(sites[0].pc, Pc(3));
            assert_eq!(sites[1].pc, Pc(0));
        }
        bridge.ret = ReturnTo::Delivery;
        callback.ret = ReturnTo::Native { frame: bridge.id };
        let frames = [Frame::Native(bridge), Frame::Script(callback), original[2]];
        let sites: Vec<_> = HostCallSites {
            frames: &frames,
            next: 0,
        }
        .collect();
        assert_eq!(sites.len(), 2);
        assert_eq!(sites[0].code, bridge.location.code);
        assert_eq!(sites[0].pc, bridge.location.pc);
        assert_eq!(sites[1].code, callback.code);
    }
}
