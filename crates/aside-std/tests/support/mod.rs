use aside_host::{aside_compiler::semantic::*, source::Api};

pub struct Resolver {
    modules: MemoryResolver,
    api: &'static Api,
}

pub fn resolver() -> Resolver {
    let api = aside_std::api();
    let mut modules = MemoryResolver::default();
    for module in &api.modules {
        modules.insert(module.clone());
    }
    Resolver { modules, api }
}

impl ModuleResolver for Resolver {
    fn resolve(&mut self, request: &ImportRequest) -> Result<ResolvedImport, ResolveError> {
        self.modules.resolve(request)
    }
    fn resolve_type_member(&mut self, ty: &str, member: &str) -> Option<ExternalMember> {
        self.api.member(ty, member)
    }
    fn is_named_subtype(&self, actual: &str, expected: &str) -> Option<bool> {
        self.api.is_subtype(actual, expected)
    }
}
