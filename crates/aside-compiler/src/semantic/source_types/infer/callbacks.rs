//! Source-only parameter shapes supplied by a host callback contract.
use super::*;
use crate::semantic::{ExternalSignature, SequenceCallbackArgument, SequenceCallbackContext};

impl Inference<'_> {
    pub(super) fn contextualize_callbacks(
        &mut self,
        m: usize,
        arguments: NodeId,
        context: SequenceCallbackContext,
        signature: &ExternalSignature,
        supplied: &[Value],
        state: &mut State,
    ) {
        let module = &self.modules[m];
        let mut bound = HashMap::new();
        let mut position = 0;
        for (argument, supplied) in module.hir.children(arguments).zip(supplied) {
            if !self.charge(1) {
                return;
            }
            let Form::Argument { name, value } = module.hir.node(argument).form else {
                return;
            };
            let name = match name {
                Some(name) => module.hir.text(name.span),
                None => {
                    let Some(parameter) = signature.parameters.get(position) else {
                        return;
                    };
                    position += 1;
                    parameter.name.as_str()
                }
            };
            if bound.insert(name, (value, supplied)).is_some() {
                return;
            }
        }

        let Some(&(provider, supplied)) = bound.get(context.provider) else {
            return;
        };
        let source = self
            .captured_provider(m, provider, state)
            .or_else(|| supplied.info.call_result.clone());
        let element = source
            .and_then(|source| match &source.ty {
                TypeHint::Array => source.element().cloned(),
                TypeHint::TypedArray(element) => Some(self.leaf((**element).clone())),
                _ => None,
            })
            .unwrap_or_else(|| self.unknown.clone());
        for callback in context.callbacks {
            let Some(&(node, _)) = bound.get(callback.parameter) else {
                continue;
            };
            let Some(function) = self.inline_closure(m, node) else {
                continue;
            };
            let definition = &module.functions[function.0 as usize];
            let Some(parameter) = definition.parameters.first() else {
                continue;
            };
            if parameter.annotation.is_some() || parameter.rest {
                continue;
            }
            let symbol = parameter.symbol;
            let ty = match callback.argument {
                SequenceCallbackArgument::Element => element.clone(),
                SequenceCallbackArgument::Record {
                    element: value,
                    index,
                } => {
                    let origin = SourceDefinition {
                        module: self.module_names[m].clone(),
                        range: module.symbols[symbol.0 as usize].declaration,
                    };
                    let fields = [(value, element.clone()), (index, self.leaf(TypeHint::Int))];
                    Arc::new(SourceType {
                        shape: Some(SourceShape::Record(
                            fields
                                .into_iter()
                                .map(|(name, value)| SourceMember {
                                    name: name.into(),
                                    value,
                                    definition: origin.clone(),
                                })
                                .collect(),
                        )),
                        ..SourceType::new(TypeHint::Table)
                    })
                }
            };
            if ty.ty == TypeHint::Any
                || self.exhausted
                || self.active.len() + self.active_globals.len() >= self.limits.max_depth
                || !self.active.insert((m, function))
            {
                continue;
            }
            let mut local = State::new();
            let value = self.materialize(ty.clone(), &mut local);
            local.bindings.insert(symbol, value);
            self.facts[m].symbols[symbol.0 as usize] = Some(ty);
            let declared = self.type_info[m][definition.return_type.0 as usize].clone();
            self.infer_function_body(
                m,
                definition.body,
                definition.kind == FunctionKind::Generator,
                declared,
                &mut local,
            );
            self.active.remove(&(m, function));
        }
    }

    fn inline_closure(&self, m: usize, node: NodeId) -> Option<FunctionId> {
        let module = &self.modules[m];
        if module.hir.node(node).kind != HirKind::Closure {
            return None;
        }
        module.facts[node.0 as usize].function
    }

    fn captured_provider(
        &mut self,
        m: usize,
        node: NodeId,
        state: &mut State,
    ) -> Option<Arc<SourceType>> {
        let id = self.inline_closure(m, node)?;
        let module = &self.modules[m];
        let body = module.functions[id.0 as usize].body;
        if !matches!(module.hir.node(body).form, Form::Name { .. }) {
            return None;
        }
        let symbol = module.facts[body.0 as usize].symbol?;
        let value = state.bindings.get(&symbol)?.clone();
        (!module.symbols[symbol.0 as usize].mutable).then(|| self.snapshot(&value, state))
    }
}
