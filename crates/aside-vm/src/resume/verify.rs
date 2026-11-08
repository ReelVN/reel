use super::*;
use crate::bytecode as bc;
use std::collections::BTreeSet;

impl bc::VerifiedProgram {
    /// Validate current continuation metadata before it can produce executable plans.
    pub fn verify_resume(
        &self,
        table: ResumeTable,
        limits: ResumeLimits,
    ) -> Result<VerifiedResumeTable, ResumeError> {
        let mut b = Budget::new(limits);
        if table.version != 1 {
            return Err(ResumeError::new("unsupported resume table version"));
        }
        let mut keys = BTreeSet::new();
        let mut functions = BTreeSet::new();
        for flow in &table.flows {
            b.work(1)?;
            b.bytes(flow.key.0.len())?;
            if flow.key.0.is_empty() || !keys.insert(&flow.key) || !functions.insert(flow.function)
            {
                return Err(ResumeError::new("duplicate or empty resume flow identity"));
            }
            let f = self
                .package()
                .functions
                .get(flow.function.index())
                .ok_or_else(|| ResumeError::new("resume flow function"))?;
            if f.kind != bc::FunctionKind::Plain
                || !f.captures.is_empty()
                || !f.regions.is_empty()
                || f.context != bc::Context::Ordinary
            {
                return Err(ResumeError::new("resume flow requires an ordinary plain function without captures or exception regions").at(flow.function, Pc(0)));
            }
            let mut points = BTreeSet::new();
            let mut returns = BTreeSet::new();
            for point in &flow.points {
                if point.key.0.is_empty() || !points.insert(&point.key) {
                    return Err(ResumeError::new("duplicate or empty point key"));
                }
                let site = analysis::analyze(
                    self,
                    flow.function,
                    Some(point.prepare),
                    point.call,
                    &mut b,
                )?;
                check_site(
                    self,
                    flow.function,
                    &site,
                    point.after_call,
                    point.result,
                    &point.layout,
                )?;
                let call = call_descriptor(f, point.call)?;
                let target =
                    super::targets::static_target(self, f, call.target, Some(point.call), &mut b)?;
                let target = &self.package().functions[target.index()];
                if !matches!(target.kind, bc::FunctionKind::Native(_))
                    || self.package().signatures[target.signature.index()]
                        .effects
                        .0
                        & bc::Effects::WAIT
                        == 0
                {
                    return Err(ResumeError::new(
                        "point must call a statically known native waiting operation",
                    )
                    .at(flow.function, point.call));
                }
            }
            for ret in &flow.returns {
                if ret.key.0.is_empty() || !returns.insert(&ret.key) {
                    return Err(ResumeError::new("duplicate or empty return key"));
                }
                let site = analysis::analyze(self, flow.function, None, ret.call, &mut b)?;
                check_site(
                    self,
                    flow.function,
                    &site,
                    ret.after_call,
                    ret.result,
                    &ret.layout,
                )?;
                let target = super::targets::static_target(
                    self,
                    f,
                    call_descriptor(f, ret.call)?.target,
                    Some(ret.call),
                    &mut b,
                )?;
                let callee = table
                    .flows
                    .iter()
                    .find(|candidate| candidate.key == ret.callee)
                    .ok_or_else(|| ResumeError::new("return site names an unknown flow"))?;
                if target != callee.function {
                    return Err(ResumeError::new(
                        "return site callee differs from current static call",
                    )
                    .at(flow.function, ret.call));
                }
            }
        }
        Ok(VerifiedResumeTable::new(self.clone(), table))
    }
}
fn check_site(
    program: &bc::VerifiedProgram,
    function: FunctionId,
    derived: &SiteLayout,
    after: Pc,
    result: Reg,
    layout: &ResumeLayout,
) -> Result<(), ResumeError> {
    if derived.after_call != after || derived.result != result || derived.cells != layout.cells {
        return Err(ResumeError::new(
            "resume call result, PC or lexical-cell layout differs from current code",
        )
        .at(function, after));
    }
    if derived.live
        != layout
            .bindings
            .iter()
            .map(|binding| binding.binding)
            .collect::<Vec<_>>()
    {
        return Err(
            ResumeError::new("resume live-binding layout differs from current code")
                .at(function, after),
        );
    }
    let f = &program.package().functions[function.index()];
    let mut names = BTreeSet::new();
    for binding in &layout.bindings {
        if !names.insert(&binding.key) {
            return Err(ResumeError::new(
                "live resume binding names must be unique at the boundary",
            )
            .at(function, after));
        }
        let def = &f.bindings[binding.binding.index()];
        if binding.key.0 != program.package().symbols[def.name.index()] {
            return Err(
                ResumeError::new("resume binding key must match its unique source name")
                    .at(function, after),
            );
        }
        crate::typed_data::data_type(program.package(), def.ty).map_err(|e| {
            ResumeError::new(format!("binding {} is not persistable: {e}", binding.key.0))
                .at(function, after)
        })?;
    }
    Ok(())
}
pub(crate) fn call_descriptor(
    f: &bc::RuntimeFunction,
    pc: Pc,
) -> Result<&bc::CallDesc, ResumeError> {
    let instruction = f
        .code
        .instruction(pc.index())
        .map_err(|e| ResumeError::new(e.to_string()))?
        .ok_or_else(|| ResumeError::new("resume call PC"))?;
    if !matches!(
        bc::instruction_spec(instruction.opcode).unwrap().opcode,
        bc::Opcode::Call | bc::Opcode::CallNoWait
    ) {
        return Err(ResumeError::new("resume PC does not identify a call"));
    }
    match &f.descriptors[instruction.operands[1] as usize] {
        bc::Descriptor::Call(call) => Ok(call),
        _ => Err(ResumeError::new("resume call descriptor")),
    }
}
/// Resolve the statically proved target of one current call instruction.
/// This query produces metadata only; execution still requires a verified plan.
pub fn static_call_target(
    program: &bc::VerifiedProgram,
    function: FunctionId,
    pc: Pc,
) -> Result<FunctionId, ResumeError> {
    let f = program
        .package()
        .functions
        .get(function.index())
        .ok_or_else(|| ResumeError::new("resume function index"))?;
    super::targets::static_target(
        program,
        f,
        call_descriptor(f, pc)?.target,
        Some(pc),
        &mut Budget::new(ResumeLimits::default()),
    )
}
