//! Exception region and cleanup verification.
use super::*;

#[test]
fn every_normal_handler_entry_and_cleanup_bypass_is_rejected() {
    for instruction in [
        Instruction::new(Opcode::Jump, 2, 0, 0),
        Instruction::new(Opcode::BranchBool, 0, 2, 6),
        Instruction::new(Opcode::Move, 0, 0, 0),
        Instruction::new(Opcode::Jump, 6, 0, 0),
    ] {
        let mut p = fixtures::exception_regions();
        p.functions[0].code[1] = instruction;
        bad(p, ErrorKind::ControlFlow);
    }
    for instruction in [
        Instruction::new(Opcode::Jump, 6, 0, 0),
        Instruction::new(Opcode::Move, 0, 0, 0),
    ] {
        let mut p = fixtures::exception_regions();
        p.functions[0].code[3] = instruction;
        bad(p, ErrorKind::ControlFlow);
    }
    let mut p = fixtures::exception_regions();
    p.functions[0].code[5].operands[0] = 1;
    bad(p, ErrorKind::Region);
    let mut p = fixtures::exception_regions();
    p.functions[0].code[4] = Instruction::new(Opcode::Return, 0, 0, 0);
    bad(p, ErrorKind::ControlFlow);
    let mut p = fixtures::exception_regions();
    p.functions[0].regions[0]
        .catch
        .as_mut()
        .unwrap()
        .range
        .start = Pc(1);
    bad(p, ErrorKind::Region);
    let mut p = fixtures::helper_native();
    p.functions[0].code.pop();
    bad(p, ErrorKind::ControlFlow);
    let mut p = fixtures::helper_native();
    p.functions[0].code[0] = Instruction::new(Opcode::Rethrow, 0, 0, 0);
    bad(p, ErrorKind::ControlFlow);
}
#[test]
fn exceptional_edges_skip_consumed_catch_and_finally_handlers() {
    let p = verify(fixtures::exception_regions(), limits()).unwrap();
    assert_eq!(
        p.exception_handler(FunctionId(0), pc(&p, 1), None).unwrap(),
        Some(HandlerTarget {
            region: RegionId(0),
            kind: HandlerKind::Catch,
            pc: pc(&p, 2),
            exception: Some(Reg(1))
        })
    );
    assert_eq!(
        p.exception_handler(FunctionId(0), pc(&p, 2), None).unwrap(),
        Some(HandlerTarget {
            region: RegionId(0),
            kind: HandlerKind::Finally,
            pc: pc(&p, 4),
            exception: None
        })
    );
    assert_eq!(
        p.exception_handler(FunctionId(0), pc(&p, 4), None).unwrap(),
        None
    );
    assert_eq!(
        p.exception_handler(FunctionId(0), pc(&p, 1), Some(RegionId(0)))
            .unwrap(),
        None
    );
    assert_eq!(Opcode::EndFinally.spec().unwind, Unwind::ResumeActive);
    let mut nested = fixtures::exception_regions();
    let f = &mut nested.functions[0];
    f.code
        .insert(0, Instruction::new(Opcode::LoadConst, 0, 0, 0));
    for i in &mut f.code {
        if i.opcode == Opcode::Leave as u16 {
            i.operands[0] += 1;
        }
    }
    f.regions[0] = ExceptionRegion {
        protected: PcRange {
            start: Pc(1),
            end: Pc(3),
        },
        catch: Some(CatchRegion {
            range: PcRange {
                start: Pc(3),
                end: Pc(5),
            },
            exception: Reg(1),
        }),
        finally: Some(PcRange {
            start: Pc(5),
            end: Pc(7),
        }),
    };
    f.regions.push(ExceptionRegion {
        protected: PcRange {
            start: Pc(0),
            end: Pc(8),
        },
        catch: Some(CatchRegion {
            range: PcRange {
                start: Pc(8),
                end: Pc(9),
            },
            exception: Reg(1),
        }),
        finally: None,
    });
    f.code.push(Instruction::new(Opcode::Return, 1, 0, 0));
    f.source_map.clear();
    let p = verify(nested, limits()).unwrap();
    assert_eq!(
        p.exception_handler(FunctionId(0), pc(&p, 5), None)
            .unwrap()
            .unwrap()
            .region,
        RegionId(1)
    );
}
#[test]
fn nested_try_can_leave_inside_finally_but_cannot_escape_it() {
    let mut p = fixtures::exception_regions();
    let f = &mut p.functions[0];
    f.code = vec![
        Instruction::new(Opcode::LoadConst, 0, 0, 0),
        Instruction::new(Opcode::Leave, 6, 0, 0),
        Instruction::new(Opcode::LoadConst, 0, 1, 0),
        Instruction::new(Opcode::Leave, 5, 0, 0),
        Instruction::new(Opcode::Leave, 5, 0, 0),
        Instruction::new(Opcode::EndFinally, 0, 0, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ];
    f.regions = vec![
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(2),
            },
            catch: None,
            finally: Some(PcRange {
                start: Pc(2),
                end: Pc(6),
            }),
        },
        ExceptionRegion {
            protected: PcRange {
                start: Pc(2),
                end: Pc(4),
            },
            catch: Some(CatchRegion {
                range: PcRange {
                    start: Pc(4),
                    end: Pc(5),
                },
                exception: Reg(1),
            }),
            finally: None,
        },
    ];
    let program = verify(p.clone(), limits()).unwrap();
    assert_eq!(
        program
            .cleanup_handler(FunctionId(0), Pc(3), CleanupDestination::Leave(Pc(5)), None)
            .unwrap(),
        None,
        "an inner Leave preserves the already active outer finally"
    );
    p.functions[0].code[3].operands[0] = 6;
    bad(p, ErrorKind::ControlFlow);
}
#[test]
fn unwind_cursor_moves_strictly_outward_across_two_finally_layers() {
    let mut p = fixtures::exception_regions();
    let f = &mut p.functions[0];
    f.source_map.clear();
    f.code = vec![
        Instruction::new(Opcode::LoadConst, 0, 1, 0),
        Instruction::new(Opcode::Throw, 0, 0, 0),
        Instruction::new(Opcode::LoadConst, 1, 0, 0),
        Instruction::new(Opcode::EndFinally, 0, 0, 0),
        Instruction::new(Opcode::LoadConst, 1, 0, 0),
        Instruction::new(Opcode::EndFinally, 1, 0, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
    ];
    f.regions = vec![
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(2),
            },
            catch: None,
            finally: Some(PcRange {
                start: Pc(2),
                end: Pc(4),
            }),
        },
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(4),
            },
            catch: None,
            finally: Some(PcRange {
                start: Pc(4),
                end: Pc(6),
            }),
        },
    ];
    let bytes = encode(&p, limits()).unwrap();
    let p = VerifiedProgram::decode(&bytes, limits()).unwrap();
    let inner = p
        .exception_handler(FunctionId(0), pc(&p, 1), None)
        .unwrap()
        .unwrap();
    assert_eq!(inner.region, RegionId(0));
    let outer = p
        .exception_handler(FunctionId(0), pc(&p, 1), Some(inner.region))
        .unwrap()
        .unwrap();
    assert_eq!(outer.region, RegionId(1));
    assert_eq!(
        p.exception_handler(FunctionId(0), pc(&p, 1), Some(outer.region))
            .unwrap(),
        None
    );
    assert_eq!(
        p.exception_handler(FunctionId(0), pc(&p, 4), Some(inner.region))
            .unwrap_err()
            .kind,
        ErrorKind::Region
    );
}
#[test]
fn protected_and_handler_blocks_cannot_have_different_lexical_parents() {
    let mut p = fixtures::exception_regions();
    let f = &mut p.functions[0];
    f.source_map.clear();
    f.code = vec![
        Instruction::new(Opcode::Jump, 2, 0, 0),
        Instruction::new(Opcode::Throw, 0, 0, 0),
        Instruction::new(Opcode::Leave, 4, 0, 0),
        Instruction::new(Opcode::Return, 0, 0, 0),
        Instruction::new(Opcode::LoadConst, 0, 0, 0),
        Instruction::new(Opcode::Throw, 0, 0, 0),
    ];
    f.regions = vec![
        ExceptionRegion {
            protected: PcRange {
                start: Pc(0),
                end: Pc(3),
            },
            catch: Some(CatchRegion {
                range: PcRange {
                    start: Pc(3),
                    end: Pc(4),
                },
                exception: Reg(1),
            }),
            finally: None,
        },
        ExceptionRegion {
            protected: PcRange {
                start: Pc(4),
                end: Pc(6),
            },
            catch: Some(CatchRegion {
                range: PcRange {
                    start: Pc(1),
                    end: Pc(2),
                },
                exception: Reg(1),
            }),
            finally: None,
        },
    ];
    bad(p, ErrorKind::Region);
}

