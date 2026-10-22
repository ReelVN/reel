//! Host call plans use the ordinary value, table, interpolation and call opcodes.
use super::*;
use crate::lowering::{CallExpansion, ExpandedValue};

pub(super) fn validate(
    module: &h::AnalyzedModule,
    expansions: &[CallExpansion],
    budget: &mut Budget,
) -> Result<(), Box<Error>> {
    let mut previous = None;
    for expansion in expansions {
        budget.work(1)?;
        let Some(node) = module.hir().nodes.get(expansion.call.0 as usize) else {
            return Err(budget.error(
                ErrorKind::LinkContract,
                "call expansion is outside source HIR",
            ));
        };
        budget.span = node.span;
        if previous.is_some_and(|last| last >= expansion.call) {
            return Err(budget.error(
                ErrorKind::LinkContract,
                "call expansions must be sorted and unique",
            ));
        }
        previous = Some(expansion.call);
        let Form::Call { callee, .. } = node.form else {
            return Err(budget.error(
                ErrorKind::LinkContract,
                "call expansion does not identify a call",
            ));
        };
        budget.work(module.external_bindings().len() as u64 + 1)?;
        let actual = module.canonical_reference(callee);
        budget.work(
            expansion.reference.module.len() as u64
                + expansion.reference.version.len() as u64
                + expansion
                    .reference
                    .export
                    .as_ref()
                    .map_or(0, |name| name.len()) as u64
                + 1,
        )?;
        if actual != Some(&expansion.reference) {
            return Err(budget.error(
                ErrorKind::LinkContract,
                "call expansion canonical identity differs from analysis",
            ));
        }
        for argument in &expansion.arguments {
            budget.work(argument.name.len() as u64 + 1)?;
            if argument.source.0 as usize >= module.hir().nodes.len() || argument.name.is_empty() {
                return Err(
                    budget.error(ErrorKind::LinkContract, "invalid call expansion argument")
                );
            }
            let nodes: &[h::NodeId] = match &argument.value {
                ExpandedValue::Expression(node) => std::slice::from_ref(node),
                ExpandedValue::Constant(_) => &[],
                ExpandedValue::Record(entries) | ExpandedValue::TextRecord(entries) => {
                    for (key, node) in entries {
                        budget.work(key.len() as u64 + 1)?;
                        if node.0 as usize >= module.hir().nodes.len() {
                            return Err(budget.error(
                                ErrorKind::LinkContract,
                                "record expression is outside source HIR",
                            ));
                        }
                    }
                    &[]
                }
            };
            if nodes
                .iter()
                .any(|node| node.0 as usize >= module.hir().nodes.len())
            {
                return Err(budget.error(
                    ErrorKind::LinkContract,
                    "argument expression is outside source HIR",
                ));
            }
        }
    }
    Ok(())
}

