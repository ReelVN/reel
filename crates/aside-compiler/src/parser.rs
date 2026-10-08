use crate::diagnostic::span;
use crate::{
    Diagnostic, DiagnosticCode as D, Fix, GreenNode, Limits, RelatedRange, SourceTooLarge,
    SyntaxKind as K, SyntaxNode, TextRange, Token, lex_with_limits,
};
use rowan::{Checkpoint, GreenNodeBuilder};

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
    diagnostics: Vec<Diagnostic>,
    exhausted: bool,
}
impl Parse {
    pub fn syntax(&self) -> SyntaxNode {
        SyntaxNode::new_root(self.green.clone())
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
    let mut p = Parser {
        source,
        tokens: lexed.tokens,
        diagnostics: lexed.diagnostics,
        cursor: 0,
        builder: GreenNodeBuilder::new(),
        limits,
        depth: 0,
        halted: false,
        exhausted: lexed.exhausted,
        frames: Vec::new(),
        allow_min: false,
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
        EntryPoint::Pattern => p.pattern(),
        EntryPoint::Type => p.ty(),
    }
    if !p.halted && p.peek() != K::Eof {
        p.error(D::ExpectedToken, "unexpected tokens after entry point");
    }
    p.remaining();
    p.finish();
    Ok(Parse {
        green: p.builder.finish(),
        diagnostics: p.diagnostics,
        exhausted: p.exhausted,
    })
}
struct Parser<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    cursor: usize,
    builder: GreenNodeBuilder<'static>,
    diagnostics: Vec<Diagnostic>,
    limits: Limits,
    depth: usize,
    halted: bool,
    exhausted: bool,
    frames: Vec<Vec<usize>>,
    allow_min: bool,
}
#[derive(Clone, Copy)]
struct Marker {
    checkpoint: Checkpoint,
    index: usize,
}
#[derive(Clone, Copy)]
struct Expr {
    kind: K,
    height: usize,
    target: bool,
    assignment: bool,
    missing_else: bool,
    children_named: bool,
}
impl Expr {
    fn new(kind: K) -> Self {
        Self {
            kind,
            height: 1,
            target: false,
            assignment: false,
            missing_else: false,
            children_named: false,
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
        self.builder.start_node(kind.raw());
        self.frames.push(Vec::new());
    }
    fn finish(&mut self) {
        self.builder.finish_node();
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
            checkpoint: self.builder.checkpoint(),
            index: self.frames.last().unwrap().len(),
        }
    }
    fn wrap(&mut self, cp: Marker, kind: K) {
        self.builder.start_node_at(cp.checkpoint, kind.raw());
        let children = self.frames.last_mut().unwrap().split_off(cp.index);
        if children.iter().copied().max().unwrap_or(0) + 1 > self.limits.max_depth && !self.halted {
            self.limit("syntax tree nesting budget exhausted");
        }
        self.frames.push(children);
    }
    fn trivia(&mut self) {
        while self.cursor < self.tokens.len() && self.tokens[self.cursor].kind.is_trivia() {
            self.raw_bump();
        }
    }
    fn raw_bump(&mut self) {
        let token = self.tokens[self.cursor];
        self.builder
            .token(token.kind.raw(), token.text(self.source));
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
    fn limit(&mut self, message: &str) {
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
            K::Use | K::Pub | K::Fn | K::Async | K::Gen | K::Class => self.declaration(false),
            K::Let => self.let_decl(false),
            K::LBrace => self.block(),
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
    fn block(&mut self) {
        if !self.enter() {
            return;
        }
        self.start(K::Block);
        self.expect(K::LBrace);
        self.statements(K::RBrace, true);
        self.expect(K::RBrace);
        self.finish();
        self.leave();
    }
    fn expr(&mut self, min: u8, head: bool, value: bool) -> Expr {
        if !self.enter() {
            return Expr::new(K::Error);
        }
        let cp = self.checkpoint();
        let mut lhs = self.atom(head, value);
        while !self.halted {
            let op = self.peek();
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
            } else if op == K::LBrace && !head && lhs.kind == K::CallExpr && 15 >= min {
                let conflict = lhs.children_named;
                self.wrap(cp, K::CallExpr);
                self.start(K::Children);
                if conflict {
                    self.error(
                        D::ChildrenConflict,
                        "explicit children and a trailing children block cannot coexist",
                    );
                }
                self.block();
                self.finish();
                self.finish();
                lhs = Expr {
                    height: lhs.height + 1,
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
                    self.ty();
                    Expr::new(K::TypeName)
                } else {
                    self.expr(right, head, true)
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
                if lhs.assignment || rhs.assignment {
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
                    ..Expr::new(kind)
                };
            } else {
                break;
            }
            if lhs.height >= self.limits.max_depth {
                self.limit("expression tree nesting budget exhausted");
            }
        }
        if value && lhs.assignment {
            self.error(D::InvalidAssignment, "assignment has no expression value");
        }
        self.leave();
        lhs
    }
    fn atom(&mut self, head: bool, value: bool) -> Expr {
        let negative = std::mem::take(&mut self.allow_min);
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
                self.class_decl();
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
                    self.expr(0, head, true)
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
        self.finish();
    }
    fn tuple(&mut self) -> Expr {
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
        Expr {
            target,
            ..Expr::new(kind)
        }
    }
    fn array(&mut self) -> Expr {
        self.start(K::ArrayExpr);
        self.bump();
        let mut target = true;
        while !matches!(self.peek(), K::RBracket | K::Eof) && !self.halted {
            let before = self.cursor;
            let spread = self.peek() == K::Ellipsis;
            if spread {
                self.start(K::Spread);
                self.bump();
            }
            let item = self.expr(0, false, true);
            target &= item.kind == K::NameExpr || spread && item.target;
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
        Expr {
            target,
            ..Expr::new(K::ArrayExpr)
        }
    }
    fn object(&mut self) {
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
        self.finish();
    }
    fn arguments(&mut self) -> bool {
        self.start(K::ArgList);
        let mut names: Vec<(String, TextRange)> = Vec::new();
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
                if let Some((_, previous)) = names.iter().find(|(n, _)| *n == name) {
                    let previous = *previous;
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
                names.push((name, range));
                self.bump();
                self.bump();
            } else {
                if !names.is_empty() {
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
        children
    }
    fn declaration(&mut self, member: bool) {
        let cp = self.checkpoint();
        self.eat(K::Pub);
        let static_member = self.eat(K::Static);
        if static_member && !member {
            self.error(
                D::ExpectedDeclaration,
                "static declarations belong to a class",
            );
        }
        match self.peek() {
            K::Fn | K::Async | K::Gen => self.fn_decl(),
            K::Class if !member => self.class_decl(),
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
        self.wrap(cp, K::ModifiedDecl);
        self.finish();
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
                self.ty();
            }
            if self.eat(K::Eq) {
                if rest {
                    self.error(D::InvalidRest, "rest parameter cannot have a default");
                }
                self.expr(0, false, true);
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
    fn class_decl(&mut self) {
        if !self.enter() {
            return;
        }
        self.start(K::ClassDecl);
        self.expect(K::Class);
        if !self.eat(K::Ident) {
            self.error(
                D::UnresolvedSyntax,
                "anonymous class syntax requires specification resolution (aside-language.md:136)",
            );
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
            self.expr(0, false, true);
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
        self.type_atom();
        while self.eat(K::Pipe) && !self.halted {
            self.wrap(cp, K::UnionType);
            self.type_atom();
            self.finish();
        }
        self.leave();
    }
    fn type_atom(&mut self) {
        match self.peek() {
            K::Ident | K::Null => {
                self.start(K::TypeName);
                self.bump();
                while self.eat(K::ColonColon) && !self.halted {
                    self.expect(K::Ident);
                }
                self.finish();
            }
            K::LParen => {
                self.start(K::TupleType);
                self.bump();
                while !matches!(self.peek(), K::RParen | K::Eof) && !self.halted {
                    let before = self.cursor;
                    self.ty();
                    if !self.eat(K::Comma) {
                        break;
                    }
                    self.progress(before);
                }
                self.expect(K::RParen);
                self.finish();
            }
            K::Fn => {
                self.start(K::FunctionType);
                self.error(D::UnresolvedSyntax, "function type spelling requires specification resolution (aside-language.md:163)");
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
                self.ty();
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
    fn pattern(&mut self) {
        if !self.enter() {
            return;
        }
        let cp = self.checkpoint();
        self.pattern_atom();
        if self.eat(K::Is) {
            self.wrap(cp, K::TypePattern);
            self.error(D::UnresolvedSyntax, "type-test pattern spelling requires specification resolution (aside-language.md:114)");
            self.ty();
            self.finish();
        }
        while self.eat(K::Pipe) && !self.halted {
            self.wrap(cp, K::OrPattern);
            self.error(
                D::UnresolvedSyntax,
                "OR pattern spelling requires specification resolution (aside-language.md:114)",
            );
            self.pattern();
            self.finish();
        }
        self.leave();
    }
    fn pattern_atom(&mut self) {
        match self.peek() {
            K::Ident => {
                self.start(if self.text() == "_" {
                    K::WildcardPattern
                } else {
                    K::NamePattern
                });
                self.bump();
                self.finish();
            }
            K::Number | K::True | K::False | K::Null | K::Minus | K::StringStart => {
                self.start(K::LiteralPattern);
                let negative = self.eat(K::Minus);
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
            }
            K::LParen | K::LBracket => {
                let tuple = self.peek() == K::LParen;
                let end = if tuple { K::RParen } else { K::RBracket };
                let cp = self.checkpoint();
                self.bump();
                let mut rest = false;
                let mut count = 0;
                let mut comma = false;
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
                        self.pattern();
                    }
                    count += 1;
                    if !self.eat(K::Comma) {
                        break;
                    }
                    comma = true;
                    self.progress(before);
                }
                self.expect(end);
                self.wrap(
                    cp,
                    if !tuple {
                        K::ArrayPattern
                    } else if count == 1 && !comma {
                        K::GroupPattern
                    } else {
                        K::TuplePattern
                    },
                );
                self.finish();
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
