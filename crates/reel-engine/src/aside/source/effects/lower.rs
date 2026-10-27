//! Pure HIR lowering with explicit function identities and finite loop expansion.
use super::*;

#[derive(Clone)]
enum Action {
    Evaluate(h::NodeId),
    Finish(h::NodeId),
    Bind(h::NodeId, Value),
}

impl Compiler<'_> {
    pub(super) fn function(
        &mut self,
        identity: EffectFunction,
        arguments: Vec<Value>,
        call: Site,
    ) -> Result<Value, EffectDiagnostic> {
        if self.active.contains(&identity) {
            return Err(self.error(call, "effect functions cannot recurse"));
        }
        if self.active.len() >= self.limits.call_depth {
            return Err(self.error(call, "effect function expansion depth exceeded"));
        }
        let module = &self.modules[identity.module];
        let function = &module.functions()[identity.function.0 as usize];
        if function.kind != h::FunctionKind::Plain
            || function.receiver != h::Receiver::None
            || !function.captures.is_empty()
            || function.parameters.iter().any(|p| p.rest)
        {
            return Err(self.error(
                Site(identity.module, function.node),
                "effect functions require fixed parameters and no captured runtime state",
            ));
        }
        let mut frame = Frame {
            module: identity.module,
            values: HashMap::new(),
        };
        for (parameter, argument) in function.parameters.iter().zip(arguments) {
            let symbol = &module.symbols()[parameter.symbol.0 as usize];
            self.contract(Site(identity.module, function.node), symbol.ty, &argument)
                .map_err(|mut error| {
                    error.span = symbol.declaration;
                    error
                })?;
            frame.values.insert(parameter.symbol, argument);
        }
        let actions = self.actions(identity.module, function.body, true);
        self.active.push(identity);
        let result = self.execute(&mut frame, &actions);
        self.active.pop();
        let result = result?;
        self.contract(
            Site(identity.module, function.node),
            function.return_type,
            &result,
        )?;
        Ok(result)
    }

    fn actions(&self, module: usize, body: h::NodeId, finish: bool) -> Vec<Action> {
        let hir = self.modules[module].hir();
        let Form::Block { tail } = hir.node(body).form else {
            return vec![if finish {
                Action::Finish(body)
            } else {
                Action::Evaluate(body)
            }];
        };
        hir.children(body)
            .map(|node| {
                if finish && Some(node) == tail {
                    Action::Finish(node)
                } else {
                    Action::Evaluate(node)
                }
            })
            .collect()
    }

    fn execute(
        &mut self,
        frame: &mut Frame,
        actions: &[Action],
    ) -> Result<Value, EffectDiagnostic> {
        for (index, action) in actions.iter().enumerate() {
            let (mut node, finish) = match action {
                Action::Bind(pattern, value) => {
                    self.bind(frame, *pattern, value.clone())?;
                    continue;
                }
                Action::Evaluate(node) => (*node, false),
                Action::Finish(node) => (*node, true),
            };
            if let Form::Statement { expression, .. } =
                self.modules[frame.module].hir().node(node).form
            {
                node = expression;
            }
            let site = Site(frame.module, node);
            self.work(site)?;
            let hir = self.modules[frame.module].hir();
            let kind = hir.node(node).kind;
            let form = hir.node(node).form;
            match (kind, form) {
                (H::Return, Form::Transfer { value, .. }) => {
                    return value.map_or(Ok(Value::Null), |value| self.expression(frame, value));
                }
                (
                    H::Let,
                    Form::Binding {
                        pattern,
                        initializer: Some(value),
                        ..
                    },
                ) => {
                    let value = self.expression(frame, value)?;
                    self.bind(frame, pattern, value)?;
                }
                (
                    H::If,
                    Form::Condition {
                        pattern: None,
                        condition,
                        body,
                        otherwise,
                    },
                ) => {
                    let condition = self.expression(frame, condition)?;
                    let mut yes = self.actions(frame.module, body, finish);
                    let mut no = otherwise
                        .map(|body| self.actions(frame.module, body, finish))
                        .unwrap_or_default();
                    yes.extend_from_slice(&actions[index + 1..]);
                    no.extend_from_slice(&actions[index + 1..]);
                    return if let Value::Bool(value) = condition {
                        self.execute(frame, if value { &yes } else { &no })
                    } else {
                        let mut yes_frame = frame.clone();
                        let mut no_frame = frame.clone();
                        let yes = self.execute(&mut yes_frame, &yes)?;
                        let no = self.execute(&mut no_frame, &no)?;
                        self.select(site, condition, yes, no)
                    };
                }
                (
                    H::For,
                    Form::For {
                        pattern,
                        source,
                        body,
                        asynchronous: false,
                    },
                ) => {
                    let Form::Binary {
                        left,
                        operator,
                        right,
                    } = hir.node(source).form
                    else {
                        return Err(self.error(
                            Site(frame.module, source),
                            "effect for requires a constant integer range",
                        ));
                    };
                    if hir.node(source).kind != H::Range {
                        return Err(
                            self.error(site, "effect for requires a constant integer range")
                        );
                    }
                    let start = self.expression(frame, left)?;
                    let end = self.expression(frame, right)?;
                    let (Value::Int(start), Value::Int(end)) = (start, end) else {
                        return Err(
                            self.error(site, "effect loop bounds must be compile-time integers")
                        );
                    };
                    let count = (i128::from(end) - i128::from(start)
                        + i128::from(operator.kind == K::RangeInclusive))
                    .max(0);
                    let body = self.actions(frame.module, body, false);
                    let expanded = count * (body.len() as i128 + 1);
                    if expanded > self.limits.work.saturating_sub(self.work) as i128 {
                        return Err(
                            self.error(site, "effect loop expansion exceeds source work limit")
                        );
                    }
                    let mut next =
                        Vec::with_capacity(expanded as usize + actions.len() - index - 1);
                    for iteration in 0..count {
                        next.push(Action::Bind(
                            pattern,
                            Value::Int((i128::from(start) + iteration) as i64),
                        ));
                        next.extend_from_slice(&body);
                    }
                    next.extend_from_slice(&actions[index + 1..]);
                    return self.execute(frame, &next);
                }
                (H::Block, _) => {
                    let mut next = self.actions(frame.module, node, finish);
                    next.extend_from_slice(&actions[index + 1..]);
                    return self.execute(frame, &next);
                }
                (H::Function, _) => {}
                (
                    H::Assignment,
                    Form::Binary {
                        left,
                        operator,
                        right,
                    },
                ) => {
                    let old = if operator.kind == K::Eq {
                        None
                    } else {
                        Some(self.expression(frame, left)?)
                    };
                    let value = self.expression(frame, right)?;
                    let value = if let Some(old) = old {
                        let op = match operator.kind {
                            K::PlusEq => K::Plus,
                            K::MinusEq => K::Minus,
                            K::StarEq => K::Star,
                            K::SlashEq => K::Slash,
                            K::PercentEq => K::Percent,
                            _ => return Err(self.error(site, "unsupported effect assignment")),
                        };
                        self.binary(site, op, old, value)?
                    } else {
                        value
                    };
                    self.assign(frame, left, value)?;
                }
                _ => {
                    let result = self.expression(frame, node)?;
                    if finish {
                        return Ok(result);
                    }
                }
            }
        }
        Ok(Value::Null)
    }

    pub(super) fn expression(
        &mut self,
        frame: &mut Frame,
        node: h::NodeId,
    ) -> Result<Value, EffectDiagnostic> {
        let site = Site(frame.module, node);
        self.work(site)?;
        let module = &self.modules[frame.module];
        let hir = module.hir();
        let kind = hir.node(node).kind;
        match hir.node(node).form {
            Form::Single { child }
            | Form::Statement {
                expression: child, ..
            } => self.expression(frame, child),
            Form::Literal { token } => match token.kind {
                K::True => Ok(Value::Bool(true)),
                K::False => Ok(Value::Bool(false)),
                K::Null => Ok(Value::Null),
                K::Number => {
                    let text = hir.text(token.span).replace('_', "");
                    let (radix, digits) = match text.get(..2) {
                        Some("0x" | "0X") => (16, &text[2..]),
                        Some("0o" | "0O") => (8, &text[2..]),
                        Some("0b" | "0B") => (2, &text[2..]),
                        _ => (10, text.as_str()),
                    };
                    if radix == 10 && text.bytes().any(|c| matches!(c, b'.' | b'e' | b'E')) {
                        let value = text
                            .parse::<f64>()
                            .map_err(|_| self.error(site, "invalid numeric literal"))?
                            as f32;
                        if !value.is_finite() {
                            return Err(self.error(site, "drawing literal exceeds finite float32"));
                        }
                        Ok(Value::Float(value))
                    } else {
                        i64::from_str_radix(digits, radix)
                            .map(Value::Int)
                            .map_err(|_| self.error(site, "compile-time integer exceeds int64"))
                    }
                }
                _ => Err(self.error(site, "effect expression requires a numeric or bool literal")),
            },
            Form::Name { .. } => {
                if let Some(symbol) = module.facts()[node.0 as usize].symbol
                    && let Some(value) = frame.values.get(&symbol)
                {
                    return Ok(value.clone());
                }
                self.callable(site).ok_or_else(|| {
                    self.error(site, "effect expression reads external runtime state")
                })
            }
            Form::Member {
                object,
                key,
                optional: false,
                ..
            } => {
                if let Some(value) = self.callable(site) {
                    return Ok(value);
                }
                if self.expression(frame, object)? != Value::Fragment {
                    return Err(self.error(site, "effect members require the fragment context"));
                }
                let input = match hir.name(key.name.unwrap()) {
                    "uv" => EffectInput::Uv,
                    "position" => EffectInput::Position,
                    "size" => EffectInput::Size,
                    _ => return Err(self.error(site, "unknown fragment property")),
                };
                Ok(Value::Node(self.push(site, Ty::Vec2, Op::Input(input))?))
            }
            Form::Index {
                object,
                index,
                optional: false,
                ..
            } => {
                let object = self.expression(frame, object)?;
                let Value::Int(index) = self.expression(frame, index)? else {
                    return Err(
                        self.error(site, "effect vector index must be a compile-time integer")
                    );
                };
                self.component(site, object, index)
            }
            Form::Sequence if kind == H::Tuple => {
                let children = hir.children(node).collect::<Vec<_>>();
                Ok(Value::Tuple(
                    children
                        .into_iter()
                        .map(|child| self.expression(frame, child))
                        .collect::<Result<_, _>>()?,
                ))
            }
            Form::Unary { operator, value } => {
                if !matches!(operator.kind, K::Plus | K::Minus | K::Bang) {
                    return Err(self.error(
                        site,
                        "effect expressions cannot wait or use this unary operation",
                    ));
                }
                let value = self.expression(frame, value)?;
                match operator.kind {
                    K::Plus => self.scalar_value(site, value),
                    K::Minus => match value {
                        Value::Int(v) => v
                            .checked_neg()
                            .map(Value::Int)
                            .ok_or_else(|| self.error(site, "compile-time integer overflow")),
                        Value::Float(v) => Ok(Value::Float(-v)),
                        value => self.unary(site, Unary::Negate, value),
                    },
                    K::Bang => match value {
                        Value::Bool(v) => Ok(Value::Bool(!v)),
                        value => self.unary(site, Unary::Not, value),
                    },
                    _ => Err(self.error(
                        site,
                        "effect expressions cannot wait or use this unary operation",
                    )),
                }
            }
            Form::Binary {
                left,
                operator,
                right,
            } if kind == H::Binary => {
                let left = self.expression(frame, left)?;
                if operator.kind == K::AndAnd || operator.kind == K::OrOr {
                    if let Value::Bool(value) = left
                        && value == (operator.kind == K::OrOr)
                    {
                        return Ok(Value::Bool(value));
                    }
                    let right = self.expression(frame, right)?;
                    return if operator.kind == K::AndAnd {
                        self.select(site, left, right, Value::Bool(false))
                    } else {
                        self.select(site, left, Value::Bool(true), right)
                    };
                }
                let right = self.expression(frame, right)?;
                self.binary(site, operator.kind, left, right)
            }
            Form::Call {
                callee,
                arguments,
                children: None,
                optional: false,
                ..
            } => self.call(frame, site, callee, arguments),
            Form::Condition {
                pattern: None,
                condition,
                body,
                otherwise: Some(otherwise),
            } if kind == H::If => {
                let condition = self.expression(frame, condition)?;
                self.value_block_admission(Site(frame.module, body))?;
                self.value_block_admission(Site(frame.module, otherwise))?;
                if let Value::Bool(value) = condition {
                    let actions =
                        self.actions(frame.module, if value { body } else { otherwise }, true);
                    return self.execute(frame, &actions);
                }
                let mut yes_frame = frame.clone();
                let mut no_frame = frame.clone();
                let yes_actions = self.actions(frame.module, body, true);
                let no_actions = self.actions(frame.module, otherwise, true);
                let yes = self.execute(&mut yes_frame, &yes_actions)?;
                let no = self.execute(&mut no_frame, &no_actions)?;
                for (symbol, value) in frame.values.clone() {
                    let yes = yes_frame
                        .values
                        .get(&symbol)
                        .cloned()
                        .unwrap_or_else(|| value.clone());
                    let no = no_frame.values.get(&symbol).cloned().unwrap_or(value);
                    frame
                        .values
                        .insert(symbol, self.select(site, condition.clone(), yes, no)?);
                }
                self.select(site, condition, yes, no)
            }
            Form::Block { .. } => {
                self.value_block_admission(site)?;
                let actions = self.actions(frame.module, node, true);
                self.execute(frame, &actions)
            }
            _ => Err(self.error(site, "unsupported expression in a drawing function")),
        }
    }

    fn value_block_admission(&self, site: Site) -> Result<(), EffectDiagnostic> {
        let hir = self.modules[site.0].hir();
        let mut pending = vec![site.1];
        while let Some(node) = pending.pop() {
            if hir.node(node).kind == H::Return {
                return Err(self.error(
                    Site(site.0, node),
                    "return inside a value block requires an enclosing statement branch",
                ));
            }
            if hir.node(node).kind != H::Function {
                pending.extend(hir.children(node));
            }
        }
        Ok(())
    }

    fn callable(&self, site: Site) -> Option<Value> {
        if let Some(function) = resolve_effect_function(self.modules, site.0, site.1) {
            return Some(Value::Function(function));
        }
        let module = &self.modules[site.0];
        let reference = module.canonical_reference(site.1)?;
        if reference.module != "std::math" || reference.version != "1" {
            return None;
        }
        let binding = module.external_bindings().iter().position(|binding| {
            binding.descriptor.reference.as_deref() == Some(reference)
                && binding.descriptor.signature.is_some()
        })?;
        Some(Value::Native {
            module: site.0,
            binding,
        })
    }

    fn call(
        &mut self,
        frame: &mut Frame,
        site: Site,
        callee: h::NodeId,
        arguments: Option<h::NodeId>,
    ) -> Result<Value, EffectDiagnostic> {
        let module = &self.modules[frame.module];
        let hir = module.hir();
        if let Form::Member {
            object,
            key,
            optional: false,
            ..
        } = hir.node(callee).form
            && matches!(hir.name(key.name.unwrap()), "sample" | "alpha" | "gray")
            && self.callable(Site(frame.module, callee)).is_none()
        {
            let receiver = self.expression(frame, object)?;
            let method = hir.name(key.name.unwrap());
            if !matches!(receiver, Value::Fragment | Value::Texture(_))
                || (receiver == Value::Fragment && method != "sample")
            {
                return Err(self.error(site, "sampling requires Fragment or Texture"));
            }
            let args = self.arguments(frame, arguments)?;
            if matches!(receiver, Value::Texture(_)) && args.is_empty() {
                return Err(self.error(site, "Texture sampling requires normalized uv coordinates"));
            }
            if args.len() > 1
                || args
                    .first()
                    .is_some_and(|(name, _)| name.as_deref().is_some_and(|name| name != "uv"))
            {
                return Err(self.error(site, "fragment.sample accepts one uv argument"));
            }
            let uv = match args.into_iter().next() {
                Some((_, value)) if value != Value::Null => value,
                _ => Value::Node(self.push(site, Ty::Vec2, Op::Input(EffectInput::Uv))?),
            };
            let uv = self.node(site, uv)?;
            if self.nodes[uv.0 as usize].ty != Ty::Vec2 {
                return Err(self.error(site, "sample coordinates require a two-float tuple"));
            }
            let (ty, op) = if let Value::Texture(binding) = receiver {
                let channel = match method {
                    "alpha" => EffectTextureChannel::Alpha,
                    "gray" => EffectTextureChannel::Gray,
                    _ => EffectTextureChannel::Color,
                };
                (
                    if channel == EffectTextureChannel::Color {
                        Ty::Vec4
                    } else {
                        Ty::Float
                    },
                    Op::TextureSample {
                        binding,
                        uv,
                        channel,
                    },
                )
            } else {
                (Ty::Vec4, Op::SourceSample { uv })
            };
            return Ok(Value::Node(self.push(site, ty, op)?));
        }
        let callable = self.expression(frame, callee)?;
        let arguments = self.arguments(frame, arguments)?;
        match callable {
            Value::Function(identity) => {
                let module = &self.modules[identity.module];
                let function = &module.functions()[identity.function.0 as usize];
                let names = function
                    .parameters
                    .iter()
                    .map(|p| {
                        module
                            .hir()
                            .name(module.symbols()[p.symbol.0 as usize].name)
                            .to_owned()
                    })
                    .collect::<Vec<_>>();
                let mut values = self.order_arguments(site, &names, arguments)?;
                let mut defaults = Frame {
                    module: identity.module,
                    values: HashMap::new(),
                };
                let mut supplied = Vec::new();
                for (i, parameter) in function.parameters.iter().enumerate() {
                    let value = if let Some(value) = values[i].take() {
                        value
                    } else if let Some(node) = parameter.default {
                        self.expression(&mut defaults, node)?
                    } else {
                        return Err(self
                            .error(site, format!("missing effect helper argument {}", names[i])));
                    };
                    defaults.values.insert(parameter.symbol, value.clone());
                    supplied.push(value);
                }
                self.function(identity, supplied, site)
            }
            Value::Native { module, binding } => {
                let descriptor = &self.modules[module].external_bindings()[binding].descriptor;
                let signature = descriptor.signature.as_ref().unwrap();
                let names = signature
                    .parameters
                    .iter()
                    .map(|p| p.name.clone())
                    .collect::<Vec<_>>();
                let name = descriptor
                    .reference
                    .as_ref()
                    .unwrap()
                    .export
                    .as_deref()
                    .unwrap();
                let values = self
                    .order_arguments(site, &names, arguments)?
                    .into_iter()
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| self.error(site, "missing math argument"))?;
                self.math(site, name, values)
            }
            _ => Err(self.error(
                site,
                "effect call target must be a statically resolved pure function",
            )),
        }
    }

    fn arguments(
        &mut self,
        frame: &mut Frame,
        arguments: Option<h::NodeId>,
    ) -> Result<Vec<(Option<String>, Value)>, EffectDiagnostic> {
        let hir = self.modules[frame.module].hir();
        let arguments = arguments
            .map(|node| hir.children(node).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut values = Vec::new();
        for argument in arguments {
            let Form::Argument { name, value } = hir.node(argument).form else {
                return Err(self.error(
                    Site(frame.module, argument),
                    "effect calls require explicit arguments",
                ));
            };
            let name = name.map(|name| hir.name(name.name.unwrap()).to_owned());
            values.push((name, self.expression(frame, value)?));
        }
        Ok(values)
    }
    fn order_arguments(
        &self,
        site: Site,
        names: &[String],
        arguments: Vec<(Option<String>, Value)>,
    ) -> Result<Vec<Option<Value>>, EffectDiagnostic> {
        let mut values = vec![None; names.len()];
        let mut positional = 0;
        for (name, value) in arguments {
            let index = if let Some(name) = name {
                names
                    .iter()
                    .position(|p| p == &name)
                    .ok_or_else(|| self.error(site, format!("unknown effect argument {name}")))?
            } else {
                let index = positional;
                positional += 1;
                index
            };
            let Some(slot) = values.get_mut(index) else {
                return Err(self.error(site, "too many effect arguments"));
            };
            if slot.replace(value).is_some() {
                return Err(self.error(site, "duplicate effect argument"));
            }
        }
        Ok(values)
    }

    fn bind(
        &mut self,
        frame: &mut Frame,
        pattern: h::NodeId,
        value: Value,
    ) -> Result<(), EffectDiagnostic> {
        let module = &self.modules[frame.module];
        let hir = module.hir();
        let site = Site(frame.module, pattern);
        match hir.node(pattern).kind {
            H::NamePattern => {
                let symbol = module.facts()[pattern.0 as usize]
                    .symbol
                    .ok_or_else(|| self.error(site, "pattern has no resolved binding"))?;
                self.contract(site, module.symbols()[symbol.0 as usize].ty, &value)?;
                frame.values.insert(symbol, value);
                Ok(())
            }
            H::WildcardPattern => Ok(()),
            H::TuplePattern => {
                let children = hir.children(pattern).collect::<Vec<_>>();
                let count = self.components(site, &value)?;
                if children.len() != count {
                    return Err(
                        self.error(site, "tuple pattern length differs from its drawing value")
                    );
                }
                for (index, pattern) in children.into_iter().enumerate() {
                    let component = self.component(site, value.clone(), index as i64)?;
                    self.bind(frame, pattern, component)?;
                }
                Ok(())
            }
            H::GroupPattern => {
                let child = hir.children(pattern).next().unwrap();
                self.bind(frame, child, value)
            }
            _ => Err(self.error(site, "effect bindings require names or tuple patterns")),
        }
    }
    fn assign(
        &mut self,
        frame: &mut Frame,
        target: h::NodeId,
        value: Value,
    ) -> Result<(), EffectDiagnostic> {
        let module = &self.modules[frame.module];
        let site = Site(frame.module, target);
        let Some(symbol) = module.facts()[target.0 as usize].symbol else {
            return Err(self.error(site, "effect assignment requires a local binding"));
        };
        if !frame.values.contains_key(&symbol) || !module.symbols()[symbol.0 as usize].mutable {
            return Err(self.error(site, "effect assignment requires a mutable local binding"));
        }
        self.contract(site, module.symbols()[symbol.0 as usize].ty, &value)?;
        frame.values.insert(symbol, value);
        Ok(())
    }
    pub(super) fn contract(
        &self,
        site: Site,
        ty: h::TypeId,
        value: &Value,
    ) -> Result<(), EffectDiagnostic> {
        let module = &self.modules[site.0];
        let valid = match &module.types()[ty.0 as usize] {
            h::Type::Any => true,
            h::Type::Function(_) | h::Type::Callable(..) | h::Type::Closure => {
                matches!(value, Value::Function(_) | Value::Native { .. })
            }
            h::Type::Int => matches!(value, Value::Int(_)),
            h::Type::Null => matches!(value, Value::Null),
            h::Type::Bool => {
                matches!(value, Value::Bool(_))
                    || matches!(value,Value::Node(id) if self.nodes[id.0 as usize].ty==Ty::Bool)
            }
            h::Type::Float => {
                matches!(value, Value::Float(_))
                    || matches!(value,Value::Node(id) if self.nodes[id.0 as usize].ty==Ty::Float)
            }
            h::Type::Tuple(items) => match value {
                Value::Tuple(values) if values.len() == items.len() => items
                    .iter()
                    .zip(values)
                    .all(|(ty, value)| self.contract(site, *ty, value).is_ok()),
                Value::Node(id) => drawing_type(module, ty) == Some(self.nodes[id.0 as usize].ty),
                _ => false,
            },
            h::Type::Named(name) => match module.hir().name(*name) {
                "reel::types::Fragment" => matches!(value, Value::Fragment),
                "reel::types::Texture" => matches!(value, Value::Texture(_)),
                _ => false,
            },
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(self.error(
                site,
                format!("drawing value does not satisfy {}", module.display_type(ty)),
            ))
        }
    }
    fn components(&self, site: Site, value: &Value) -> Result<usize, EffectDiagnostic> {
        match value {
            Value::Tuple(v) => Ok(v.len()),
            Value::Node(id) => match self.nodes[id.0 as usize].ty {
                Ty::Vec2 => Ok(2),
                Ty::Vec3 => Ok(3),
                Ty::Vec4 => Ok(4),
                _ => Err(self.error(site, "value is not a drawing vector")),
            },
            _ => Err(self.error(site, "value is not a drawing vector")),
        }
    }
    fn component(
        &mut self,
        site: Site,
        value: Value,
        index: i64,
    ) -> Result<Value, EffectDiagnostic> {
        let count = self.components(site, &value)?;
        if index < 0 || index as u64 >= count as u64 {
            return Err(self.error(site, "drawing vector component is out of range"));
        }
        if let Value::Tuple(values) = value {
            return Ok(values[index as usize].clone());
        }
        let Value::Node(value) = value else {
            unreachable!()
        };
        Ok(Value::Node(self.push(
            site,
            Ty::Float,
            Op::Extract {
                value,
                lane: index as u8,
            },
        )?))
    }
    fn scalar_value(&self, site: Site, value: Value) -> Result<Value, EffectDiagnostic> {
        if matches!(value, Value::Int(_) | Value::Float(_))
            || matches!(value,Value::Node(id) if self.nodes[id.0 as usize].ty==Ty::Float)
        {
            Ok(value)
        } else {
            Err(self.error(
                site,
                "Aside arithmetic requires scalar numbers; use a vector helper for tuples",
            ))
        }
    }
    fn unary(&mut self, site: Site, op: Unary, value: Value) -> Result<Value, EffectDiagnostic> {
        let value = self.node(site, value)?;
        let ty = self.nodes[value.0 as usize].ty;
        if (op == Unary::Not && ty != Ty::Bool) || (op != Unary::Not && ty != Ty::Float) {
            return Err(self.error(site, "unary effect operand has the wrong type"));
        }
        Ok(Value::Node(self.push(site, ty, Op::Unary { op, value })?))
    }
    fn binary(
        &mut self,
        site: Site,
        operator: K,
        left: Value,
        right: Value,
    ) -> Result<Value, EffectDiagnostic> {
        if let (Value::Int(a), Value::Int(b)) = (&left, &right) {
            let integer = match operator {
                K::Plus => a.checked_add(*b),
                K::Minus => a.checked_sub(*b),
                K::Star => a.checked_mul(*b),
                K::Slash => a.checked_div(*b),
                K::Percent => a.checked_rem(*b),
                _ => None,
            };
            if matches!(
                operator,
                K::Plus | K::Minus | K::Star | K::Slash | K::Percent
            ) {
                return integer.map(Value::Int).ok_or_else(|| {
                    self.error(site, "compile-time integer overflow or division by zero")
                });
            }
            let result = match operator {
                K::EqEq => Some(a == b),
                K::NotEq => Some(a != b),
                K::Lt => Some(a < b),
                K::Le => Some(a <= b),
                K::Gt => Some(a > b),
                K::Ge => Some(a >= b),
                _ => None,
            };
            if let Some(value) = result {
                return Ok(Value::Bool(value));
            }
        }
        let op = match operator {
            K::Plus => Binary::Add,
            K::Minus => Binary::Subtract,
            K::Star => Binary::Multiply,
            K::Slash => Binary::Divide,
            K::Percent => Binary::Remainder,
            K::EqEq => Binary::Equal,
            K::NotEq => Binary::NotEqual,
            K::Lt => Binary::Less,
            K::Le => Binary::LessEqual,
            K::Gt => Binary::Greater,
            K::Ge => Binary::GreaterEqual,
            _ => return Err(self.error(site, "unsupported effect binary operation")),
        };
        if !matches!(op, Binary::Equal | Binary::NotEqual) {
            self.scalar_value(site, left.clone())?;
            self.scalar_value(site, right.clone())?;
        }
        let number = |value: &Value| match value {
            Value::Int(v) => Some(*v as f32),
            Value::Float(v) => Some(*v),
            _ => None,
        };
        if let (Some(a), Some(b)) = (number(&left), number(&right)) {
            let value = match op {
                Binary::Add => Some(a + b),
                Binary::Subtract => Some(a - b),
                Binary::Multiply => Some(a * b),
                Binary::Divide => Some(a / b),
                Binary::Remainder => Some(a % b),
                _ => None,
            };
            if let Some(value) = value.filter(|v| v.is_finite()) {
                return Ok(Value::Float(value));
            }
            let value = match op {
                Binary::Equal => Some(a == b),
                Binary::NotEqual => Some(a != b),
                Binary::Less => Some(a < b),
                Binary::LessEqual => Some(a <= b),
                Binary::Greater => Some(a > b),
                Binary::GreaterEqual => Some(a >= b),
                _ => None,
            };
            if let Some(value) = value {
                return Ok(Value::Bool(value));
            }
        }
        let left = self.node(site, left)?;
        let right = self.node(site, right)?;
        if self.nodes[left.0 as usize].ty != self.nodes[right.0 as usize].ty {
            return Err(self.error(site, "effect operands have different types"));
        }
        let ty = if matches!(
            op,
            Binary::Equal
                | Binary::NotEqual
                | Binary::Less
                | Binary::LessEqual
                | Binary::Greater
                | Binary::GreaterEqual
        ) {
            Ty::Bool
        } else {
            Ty::Float
        };
        Ok(Value::Node(self.push(
            site,
            ty,
            Op::Binary { op, left, right },
        )?))
    }

    fn math(
        &mut self,
        site: Site,
        name: &str,
        values: Vec<Value>,
    ) -> Result<Value, EffectDiagnostic> {
        for value in &values {
            self.scalar_value(site, value.clone())?;
        }
        let numbers = values
            .iter()
            .map(|v| match v {
                Value::Int(v) => Some(*v as f32),
                Value::Float(v) => Some(*v),
                _ => None,
            })
            .collect::<Option<Vec<_>>>();
        if name == "to_int" {
            if let Value::Int(value) = values[0] {
                return Ok(Value::Int(value));
            }
            return if let Some(numbers) = numbers {
                let value = numbers[0];
                if value.is_finite()
                    && (-9223372036854775808.0_f32..9223372036854775808.0_f32).contains(&value)
                {
                    Ok(Value::Int(value as i64))
                } else {
                    Err(self.error(site, "constant to_int is outside int64"))
                }
            } else {
                Err(self.error(
                    site,
                    "effect integer conversions must be compile-time constants",
                ))
            };
        }
        if let Some(n) = numbers {
            let v = n[0];
            let result = match name {
                "sin" => Some(v.sin()),
                "cos" => Some(v.cos()),
                "tan" => Some(v.tan()),
                "asin" => Some(v.asin()),
                "acos" => Some(v.acos()),
                "atan" => Some(v.atan()),
                "atan2" => Some(v.atan2(n[1])),
                "sqrt" => Some(v.sqrt()),
                "hypot" => Some(v.hypot(n[1])),
                "pow" => Some(v.powf(n[1])),
                "exp" => Some(v.exp()),
                "ln" => Some(v.ln()),
                "log2" => Some(v.log2()),
                "log10" => Some(v.log10()),
                "abs" => Some(v.abs()),
                "floor" => Some(v.floor()),
                "ceil" => Some(v.ceil()),
                "round" => Some(v.round()),
                "radians" => Some(v.to_radians()),
                "degrees" => Some(v.to_degrees()),
                _ => None,
            };
            if let Some(value) = result.filter(|v| v.is_finite()) {
                return Ok(Value::Float(value));
            }
        }
        let first = values[0].clone();
        let unary = match name {
            "sin" => Some(Unary::Sin),
            "cos" => Some(Unary::Cos),
            "tan" => Some(Unary::Tan),
            "asin" => Some(Unary::Asin),
            "acos" => Some(Unary::Acos),
            "atan" => Some(Unary::Atan),
            "sqrt" => Some(Unary::Sqrt),
            "exp" => Some(Unary::Exp),
            "ln" => Some(Unary::Log),
            "log2" => Some(Unary::Log2),
            "abs" => Some(Unary::Abs),
            "floor" => Some(Unary::Floor),
            "ceil" => Some(Unary::Ceil),
            _ => None,
        };
        if let Some(op) = unary {
            return self.unary(site, op, first);
        }
        match name {
            "pow" | "atan2" => {
                let left = self.node(site, first)?;
                let right = self.node(site, values[1].clone())?;
                Ok(Value::Node(self.push(
                    site,
                    Ty::Float,
                    Op::Binary {
                        op: if name == "pow" {
                            Binary::Pow
                        } else {
                            Binary::Atan2
                        },
                        left,
                        right,
                    },
                )?))
            }
            "hypot" => {
                let a = self.node(site, first)?;
                let b = self.node(site, values[1].clone())?;
                let value = self.push(site, Ty::Vec2, Op::Vec2([a, b]))?;
                Ok(Value::Node(self.push(
                    site,
                    Ty::Float,
                    Op::Unary {
                        op: Unary::Length,
                        value,
                    },
                )?))
            }
            "log10" => {
                let value = self.unary(site, Unary::Log, first)?;
                self.binary(site, K::Slash, value, Value::Float(10_f32.ln()))
            }
            "radians" | "degrees" => self.binary(
                site,
                K::Star,
                first,
                Value::Float(if name == "radians" {
                    std::f32::consts::PI / 180.0
                } else {
                    180.0 / std::f32::consts::PI
                }),
            ),
            "round" => {
                let absolute = self.unary(site, Unary::Abs, first.clone())?;
                let whole = self.unary(site, Unary::Floor, absolute.clone())?;
                let fraction = self.binary(site, K::Minus, absolute, whole.clone())?;
                let up = self.binary(site, K::Ge, fraction, Value::Float(0.5))?;
                let next = self.binary(site, K::Plus, whole.clone(), Value::Float(1.0))?;
                let rounded = self.select(site, up, next, whole)?;
                let negative = self.unary(site, Unary::Negate, rounded.clone())?;
                let condition = self.binary(site, K::Lt, first, Value::Float(0.0))?;
                self.select(site, condition, negative, rounded)
            }
            _ => Err(self.error(
                site,
                format!("math function {name} has no drawing lowering"),
            )),
        }
    }
}
