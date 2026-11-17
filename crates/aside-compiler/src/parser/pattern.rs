//! Pattern grammar and bounded assignment lookahead.
use super::{D, GreenNodeBuilder, K, Parser, PatternShape};

impl Parser<'_> {
    // Speculate with the actual pattern/type/default-expression productions.
    // The borrowed tokens, isolated cursor and silent sink preserve caller state.
    // Completed nested grammar results are cached; all trials share a linear
    // token-work budget, including recursively nested default expressions.
    pub(super) fn assignment_pattern_ahead(&mut self) -> bool {
        if self.halted || !matches!(self.peek(), K::LParen | K::LBracket | K::LBrace) {
            return false;
        }
        let key = (self.significant_cursor(), self.stop_pipe);
        if let Some(&result) = self.probes.cache.borrow().get(&key) {
            return result;
        }
        let mut trial = Parser {
            source: self.source,
            tokens: self.tokens,
            cursor: self.cursor,
            builder: GreenNodeBuilder::new(),
            import_offsets: Vec::new(),
            diagnostics: Vec::new(),
            limits: self.limits,
            depth: self.depth,
            halted: false,
            exhausted: false,
            exhaustion_reason: None,
            frames: Vec::new(),
            allow_min: false,
            stop_pipe: self.stop_pipe,
            probe: true,
            probes: self.probes,
        };
        trial.start(K::Root);
        let shape = trial.pattern();
        let result = !trial.halted && shape.destructuring && trial.peek() == K::Eq;
        if trial.exhausted {
            self.limit(
                trial
                    .exhaustion_reason
                    .expect("trial exhaustion records its cause"),
            );
        } else {
            self.probes.cache.borrow_mut().insert(key, result);
        }
        result
    }

    pub(super) fn pattern(&mut self) -> PatternShape {
        let key = (self.significant_cursor(), self.stop_pipe);
        if !self.enter() {
            return PatternShape::default();
        }
        let cp = self.checkpoint();
        let mut shape = self.pattern_atom();
        if self.eat(K::Is) {
            self.wrap(cp, K::TypePattern);
            self.type_atom(true);
            self.finish();
            shape = PatternShape {
                kind: Some(K::TypePattern),
                destructuring: true,
            };
        }
        while self.eat(K::Pipe) && !self.halted {
            self.wrap(cp, K::OrPattern);
            self.pattern();
            self.finish();
            shape = PatternShape {
                kind: Some(K::OrPattern),
                destructuring: true,
            };
        }
        self.leave();
        if self.probe
            && !self.halted
            && matches!(
                self.tokens.get(key.0).map(|t| t.kind),
                Some(K::LParen | K::LBracket | K::LBrace)
            )
        {
            self.probes
                .cache
                .borrow_mut()
                .insert(key, shape.destructuring && self.peek() == K::Eq);
        }
        shape
    }
    fn pattern_atom(&mut self) -> PatternShape {
        match self.peek() {
            K::Ident => {
                let kind = if self.text() == "_" {
                    K::WildcardPattern
                } else {
                    K::NamePattern
                };
                self.start(kind);
                self.bump();
                self.finish();
                PatternShape {
                    kind: Some(kind),
                    destructuring: false,
                }
            }
            K::Number | K::True | K::False | K::Null | K::Minus | K::StringStart => {
                self.start(K::LiteralPattern);
                let negative = self.eat(K::Minus);
                if negative && self.peek() != K::Number {
                    self.error(
                        D::ExpectedPattern,
                        "negative literal patterns require a number",
                    );
                }
                if self.peek() == K::StringStart {
                    if self.text().starts_with('f') {
                        self.error(
                            D::ExpectedPattern,
                            "interpolated strings are expressions, not literal patterns",
                        );
                    }
                    self.string_expr();
                } else if matches!(self.peek(), K::Number | K::True | K::False | K::Null) {
                    self.literal(negative);
                } else {
                    self.error(D::ExpectedPattern, "expected numeric literal after '-'");
                }
                self.finish();
                PatternShape {
                    kind: Some(K::LiteralPattern),
                    destructuring: false,
                }
            }
            K::LParen | K::LBracket => {
                let tuple = self.peek() == K::LParen;
                let end = if tuple { K::RParen } else { K::RBracket };
                let cp = self.checkpoint();
                self.bump();
                let mut rest = false;
                let mut count = 0;
                let mut comma = false;
                let mut child_shape = PatternShape::default();
                while self.peek() != end && self.peek() != K::Eof && !self.halted {
                    let before = self.cursor;
                    if rest {
                        self.error(D::InvalidRest, "rest pattern must be last");
                    }
                    if self.peek() == K::Ellipsis {
                        rest = true;
                        self.start(K::RestPattern);
                        self.bump();
                        self.expect(K::Ident);
                        self.finish();
                    } else {
                        child_shape = self.pattern();
                    }
                    count += 1;
                    if !self.eat(K::Comma) {
                        break;
                    }
                    comma = true;
                    self.progress(before);
                }
                self.expect(end);
                let kind = if !tuple {
                    K::ArrayPattern
                } else if count == 1 && !comma {
                    K::GroupPattern
                } else {
                    K::TuplePattern
                };
                self.wrap(cp, kind);
                self.finish();
                PatternShape {
                    kind: Some(kind),
                    destructuring: kind != K::GroupPattern || child_shape.destructuring,
                }
            }
            K::LBrace => {
                self.start(K::ObjectPattern);
                self.bump();
                let mut rest = false;
                while !matches!(self.peek(), K::RBrace | K::Eof) && !self.halted {
                    let before = self.cursor;
                    if rest {
                        self.error(D::InvalidRest, "rest pattern must be last");
                    }
                    if self.peek() == K::Ellipsis {
                        rest = true;
                        self.start(K::RestPattern);
                        self.bump();
                        self.expect(K::Ident);
                        self.finish();
                    } else {
                        self.start(K::PatternField);
                        self.expect(K::Ident);
                        if self.eat(K::Colon) {
                            self.pattern();
                        }
                        if self.eat(K::Eq) {
                            self.expr(0, false, true);
                        }
                        self.finish();
                    }
                    if !self.eat(K::Comma) {
                        break;
                    }
                    self.progress(before);
                }
                self.expect(K::RBrace);
                self.finish();
                PatternShape {
                    kind: Some(K::ObjectPattern),
                    destructuring: true,
                }
            }
            _ => {
                self.start(K::Error);
                self.error(D::ExpectedPattern, "expected pattern");
                if !matches!(
                    self.peek(),
                    K::Eof
                        | K::Eq
                        | K::Colon
                        | K::Comma
                        | K::RParen
                        | K::RBracket
                        | K::RBrace
                        | K::In
                        | K::FatArrow
                        | K::If
                ) {
                    self.bump();
                }
                self.finish();
                PatternShape::default()
            }
        }
    }
}
