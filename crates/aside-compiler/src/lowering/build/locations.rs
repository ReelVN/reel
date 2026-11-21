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
    pub(super) fn read_member(
        &mut self,
        out: bc::Reg,
        receiver: bc::Reg,
        name: bc::SymbolId,
        node: h::NodeId,
    ) -> Result<(), Box<Error>> {
        if let Some((ty, optional)) = self.record_member_type(node) {
            let key = self.l.b.string(&self.l.package.symbols[name.index()])?;
            let key = self.constant(bc::Const::Utf8(key), node)?;
            if optional {
                self.op(bc::Opcode::PatternField, key.0, receiver.0, out.0, node)?;
                if self.l.m.types()[ty.0 as usize] != h::Type::Any {
                    let present = self.block()?;
                    let join = self.block()?;
                    self.terminate(
                        Terminator::BranchBool { value: key, yes: present, no: join },
                        node,
                    )?;
                    self.release(key)?;
                    self.start(present)?;
                    self.check_member_value(out, ty, node)?;
                    self.terminate(Terminator::Jump(join), node)?;
                    self.start(join)?;
                } else {
                    self.release(key)?;
                }
                Ok(())
            } else {
                self.op(bc::Opcode::IndexGet, out.0, receiver.0, key.0, node)?;
                self.release(key)?;
                self.check_member_value(out, ty, node)
            }
        } else {
            self.op(bc::Opcode::MemberGet, out.0, receiver.0, name.0, node)
        }
    }

    fn record_member_type(&self, node: h::NodeId) -> Option<(h::TypeId, bool)> {
        let m = self.l.m;
        if m.facts()[node.0 as usize].checks.0 & h::RuntimeChecks::RECORD_FIELD == 0 {
            return None;
        }
        let Form::Member { object, key, .. } = m.hir().node(node).form else {
            unreachable!("bound record field");
        };
        let fields = h::record_fields(m.types(), m.facts()[object.0 as usize].ty)?;
        let name = key.name?;
        fields
            .binary_search_by_key(&name, |&(key, _)| key)
            .ok()
            .map(|i| h::field_contract(m.types(), fields[i].1))
    }

    fn check_member_value(
        &mut self,
        value: bc::Reg,
        ty: h::TypeId,
        node: h::NodeId,
    ) -> Result<(), Box<Error>> {
        if self.l.m.types()[ty.0 as usize] != h::Type::Any {
            let ty = self.l.ty(ty, 0)?;
            self.op(bc::Opcode::CheckType, value.0, ty.0, 0, node)?;
        }
        Ok(())
    }
    pub(super) fn value_of(
        &mut self,
        node: h::NodeId,
        arguments: Option<h::NodeId>,
        children: Option<h::NodeId>,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        let argument = arguments.and_then(|arguments| {
            let mut arguments = self.l.m.hir().children(arguments);
            let argument = arguments.next()?;
            (arguments.next().is_none()).then_some(argument)
        });
        let Some(Form::Argument { value, .. }) = argument
            .map(|id| self.l.m.hir().node(id).form)
            .filter(|_| children.is_none())
        else {
            return Err(self
                .l
                .b
                .error(ErrorKind::Unsupported, "value_of requires one value"));
        };
        let Some(value) = self.value(value, depth + 1)? else {
            return Ok(None);
        };
        let out = self.temp()?;
        self.op(bc::Opcode::Move, out.0, value.0, 0, node)?;
        let check = self.temp()?;
        let binding = self.l.builtin(bc::BuiltinType::PropertyRef)?;
        self.op(bc::Opcode::Is, check.0, value.0, binding.0, node)?;
        let read = self.block()?;
        let join = self.block()?;
        self.terminate(
            Terminator::BranchBool {
                value: check,
                yes: read,
                no: join,
            },
            node,
        )?;
        self.release(check)?;
        self.start(read)?;
        let name = self.l.name("value")?;
        self.op(bc::Opcode::MemberGet, out.0, value.0, name.0, node)?;
        self.terminate(Terminator::Jump(join), node)?;
        self.start(join)?;
        self.release(value)?;
        Ok(Some(out))
    }

    pub(super) fn property_reference(
        &mut self,
        node: h::NodeId,
        arguments: Option<h::NodeId>,
        children: Option<h::NodeId>,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        let argument = arguments.and_then(|a| {
            let mut args = self.l.m.hir().children(a);
            let first = args.next()?;
            if args.next().is_some() {
                None
            } else {
                Some(first)
            }
        });
        let value = match argument.map(|a| self.l.m.hir().node(a).form) {
            Some(Form::Argument { name: None, value }) if children.is_none() => value,
            _ => {
                return Err(self.l.b.error(
                    ErrorKind::Unsupported,
                    "bind requires one positional property location",
                ));
            }
        };
        let Some(location) = self.prepare_location(value, depth + 1)? else {
            return Ok(None);
        };
        let (object, key) = match location {
            PreparedLocation::Index { object, key, .. } => (object, key),
            PreparedLocation::Member { object, symbol, .. } => {
                let name = self.l.package.symbols[symbol.index()].clone();
                (object, self.constant(bc::Const::Utf8(name), node)?)
            }
            PreparedLocation::Name { .. } => {
                return Err(self
                    .l
                    .b
                    .error(ErrorKind::Unsupported, "bind requires a property location"));
            }
        };
        let descriptor = self.descriptor(bc::Descriptor::Property {
            key,
            writable: true,
        })?;
        let out = self.temp()?;
        self.op(bc::Opcode::PropertyRef, out.0, object.0, descriptor.0, node)?;
        self.release(object)?;
        self.release(key)?;
        Ok(Some(out))
    }

    fn prepare_location(
        &mut self,
        mut n: h::NodeId,
        mut depth: usize,
    ) -> Result<Option<PreparedLocation>, Box<Error>> {
        loop {
            self.l.b.span = self.span(n);
            self.l.b.work(1)?;
            if depth >= self.l.b.limits.max_depth {
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
                        "optional chain assignment target is unsupported",
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
                        "optional chain assignment target is unsupported",
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
    fn read_location(&mut self, location: PreparedLocation) -> Result<bc::Reg, Box<Error>> {
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
                self.read_member(out, object, symbol, node)?;
                Ok(out)
            }
        }
    }
    fn write_location(
        &mut self,
        location: PreparedLocation,
        value: bc::Reg,
    ) -> Result<(), Box<Error>> {
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
            } => {
                if let Some((ty, _)) = self.record_member_type(node) {
                    self.check_member_value(value, ty, node)?;
                }
                self.op(bc::Opcode::MemberSet, object.0, value.0, symbol.0, node)
            }
        }
    }
    fn release_location(&mut self, location: PreparedLocation) -> Result<(), Box<Error>> {
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
    fn release_locations(&mut self, locations: Vec<PreparedLocation>) -> Result<(), Box<Error>> {
        self.l.b.work(locations.len() as u64)?;
        for &location in &locations {
            self.release_location(location)?;
        }
        self.l.b.release(locations);
        Ok(())
    }
    pub(super) fn assignment(&mut self, n: h::NodeId, depth: usize) -> Result<(), Box<Error>> {
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
