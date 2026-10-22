//! Both direct native calls and callable registrations use one preparation path.
use super::*;

pub(super) struct NativeSite {
    pub owner: RootOwner,
    pub module: ModuleInstanceId,
    pub context: ContextFlags,
    pub effects: Effects,
    pub location: Location,
    pub ret: ReturnTo,
}

impl Vm {
    pub(super) fn native_site(
        &self,
        function: FunctionId,
        owner: RootOwner,
        context: ContextFlags,
        effects: Effects,
        ret: ReturnTo,
    ) -> Result<NativeSite, VmError> {
        let caller = ret.caller();
        let (module, location) = if let Some((frame, call)) = caller {
            match self.active.frames.data[self.index(frame)?] {
                Frame::Script(s) => (s.module, self.loc(s, call.unwrap_or(s.pc))),
                Frame::Native(n) => (n.module, n.location),
            }
        } else {
            let p = self.program.package();
            (
                ModuleInstanceId(p.functions[function.index()].module.0),
                Location {
                    code: CodeRef {
                        function,
                        version: p.content_version,
                    },
                    pc: Pc(0),
                    source: self.program.source_at(function, Pc(0)),
                },
            )
        };
        Ok(NativeSite {
            owner,
            module,
            context,
            effects,
            location,
            ret,
        })
    }
}
