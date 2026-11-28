use super::*;
use crate::bytecode as bc;
use std::collections::BTreeSet;

impl bc::VerifiedProgram {
    /// Validate current continuation metadata before it can produce executable plans.
    /// `host_types` comes from the same independent nominal registrations used for
    /// VM admission. Bound method points resolve through these declarations.
    pub fn verify_resume(
        &self,
        table: ResumeTable,
        host_types: &[crate::HostTypeSupport],
        limits: ResumeLimits,
    ) -> Result<VerifiedResumeTable, ResumeError> {
        let mut b = Budget::new(limits);
        if table.version != 1 {
            return Err(ResumeError::new("unsupported resume table version"));
        }
        contracts::normalize(self, &table, None, &mut b)?;
        let mut keys = BTreeSet::new();
        let mut functions = BTreeSet::new();
        for flow in &table.flows {
            b.work(1)?;
            b.bytes(flow.key.0.len())?;
            if flow.key.0.is_empty() || !keys.insert(&flow.key) || !functions.insert(flow.function)
            {
                return Err(ResumeError::new("duplicate or empty resume flow identity"));
            }
        }
        let point_count = table
            .flows
            .iter()
            .try_fold(0usize, |count, flow| count.checked_add(flow.points.len()))
            .ok_or_else(|| ResumeError::new("resume point count"))?;
        let mut point_targets = Vec::new();
        b.bytes(
            point_count
                .checked_mul(std::mem::size_of::<FunctionId>())
                .ok_or_else(|| ResumeError::new("resume point target size"))?,
        )?;
        point_targets
            .try_reserve_exact(point_count)
            .map_err(|_| ResumeError::new("resume point target allocation"))?;
        for flow in &table.flows {
            let f = self
                .package()
                .functions
                .get(flow.function.index())
                .ok_or_else(|| ResumeError::new("resume flow function"))?;
            if f.kind != bc::FunctionKind::Plain
                || !f.captures.is_empty()
                || f.context != bc::Context::Ordinary
            {
                return Err(ResumeError::new(
                    "resume flow requires an ordinary plain function without captures",
                )
                .at(flow.function, Pc(0)));
            }
            if flow.points.is_empty() && flow.returns.is_empty() {
                continue;
            }
            let analysis = analysis::prepare_flow(self, flow.function, &mut b)
                .map_err(|error| error.at(flow.function, Pc(0)))?;
            b.bytes(flow.layouts.len())?;
            let mut checked_layouts = Vec::new();
            checked_layouts
                .try_reserve_exact(flow.layouts.len())
                .map_err(|_| ResumeError::new("resume layout verification allocation"))?;
            checked_layouts.resize(flow.layouts.len(), false);
            let mut sites = BTreeSet::new();
            for point in &flow.points {
                if point.key.0.is_empty() || !sites.insert(point.key.0.as_str()) {
                    return Err(ResumeError::new("duplicate or empty point key"));
                }
                let site = analysis.site_in(Some(point.prepare), point.call, host_types, &mut b)?;
                check_site(
                    self,
                    flow,
                    &site,
                    (point.after_call, point.result),
                    point.layout,
                    &mut checked_layouts,
                )?;
                let target = analysis.target_in(point.call, host_types, &mut b)?;
                let function = &self.package().functions[target.index()];
                if !matches!(function.kind, bc::FunctionKind::Native(_))
                    || self.package().signatures[function.signature.index()]
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
                point_targets.push(target);
            }
            for ret in &flow.returns {
                if ret.key.0.is_empty() || !sites.insert(ret.key.0.as_str()) {
                    return Err(ResumeError::new("duplicate or empty point key"));
                }
                let site = analysis.site_in(None, ret.call, host_types, &mut b)?;
                check_site(
                    self,
                    flow,
                    &site,
                    (ret.after_call, ret.result),
                    ret.layout,
                    &mut checked_layouts,
                )?;
                let target = analysis.target_in(ret.call, host_types, &mut b)?;
                b.work(u64::from(functions.len().max(1).ilog2()) + 1)?;
                if target != ret.callee || !functions.contains(&target) {
                    return Err(ResumeError::new(
                        "return site callee differs from current static call",
                    )
                    .at(flow.function, ret.call));
                }
            }
            b.bytes -= analysis.capacity_bytes + flow.layouts.len();
        }
        VerifiedResumeTable::new(self.clone(), table, &point_targets, &mut b)
    }
}
fn check_site(
    program: &bc::VerifiedProgram,
    flow: &ResumeFlow,
    derived: &SiteLayout,
    (after, result): (Pc, Reg),
    index: u32,
    checked_layouts: &mut [bool],
) -> Result<(), ResumeError> {
    let function = flow.function;
    let layout = flow
        .layouts
        .get(index as usize)
        .ok_or_else(|| ResumeError::new("resume layout index").at(function, after))?;
    if derived.after_call != after
        || derived.result != result
        || derived.cells != layout.cells
        || derived.initialized_dead != layout.initialized_dead
    {
        return Err(ResumeError::new(
            "resume call result, PC or lexical-cell layout differs from current code",
        )
        .at(function, after));
    }
    if derived
        .live
        .iter()
        .copied()
        .ne(layout.bindings.iter().map(|binding| binding.binding))
    {
        return Err(
            ResumeError::new("resume live-binding layout differs from current code")
                .at(function, after),
        );
    }
    let checked = &mut checked_layouts[index as usize];
    if *checked {
        return Ok(());
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
    *checked = true;
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
/// Resolve the statically proved native or script target of a current call or await.
/// This query produces metadata only; execution still requires a verified plan.
/// Host member calls require a typed receiver and an independently registered method.
pub fn static_call_target(
    program: &bc::VerifiedProgram,
    function: FunctionId,
    pc: Pc,
    host_types: &[crate::HostTypeSupport],
) -> Result<FunctionId, ResumeError> {
    current_target(
        program,
        function,
        pc,
        host_types,
        &mut Budget::new(ResumeLimits::default()),
        None,
    )
}
pub(crate) fn current_target(
    program: &bc::VerifiedProgram,
    function: FunctionId,
    pc: Pc,
    host_types: &[crate::HostTypeSupport],
    b: &mut Budget,
    block_entry: Option<usize>,
) -> Result<FunctionId, ResumeError> {
    let f = program
        .package()
        .functions
        .get(function.index())
        .ok_or_else(|| ResumeError::new("resume function index"))?;
    let instruction = f
        .code
        .instruction(pc.index())
        .map_err(|e| ResumeError::new(e.to_string()))?
        .ok_or_else(|| ResumeError::new("resume point PC"))?;
    if instruction.opcode == bc::Opcode::Await as u16 {
        return super::targets::await_target(
            program,
            f,
            Reg(instruction.operands[1]),
            pc,
            host_types,
            b,
        );
    }
    super::targets::static_target(
        program,
        f,
        call_descriptor(f, pc)?.target,
        pc,
        block_entry,
        host_types,
        b,
    )
}
/// Resolve an immutable module function through the shared current-code proof.
pub fn static_global_function(
    program: &bc::VerifiedProgram,
    module: ModuleId,
    global: GlobalId,
) -> Result<FunctionId, ResumeError> {
    super::targets::global_target(
        program,
        module,
        global,
        &[],
        &mut Budget::new(ResumeLimits::default()),
    )
}