impl<'a> FunctionBuilder<'_, 'a> {
    pub(super) fn expansion(
        &mut self,
        call: h::NodeId,
    ) -> Result<Option<&'a CallExpansion>, Box<Error>> {
        let expansions = self.l.links.expansions;
        let (mut lo, mut hi) = (0, expansions.len());
        while lo < hi {
            self.l.b.work(1)?;
            let mid = (lo + hi) / 2;
            match expansions[mid].call.cmp(&call) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Ok(Some(&expansions[mid])),
            }
        }
        Ok(None)
    }

    pub(super) fn expanded_call(
        &mut self,
        call: h::NodeId,
        expansion: &CallExpansion,
        target: bc::CallTarget,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        let count = u32::try_from(expansion.arguments.len()).map_err(|_| {
            self.l
                .b
                .error(ErrorKind::Limit, "expanded call window overflow")
        })?;
        let window = self.registers.get(count, &mut self.l.b)?;
        let mut names = Vec::new();
        for (i, argument) in expansion.arguments.iter().enumerate() {
            self.l.b.work(1)?;
            self.l.b.span = self.span(argument.source);
            let value = match &argument.value {
                ExpandedValue::Expression(node) => self.value(*node, depth)?,
                ExpandedValue::Constant(value) => {
                    let value = match value {
                        bc::Const::Utf8(text) => bc::Const::Utf8(self.l.b.string(text)?),
                        bc::Const::Bytes(bytes) => {
                            let mut owned = Vec::new();
                            self.l.b.reserve(&mut owned, bytes.len())?;
                            self.l.b.work(bytes.len() as u64)?;
                            owned.extend_from_slice(bytes);
                            bc::Const::Bytes(owned)
                        }
                        bc::Const::Tuple(items) => {
                            let mut owned = Vec::new();
                            self.l.b.reserve(&mut owned, items.len())?;
                            self.l.b.work(items.len() as u64)?;
                            owned.extend_from_slice(items);
                            bc::Const::Tuple(owned)
                        }
                        other => other.clone(),
                    };
                    Some(self.constant(value, argument.source)?)
                }
                ExpandedValue::Record(entries) => {
                    self.expanded_record(argument.source, entries, false, depth)?
                }
                ExpandedValue::TextRecord(entries) => {
                    self.expanded_record(argument.source, entries, true, depth)?
                }
            };
            let Some(value) = value else {
                self.l.b.release(names);
                self.release_window(window, call)?;
                return Ok(None);
            };
            self.op(
                bc::Opcode::Move,
                window.start.0 + i as u32,
                value.0,
                0,
                argument.source,
            )?;
            self.release(value)?;
            let name = self.l.name(&argument.name)?;
            self.l.b.push(
                &mut names,
                bc::NamedArg {
                    name,
                    offset: i as u32,
                },
            )?;
        }
        self.l.b.span = self.span(call);
        let descriptor = self.descriptor(bc::Descriptor::Call(bc::CallDesc {
            contract: None,
            target,
            arguments: bc::Arguments::Window(window),
            names,
        }))?;
        let out = self.temp()?;
        self.op(self.call_opcode(), out.0, descriptor.0, 0, call)?;
        self.release_window(window, call)?;
        Ok(Some(out))
    }

    fn expanded_record(
        &mut self,
        source: h::NodeId,
        entries: &[(String, h::NodeId)],
        text: bool,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        let count = entries
            .len()
            .checked_mul(2)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| {
                self.l
                    .b
                    .error(ErrorKind::Limit, "expanded record window overflow")
            })?;
        let window = self.registers.get(count, &mut self.l.b)?;
        let mut pairs = Vec::new();
        for (i, (key, node)) in entries.iter().enumerate() {
            self.l.b.work(1)?;
            let key = self.l.b.string(key)?;
            let key = self.constant(bc::Const::Utf8(key), *node)?;
            let key_slot = bc::Reg(window.start.0 + i as u32 * 2);
            let value_slot = bc::Reg(key_slot.0 + 1);
            self.op(bc::Opcode::Move, key_slot.0, key.0, 0, *node)?;
            self.release(key)?;
            let Some(value) = self.value(*node, depth)? else {
                self.l.b.release(pairs);
                self.release_window(window, source)?;
                return Ok(None);
            };
            let value = if text {
                let out = self.temp()?;
                self.op(bc::Opcode::TextNew, out.0, 0, 0, *node)?;
                self.op(bc::Opcode::TextPush, out.0, value.0, 0, *node)?;
                self.release(value)?;
                self.op(bc::Opcode::TextFinish, out.0, out.0, 0, *node)?;
                out
            } else {
                value
            };
            self.op(bc::Opcode::Move, value_slot.0, value.0, 0, *node)?;
            self.release(value)?;
            self.l.b.push(&mut pairs, [key_slot, value_slot])?;
        }
        let descriptor = self.descriptor(bc::Descriptor::Table(pairs))?;
        let out = self.temp()?;
        self.op(bc::Opcode::MakeTable, out.0, descriptor.0, 0, source)?;
        self.release_window(window, source)?;
        Ok(Some(out))
    }
}
