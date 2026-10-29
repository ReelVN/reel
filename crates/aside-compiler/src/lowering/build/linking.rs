//! Native and source import linkage.
use super::*;

impl Lowerer<'_> {
    pub(super) fn check_links(&mut self) -> Result<(), Box<Error>> {
        self.b.work(self.links.natives.len() as u64)?;
        for link in self.links.natives {
            self.b.span = match link.target {
                ExternalTarget::Symbol(id) => self
                    .m
                    .symbols()
                    .get(id.0 as usize)
                    .map_or(self.b.span, |s| s.declaration),
                ExternalTarget::Node(id) => self
                    .m
                    .hir()
                    .nodes
                    .get(id.0 as usize)
                    .map_or(self.b.span, |n| n.span),
            };
            if self.links.package.is_none() || link.native.index() >= self.package.natives.len() {
                return Err(self.b.error(
                    ErrorKind::MissingLink,
                    "native link has no trusted implementation",
                ));
            }
            self.b.work(self.m.external_bindings().len() as u64)?;
            let valid =
                match link.target {
                    ExternalTarget::Symbol(id) => self.m.external_bindings().iter().any(|e| {
                        e.kind == h::ExternalBindingKind::Reference && e.symbol == Some(id)
                    }),
                    ExternalTarget::Node(id) => {
                        self.m.external_bindings().iter().any(|e| {
                            e.kind == h::ExternalBindingKind::Reference && e.node == Some(id)
                        })
                    }
                };
            if !valid {
                return Err(self.b.error(
                    ErrorKind::LinkContract,
                    "native link does not identify an analyzed executable reference",
                ));
            }
            self.b.work(self.m.external_bindings().len() as u64)?;
            let external = self
                .m
                .external_bindings()
                .iter()
                .find(|e| {
                    e.kind == h::ExternalBindingKind::Reference
                        && match link.target {
                            ExternalTarget::Symbol(id) => e.symbol == Some(id),
                            ExternalTarget::Node(id) => e.node == Some(id),
                        }
                })
                .expect("checked identity");
            let reference = external.descriptor.reference.as_deref().ok_or_else(|| {
                self.b.error(
                    ErrorKind::MissingLink,
                    "analysis external binding has no canonical registration identity",
                )
            })?;
            if !self
                .b
                .compare(&reference.module, &link.reference.module)?
                .is_eq()
                || !self
                    .b
                    .compare(&reference.version, &link.reference.version)?
                    .is_eq()
                || !match (&reference.export, &link.reference.export) {
                    (Some(a), Some(b)) => self.b.compare(a, b)?.is_eq(),
                    (None, None) => true,
                    _ => false,
                }
            {
                return Err(self.b.error(
                    ErrorKind::LinkContract,
                    "native registration identity/version differs from analysis",
                ));
            }
            if external.descriptor.intrinsic == Some(crate::semantic::Intrinsic::Bind) {
                return Err(self.b.error(
                    ErrorKind::LinkContract,
                    "compiler intrinsic cannot be replaced with a native registration",
                ));
            }
            if link.source_signature.is_some() && external.descriptor.signature.is_none() {
                return Err(self.b.error(
                    ErrorKind::LinkContract,
                    "analysis omitted the trusted native source signature",
                ));
            }
            if let Some(signature) = &external.descriptor.signature {
                if link.source_signature.is_none() && !signature.result_cases.is_empty() {
                    return Err(self.b.error(
                        ErrorKind::LinkContract,
                        "conditional native source results require a trusted source declaration",
                    ));
                }
                if let Some(source) = link.source_signature
                    && !source_signature_matches(signature, source, &mut self.b)?
                {
                    return Err(self.b.error(
                        ErrorKind::LinkContract,
                        "analysis differs from trusted native source signature",
                    ));
                }
                let native = &self.package.natives[link.native.index()];
                let trusted = &self.package.signatures[native.signature.index()];
                self.b.work(
                    signature.parameters.len() as u64 + trusted.parameters.len() as u64 + 1,
                )?;
                if signature.parameters.len()
                    != trusted.parameters.len() + usize::from(trusted.rest.is_some())
                {
                    return Err(self.b.error(
                        ErrorKind::LinkContract,
                        "analysis/native parameter count differs",
                    ));
                }
                for (i, parameter) in signature.parameters.iter().enumerate() {
                    let actual = trusted
                        .parameters
                        .get(i)
                        .or(trusted.rest.as_ref())
                        .expect("checked native parameter count");
                    if !self
                        .b
                        .compare(&parameter.name, &self.package.symbols[actual.name.index()])?
                        .is_eq()
                        || parameter.mutable != actual.mutable
                        || parameter.default != actual.default.is_some()
                        || parameter.rest != (i == trusted.parameters.len())
                        || !(link.source_signature.is_some()
                            && self.package.types[actual.ty.index()]
                                == bc::TypeDef::Builtin(bc::BuiltinType::Any)
                            || hint_matches(
                                &parameter.ty,
                                actual.ty,
                                &self.package,
                                &mut self.b,
                                0,
                            )?)
                    {
                        return Err(self.b.error(
                            ErrorKind::LinkContract,
                            "analysis/native parameter contract differs",
                        ));
                    }
                }
                if !(link.source_signature.is_some()
                    && self.package.types[trusted.return_type.index()]
                        == bc::TypeDef::Builtin(bc::BuiltinType::Any)
                    || hint_matches(
                        &signature.result,
                        trusted.return_type,
                        &self.package,
                        &mut self.b,
                        0,
                    )?)
                    || signature.effect == crate::semantic::CallEffect::NoWait
                        && trusted.effects.0 & bc::Effects::WAIT != 0
                {
                    return Err(self.b.error(
                        ErrorKind::LinkContract,
                        "analysis/native return/effect contract differs",
                    ));
                }
            }
            self.b.push(
                &mut self.native_identities,
                super::super::NativeIdentity {
                    target: link.target,
                    native: link.native,
                    reference: external
                        .descriptor
                        .reference
                        .as_ref()
                        .expect("checked native identity")
                        .clone(),
                },
            )?;
            let slot = match link.target {
                ExternalTarget::Node(node) => &mut self.native_nodes[node.0 as usize],
                ExternalTarget::Symbol(symbol) => &mut self.native_symbols[symbol.0 as usize],
            };
            if slot.is_some_and(|old| old != link.native) {
                return Err(self
                    .b
                    .error(ErrorKind::LinkContract, "ambiguous native binding"));
            }
            *slot = Some(link.native);
        }
        Ok(())
    }
    pub(super) fn native(&mut self, node: h::NodeId) -> Result<Option<bc::NativeId>, Box<Error>> {
        self.b.work(1)?;
        if self.links.natives.is_empty() {
            return Ok(None);
        }
        let mut node = node;
        while let Form::Single { child } = self.m.hir().node(node).form {
            if self.m.hir().node(node).kind != H::Group {
                break;
            }
            self.b.work(1)?;
            node = child;
        }
        let symbol = self.m.facts()[node.0 as usize].symbol;
        let node_link = self.native_nodes[node.0 as usize];
        let symbol_link = symbol.and_then(|s| self.native_symbols[s.0 as usize]);
        if let (Some(a), Some(b)) = (node_link, symbol_link)
            && a != b
        {
            return Err(self
                .b
                .error(ErrorKind::LinkContract, "ambiguous native binding"));
        }
        Ok(node_link.or(symbol_link))
    }

    pub(super) fn global(
        &mut self,
        mut node: h::NodeId,
    ) -> Result<Option<super::super::project::GlobalTarget>, Box<Error>> {
        let Some(project) = self.project else {
            return Ok(None);
        };
        self.b.work(1)?;
        while let Form::Single { child } = self.m.hir().node(node).form {
            if self.m.hir().node(node).kind != H::Group {
                break;
            }
            self.b.work(1)?;
            node = child;
        }
        Ok(project.node(node).or_else(|| {
            self.m.facts()[node.0 as usize]
                .symbol
                .and_then(|s| project.symbol(s))
        }))
    }
    pub(super) fn check_imports(&mut self) -> Result<(), Box<Error>> {
        for import in self.m.imports() {
            self.b.span = self.m.hir().node(import.node).span;
            self.b.work(import.names.len() as u64)?;
            for &(_, symbol) in &import.names {
                self.b.work(self.m.external_bindings().len() as u64)?;
                let external = self
                    .m
                    .external_bindings()
                    .iter()
                    .find(|e| e.symbol == Some(symbol))
                    .ok_or_else(|| {
                        self.b.error(
                            ErrorKind::InvalidIr,
                            "import lacks retained external identity",
                        )
                    })?;
                // Type declarations and namespaces name compile-time entities.
                // Used native/source values retain their own executable links.
                if external.kind == h::ExternalBindingKind::Type
                    || external.descriptor.namespace.is_some()
                    || external.descriptor.intrinsic.is_some()
                    || external.descriptor.constant.is_some()
                {
                    continue;
                }
                self.b.work(self.links.natives.len() as u64)?;
                let linked = self
                    .links
                    .natives
                    .iter()
                    .any(|link| link.target == ExternalTarget::Symbol(symbol))
                    || self.project.is_some_and(|p| p.symbol(symbol).is_some());
                if !linked {
                    return Err(self.b.error(
                        ErrorKind::MissingLink,
                        "import has no linked native or source binding",
                    ));
                }
            }
        }
        Ok(())
    }
}
