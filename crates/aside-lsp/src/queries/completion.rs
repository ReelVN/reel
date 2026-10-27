//! Contextual code completion.
use super::*;

impl Workspace {
    pub fn complete(
        &mut self,
        uri: &Uri,
        position: Position,
        encoding: Encoding,
    ) -> Result<Vec<CompletionItem>, String> {
        let snapshot = self.snapshot(uri)?;
        let source = snapshot.source();
        let offset = source
            .lines
            .offset(&source.text, position, encoding)
            .ok_or("invalid completion position")?;
        if completion_in_text(source, offset) {
            return Ok(Vec::new());
        }
        if let Some(region) = snapshot
            .source_features(&snapshot.entry)
            .into_iter()
            .flat_map(|features| &features.completions)
            .filter(|region| contains(region.range, offset))
            .min_by_key(|region| u32::from(region.range.len()))
        {
            return Ok(region.items.to_vec());
        }
        if let Some(context) = import_context(source, offset) {
            let prefix = match context {
                ImportCompletion::Path(prefix) => prefix,
                ImportCompletion::Alias => return Ok(Vec::new()),
                ImportCompletion::Complete => {
                    return Ok(vec![CompletionItem {
                        label: "as".into(),
                        kind: Some(CompletionItemKind::KEYWORD),
                        ..Default::default()
                    }]);
                }
            };
            let prefix_refs: Vec<_> = prefix.iter().map(String::as_str).collect();
            let path = prefix.join("::");
            let mut items = std::collections::BTreeMap::new();
            for name in self.import_modules(uri, &prefix_refs) {
                items.insert(
                    name.clone(),
                    CompletionItem {
                        label: name,
                        kind: Some(CompletionItemKind::MODULE),
                        ..Default::default()
                    },
                );
            }
            for name in self.host.modules.modules.keys().map(String::as_str).chain(
                snapshot
                    .project
                    .modules()
                    .map(|module| module.canonical_path()),
            ) {
                let suffix = if path.is_empty() {
                    Some(name)
                } else {
                    name.strip_prefix(&format!("{path}::"))
                };
                if let Some(name) = suffix
                    .and_then(|suffix| suffix.split("::").next())
                    .filter(|name| !name.is_empty())
                {
                    items.entry(name.into()).or_insert_with(|| CompletionItem {
                        label: name.into(),
                        kind: Some(CompletionItemKind::MODULE),
                        ..Default::default()
                    });
                }
            }
            if let Some(interface) = self.host.modules.modules.get(&path) {
                for symbol in &interface.exports {
                    items.insert(
                        symbol.name.clone(),
                        completion(symbol, &snapshot, &self.host),
                    );
                }
            }
            if let Some(module) = snapshot
                .project
                .modules()
                .find(|module| module.canonical_path() == path)
                && let Some(source) = snapshot.sources.get(&path)
            {
                for &id in module.exports() {
                    let item = source_completion(module, source, id, &snapshot, &self.host);
                    items.insert(item.label.clone(), item);
                }
            }
            return Ok(items.into_values().collect());
        }
        let keywords = keyword_completions(source, offset);
        if previous_completion_token(source, offset).is_some_and(|token| {
            matches!(
                token.kind(),
                K::Let | K::Mut | K::Fn | K::Class | K::As | K::Pub | K::Static | K::Async | K::Gen
            )
        }) && !type_completion_context(source, offset)
        {
            return Ok(keywords);
        }
        let Some(module) = snapshot.module() else {
            return Ok(keywords);
        };
        if type_completion_context(source, offset) {
            if let Some(node) = completion_token(source, offset).and_then(|token| {
                token
                    .parent_ancestors()
                    .find(|node| node.kind() == K::TypeName)
            }) {
                let parts: Vec<_> = node
                    .children_with_tokens()
                    .filter_map(|element| element.into_token())
                    .filter(|token| {
                        token.kind() == K::Ident
                            && (u32::from(token.text_range().end()) as usize) < offset
                    })
                    .collect();
                if let Some(first) = parts.first()
                    && let Some(namespace) = visible(module, offset).into_iter().find_map(|id| {
                        let symbol = &module.symbols()[id.0 as usize];
                        (module.hir().name(symbol.name) == first.text())
                            .then(|| {
                                external(module, None, Some(id))
                                    .and_then(|symbol| symbol.namespace.as_ref())
                            })
                            .flatten()
                    })
                {
                    let mut path = namespace.module.clone();
                    for part in parts.iter().skip(1) {
                        path.push_str("::");
                        path.push_str(part.text());
                    }
                    return Ok(namespace_completions(&snapshot, &self.host, &path)
                        .into_iter()
                        .filter(|item| {
                            matches!(
                                item.kind,
                                Some(CompletionItemKind::CLASS | CompletionItemKind::MODULE)
                            )
                        })
                        .collect());
                }
            }
            let mut items: std::collections::BTreeMap<_, _> = BUILTIN_TYPE_NAMES
                .iter()
                .map(|name| {
                    (
                        (*name).to_owned(),
                        CompletionItem {
                            label: (*name).into(),
                            kind: Some(CompletionItemKind::TYPE_PARAMETER),
                            ..Default::default()
                        },
                    )
                })
                .collect();
            for id in visible(module, offset) {
                let item = source_completion(module, source, id, &snapshot, &self.host);
                if matches!(
                    item.kind,
                    Some(CompletionItemKind::CLASS | CompletionItemKind::MODULE)
                ) {
                    items.insert(item.label.clone(), item);
                }
            }
            for item in keywords {
                items.entry(item.label.clone()).or_insert(item);
            }
            return Ok(items.into_values().collect());
        }
        if let Some(base) = member_base(&snapshot, module, offset) {
            if let Some(class) =
                external(module, Some(base), module.facts()[base.0 as usize].symbol)
                    .and_then(|symbol| symbol.class.as_ref())
            {
                if let Some(name) = named_type(module, module.facts()[base.0 as usize].ty)
                    && let Some((module, source, class)) = source_class(&snapshot, name)
                {
                    return Ok(class_completion(&snapshot, module, source, class, false));
                }
                return Ok(class
                    .static_members
                    .iter()
                    .map(|member| completion(&member.symbol, &snapshot, &self.host))
                    .collect());
            }
            if let Some(name) = named_type(module, module.facts()[base.0 as usize].ty)
                && let Some(members) = self.host.types.get(name)
            {
                return Ok(members
                    .values()
                    .map(|member| {
                        let mut item = completion(&member.symbol, &snapshot, &self.host);
                        if member.symbol.signature.is_some() {
                            item.kind = Some(CompletionItemKind::METHOD);
                        }
                        item
                    })
                    .collect());
            }
            if let Some(name) = named_type(module, module.facts()[base.0 as usize].ty)
                && let Some((module, source, class)) = source_class(&snapshot, name)
            {
                return Ok(class_completion(&snapshot, module, source, class, true));
            }
            let ty = module.facts()[base.0 as usize].ty;
            if matches!(
                module.types()[ty.0 as usize],
                Type::Class(_) | Type::Instance(_)
            ) {
                let (class, instance) = match module.types()[ty.0 as usize] {
                    Type::Class(class) => (class, false),
                    Type::Instance(class) => (class, true),
                    _ => unreachable!(),
                };
                return Ok(class_completion(&snapshot, module, source, class, instance));
            }
            if let Some(namespace) =
                external(module, Some(base), module.facts()[base.0 as usize].symbol)
                    .and_then(|symbol| symbol.namespace.as_ref())
            {
                if let Some(interface) = self.host.modules.modules.get(&namespace.module) {
                    return Ok(interface
                        .exports
                        .iter()
                        .map(|symbol| completion(symbol, &snapshot, &self.host))
                        .collect());
                }
                if let Some(module) = snapshot
                    .project
                    .modules()
                    .find(|module| module.canonical_path() == namespace.module)
                {
                    return Ok(module
                        .exports()
                        .iter()
                        .map(|&id| {
                            source_completion(
                                module,
                                &snapshot.sources[module.canonical_path()],
                                id,
                                &snapshot,
                                &self.host,
                            )
                        })
                        .collect());
                }
            }
            let inferred = module.source_types().value(base);
            if let Some(group) = builtin_members(
                module.hir(),
                module.types(),
                module.facts()[base.0 as usize].ty,
            )
            .or_else(|| inferred.and_then(|info| inferred_builtin_members(&info.ty)))
            {
                let mut items: std::collections::BTreeMap<_, _> = group
                    .members
                    .iter()
                    .map(|member| {
                        let mut item = completion(&member.symbol, &snapshot, &self.host);
                        item.kind = Some(CompletionItemKind::METHOD);
                        (item.label.clone(), item)
                    })
                    .collect();
                if group.receiver == TypeHint::Table
                    && let Some(SourceShape::Record(fields)) =
                        inferred.and_then(|info| info.shape.as_ref())
                {
                    for field in fields {
                        items.insert(
                            field.name.clone(),
                            CompletionItem {
                                label: field.name.clone(),
                                kind: Some(CompletionItemKind::FIELD),
                                detail: Some(field.value.ty.to_string()),
                                ..Default::default()
                            },
                        );
                    }
                }
                return Ok(items.into_values().collect());
            }
            if let Some(info) = inferred
                && let TypeHint::Named(name) = &info.ty
            {
                if let Some(members) = self.host.types.get(name) {
                    return Ok(members
                        .values()
                        .map(|member| {
                            let mut item = completion(&member.symbol, &snapshot, &self.host);
                            if member.symbol.signature.is_some() {
                                item.kind = Some(CompletionItemKind::METHOD);
                            }
                            item
                        })
                        .collect());
                }
                if let Some((module, source, class)) = source_class(&snapshot, name) {
                    return Ok(class_completion(&snapshot, module, source, class, true));
                }
            }
        }
        let mut items: Vec<_> = visible(module, offset)
            .into_iter()
            .map(|id| source_completion(module, source, id, &snapshot, &self.host))
            .collect();
        items.extend(keywords);
        if let Some((callee, arguments)) = call_at(module, offset)
            && let Some(signature) = signature_for(&snapshot, module, source, &self.host, callee)
        {
            let mut supplied = BTreeSet::new();
            let mut positional = 0;
            for arg in arguments
                .into_iter()
                .flat_map(|args| module.hir().children(args))
            {
                match module.hir().node(arg).form {
                    Form::Argument {
                        name: Some(name), ..
                    } => {
                        supplied.insert(module.hir().text(name.span).to_owned());
                    }
                    Form::Argument { name: None, value }
                        if module.hir().node(value).kind == HirKind::Name
                            && contains(module.hir().node(value).span, offset) => {}
                    Form::Argument { name: None, .. } => {
                        if let Some((name, _)) = signature.parameters.get(positional) {
                            supplied.insert(name.clone());
                        }
                        positional += 1;
                    }
                    _ => {}
                }
            }
            items.extend(
                signature
                    .parameters
                    .into_iter()
                    .filter(|(name, label)| {
                        !name.is_empty() && !supplied.contains(name) && !label.starts_with("...")
                    })
                    .map(|(name, label)| CompletionItem {
                        label: format!("{name}:"),
                        kind: Some(CompletionItemKind::FIELD),
                        detail: Some(label),
                        insert_text: Some(format!("{name}: ")),
                        sort_text: Some(format!("0{name}")),
                        ..Default::default()
                    }),
            );
        }
        Ok(items)
    }
}
