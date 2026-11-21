use super::super::effects::{EffectCall, EffectTarget};
use super::*;

impl Binder<'_> {
    pub(super) fn collect_effect_calls(&mut self, calls: &[CallSite]) {
        if !self.project_effects
            && !calls.iter().any(|call| call.derived_forbidden || call.effect == CallEffect::StartsTask)
        {
            return;
        }
        for call in calls {
            if !self.budget(call.node) {
                break;
            }
            let Form::Call {
                callee,
                optional: false,
                chain: false,
                ..
            } = self.out.hir.node(call.node).form
            else {
                continue;
            };
            let symbol = self.out.facts[callee.0 as usize].symbol;
            let target = if let Some(function) = call.callee
                && !call.bound
                && symbol
                    .is_some_and(|id| self.out.symbols[id.0 as usize].kind == SymbolKind::Function)
                && self.out.functions[function.0 as usize].kind == FunctionKind::Plain
            {
                EffectTarget::Local(function)
            } else if !call.bound
                && let Some(descriptor) = self
                    .external_nodes
                    .get(&callee)
                    .or_else(|| symbol.and_then(|symbol| self.external.get(&symbol)))
                && descriptor.kind == ExternalBindingKind::Reference
                && let Some(reference) = &descriptor.reference
                && reference.export.is_some()
            {
                EffectTarget::Reference(reference.clone())
            } else if call.effect == CallEffect::Unknown && !call.derived_forbidden {
                continue;
            } else {
                EffectTarget::Host
            };
            self.out.effect_calls.push(EffectCall {
                target,
                node: call.node,
                caller: call.caller,
                effect: call.effect,
                derived_forbidden: call.derived_forbidden,
                no_wait: call.no_wait && !call.conditional,
                derived: call.derived && !call.conditional,
            });
        }
    }
}