#[test]
fn return_cleanup_uses_catch_nesting_and_strict_consumed_ancestors() {
    let bytes = encode(&cleanups_nested_in_catch(), limits()).unwrap();
    let p = VerifiedProgram::decode(&bytes, limits()).unwrap();
    let mut consumed = None;
    for (region, ordinal) in [(2, 7), (1, 10), (0, 14)] {
        let target = p
            .cleanup_handler(
                FunctionId(0),
                pc(&p, 4),
                CleanupDestination::Return,
                consumed,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            target,
            HandlerTarget {
                region: RegionId(region),
                kind: HandlerKind::Finally,
                pc: pc(&p, ordinal),
                exception: None,
            }
        );
        consumed = Some(target.region);
    }
    assert_eq!(
        p.cleanup_handler(
            FunctionId(0),
            pc(&p, 4),
            CleanupDestination::Return,
            consumed
        )
        .unwrap(),
        None
    );
    assert_eq!(
        p.exception_handler(FunctionId(0), pc(&p, 4), Some(RegionId(2)))
            .unwrap()
            .unwrap()
            .region,
        RegionId(1)
    );
    assert_eq!(
        p.cleanup_handler(
            FunctionId(0),
            pc(&p, 13),
            CleanupDestination::Return,
            Some(RegionId(2))
        )
        .unwrap_err()
        .kind,
        ErrorKind::Region
    );
    assert_eq!(
        p.cleanup_handler(FunctionId(0), pc(&p, 16), CleanupDestination::Return, None)
            .unwrap(),
        None
    );
}

