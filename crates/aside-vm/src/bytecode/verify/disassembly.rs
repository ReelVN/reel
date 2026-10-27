//! Verified bytecode disassembly.
use super::*;

/// Streaming disassembly to a caller-owned sink; text includes descriptor fields,
/// source byte spans and the exact shared flow/suspension contract.
pub fn disassemble(program: &VerifiedProgram, out: &mut impl Write) -> std::fmt::Result {
    let p = program.package();
    writeln!(
        out,
        "ASIDBC01 format={} language={} abi={} content={} bytes={} capacity={}",
        p.format_version,
        p.language_version,
        p.host_abi_version,
        p.content_version,
        program.stats().encoded_bytes,
        program.stats().owned_capacity_bytes
    )?;
    for (id, module) in p.modules.iter().enumerate() {
        writeln!(
            out,
            "module m{id} {} {module:?}",
            p.symbols[module.path.index()]
        )?;
    }
    for (id, s) in p.signatures.iter().enumerate() {
        writeln!(out, "signature s{id} {s:?}")?;
    }
    for (id, c) in p.constants.iter().enumerate() {
        writeln!(out, "const k{id} {c:?}")?;
    }
    for (id, n) in p.natives.iter().enumerate() {
        writeln!(
            out,
            "native n{id} {}@{} {n:?}",
            p.symbols[n.symbol.index()],
            n.abi_version
        )?;
    }
    for (id, f) in p.functions.iter().enumerate() {
        writeln!(
            out,
            "function f{id} {} {:?} {:?} s{} regs={} captures={:?}",
            p.symbols[f.name.index()],
            f.kind,
            f.context,
            f.signature.0,
            f.register_count,
            f.captures
        )?;
        for (d, desc) in f.descriptors.iter().enumerate() {
            writeln!(out, "  descriptor d{d} {desc:?}")?;
        }
        for (binding, definition) in f.bindings.iter().enumerate() {
            writeln!(
                out,
                "  binding b{binding} {} {definition:?}",
                p.symbols[definition.name.index()]
            )?;
        }
        for (r, region) in f.regions.iter().enumerate() {
            writeln!(out, "  region e{r} {region:?}")?;
        }
        for item in f.code.indexed_iter() {
            let (pc, i) = item.map_err(|_| std::fmt::Error)?;
            let spec = instruction_spec(i.opcode).expect("verified opcode");
            write!(out, "  {pc:04} {:04x} {}", i.opcode, spec.mnemonic)?;
            for (kind, operand) in spec.operands.iter().zip(i.operands) {
                if *kind != OperandKind::Unused {
                    write!(out, " {kind:?}={operand}")?;
                }
            }
            write!(
                out,
                " ; {:?} throw={} suspend={:?}",
                spec.flow, spec.may_throw, spec.suspension
            )?;
            if let Some(span) = program.source_at(FunctionId(id as u32), Pc(pc as u32)) {
                write!(
                    out,
                    " {}:{}..{}",
                    p.symbols[p.sources[span.file.index()].path.index()],
                    span.start,
                    span.end
                )?;
            }
            writeln!(out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod bounded_index_tests {
    use super::super::*;

    #[test]
    fn symbol_hash_collisions_keep_exact_unique_checks_and_bounded_scratch() {
        let mut p: BytecodePackage = BytecodePackage {
            symbols: (0..10_000).map(|i| format!("s{i:04x}")).collect(),
            ..BytecodePackage::default()
        };
        let limits = Limits::default();
        let (_, owned, _) = super::super::super::codec::measure(&p, limits).unwrap();
        let mut b = Budget::new(limits, owned);
        let allocation = allocation_counter::measure(|| {
            // Force every distinct symbol into one bucket. Equality stays exact,
            // the fallback remains sorted, and no probabilistic assumption is needed.
            validate_symbols_with_hash(&p, &mut b, |_| 0).unwrap();
        });
        assert_eq!(allocation.bytes_current, 0);
        assert_eq!(allocation.bytes_max as usize, b.peak);
        assert_eq!(b.scratch, 0);
        assert_eq!(
            b.peak,
            p.symbols.len() * (std::mem::size_of::<(u64, u32)>() + std::mem::size_of::<&str>())
        );
        assert!(b.work < limits.max_work);

        p.symbols.push(p.symbols[0].clone());
        let mut b = Budget::new(limits, owned);
        assert_eq!(
            validate_symbols_with_hash(&p, &mut b, |_| 0)
                .unwrap_err()
                .kind,
            ErrorKind::Encoding
        );
        let mut b = Budget::new(
            Limits {
                max_work: 100,
                ..limits
            },
            owned,
        );
        assert_eq!(
            validate_symbols_with_hash(&p, &mut b, |_| 0)
                .unwrap_err()
                .kind,
            ErrorKind::Limit
        );
        assert_eq!(
            b.peak, 0,
            "work limit is checked before reserving the index"
        );
    }

    #[test]
    fn collision_fallback_capacity_is_checked_before_its_reservation() {
        let p: BytecodePackage = BytecodePackage {
            symbols: (0..1000).map(|i| format!("s{i:04x}")).collect(),
            ..BytecodePackage::default()
        };
        let limits = Limits::default();
        let (_, owned, _) = super::super::super::codec::measure(&p, limits).unwrap();
        let index_bytes = p.symbols.len() * std::mem::size_of::<(u64, u32)>();
        let group_bytes = p.symbols.len() * std::mem::size_of::<&str>();
        let mut b = Budget::new(
            Limits {
                max_allocation_bytes: owned + index_bytes + group_bytes - 1,
                ..limits
            },
            owned,
        );
        assert_eq!(
            validate_symbols_with_hash(&p, &mut b, |_| 0)
                .unwrap_err()
                .kind,
            ErrorKind::Limit
        );
        assert_eq!(b.peak, index_bytes);
    }

    #[test]
    fn parameter_index_capacity_matches_allocator_and_fails_before_reservation() {
        let p = fixtures::parameter_cell_default();
        let limits = Limits::default();
        let (_, owned, _) = super::super::super::codec::measure(&p, limits).unwrap();
        let mut b = Budget::new(limits, owned);
        let allocation = allocation_counter::measure(|| {
            let layouts = ParameterLayouts::build(&p, &mut b).unwrap();
            assert!(layouts.cell(1, 0, &mut b).unwrap());
            assert!(layouts.cell(2, 0, &mut b).unwrap());
            assert!(!layouts.cell(1, 1, &mut b).unwrap());
            layouts.release(&mut b);
        });
        let declarations = 3 * std::mem::size_of::<(u32, u32, bool)>();
        let expected = declarations + (p.functions.len() + 1) * std::mem::size_of::<usize>();
        assert_eq!(b.peak, expected);
        assert_eq!(allocation.bytes_max as usize, expected);
        assert_eq!(allocation.bytes_current, 0);
        assert_eq!(b.scratch, 0);
        let mut b = Budget::new(
            Limits {
                max_allocation_bytes: owned + expected - 1,
                ..limits
            },
            owned,
        );
        assert_eq!(
            ParameterLayouts::build(&p, &mut b).err().unwrap().kind,
            ErrorKind::Limit
        );
        assert_eq!(b.peak, declarations);
        let mut b = Budget::new(
            Limits {
                max_work: 6,
                ..limits
            },
            owned,
        );
        assert_eq!(
            ParameterLayouts::build(&p, &mut b).err().unwrap().kind,
            ErrorKind::Limit
        );
        assert_eq!(b.peak, 0);
    }

    #[test]
    fn non_power_of_two_parameter_search_prepays_the_last_comparison() {
        let mut p = fixtures::parameter_cell_default();
        p.signatures[1].parameters.push(Parameter {
            name: SymbolId(12),
            ty: TypeId(0),
            mutable: false,
            default: None,
        });
        p.functions[1].bindings.push(BindingDef {
            name: SymbolId(12),
            ty: TypeId(0),
            mutable: false,
            storage: BindingStorage::Value(Reg(2)),
            initialization: BindingInitialization::Parameter(2),
        });
        let limits = Limits::default();
        let (_, owned, _) = super::super::super::codec::measure(&p, limits).unwrap();
        let mut b = Budget::new(limits, owned);
        let layouts = ParameterLayouts::build(&p, &mut b).unwrap();
        let mut small = Budget::new(
            Limits {
                max_work: 2,
                ..limits
            },
            owned,
        );
        assert_eq!(
            layouts.cell(1, 2, &mut small).unwrap_err().kind,
            ErrorKind::Limit
        );
        assert_eq!(small.work, 3);
        layouts.release(&mut b);
    }

    #[test]
    fn native_layout_capacity_is_retained_and_prechecked_with_live_parameter_scratch() {
        let p = fixtures::native_parameter_defaults();
        let limits = Limits::default();
        let (_, owned, _) = super::super::super::codec::measure(&p, limits).unwrap();
        let mut b = Budget::new(limits, owned);
        let layouts = ParameterLayouts::build(&p, &mut b).unwrap();
        let layout_bytes = b.scratch;
        let allocation = allocation_counter::measure(|| {
            let cells = native_parameter_cells(&p, &layouts, &mut b).unwrap();
            assert_eq!(cells, [(1, 0), (1, 1)]);
            assert_eq!(cells.capacity() * std::mem::size_of::<(u32, u32)>(), 16);
            assert_eq!(b.scratch, layout_bytes + 16);
            b.release(cells);
        });
        assert_eq!(allocation.bytes_max, 20); // signature u32 index + two Cell pairs
        assert_eq!(allocation.bytes_current, 0);
        assert_eq!(b.peak, layout_bytes + 20);
        let mut small = Budget::new(
            Limits {
                max_allocation_bytes: owned + layout_bytes + 19,
                ..limits
            },
            owned,
        );
        let small_layouts = ParameterLayouts::build(&p, &mut small).unwrap();
        assert_eq!(
            native_parameter_cells(&p, &small_layouts, &mut small)
                .unwrap_err()
                .kind,
            ErrorKind::Limit
        );
        assert_eq!(small.peak, layout_bytes + 4); // failed before reserving Cell rows
        layouts.release(&mut b);
        assert_eq!(b.scratch, 0);
    }

    #[test]
    fn native_layout_work_limit_precedes_prefix_lookup_and_allocation() {
        let p = fixtures::native_parameter_defaults();
        let limits = Limits::default();
        let (_, owned, _) = super::super::super::codec::measure(&p, limits).unwrap();
        let mut b = Budget::new(limits, owned);
        let layouts = ParameterLayouts::build(&p, &mut b).unwrap();
        let mut small = Budget::new(
            Limits {
                max_work: 9,
                ..limits
            },
            owned,
        );
        let allocation = allocation_counter::measure(|| {
            assert_eq!(
                native_parameter_cells(&p, &layouts, &mut small)
                    .unwrap_err()
                    .kind,
                ErrorKind::Limit
            );
        });
        assert_eq!(small.peak, 4); // only bounded signature normalization
        assert_eq!(
            allocation.bytes_max,
            4 + "verification work limit".len() as u64
        );
        layouts.release(&mut b);
    }
}
