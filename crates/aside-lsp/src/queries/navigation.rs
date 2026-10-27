//! Hover, definition and call signature queries.
use super::*;

impl Workspace {
    pub fn hover(
        &mut self,
        uri: &Uri,
        position: Position,
        encoding: Encoding,
    ) -> Result<Option<Hover>, String> {
        if let Some(document) = self
            .host
            .documents
            .get(uri.as_str())
            .filter(|document| !document.references.is_empty())
        {
            let lines = &document.lines;
            let offset = lines
                .offset(&document.text, position, encoding)
                .ok_or("invalid hover position")?;
            let Some(reference) = document.reference_at(offset) else {
                return Ok(None);
            };
            let symbol = &reference.symbol;
            let label = if symbol.type_only {
                format!("class {}", symbol.ty)
            } else if let Some(namespace) = &symbol.namespace {
                format!("use {}", namespace.module)
            } else if let Some(signature) = host_signature(symbol, &self.host) {
                hover_signature(&signature, &summarized_properties(symbol, &self.host))
            } else {
                property_label(symbol, &self.host)
            };
            let mut value = format!("```aside\n{label}\n```");
            value.push_str(&property_group_summary(symbol, &self.host));
            if let Some(declaration) = self
                .host
                .declaration(symbol)
                .filter(|declaration| !declaration.markdown.is_empty())
            {
                value.push_str("\n\n");
                value.push_str(&declaration.markdown);
            }
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value,
                }),
                range: Some(lines.range(&document.text, reference.range, encoding)),
            }));
        }
        let snapshot = self.snapshot(uri)?;
        let source = snapshot.source();
        let offset = source
            .lines
            .offset(&source.text, position, encoding)
            .ok_or("invalid hover position")?;
        if let Some(path) = import_path_at(source, offset)
            && let Some(value) = self.module_hover(uri, &path)
        {
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value,
                }),
                range: Some(source.lines.range(&source.text, path.range, encoding)),
            }));
        }
        if let Some(hover) = snapshot
            .source_features(&snapshot.entry)
            .into_iter()
            .flat_map(|features| &features.hovers)
            .filter(|hover| hover.range.contains(TextSize::from(offset as u32)))
            .min_by_key(|hover| u32::from(hover.range.len()))
        {
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: hover.markdown.to_string(),
                }),
                range: Some(source.lines.range(&source.text, hover.range, encoding)),
            }));
        }
        let Some(module) = snapshot.module() else {
            return Ok(None);
        };
        let Some((target, range)) = target_at(module, offset) else {
            return Ok(None);
        };
        let aliases = hover_type_aliases(module, offset, &self.host);
        let property_groups = match &target {
            Target::External(symbol) => property_group_summary(symbol, &self.host),
            _ => String::new(),
        };
        let (label, docs, additional_properties) = match target {
            Target::External(symbol) => {
                let signature = external_signature(symbol, &snapshot, &self.host);
                let docs = signature
                    .as_ref()
                    .and_then(|signature| signature.markdown.clone())
                    .or_else(|| external_docs(symbol, &snapshot, &self.host));
                let (label, summarized) = external_hover(
                    symbol,
                    &snapshot,
                    &self.host,
                    &aliases,
                    module.hir().text(range),
                );
                (label, docs, summarized)
            }
            Target::Symbol { id, ty, node } => {
                let symbol = &module.symbols()[id.0 as usize];
                (
                    local_signature(module, source, id).map_or_else(
                        || match (
                            module
                                .source_types()
                                .value(node)
                                .or_else(|| module.source_types().symbol(id)),
                            ty,
                        ) {
                            (Some(info), _)
                                if info.ty != TypeHint::Any
                                    && matches!(
                                        module.types()[ty.unwrap_or(symbol.ty).0 as usize],
                                        Type::Any | Type::Table | Type::Array
                                    ) =>
                            {
                                format!(
                                    "let {}{}: {}",
                                    if symbol.mutable { "mut " } else { "" },
                                    module.hir().name(symbol.name),
                                    inferred_label(module, &hover_type(&info.ty, &aliases))
                                )
                            }
                            (_, Some(ty)) if ty != symbol.ty => format!(
                                "let {}: {}",
                                module.hir().name(symbol.name),
                                type_label(module, ty)
                            ),
                            _ => local_label(module, source, id),
                        },
                        |signature| hover_signature(&signature, &BTreeSet::new()),
                    ),
                    source_docs(source, symbol.declaration),
                    0,
                )
            }
            Target::Member { node, key, ty } => (
                value_signature(
                    module,
                    source,
                    ty,
                    module.hir().text(key.span),
                    module.hir().node(node).span,
                )
                .map_or_else(
                    || {
                        format!(
                            "let {}: {}",
                            module.hir().text(key.span),
                            module
                                .source_types()
                                .value(node)
                                .filter(|info| info.ty != TypeHint::Any
                                    && matches!(
                                        module.types()[ty.0 as usize],
                                        Type::Any | Type::Table | Type::Array
                                    ))
                                .map_or_else(
                                    || type_label(module, ty),
                                    |info| inferred_label(module, &hover_type(&info.ty, &aliases))
                                )
                        )
                    },
                    |signature| hover_signature(&signature, &BTreeSet::new()),
                ),
                source_docs(source, module.hir().node(node).span),
                0,
            ),
        };
        let mut value = format!("```aside\n{label}\n```");
        if additional_properties > 0 {
            value.push_str(&format!("\n\nAdditional named properties: {additional_properties}. Use named-argument completion or Go to Definition."));
        }
        value.push_str(&property_groups);
        if let Some(docs) = docs.filter(|docs| !docs.is_empty()) {
            value.push_str("\n\n");
            value.push_str(&docs);
        }
        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value,
            }),
            range: Some(source.lines.range(&source.text, range, encoding)),
        }))
    }
    pub fn definition(
        &mut self,
        uri: &Uri,
        position: Position,
        encoding: Encoding,
    ) -> Result<Option<Location>, String> {
        if let Some(document) = self
            .host
            .documents
            .get(uri.as_str())
            .filter(|document| !document.references.is_empty())
        {
            let lines = &document.lines;
            let offset = lines
                .offset(&document.text, position, encoding)
                .ok_or("invalid definition position")?;
            let Some(reference) = document.reference_at(offset) else {
                return Ok(None);
            };
            let definition = self
                .host
                .declaration(&reference.symbol)
                .and_then(|declaration| declaration.definition.as_ref());
            return Ok(definition.and_then(|definition| {
                let document = self.host.documents.get(definition.uri.as_str())?;
                Some(Location {
                    uri: definition.uri.clone(),
                    range: document
                        .lines
                        .range(&document.text, definition.range, encoding),
                })
            }));
        }
        let snapshot = self.snapshot(uri)?;
        let source = snapshot.source();
        let offset = source
            .lines
            .offset(&source.text, position, encoding)
            .ok_or("invalid definition position")?;
        if let Some(path) = import_path_at(source, offset)
            && let Some(location) = self.module_definition(uri, &path, encoding)
        {
            return Ok(Some(location));
        }
        let Some(module) = snapshot.module() else {
            return Ok(None);
        };
        let Some((target, _)) = target_at(module, offset) else {
            return Ok(None);
        };
        let host_declaration = match &target {
            Target::External(symbol) => self.host.declaration(symbol),
            _ => None,
        };
        let reference = match target {
            Target::Symbol { id, .. } => {
                return Ok(Some(Location {
                    uri: source.uri.clone(),
                    range: source.lines.range(
                        &source.text,
                        module.symbols()[id.0 as usize].declaration,
                        encoding,
                    ),
                }));
            }
            Target::Member { node, .. } => {
                if let Some(definition) = module.source_types().definition(node)
                    && let Some(source) = snapshot.sources.get(definition.module.as_ref())
                {
                    return Ok(Some(Location {
                        uri: source.uri.clone(),
                        range: source.lines.range(&source.text, definition.range, encoding),
                    }));
                }
                let span = match module.hir().node(node).form {
                    Form::Function {
                        name: Some(name), ..
                    } => name.span,
                    Form::Binding { pattern, .. } => module.hir().node(pattern).span,
                    _ => unreachable!("class member is a field or method"),
                };
                return Ok(Some(Location {
                    uri: source.uri.clone(),
                    range: source.lines.range(&source.text, span, encoding),
                }));
            }
            Target::External(symbol) => symbol.reference.as_deref(),
        };
        let Some(reference) = reference else {
            return Ok(None);
        };
        if let Some(definition) =
            host_declaration.and_then(|declaration| declaration.definition.as_ref())
            && let Some(document) = self.host.documents.get(definition.uri.as_str())
        {
            return Ok(Some(Location {
                uri: definition.uri.clone(),
                range: document
                    .lines
                    .range(&document.text, definition.range, encoding),
            }));
        }
        if let Some((module, source, node, _)) = source_member_reference(&snapshot, reference) {
            let range = match module.hir().node(node).form {
                Form::Function {
                    name: Some(name), ..
                } => name.span,
                Form::Binding { pattern, .. } => module.hir().node(pattern).span,
                _ => unreachable!(),
            };
            return Ok(Some(Location {
                uri: source.uri.clone(),
                range: source.lines.range(&source.text, range, encoding),
            }));
        }
        let Some(target) = snapshot
            .project
            .modules()
            .find(|module| module.canonical_path() == reference.module)
        else {
            return Ok(None);
        };
        let Some(source) = snapshot.sources.get(&reference.module) else {
            return Ok(None);
        };
        if reference.export.is_none() {
            return Ok(Some(Location {
                uri: source.uri.clone(),
                range: Range::new(Position::default(), Position::default()),
            }));
        }
        let symbol = target.exports().iter().find_map(|&id| {
            let symbol = &target.symbols()[id.0 as usize];
            (Some(target.hir().name(symbol.name)) == reference.export.as_deref()).then_some(symbol)
        });
        Ok(symbol.map(|symbol| Location {
            uri: source.uri.clone(),
            range: source
                .lines
                .range(&source.text, symbol.declaration, encoding),
        }))
    }
    pub fn signature(
        &mut self,
        uri: &Uri,
        position: Position,
        encoding: Encoding,
    ) -> Result<Option<SignatureHelp>, String> {
        let snapshot = self.snapshot(uri)?;
        let source = snapshot.source();
        let offset = source
            .lines
            .offset(&source.text, position, encoding)
            .ok_or("invalid signature position")?;
        let Some(module) = snapshot.module() else {
            return Ok(None);
        };
        let Some((callee, args)) = call_at(module, offset) else {
            return Ok(None);
        };
        let Some(signature) = signature_for(&snapshot, module, source, &self.host, callee) else {
            return Ok(None);
        };
        let mut active = 0;
        if let Some(args) = args {
            // Only separators owned by this argument list advance the parameter.
            // Commas inside nested calls, tuples and arrays belong to their nodes.
            active = module
                .hir()
                .tokens(args)
                .filter(|token| {
                    token.kind == K::Comma && (u32::from(token.span.start()) as usize) < offset
                })
                .count();
            for arg in module.hir().children(args) {
                if contains(module.hir().node(arg).span, offset)
                    && let Form::Argument {
                        name: Some(name), ..
                    } = module.hir().node(arg).form
                    && let Some(index) = signature.parameters.iter().position(|(parameter, _)| {
                        Some(parameter.as_str()) == name.name.map(|name| module.hir().name(name))
                    })
                {
                    active = index;
                }
            }
        }
        let active = active.min(signature.parameters.len().saturating_sub(1)) as u32;
        let mut parameter_offset = 4 + signature.name.encode_utf16().count() as u32;
        let parameters = signature
            .parameters
            .into_iter()
            .map(|(name, label)| {
                let end = parameter_offset + label.encode_utf16().count() as u32;
                let label = if name.is_empty() {
                    ParameterLabel::LabelOffsets([parameter_offset, end])
                } else {
                    ParameterLabel::Simple(label)
                };
                parameter_offset = end + 2;
                ParameterInformation {
                    label,
                    documentation: signature
                        .parameter_documentation
                        .get(&name)
                        .map(|markdown| {
                            Documentation::MarkupContent(MarkupContent {
                                kind: MarkupKind::Markdown,
                                value: markdown.to_string(),
                            })
                        }),
                }
            })
            .collect();
        Ok(Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label: signature.label,
                documentation: signature.markdown.map(Documentation::String),
                parameters: Some(parameters),
                active_parameter: Some(active),
            }],
            active_signature: Some(0),
            active_parameter: Some(active),
        }))
    }
}
