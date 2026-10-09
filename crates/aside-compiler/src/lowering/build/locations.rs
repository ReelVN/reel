//! Prepared positions keep receiver values live until the final read/write.
use super::*;

#[derive(Clone, Copy)]
enum PreparedLocation {
    Name {
        symbol: h::SymbolId,
        node: h::NodeId,
    },
    Index {
        object: bc::Reg,
        key: bc::Reg,
        node: h::NodeId,
    },
    Member {
        object: bc::Reg,
        symbol: bc::SymbolId,
        node: h::NodeId,
    },
}

impl FunctionBuilder<'_, '_> {
    fn prepare_location(
        &mut self,
        mut n: h::NodeId,
        mut depth: usize,
    ) -> Result<Option<PreparedLocation>, Error> {
        loop {
            self.l.b.span = self.span(n);
            self.l.b.work(1)?;
            if depth >= self.l.b.limits.max_depth.min(128) {
                return Err(self
                    .l
                    .b
                    .error(ErrorKind::Limit, "location lowering depth exceeded"));
            }
            match (self.l.m.hir().node(n).kind, self.l.m.hir().node(n).form) {
                (H::Group, Form::Single { child }) => {
                    n = child;
                    depth += 1;
                }
                (H::Name, _) => {
                    let symbol = self.l.m.facts()[n.0 as usize].symbol.ok_or_else(|| {
                        self.l
                            .b
                            .error(ErrorKind::InvalidIr, "assignment target identity")
                    })?;
                    return Ok(Some(PreparedLocation::Name { symbol, node: n }));
                }
                (
                    H::Index,
                    Form::Index {
                        object,
                        index,
                        optional: false,
                        chain: false,
                    },
                ) => {
                    let Some(object) = self.value(object, depth + 1)? else {
                        return Ok(None);
                    };
                    let Some(key) = self.value(index, depth + 1)? else {
                        self.release(object)?;
                        return Ok(None);
                    };
                    return Ok(Some(PreparedLocation::Index {
                        object,
                        key,
                        node: n,
                    }));
                }
                (H::Index, _) => {
                    return Err(self.l.b.error(
                        ErrorKind::Unsupported,
                        "optional index/chain awaits optional-chain lowering",
                    ));
                }
                (
                    H::Member,
                    Form::Member {
                        object,
                        key,
                        optional: false,
                        chain: false,
                    },
                ) => {
                    let symbol =
                        self.l.name(self.l.m.hir().name(key.name.ok_or_else(|| {
                            self.l.b.error(ErrorKind::InvalidIr, "member name identity")
                        })?))?;
                    let Some(object) = self.value(object, depth + 1)? else {
                        return Ok(None);
                    };
                    // Capture the object value and name, never a slot or setter.
                    return Ok(Some(PreparedLocation::Member {
                        object,
                        symbol,
                        node: n,
                    }));
                }
                (H::Member, _) => {
                    return Err(self.l.b.error(
                        ErrorKind::Unsupported,
                        "optional member/chain awaits optional-chain lowering",
                    ));
                }
                _ => {
                    return Err(self.l.b.error(
                        ErrorKind::Unsupported,
                        "destructuring/pattern assignment awaits pattern lowering",
                    ));
                }
            }
        }
    }
    fn read_location(&mut self, location: PreparedLocation) -> Result<bc::Reg, Error> {
        match location {
            PreparedLocation::Name { symbol, node } => {
                self.l.b.span = self.span(node);
                self.read(symbol, node)
            }
            PreparedLocation::Index { object, key, node } => {
                self.l.b.span = self.span(node);
                let out = self.temp()?;
                self.op(bc::Opcode::IndexGet, out.0, object.0, key.0, node)?;
                Ok(out)
            }
            PreparedLocation::Member {
                object,
                symbol,
                node,
            } => {
                self.l.b.span = self.span(node);
                let out = self.temp()?;
                // B01 binds class methods only; callable values are not rewritten.
                self.op(bc::Opcode::MemberGet, out.0, object.0, symbol.0, node)?;
                Ok(out)
            }
        }
    }
    fn write_location(&mut self, location: PreparedLocation, value: bc::Reg) -> Result<(), Error> {
        match location {
            PreparedLocation::Name { symbol, node } => {
                self.l.b.span = self.span(node);
                self.write(symbol, value, false, node)
            }
            PreparedLocation::Index { object, key, node } => {
                self.op(bc::Opcode::IndexSet, object.0, key.0, value.0, node)
            }
            PreparedLocation::Member {
                object,
                symbol,
                node,
            } => self.op(bc::Opcode::MemberSet, object.0, value.0, symbol.0, node),
        }
    }
    fn release_location(&mut self, location: PreparedLocation) -> Result<(), Error> {
        match location {
            PreparedLocation::Index { object, key, node } => {
                self.l.b.span = self.span(node);
                self.release(object)?;
                self.release(key)?;
            }
            PreparedLocation::Member { object, node, .. } => {
                self.l.b.span = self.span(node);
                self.release(object)?;
            }
            PreparedLocation::Name { .. } => (),
        }
        Ok(())
    }
    fn release_locations(&mut self, locations: Vec<PreparedLocation>) -> Result<(), Error> {
        self.l.b.work(locations.len() as u64)?;
        for &location in &locations {
            self.release_location(location)?;
        }
        self.l.b.release(locations);
        Ok(())
    }
    pub(super) fn position_value(
        &mut self,
        n: h::NodeId,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Error> {
        let Some(location) = self.prepare_location(n, depth)? else {
            return Ok(None);
        };
        let out = self.read_location(location)?;
        self.release_location(location)?;
        Ok(Some(out))
    }
    pub(super) fn assignment(&mut self, n: h::NodeId, depth: usize) -> Result<(), Error> {
        let mut targets = Vec::new();
        let mut current = n;
        let (rhs, compound) = loop {
            self.l.b.span = self.span(current);
            self.l.b.work(1)?;
            let Form::Binary {
                left,
                operator,
                right,
            } = self.l.m.hir().node(current).form
            else {
                return Err(self.l.b.error(ErrorKind::InvalidIr, "assignment form"));
            };
            let Some(location) = self.prepare_location(left, depth)? else {
                self.release_locations(targets)?;
                return Ok(());
            };
            self.l.b.span = self.span(left);
            self.l.b.push(&mut targets, location)?;
            if operator.kind == K::Eq && self.l.m.hir().node(right).kind == H::Assignment {
                current = right;
                continue;
            }
            break (right, (operator.kind != K::Eq).then_some(operator.kind));
        };
        let value = if let Some(operator) = compound {
            self.l.b.span = self.span(current);
            let opcode = binary(operator)
                .ok_or_else(|| self.l.b.error(ErrorKind::Unsupported, "compound operator"))?;
            let left = self.read_location(*targets.last().expect("assignment target"))?;
            let Some(right) = self.value(rhs, depth)? else {
                self.release(left)?;
                self.release_locations(targets)?;
                return Ok(());
            };
            let out = self.temp()?;
            self.op(opcode, out.0, left.0, right.0, current)?;
            self.release(left)?;
            self.release(right)?;
            out
        } else {
            let Some(value) = self.value(rhs, depth)? else {
                self.release_locations(targets)?;
                return Ok(());
            };
            value
        };
        self.l.b.work(targets.len() as u64)?;
        for &location in targets.iter().rev() {
            self.write_location(location, value)?;
        }
        self.release(value)?;
        self.release_locations(targets)?;
        Ok(())
    }
}
