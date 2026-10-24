use crate::diagnostic::span;
use crate::{
    Diagnostic, DiagnosticCode as D, Fix, GreenNode, Limits, RelatedRange, SourceTooLarge,
    SyntaxKind as K, SyntaxNode, TextRange, Token, lex_with_limits,
};
use aside_syntax::{Checkpoint, GreenNodeBuilder};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EntryPoint {
    #[default]
    Module,
    Block,
    Expression,
    Pattern,
    Type,
}
#[derive(Debug, Clone, Copy, Default)]
pub struct ParseOptions {
    pub entry: EntryPoint,
    pub limits: Limits,
}
#[derive(Debug, Clone)]
pub struct Parse {
    green: GreenNode,
    import_offsets: Vec<crate::TextSize>,
    diagnostics: Vec<Diagnostic>,
    exhausted: bool,
}
impl Parse {
    pub fn syntax(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
    }
    /// Keyword positions recorded by the import grammar, including nested uses.
    pub(crate) fn import_offsets(&self) -> &[crate::TextSize] {
        &self.import_offsets
    }
    pub fn green(&self) -> &GreenNode {
        &self.green
    }
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub fn exhausted(&self) -> bool {
        self.exhausted
    }
    pub fn is_ok(&self) -> bool {
        self.diagnostics.is_empty() && !self.exhausted
    }
}
pub fn parse(source: &str) -> Result<Parse, SourceTooLarge> {
    parse_with_options(source, ParseOptions::default())
}
pub fn parse_with_options(source: &str, options: ParseOptions) -> Result<Parse, SourceTooLarge> {
    let limits = options.limits.normalized();
    let lexed = lex_with_limits(source, limits)?;
    let probes = PatternProbes {
        remaining: Cell::new(lexed.tokens.len().saturating_mul(16)),
        cache: RefCell::new(HashMap::new()),
    };
    let mut p = Parser {
        source,
        tokens: &lexed.tokens,
        diagnostics: lexed.diagnostics,
        cursor: 0,
        builder: GreenNodeBuilder::new(),
        import_offsets: Vec::new(),
        limits,
        depth: 0,
        halted: false,
        exhausted: lexed.exhausted,
        exhaustion_reason: None,
        frames: Vec::new(),
        allow_min: false,
        stop_pipe: false,
        probe: false,
        probes: &probes,
    };
    p.start(K::Root);
    match options.entry {
        EntryPoint::Module => p.statements(K::Eof, false),
        EntryPoint::Block => {
            p.start(K::Block);
            p.statements(K::Eof, true);
            p.finish();
        }
        EntryPoint::Expression => {
            p.expr(0, false, true);
        }
        EntryPoint::Pattern => {
            p.pattern();
        }
        EntryPoint::Type => p.ty(),
    }
    if !p.halted && p.peek() != K::Eof {
        p.error(D::ExpectedToken, "unexpected tokens after entry point");
    }
    p.remaining();
    p.finish();
    Ok(Parse {
        green: p.builder.finish(),
        import_offsets: p.import_offsets,
        diagnostics: p.diagnostics,
        exhausted: p.exhausted,
    })
}
struct Parser<'a> {
    source: &'a str,
    tokens: &'a [Token],
    cursor: usize,
    builder: GreenNodeBuilder<'static>,
    import_offsets: Vec<crate::TextSize>,
    diagnostics: Vec<Diagnostic>,
    limits: Limits,
    depth: usize,
    halted: bool,
    exhausted: bool,
    exhaustion_reason: Option<&'static str>,
    frames: Vec<Vec<usize>>,
    allow_min: bool,
    stop_pipe: bool,
    probe: bool,
    probes: &'a PatternProbes,
}
struct PatternProbes {
    // Shared across every candidate and nested default expression in one parse.
    // A cache entry is a completed grammar result, never a token-shape guess.
    remaining: Cell<usize>,
    cache: RefCell<HashMap<(usize, bool), bool>>,
}
#[derive(Clone, Copy, Default)]
struct PatternShape {
    kind: Option<K>,
    destructuring: bool,
}
#[derive(Clone, Copy)]
struct Marker {
    checkpoint: Option<Checkpoint>,
    index: usize,
}
#[derive(Clone, Copy)]
struct Expr {
    kind: K,
    height: usize,
    target: bool,
    assignment: bool,
    assignment_range: Option<TextRange>,
    missing_else: bool,
    children_named: bool,
    children_block: bool,
}
impl Expr {
    fn new(kind: K) -> Self {
        Self {
            kind,
            height: 1,
            target: false,
            assignment: false,
            assignment_range: None,
            missing_else: false,
            children_named: false,
            children_block: false,
        }
    }
    fn target(kind: K) -> Self {
        Self {
            target: true,
            ..Self::new(kind)
        }
    }
}
impl Parser<'_> {
    fn significant_cursor(&self) -> usize {
        self.tokens[self.cursor..]
            .iter()
            .position(|t| !t.kind.is_trivia())
            .map_or(self.tokens.len(), |offset| self.cursor + offset)
    }
    fn nth(&self, n: usize) -> K {
        self.tokens[self.cursor..]
            .iter()
            .filter(|t| !t.kind.is_trivia())
            .nth(n)
            .map_or(K::Eof, |t| t.kind)
    }
    fn peek(&self) -> K {
        self.nth(0)
    }
    fn token(&self) -> Option<Token> {
        self.tokens[self.cursor..]
            .iter()
            .find(|t| !t.kind.is_trivia())
            .copied()
    }
    fn range(&self) -> TextRange {
        self.token()
            .map_or(span(self.source.len(), self.source.len()), |t| t.range)
    }
    fn text(&self) -> &str {
        self.token().map_or("", |t| t.text(self.source))
    }
    fn start(&mut self, kind: K) {
        if !self.probe {
            self.builder.start_node(kind.raw());
        }
        self.frames.push(Vec::new());
    }
    fn finish(&mut self) {
        if !self.probe {
            self.builder.finish_node();
        }
        let height = self.frames.pop().unwrap().into_iter().max().unwrap_or(0) + 1;
        if let Some(frame) = self.frames.last_mut() {
            frame.push(height);
        }
        if height > self.limits.max_depth && !self.halted {
            self.limit("syntax tree nesting budget exhausted");
        }
    }
    fn checkpoint(&mut self) -> Marker {
        self.trivia();
        Marker {
            checkpoint: (!self.probe).then(|| self.builder.checkpoint()),
            index: self.frames.last().unwrap().len(),
        }
    }
    fn wrap(&mut self, cp: Marker, kind: K) {
        if let Some(checkpoint) = cp.checkpoint {
            self.builder.start_node_at(checkpoint, kind.raw());
        }
        let mut children = self.frames.last_mut().unwrap().split_off(cp.index);
        // A left-associated operation extends a flat chain. Only its new
        // operands/arguments add nesting; the receiver is already complete.
        if matches!(
            kind,
            K::BinaryExpr | K::MemberExpr | K::IndexExpr | K::CallExpr
        ) && let Some(height) = children.iter_mut().find(|height| **height != 0)
        {
            *height -= 1;
        }
        if children.iter().copied().max().unwrap_or(0) + 1 > self.limits.max_depth && !self.halted {
            self.limit("syntax tree nesting budget exhausted");
        }
        self.frames.push(children);
    }
    fn trivia(&mut self) {
        while self.cursor < self.tokens.len()
            && self.tokens[self.cursor].kind.is_trivia()
            && !self.halted
        {
            self.raw_bump();
        }
    }
    fn raw_bump(&mut self) {
        if self.probe {
            let remaining = self.probes.remaining.get();
            if remaining == 0 {
                self.limit("assignment pattern trial budget exhausted");
                return;
            }
            self.probes.remaining.set(remaining - 1);
        }
        let token = self.tokens[self.cursor];
        if !self.probe {
            self.builder
                .token(token.kind.raw(), token.text(self.source));
        }
        self.cursor += 1;
        self.frames.last_mut().unwrap().push(0);
    }
    fn bump(&mut self) {
        if !self.halted {
            self.trivia();
            if self.cursor < self.tokens.len() {
                self.raw_bump();
            }
        }
    }
    fn eat(&mut self, kind: K) -> bool {
        if self.peek() == kind && !self.halted {
            self.bump();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, kind: K) {
        if !self.eat(kind) {
            self.error(D::ExpectedToken, format!("expected {kind:?}"));
        }
    }
    fn error_at(&mut self, code: D, range: TextRange, message: impl Into<String>) {
        if self.halted {
            return;
        }
        if self.probe {
            // A failed trial neither recovers nor emits caller diagnostics.
            self.halted = true;
            return;
        }
        if self.diagnostics.len() + 1 >= self.limits.max_diagnostics {
            if !self.diagnostics.iter().any(|d| d.code == D::BudgetExceeded) {
                self.diagnostics.push(Diagnostic::error(
                    D::BudgetExceeded,
                    range,
                    "diagnostic budget exhausted",
                ));
            }
            self.halted = true;
            self.exhausted = true;
        } else {
            self.diagnostics
                .push(Diagnostic::error(code, range, message));
        }
    }
    fn error(&mut self, code: D, message: impl Into<String>) {
        self.error_at(code, self.range(), message);
    }
    fn enter(&mut self) -> bool {
        if self.halted {
            return false;
        }
        if self.depth >= self.limits.max_depth {
            self.limit("recursive syntax nesting budget exhausted");
            return false;
        }
        self.depth += 1;
        true
    }
    fn leave(&mut self) {
        self.depth -= 1;
    }
    fn limit(&mut self, message: &'static str) {
        self.exhaustion_reason.get_or_insert(message);
        self.error(D::BudgetExceeded, message);
        self.halted = true;
        self.exhausted = true;
    }
    fn remaining(&mut self) {
        self.trivia();
        if self.cursor < self.tokens.len() {
            self.start(K::Error);
            while self.cursor < self.tokens.len() {
                self.raw_bump();
            }
            self.finish();
        }
    }
    fn progress(&mut self, before: usize) {
        if self.cursor == before && self.peek() != K::Eof && !self.halted {
            self.start(K::Error);
            self.bump();
            self.finish();
        }
    }
    fn semicolon(&mut self) {
        if !self.eat(K::Semi) {
            let range = TextRange::empty(self.range().start());
            self.error_at(
                D::MissingSemicolon,
                range,
                "ordinary statements require ';'",
            );
            if let Some(d) = self.diagnostics.last_mut()
                && d.code == D::MissingSemicolon
            {
                d.fix = Some(Fix {
                    range,
                    replacement: ";".into(),
                });
            }
        }
    }
    fn statements(&mut self, end: K, tail: bool) {
        while self.peek() != end && self.peek() != K::Eof && !self.halted {
            let before = self.cursor;
            self.statement(end, tail);
            self.progress(before);
        }
    }
    fn statement(&mut self, end: K, tail: bool) {
        match self.peek() {
            K::Semi => self.bump(),
            K::Use | K::Pub | K::Fn | K::Async | K::Gen => self.declaration(false),
            K::Class if self.nth(1) == K::Ident => self.declaration(false),
            K::Let => self.let_decl(false),
            K::LBrace if !tail || !self.object_statement_ahead() => self.block(),
            K::Return | K::Throw | K::Break | K::Continue => self.jump(),
            K::Try => self.try_stmt(),
            K::While | K::For | K::Loop => {
                self.loop_stmt();
                self.eat(K::Semi);
            }
            K::Ident
                if self.nth(1) == K::Colon
                    && matches!(self.nth(2), K::While | K::For | K::Loop) =>
            {
                self.start(K::LabelStmt);
                self.bump();
                self.bump();
                self.loop_stmt();
                self.finish();
                self.eat(K::Semi);
            }
            _ => {
                self.start(K::ExprStmt);
                let expr = self.expr(0, false, false);
                if !self.eat(K::Semi)
                    && !expr.children_block
                    && !matches!(expr.kind, K::IfExpr | K::MatchExpr)
                    && !(tail
                        && !expr.assignment
                        && expr.kind != K::YieldExpr
                        && (self.peek() == end || self.peek() == K::Eof))
                {
                    self.semicolon();
                }
                self.finish();
            }
        }
    }
    fn object_statement_ahead(&self) -> bool {
        let mut tokens = self.tokens[self.cursor..]
            .iter()
            .filter(|token| !token.kind.is_trivia())
            .map(|token| token.kind);
        tokens.next(); // Opening brace; required outer blocks bypass this choice.
        match tokens.next() {
            Some(K::Ident) => {
                tokens.next() == Some(K::Colon)
                    && !matches!(tokens.next(), Some(K::While | K::For | K::Loop))
            }
            Some(open @ (K::StringStart | K::LBracket)) => {
                let close = if open == K::StringStart {
                    K::StringEnd
                } else {
                    K::RBracket
                };
                let mut depth = 1usize;
                for kind in tokens.by_ref() {
                    if kind == open {
                        depth += 1;
                    } else if kind == close {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                depth == 0 && tokens.next() == Some(K::Colon)
            }
            Some(K::Ellipsis) => true,
            _ => false,
        }
    }
    fn block(&mut self) {
        if !self.enter() {
            return;
        }
        let previous_pipe = std::mem::replace(&mut self.stop_pipe, false);
        self.start(K::Block);
        self.expect(K::LBrace);
        self.statements(K::RBrace, true);
        self.expect(K::RBrace);
        self.finish();
        self.leave();
        self.stop_pipe = previous_pipe;
    }
    fn expr(&mut self, min: u8, head: bool, value: bool) -> Expr {
        if !self.enter() {
            return Expr::new(K::Error);
        }
        let cp = self.checkpoint();
        let mut lhs = self.atom(head, value);
        while !self.halted {
            if lhs.children_block
                && !value
                && min == 0
                && self.tokens[self.cursor..]
                    .iter()
                    .take_while(|t| t.kind.is_trivia())
                    .any(|t| t.text(self.source).contains(['\n', '\r']))
            {
                break;
            }
            let op = self.peek();
            if self.stop_pipe && op == K::Pipe {
                break;
            }
            if matches!(
                op,
                K::Dot
                    | K::QuestionDot
                    | K::LBracket
                    | K::QuestionBracket
                    | K::LParen
                    | K::QuestionParen
            ) && 15 >= min
            {
                if lhs.missing_else {
                    self.error(D::MissingElse, "if used as a value requires else");
                }
                let kind = match op {
                    K::Dot | K::QuestionDot => K::MemberExpr,
                    K::LBracket | K::QuestionBracket => K::IndexExpr,
                    _ => K::CallExpr,
                };
                self.wrap(cp, kind);
                self.bump();
                let mut named_children = false;
                match kind {
                    K::MemberExpr => self.expect(K::Ident),
                    K::IndexExpr => {
                        self.expr(0, false, true);
                        self.expect(K::RBracket);
                    }
                    _ => named_children = self.arguments(),
                }
                self.finish();
                lhs = Expr {
                    kind,
                    height: lhs.height + 1,
                    target: matches!(op, K::Dot | K::LBracket) && lhs.kind != K::AssignExpr,
                    children_named: named_children,
                    ..Expr::new(kind)
                };
            } else if op == K::LBrace
                && !head
                && matches!(
                    lhs.kind,
                    K::CallExpr | K::NameExpr | K::MemberExpr | K::IndexExpr | K::GroupExpr
                )
                && 15 >= min
            {
                let conflict = lhs.children_named || lhs.children_block;
                self.wrap(cp, K::CallExpr);
                self.start(K::Children);
                if conflict {
                    self.error(D::ChildrenConflict, "a call can supply children only once");
                }
                self.block();
                self.finish();
                self.finish();
                lhs = Expr {
                    height: lhs.height + 1,
                    children_block: true,
                    ..Expr::new(K::CallExpr)
                };
            } else if let Some((left, right, kind)) = infix(op) {
                if left < min {
                    break;
                }
                let operator_range = self.range();
                if lhs.missing_else {
                    self.error_at(
                        D::MissingElse,
                        operator_range,
                        "if used as a value requires else",
                    );
                }
                self.wrap(cp, kind);
                self.bump();
                let rhs = if op == K::Is {
                    self.type_atom(true);
                    Expr::new(K::TypeName)
                } else {
                    self.expr(right, head, kind != K::AssignExpr || value)
                };
                if is_comparison(op)
                    && (is_comparison_kind(lhs.kind) || is_comparison_kind(rhs.kind))
                {
                    self.error_at(
                        D::ChainedComparison,
                        operator_range,
                        "comparisons require parentheses when chained",
                    );
                }
                if kind == K::RangeExpr && (lhs.kind == K::RangeExpr || rhs.kind == K::RangeExpr) {
                    self.error_at(
                        D::ChainedRange,
                        operator_range,
                        "ranges require parentheses when chained",
                    );
                }
                if kind == K::AssignExpr && !lhs.target {
                    self.error_at(
                        D::InvalidAssignment,
                        operator_range,
                        "assignment requires a location or destructuring names",
                    );
                }
                if lhs.assignment || rhs.assignment && kind != K::AssignExpr {
                    self.error_at(
                        D::InvalidAssignment,
                        operator_range,
                        "assignment has no value for another expression",
                    );
                }
                self.finish();
                // ComparisonExpr has a separate internal marker in Expr; the tree remains BinaryExpr.
                lhs = Expr {
                    kind: if is_comparison(op) { K::Is } else { kind },
                    height: lhs.height.max(rhs.height) + 1,
                    assignment: kind == K::AssignExpr,
                    assignment_range: (kind == K::AssignExpr).then_some(operator_range),
                    ..Expr::new(kind)
                };
            } else {
                break;
            }
        }
        if value && lhs.assignment {
            self.error_at(
                D::InvalidAssignment,
                lhs.assignment_range.unwrap_or(self.range()),
                "assignment has no expression value",
            );
        }
        self.leave();
        lhs
    }
    // Speculate with the actual pattern/type/default-expression productions.
    // The borrowed tokens, isolated cursor and silent sink preserve caller state.
    // Completed nested grammar results are cached; all trials share a linear
    // token-work budget, including recursively nested default expressions.
    fn assignment_pattern_ahead(&mut self) -> bool {
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
    fn atom(&mut self, head: bool, value: bool) -> Expr {
        let negative = std::mem::take(&mut self.allow_min);
        if self.assignment_pattern_ahead() {
            let shape = self.pattern();
            return Expr::target(shape.kind.unwrap_or(K::Error));
        }
        match self.peek() {
            K::Ident => {
                self.start(K::NameExpr);
                self.bump();
                self.finish();
                Expr::target(K::NameExpr)
            }
            K::Number | K::True | K::False | K::Null => {
                self.literal(negative);
                Expr::new(K::LiteralExpr)
            }
            K::StringStart => {
                self.string_expr();
                Expr::new(K::StringExpr)
            }
            K::LParen => self.tuple(),
            K::LBracket => self.array(),
            K::LBrace => {
                self.object();
                Expr::new(K::ObjectExpr)
            }
            K::Pipe | K::OrOr => {
                self.closure();
                Expr::new(K::ClosureExpr)
            }
            K::If => self.if_expr(value),
            K::Match => {
                self.match_expr();
                Expr::new(K::MatchExpr)
            }
            K::Class => {
                self.class_decl(false);
                Expr::new(K::ClassDecl)
            }
            K::Await | K::Bang | K::Tilde | K::Plus | K::Minus => {
                self.start(K::PrefixExpr);
                let op = self.peek();
                self.bump();
                self.allow_min = op == K::Minus;
                let rhs = self.expr(14, head, true);
                self.finish();
                Expr {
                    height: rhs.height + 1,
                    ..Expr::new(K::PrefixExpr)
                }
            }
            K::Yield => {
                self.start(K::YieldExpr);
                self.bump();
                let rhs = if matches!(self.peek(), K::Semi | K::RBrace | K::Comma | K::Eof) {
                    Expr::new(K::LiteralExpr)
                } else {
                    self.expr(2, head, true)
                };
                self.finish();
                Expr {
                    height: rhs.height + 1,
                    ..Expr::new(K::YieldExpr)
                }
            }
            _ => {
                self.start(K::Error);
                self.error(D::ExpectedExpression, "expected expression");
                if !matches!(
                    self.peek(),
                    K::Eof
                        | K::Semi
                        | K::Comma
                        | K::RParen
                        | K::RBracket
                        | K::RBrace
                        | K::InterpolationEnd
                        | K::FatArrow
                        | K::Let
                        | K::Fn
                        | K::Class
                        | K::Else
                ) {
                    self.bump();
                }
                self.finish();
                Expr::new(K::Error)
            }
        }
    }
    fn literal(&mut self, negative: bool) {
        self.start(K::LiteralExpr);
        if self.peek() == K::Number && !negative {
            let text: String = self.text().chars().filter(|&c| c != '_').collect();
            let (digits, radix) = if text.starts_with("0x") || text.starts_with("0X") {
                (&text[2..], 16)
            } else if text.starts_with("0o") || text.starts_with("0O") {
                (&text[2..], 8)
            } else if text.starts_with("0b") || text.starts_with("0B") {
                (&text[2..], 2)
            } else {
                (&text[..], 10)
            };
            if u64::from_str_radix(digits, radix).is_ok_and(|n| n == 1 << 63) {
                self.error(
                    D::IntegerOverflow,
                    "positive integer exceeds int64; the minimum magnitude requires unary minus",
                );
            }
        }
        self.bump();
        self.finish();
    }
    fn string_expr(&mut self) {
        let previous_pipe = std::mem::replace(&mut self.stop_pipe, false);
        self.start(K::StringExpr);
        self.bump();
        while !matches!(self.peek(), K::StringEnd | K::Eof) && !self.halted {
            let before = self.cursor;
            if self.eat(K::StringText) {
                continue;
            }
            if self.peek() == K::InterpolationStart {
                self.start(K::Interpolation);
                self.bump();
                self.expr(0, false, true);
                self.expect(K::InterpolationEnd);
                self.finish();
            } else {
                break;
            }
            self.progress(before);
        }
        self.expect(K::StringEnd);
        self.stop_pipe = previous_pipe;
        self.finish();
    }
    fn tuple(&mut self) -> Expr {
        let previous_pipe = std::mem::replace(&mut self.stop_pipe, false);
        let cp = self.checkpoint();
        self.bump();
        let mut count = 0usize;
        let mut comma = false;
        let mut target = true;
        while !matches!(self.peek(), K::RParen | K::Eof) && !self.halted {
            let before = self.cursor;
            let item = self.expr(0, false, true);
            target &= item.target;
            count += 1;
            if !self.eat(K::Comma) {
                break;
            }
            comma = true;
            self.progress(before);
        }
        self.expect(K::RParen);
        let kind = if count == 1 && !comma {
            K::GroupExpr
        } else {
            K::TupleExpr
        };
        self.wrap(cp, kind);
        self.finish();
        self.stop_pipe = previous_pipe;
        Expr {
            target: kind == K::GroupExpr && target,
            ..Expr::new(kind)
        }
    }
    fn array(&mut self) -> Expr {
        let previous_pipe = std::mem::replace(&mut self.stop_pipe, false);
        self.start(K::ArrayExpr);
        self.bump();
        while !matches!(self.peek(), K::RBracket | K::Eof) && !self.halted {
            let before = self.cursor;
            let spread = self.peek() == K::Ellipsis;
            if spread {
                self.start(K::Spread);
                self.bump();
            }
            self.expr(0, false, true);
            if spread {
                self.finish();
            }
            if !self.eat(K::Comma) {
                break;
            }
            self.progress(before);
        }
        self.expect(K::RBracket);
        self.finish();
        self.stop_pipe = previous_pipe;
        Expr::new(K::ArrayExpr)
    }
    fn object(&mut self) {
        let previous_pipe = std::mem::replace(&mut self.stop_pipe, false);
        self.start(K::ObjectExpr);
        self.bump();
        while !matches!(self.peek(), K::RBrace | K::Eof) && !self.halted {
            let before = self.cursor;
            if self.peek() == K::Ellipsis {
                self.start(K::Spread);
                self.bump();
                self.expr(0, false, true);
                self.finish();
            } else {
                self.start(K::ObjectField);
                if self.eat(K::LBracket) {
                    self.expr(0, false, true);
                    self.expect(K::RBracket);
                } else if self.peek() == K::StringStart {
                    self.string_expr();
                } else {
                    self.expect(K::Ident);
                }
                self.expect(K::Colon);
                self.expr(0, false, true);
                self.finish();
            }
            if !self.eat(K::Comma) {
                break;
            }
            self.progress(before);
        }
        self.expect(K::RBrace);
        self.stop_pipe = previous_pipe;
        self.finish();
    }
    fn arguments(&mut self) -> bool {
        let previous_pipe = std::mem::replace(&mut self.stop_pipe, false);
        self.start(K::ArgList);
        let mut names: std::collections::HashMap<String, TextRange> =
            std::collections::HashMap::new();
        let mut children = false;
        while !matches!(self.peek(), K::RParen | K::Eof) && !self.halted {
            let before = self.cursor;
            let named = self.peek() == K::Ident && self.nth(1) == K::Colon;
            self.start(if named {
                K::NamedArg
            } else if self.peek() == K::Ellipsis {
                K::Spread
            } else {
                K::Arg
            });
            if named {
                let range = self.range();
                let name = self.text().to_owned();
                children |= name == "children";
                if let Some(&previous) = names.get(&name) {
                    self.error_at(D::DuplicateArgument, range, "duplicate named argument");
                    if let Some(d) = self.diagnostics.last_mut()
                        && d.code == D::DuplicateArgument
                    {
                        d.related.push(RelatedRange {
                            range: previous,
                            message: "first argument with this name".into(),
                        });
                    }
                }
                names.entry(name).or_insert(range);
                self.bump();
                self.bump();
            } else {
                if !names.is_empty() && self.peek() != K::Ellipsis {
                    self.error(
                        D::InvalidArgumentOrder,
                        "positional arguments must precede named arguments",
                    );
                }
                self.eat(K::Ellipsis);
            }
            self.expr(0, false, true);
            self.finish();
            if !self.eat(K::Comma) {
                break;
            }
            self.progress(before);
        }
        self.expect(K::RParen);
        self.finish();
        self.stop_pipe = previous_pipe;
        children
    }
    fn declaration(&mut self, member: bool) {
        let cp = self.checkpoint();
        let public = self.eat(K::Pub);
        let static_member = self.eat(K::Static);
        if static_member && !member {
            self.error(
                D::ExpectedDeclaration,
                "static declarations belong to a class",
            );
        }
        match self.peek() {
            K::Fn | K::Async | K::Gen => self.fn_decl(),
            K::Class if !member => self.class_decl(true),
            K::Use if !member => self.use_decl(),
            K::Let => self.let_decl(member),
            _ => {
                self.error(
                    D::ExpectedDeclaration,
                    "expected declaration after modifier",
                );
                self.start(K::Error);
                self.bump();
                self.finish();
            }
        }
        // Modifiers are owned by the same declaration node, without changing its grammar.
        if public || static_member {
            self.wrap(cp, K::ModifiedDecl);
            self.finish();
        }
    }
    fn let_decl(&mut self, field: bool) {
        self.start(K::LetDecl);
        self.expect(K::Let);
        self.eat(K::Mut);
        self.pattern();
        if self.eat(K::Colon) {
            self.ty();
        }
        if self.eat(K::Eq) {
            self.expr(0, false, true);
        } else if !field {
            self.error(D::ExpectedToken, "let binding requires an initializer");
        }
        self.semicolon();
        self.finish();
    }
    fn fn_decl(&mut self) {
        self.start(K::FnDecl);
        if matches!(self.peek(), K::Async | K::Gen) {
            self.bump();
        }
        self.expect(K::Fn);
        self.expect(K::Ident);
        self.parameters(K::RParen, true);
        if self.eat(K::Arrow) {
            self.ty();
        }
        self.block();
        self.finish();
    }
    fn parameters(&mut self, end: K, parens: bool) {
        self.start(K::ParamList);
        if parens {
            self.expect(K::LParen);
        }
        let mut rest_seen = false;
        while !matches!(self.peek(), K::Eof) && self.peek() != end && !self.halted {
            let before = self.cursor;
            self.start(K::Param);
            if rest_seen {
                self.error(D::InvalidRest, "rest parameter must be last");
            }
            let rest = self.eat(K::Ellipsis);
            rest_seen |= rest;
            self.eat(K::Mut);
            self.expect(K::Ident);
            if self.eat(K::Colon) {
                if parens {
                    self.ty();
                } else {
                    self.type_atom(true);
                }
            }
            if self.eat(K::Eq) {
                if rest {
                    self.error(D::InvalidRest, "rest parameter cannot have a default");
                }
                let previous = self.stop_pipe;
                self.stop_pipe = !parens;
                self.expr(0, false, true);
                self.stop_pipe = previous;
            }
            self.finish();
            if !self.eat(K::Comma) {
                break;
            }
            self.progress(before);
        }
        self.expect(end);
        self.finish();
    }
    fn closure(&mut self) {
        self.start(K::ClosureExpr);
        if !self.eat(K::OrOr) {
            self.expect(K::Pipe);
            self.parameters(K::Pipe, false);
        }
        if self.peek() == K::LBrace {
            self.block();
        } else {
            self.expr(0, false, true);
        }
        self.finish();
    }
    fn class_decl(&mut self, named: bool) {
        if !self.enter() {
            return;
        }
        self.start(K::ClassDecl);
        self.expect(K::Class);
        if named {
            self.expect(K::Ident);
        } else if self.peek() == K::Ident {
            self.error(
                D::ExpectedDeclaration,
                "class expressions are anonymous; reference a named class declaration by its name",
            );
            self.bump();
        }
        if self.eat(K::Colon) {
            self.expr(0, true, true);
        }
        self.expect(K::LBrace);
        while !matches!(self.peek(), K::RBrace | K::Eof) && !self.halted {
            let before = self.cursor;
            if matches!(
                self.peek(),
                K::Let | K::Fn | K::Async | K::Gen | K::Static | K::Pub
            ) {
                self.declaration(true);
            } else {
                self.error(
                    D::ExpectedDeclaration,
                    "class body requires fields or methods",
                );
                self.start(K::Error);
                self.bump();
                self.finish();
            }
            self.progress(before);
        }
        self.expect(K::RBrace);
        self.finish();
        self.leave();
    }
    fn use_decl(&mut self) {
        if !self.probe {
            self.import_offsets.push(self.range().start());
        }
        self.start(K::UseDecl);
        self.expect(K::Use);
        self.use_path();
        self.semicolon();
        self.finish();
    }
    fn use_path(&mut self) {
        if !self.enter() {
            return;
        }
        self.start(K::UsePath);
        if self.eat(K::Star) {
            self.finish();
            self.leave();
            return;
        }
        self.expect(K::Ident);
        while self.eat(K::ColonColon) && !self.halted {
            if self.eat(K::Star) {
                break;
            }
            if self.peek() == K::LBrace {
                self.start(K::UseGroup);
                self.bump();
                while !matches!(self.peek(), K::RBrace | K::Eof) && !self.halted {
                    let before = self.cursor;
                    self.use_path();
                    if !self.eat(K::Comma) {
                        break;
                    }
                    self.progress(before);
                }
                self.expect(K::RBrace);
                self.finish();
                break;
            }
            self.expect(K::Ident);
        }
        if self.eat(K::As) {
            self.expect(K::Ident);
        }
        self.finish();
        self.leave();
    }
    fn if_expr(&mut self, value: bool) -> Expr {
        if !self.enter() {
            return Expr::new(K::Error);
        }
        self.start(K::IfExpr);
        self.bump();
        self.condition();
        self.block();
        let has_else = self.eat(K::Else);
        if has_else {
            if self.peek() == K::If {
                self.if_expr(value);
            } else {
                self.block();
            }
        } else if value {
            self.error(D::MissingElse, "if used as a value requires else");
        }
        self.finish();
        self.leave();
        Expr {
            missing_else: !has_else,
            ..Expr::new(K::IfExpr)
        }
    }
    fn condition(&mut self) {
        if self.eat(K::Let) {
            self.pattern();
            self.expect(K::Eq);
        }
        self.expr(0, true, true);
    }
    fn loop_stmt(&mut self) {
        let keyword = self.peek();
        self.start(match keyword {
            K::While => K::WhileStmt,
            K::For => K::ForStmt,
            _ => K::LoopStmt,
        });
        self.bump();
        match keyword {
            K::While => self.condition(),
            K::For => {
                self.eat(K::Await);
                self.pattern();
                self.expect(K::In);
                self.expr(0, true, true);
            }
            _ => (),
        }
        self.block();
        self.finish();
    }
    fn match_expr(&mut self) {
        self.start(K::MatchExpr);
        self.bump();
        self.expr(0, true, true);
        self.expect(K::LBrace);
        while !matches!(self.peek(), K::RBrace | K::Eof) && !self.halted {
            let before = self.cursor;
            self.start(K::MatchArm);
            self.pattern();
            if self.eat(K::If) {
                self.expr(0, true, true);
            }
            self.expect(K::FatArrow);
            if self.peek() == K::LBrace {
                self.block();
            } else {
                self.expr(0, false, true);
            }
            self.finish();
            if !self.eat(K::Comma) && self.peek() != K::RBrace {
                self.error(D::ExpectedToken, "match arms require ','");
            }
            self.progress(before);
        }
        self.expect(K::RBrace);
        self.finish();
    }
    fn jump(&mut self) {
        let keyword = self.peek();
        self.start(match keyword {
            K::Return => K::ReturnStmt,
            K::Throw => K::ThrowStmt,
            K::Break => K::BreakStmt,
            _ => K::ContinueStmt,
        });
        self.bump();
        if matches!(keyword, K::Break | K::Continue) {
            self.eat(K::Ident);
        } else if !matches!(self.peek(), K::Semi | K::RBrace | K::Eof) {
            self.expr(0, false, true);
        }
        self.semicolon();
        self.finish();
    }
    fn try_stmt(&mut self) {
        self.start(K::TryStmt);
        self.bump();
        self.block();
        let mut handler = false;
        if self.eat(K::Catch) {
            handler = true;
            self.start(K::CatchClause);
            self.expect(K::Ident);
            self.block();
            self.finish();
        }
        if self.eat(K::Finally) {
            handler = true;
            self.start(K::FinallyClause);
            self.block();
            self.finish();
        }
        if !handler {
            self.error(D::ExpectedToken, "try requires catch or finally");
        }
        self.finish();
        self.eat(K::Semi);
    }
    fn ty(&mut self) {
        if !self.enter() {
            return;
        }
        let cp = self.checkpoint();
        self.type_atom(false);
        while self.eat(K::Pipe) && !self.halted {
            self.wrap(cp, K::UnionType);
            self.type_atom(false);
            self.finish();
        }
        self.leave();
    }
    fn type_atom(&mut self, stop_union: bool) {
        match self.peek() {
            K::Ident | K::Null | K::Class => {
                self.start(K::TypeName);
                self.bump();
                while self.eat(K::ColonColon) && !self.halted {
                    self.expect(K::Ident);
                }
                self.finish();
            }
            K::LParen => {
                let cp = self.checkpoint();
                self.bump();
                let mut count = 0;
                let mut comma = false;
                while !matches!(self.peek(), K::RParen | K::Eof) && !self.halted {
                    let before = self.cursor;
                    self.ty();
                    count += 1;
                    if !self.eat(K::Comma) {
                        break;
                    }
                    comma = true;
                    self.progress(before);
                }
                self.expect(K::RParen);
                self.wrap(
                    cp,
                    if count == 1 && !comma {
                        K::GroupType
                    } else {
                        K::TupleType
                    },
                );
                self.finish();
            }
            K::Fn => {
                self.start(K::FunctionType);
                self.bump();
                self.expect(K::LParen);
                while !matches!(self.peek(), K::RParen | K::Eof) && !self.halted {
                    let before = self.cursor;
                    self.ty();
                    if !self.eat(K::Comma) {
                        break;
                    }
                    self.progress(before);
                }
                self.expect(K::RParen);
                self.expect(K::Arrow);
                if stop_union {
                    if self.enter() {
                        self.type_atom(true);
                        self.leave();
                    }
                } else {
                    self.ty();
                }
                self.finish();
            }
            _ => {
                self.start(K::Error);
                self.error(D::ExpectedType, "expected type annotation");
                if !matches!(
                    self.peek(),
                    K::Eof | K::Eq | K::Comma | K::RParen | K::LBrace | K::Semi | K::Pipe
                ) {
                    self.bump();
                }
                self.finish();
            }
        }
    }
    fn pattern(&mut self) -> PatternShape {
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
fn infix(op: K) -> Option<(u8, u8, K)> {
    Some(match op {
        k if k.is_assignment() => (1, 1, K::AssignExpr),
        K::Range | K::RangeInclusive => (2, 3, K::RangeExpr),
        K::QuestionQuestion => (3, 3, K::BinaryExpr),
        K::OrOr => (4, 5, K::BinaryExpr),
        K::AndAnd => (5, 6, K::BinaryExpr),
        K::EqEq | K::NotEq => (6, 7, K::BinaryExpr),
        K::Lt | K::Le | K::Gt | K::Ge | K::Is => (7, 8, K::BinaryExpr),
        K::Pipe => (8, 9, K::BinaryExpr),
        K::Caret => (9, 10, K::BinaryExpr),
        K::Amp => (10, 11, K::BinaryExpr),
        K::Shl | K::Shr => (11, 12, K::BinaryExpr),
        K::Plus | K::Minus => (12, 13, K::BinaryExpr),
        K::Star | K::Slash | K::Percent => (13, 14, K::BinaryExpr),
        _ => return None,
    })
}

fn is_comparison(op: K) -> bool {
    matches!(
        op,
        K::Lt | K::Le | K::Gt | K::Ge | K::Is | K::EqEq | K::NotEq
    )
}
fn is_comparison_kind(kind: K) -> bool {
    kind == K::Is
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    #[test]
    fn trials_isolate_caller_state_and_exhaust_shared_work_without_consuming_it() {
        for (source, budget, expected, exhausted) in [
            ("((a,b) is Pair) = value;", 100, true, false),
            ("(f(a,b).x) = value;", 100, false, false),
            ("((a,b) is Pair) = value;", 1, false, true),
        ] {
            let limits = Limits::default().normalized();
            let lexed = lex_with_limits(source, limits).unwrap();
            let probes = PatternProbes {
                remaining: Cell::new(budget),
                cache: RefCell::new(HashMap::new()),
            };
            let mut parser = Parser {
                source,
                tokens: &lexed.tokens,
                cursor: 0,
                builder: GreenNodeBuilder::new(),
                import_offsets: Vec::new(),
                diagnostics: Vec::new(),
                limits,
                depth: 0,
                halted: false,
                exhausted: false,
                exhaustion_reason: None,
                frames: Vec::new(),
                allow_min: true,
                stop_pipe: true,
                probe: false,
                probes: &probes,
            };
            parser.start(K::Root);
            let checkpoint = format!("{:?}", parser.builder.checkpoint());
            assert_eq!(parser.assignment_pattern_ahead(), expected);
            assert_eq!(parser.cursor, 0);
            assert_eq!(format!("{:?}", parser.builder.checkpoint()), checkpoint);
            assert_eq!(parser.frames, vec![Vec::<usize>::new()]);
            assert!(parser.import_offsets.is_empty());
            assert_eq!(parser.depth, 0);
            assert!(parser.allow_min && parser.stop_pipe);
            assert_eq!(parser.exhausted, exhausted);
            if exhausted {
                assert_eq!(probes.remaining.get(), 0);
                assert_eq!(parser.diagnostics.len(), 1);
                assert_eq!(parser.diagnostics[0].code, D::BudgetExceeded);
                assert_eq!(
                    parser.diagnostics[0].message,
                    "assignment pattern trial budget exhausted"
                );
                assert_eq!(parser.diagnostics[0].range, span(0, 1));
                assert!(probes.cache.borrow().is_empty());
            } else {
                assert!(parser.diagnostics.is_empty());
                let remaining = probes.remaining.get();
                assert_eq!(parser.assignment_pattern_ahead(), expected);
                assert_eq!(probes.remaining.get(), remaining);
            }
            parser.remaining();
            parser.finish();
            let tree = SyntaxNode::new_root(parser.builder.finish());
            assert_eq!(tree.text().to_string(), source);
        }
    }
}
