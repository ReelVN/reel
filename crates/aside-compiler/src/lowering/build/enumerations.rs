//! Known enum constructors use ordinary record and tuple storage.
use super::*;
use crate::semantic::CompileConstant;

impl FunctionBuilder<'_, '_> {
    pub(super) fn enum_value(
        &mut self,
        node: h::NodeId,
        callee: h::NodeId,
        arguments: Option<h::NodeId>,
        depth: usize,
    ) -> Result<Option<bc::Reg>, Box<Error>> {
        let binding = self
            .l
            .m
            .external_binding(None, Some(callee))
            .expect("known enum case");
        let Some(CompileConstant::Utf8(tag)) = binding.descriptor.constant.as_deref() else {
            unreachable!("enum tag");
        };
        let signature = binding
            .descriptor
            .signature
            .as_ref()
            .expect("enum payload signature");
        let ty = self.l.m.facts()[node.0 as usize].ty;
        let h::Type::Enum(_, cases) = &self.l.m.types()[ty.0 as usize] else {
            unreachable!("enum constructor type");
        };
        let record = cases.iter().find_map(|case| {
            if let h::Type::Tuple(parts) = &self.l.m.types()[case.0 as usize]
                && matches!(&self.l.m.types()[parts[0].0 as usize], h::Type::StringLiteral(text) if text == tag)
                && let h::Type::Record(fields) = &self.l.m.types()[parts[1].0 as usize]
            {
                return Some(fields);
            }
            None
        }).expect("enum payload contract");

        let mut fields = Vec::new();
        self.l.b.reserve(&mut fields, record.len())?;
        fields.extend(
            record
                .iter()
                .map(|&(name, ty)| (self.l.m.hir().name(name), ty)),
        );
        fields.sort_unstable_by_key(|&(name, _)| name);
        let width = u32::try_from(fields.len())
            .ok()
            .and_then(|value| value.checked_mul(2))
            .ok_or_else(|| {
                self.l
                    .b
                    .error(ErrorKind::Limit, "enum payload window length overflow")
            })?;
        let window = self.registers.get(width, &mut self.l.b)?;
        let mut positional = 0;
        for argument in arguments
            .into_iter()
            .flat_map(|arguments| self.l.m.hir().children(arguments))
        {
            let Form::Argument { name, value } = self.l.m.hir().node(argument).form else {
                unreachable!("enum payload argument");
            };
            let name = if let Some(name) = name {
                self.l.m.hir().text(name.span)
            } else {
                let name = &signature.parameters[positional].name;
                positional += 1;
                name
            };
            let index = fields
                .binary_search_by_key(&name, |&(name, _)| name)
                .expect("validated enum argument name");
            let Some(value) = self.value(value, depth)? else {
                self.release_window(window, node)?;
                self.l.b.release(fields);
                return Ok(None);
            };
            let slot = window.start.0 + index as u32 * 2 + 1;
            self.op(bc::Opcode::Move, slot, value.0, 0, argument)?;
            self.release(value)?;
        }
        let mut pairs = Vec::new();
        self.l.b.reserve(&mut pairs, fields.len())?;
        for (index, &(name, ty)) in fields.iter().enumerate() {
            let key = bc::Reg(window.start.0 + index as u32 * 2);
            let value = bc::Reg(key.0 + 1);
            let ty = self.l.ty(ty, 0)?;
            self.op(bc::Opcode::CheckType, value.0, ty.0, 0, node)?;
            let name = self.l.b.string(name)?;
            let name = self.constant_id(bc::Const::Utf8(name))?;
            self.op(bc::Opcode::LoadConst, key.0, name.0, 0, node)?;
            self.l.b.push(&mut pairs, [key, value])?;
        }
        self.l.b.release(fields);
        let tuple = self.registers.get(2, &mut self.l.b)?;
        let payload = self.descriptor(bc::Descriptor::Table(pairs))?;
        self.op(bc::Opcode::MakeTable, tuple.start.0 + 1, payload.0, 0, node)?;
        self.release_window(window, node)?;
        let tag = self.l.b.string(tag)?;
        let tag = self.constant_id(bc::Const::Utf8(tag))?;
        self.op(bc::Opcode::LoadConst, tuple.start.0, tag.0, 0, node)?;
        let descriptor = self.descriptor(bc::Descriptor::Sequence(tuple))?;
        let output = self.temp()?;
        self.op(bc::Opcode::MakeTuple, output.0, descriptor.0, 0, node)?;
        self.release_window(tuple, node)?;
        Ok(Some(output))
    }
}
