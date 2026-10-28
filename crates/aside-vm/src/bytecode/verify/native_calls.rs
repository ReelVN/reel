//! Immutable argument permutations derived from verified fixed native calls.
use super::*;

#[derive(Debug)]
pub(super) struct NativeCallPlans {
    // Sorted by (function, descriptor); slots preserve source argument order.
    sites: Vec<(u32, u32, u32, u32)>,
    slots: Vec<u32>,
    #[cfg(test)]
    positional: bool,
}
impl NativeCallPlans {
    pub(super) fn capacity_bytes(&self) -> usize {
        self.sites.capacity() * size_of::<(u32, u32, u32, u32)>()
            + self.slots.capacity() * size_of::<u32>()
    }
}
impl VerifiedProgram {
    pub(crate) fn native_call_lookup_work(&self) -> u64 {
        1 + self.0.native_calls.sites.len().checked_ilog2().unwrap_or(0) as u64
    }
    #[cfg(test)]
    pub(crate) fn native_call_capacity_bytes(&self) -> usize {
        self.0.native_calls.capacity_bytes()
    }
    #[cfg(test)]
    pub(crate) fn without_native_call_plans(mut self) -> Self {
        let calls = &mut std::sync::Arc::get_mut(&mut self.0).unwrap().native_calls;
        calls.sites.clear();
        calls.positional = false;
        self
    }

    #[cfg(test)]
    pub(crate) fn positional_native_plans(&self) -> bool {
        self.0.native_calls.positional
    }

    pub(crate) fn native_call_plan(
        &self,
        function: FunctionId,
        descriptor: DescriptorId,
    ) -> Option<u32> {
        self.0
            .native_calls
            .sites
            .binary_search_by_key(&(function.0, descriptor.0), |s| (s.0, s.1))
            .ok()
            .map(|i| i as u32)
    }
    pub(crate) fn native_call_identity(
        &self,
        plan: u32,
    ) -> Option<(FunctionId, DescriptorId, NativeId)> {
        let &(function, descriptor, _, _) = self.0.native_calls.sites.get(plan as usize)?;
        let Descriptor::Call(call) =
            &self.0.package.functions[function as usize].descriptors[descriptor as usize]
        else {
            unreachable!()
        };
        let native = match call.target {
            CallTarget::Native(id) => id,
            CallTarget::Function(id) => match self.0.package.functions[id.index()].kind {
                FunctionKind::Native(id) => id,
                _ => unreachable!(),
            },
            _ => unreachable!(),
        };
        Some((FunctionId(function), DescriptorId(descriptor), native))
    }
    pub(crate) fn native_call_slots(&self, plan: u32) -> Option<&[u32]> {
        let &(_, _, start, count) = self.0.native_calls.sites.get(plan as usize)?;
        self.0
            .native_calls
            .slots
            .get(start as usize..(start + count) as usize)
    }
}
fn candidate<'a>(
    p: &'a BytecodePackage<impl InstructionSequence, impl SourceMapSequence>,
    d: &'a Descriptor,
    cells: &[(u32, u32)],
) -> Option<(&'a CallDesc, RegWindow, &'a Signature)> {
    let Descriptor::Call(call) = d else {
        return None;
    };
    if call.contract.is_some() {
        return None;
    }
    let Arguments::Window(window) = call.arguments else {
        return None;
    };
    let native = match call.target {
        CallTarget::Native(id) => id,
        CallTarget::Function(id) => match p.functions[id.index()].kind {
            FunctionKind::Native(id) => id,
            _ => return None,
        },
        _ => return None,
    };
    let signature = p.natives[native.index()].signature;
    let sig = &p.signatures[signature.index()];
    if sig.rest.is_some()
        || sig.property_bag_tail.is_some()
        || cells.binary_search_by_key(&signature.0, |x| x.0).is_ok()
    {
        return None;
    }
    Some((call, window, sig))
}
pub(super) fn build(
    p: &BytecodePackage<impl InstructionSequence, impl SourceMapSequence>,
    cells: &[(u32, u32)],
    b: &mut Budget,
) -> Result<NativeCallPlans, Error> {
    let mut sites = 0usize;
    let mut slots = 0usize;
    for f in &p.functions {
        for d in &f.descriptors {
            b.work(2 + cells.len().checked_ilog2().unwrap_or(0) as u64)?;
            if let Some((_, w, _)) = candidate(p, d, cells) {
                sites += 1;
                slots = slots
                    .checked_add(w.len as usize)
                    .ok_or_else(|| Error::new(ErrorKind::Limit, "native call plan"))?;
            }
        }
    }
    u32::try_from(slots).map_err(|_| Error::new(ErrorKind::Limit, "native call slot count"))?;
    let mut plans = NativeCallPlans {
        #[cfg(test)]
        positional: true,
        sites: b.vec(sites)?,
        slots: b.vec(slots)?,
    };
    for (fi, f) in p.functions.iter().enumerate() {
        for (di, d) in f.descriptors.iter().enumerate() {
            b.work(2 + cells.len().checked_ilog2().unwrap_or(0) as u64)?;
            let Some((call, w, sig)) = candidate(p, d, cells) else {
                continue;
            };
            b.work(w.len as u64 + call.names.len() as u64 * sig.parameters.len() as u64)?;
            let start = u32::try_from(plans.slots.len())
                .map_err(|_| Error::new(ErrorKind::Limit, "native call slots"))?;
            let positional = w.len as usize - call.names.len();
            plans.slots.extend(0..positional as u32);
            for name in &call.names {
                plans.slots.push(
                    sig.parameters
                        .iter()
                        .position(|p| p.name == name.name)
                        .expect("verified native argument") as u32,
                );
            }
            plans.sites.push((fi as u32, di as u32, start, w.len));
        }
    }
    Ok(plans)
}