#[test]
fn leave_cleanup_preserves_enclosing_catch_and_validates_destination() {
    let p = verify(cleanups_nested_in_catch(), limits()).unwrap();
    for (source, destination, region) in [(5, 9, 2), (9, 12, 1), (12, 16, 0)] {
        let exit = CleanupDestination::Leave(pc(&p, destination));
        assert_eq!(
            p.cleanup_handler(FunctionId(0), pc(&p, source), exit, None)
                .unwrap()
                .unwrap()
                .region,
            RegionId(region)
        );
        assert_eq!(
            p.cleanup_handler(FunctionId(0), pc(&p, source), exit, Some(RegionId(region)))
                .unwrap(),
            None
        );
    }
    assert_eq!(
        p.cleanup_handler(
            FunctionId(0),
            pc(&p, 12),
            CleanupDestination::Leave(pc(&p, 17)),
            None
        )
        .unwrap_err()
        .kind,
        ErrorKind::Index
    );
    assert_eq!(
        p.cleanup_handler(FunctionId(0), pc(&p, 17), CleanupDestination::Return, None)
            .unwrap_err()
            .kind,
        ErrorKind::Index
    );
    assert_eq!(
        p.cleanup_handler(FunctionId(1), pc(&p, 4), CleanupDestination::Return, None)
            .unwrap_err()
            .kind,
        ErrorKind::Index
    );
}
