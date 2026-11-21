//! CST-based author formatting shared by command-line and editor clients.

use crate::{Diagnostic, Parse, SourceTooLarge, SyntaxElement, SyntaxKind as K, SyntaxNode, parse};

/// A source-size or syntax error that prevents formatting.
#[derive(Debug)]
pub enum FormatError {
    SourceTooLarge(SourceTooLarge),
    Syntax(Vec<Diagnostic>),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceTooLarge(error) => error.fmt(f),
            Self::Syntax(diagnostics) => {
                write!(f, "source has {} syntax diagnostics", diagnostics.len())
            }
        }
    }
}

impl std::error::Error for FormatError {}

/// Format Aside source with four-space indentation and a 100-character line width.
///
/// Columns count Unicode scalar values. Strings, numbers, comments, and all other
/// non-whitespace tokens retain their spelling and order. Invalid syntax returns
/// diagnostics instead of a replacement. Nonempty output ends with one LF.
pub fn format(source: &str) -> Result<String, FormatError> {
    let parsed = parse(source).map_err(FormatError::SourceTooLarge)?;
    format_parsed(&parsed).map_err(FormatError::Syntax)
}

/// Format an existing successful parse without parsing its source again.
///
/// This uses the same printer and fixed layout as [`format`]. Semantic analysis
/// is unnecessary; unresolved names do not prevent formatting.
pub fn format_parsed(parsed: &Parse) -> Result<String, Vec<Diagnostic>> {
    if !parsed.is_ok() {
        return Err(parsed.diagnostics().to_vec());
    }
    let root = parsed.syntax();
    let source = root.text().to_string();
    let mut builder = Builder::new(&source);
    let document = builder.build(root);
    Ok(builder.print(document))
}

const WIDTH: usize = 100;
const INFINITE: usize = WIDTH + 1;
type Id = usize;

enum Doc {
    Text(std::ops::Range<usize>),
    Space,
    Line(bool),
    HardLine,
    Concat(Vec<Id>),
    Indent(Id),
    Group(Id),
    Fill(Vec<Id>),
    Chain {
        base: Id,
        tail: Id,
        split: bool,
        call: bool,
    },
    Flow {
        base: Id,
        tail: Id,
        split: bool,
    },
}

struct Entry {
    doc: Doc,
    flat: usize,
    prefix: usize,
    unbroken: bool,
}

struct Part {
    doc: Id,
    kind: K,
    first: Option<K>,
    last: Option<K>,
    leading: usize,
    trailing: usize,
    // The parser owns the closing parenthesis in ArgList, not CallExpr.
    arguments: Option<Vec<Part>>,
}

impl Part {
    fn empty(kind: K, lines: usize) -> Self {
        Self {
            doc: 0,
            kind,
            first: None,
            last: None,
            leading: lines,
            trailing: lines,
            arguments: None,
        }
    }
}

enum Work {
    Node(SyntaxNode, bool),
    Token(crate::SyntaxToken),
    Finish(K, usize, bool),
}

fn compact_value_if(node: &SyntaxNode) -> bool {
    if node.kind() != K::IfExpr
        || node
            .parent()
            .is_some_and(|parent| matches!(parent.kind(), K::ExprStmt | K::IfExpr))
    {
        return false;
    }
    let mut branches = node.children().filter(|part| part.kind() == K::Block);
    if !(0..2).all(|_| {
        branches.next().is_some_and(|branch| {
            let mut statements = branch.children();
            statements
                .next()
                .is_some_and(|statement| statement.kind() == K::ExprStmt)
                && statements.next().is_none()
        })
    }) {
        return false;
    }
    let mut blocks = 0;
    let mut tokens = 0;
    for part in node.descendants_with_tokens() {
        match part.kind() {
            K::Semi | K::LineComment | K::BlockComment => return false,
            K::Block => {
                blocks += 1;
                if blocks > 2 {
                    return false;
                }
            }
            kind if part.as_token().is_some() && !kind.is_trivia() => {
                tokens += 1;
                if tokens > WIDTH {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

struct Builder<'a> {
    source: &'a str,
    docs: Vec<Entry>,
}

impl<'a> Builder<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            docs: vec![Entry {
                doc: Doc::Concat(Vec::new()),
                flat: 0,
                prefix: 0,
                unbroken: true,
            }],
        }
    }

    fn add(&mut self, doc: Doc) -> Id {
        let flat = match &doc {
            Doc::Text(range) => {
                let text = &self.source[range.clone()];
                if text.contains(['\n', '\r']) {
                    INFINITE
                } else {
                    text.chars().count().min(INFINITE)
                }
            }
            Doc::Space | Doc::Line(true) => 1,
            Doc::Line(false) => 0,
            Doc::HardLine => INFINITE,
            Doc::Concat(parts) => parts.iter().fold(0usize, |width, &id| {
                width.saturating_add(self.docs[id].flat).min(INFINITE)
            }),
            Doc::Indent(id) | Doc::Group(id) => self.docs[*id].flat,
            Doc::Fill(parts) => parts
                .iter()
                .fold(parts.len().saturating_sub(1).min(INFINITE), |width, &id| {
                    width.saturating_add(self.docs[id].flat).min(INFINITE)
                }),
            Doc::Chain { base, tail, .. } => self.docs[*base]
                .flat
                .saturating_add(self.docs[*tail].flat)
                .min(INFINITE),
            Doc::Flow { base, tail, .. } => self.docs[*base]
                .flat
                .saturating_add(1)
                .saturating_add(self.docs[*tail].flat)
                .min(INFINITE),
        };
        let (prefix, unbroken) = match &doc {
            Doc::Text(range) => {
                let text = &self.source[range.clone()];
                (
                    text.split(['\n', '\r'])
                        .next()
                        .unwrap_or("")
                        .chars()
                        .count()
                        .min(INFINITE),
                    !text.contains(['\n', '\r']),
                )
            }
            Doc::Space => (1, true),
            Doc::Line(_) | Doc::HardLine => (0, false),
            Doc::Indent(id) | Doc::Group(id) => (self.docs[*id].prefix, self.docs[*id].unbroken),
            Doc::Concat(parts) => self.prefix(parts.iter().copied()),
            Doc::Fill(parts) => {
                let (prefix, unbroken) = self.prefix(parts.iter().take(1).copied());
                (prefix, unbroken && parts.len() <= 1)
            }
            Doc::Chain { base, tail, .. } => self.prefix([*base, *tail].into_iter()),
            Doc::Flow { base, .. } => (self.docs[*base].prefix, false),
        };
        let id = self.docs.len();
        self.docs.push(Entry {
            doc,
            flat,
            prefix,
            unbroken,
        });
        id
    }

    fn prefix(&self, parts: impl Iterator<Item = Id>) -> (usize, bool) {
        let mut prefix = 0usize;
        for id in parts {
            prefix = prefix.saturating_add(self.docs[id].prefix).min(INFINITE);
            if !self.docs[id].unbroken {
                return (prefix, false);
            }
        }
        (prefix, true)
    }

    fn concat(&mut self, parts: Vec<Id>) -> Id {
        match parts.len() {
            0 => 0,
            1 => parts[0],
            _ => self.add(Doc::Concat(parts)),
        }
    }

    fn build(&mut self, root: SyntaxNode) -> Id {
        let mut work = vec![Work::Node(root, false)];
        let mut results = Vec::new();
        while let Some(task) = work.pop() {
            match task {
                Work::Node(node, compact_branch) => {
                    if node.kind() == K::StringExpr {
                        let mut leading = Vec::new();
                        let mut token = node.first_token();
                        while let Some(current) = token {
                            if current.kind() == K::StringStart {
                                let range = usize::from(current.text_range().start())
                                    ..usize::from(node.text_range().end());
                                let doc = self.add(Doc::Text(range));
                                leading.push(Part {
                                    doc,
                                    kind: K::StringExpr,
                                    first: Some(K::StringStart),
                                    last: Some(K::StringEnd),
                                    leading: 0,
                                    trailing: 0,
                                    arguments: None,
                                });
                                break;
                            }
                            leading.push(self.token(current.clone()));
                            token = current.next_token();
                        }
                        results.push(self.sequence(K::StringExpr, leading, false, false));
                    } else {
                        let compact_if = compact_value_if(&node);
                        let children: Vec<_> = node.children_with_tokens().collect();
                        work.push(Work::Finish(
                            node.kind(),
                            children.len(),
                            compact_if || compact_branch,
                        ));
                        for child in children.into_iter().rev() {
                            work.push(match child {
                                SyntaxElement::Node(node) => {
                                    let compact_branch = compact_if && node.kind() == K::Block;
                                    Work::Node(node, compact_branch)
                                }
                                SyntaxElement::Token(token) => Work::Token(token),
                            });
                        }
                    }
                }
                Work::Token(token) => results.push(self.token(token)),
                Work::Finish(kind, count, compact) => {
                    let parts = results.split_off(results.len() - count);
                    results.push(self.node(kind, parts, compact));
                }
            }
        }
        results.pop().map_or(0, |part| part.doc)
    }

    fn token(&mut self, token: crate::SyntaxToken) -> Part {
        let kind = token.kind();
        if kind == K::Whitespace || kind == K::Eof {
            return Part::empty(
                kind,
                token
                    .text()
                    .bytes()
                    .filter(|&byte| byte == b'\n')
                    .count()
                    .max(usize::from(token.text().contains('\r'))),
            );
        }
        let range = usize::from(token.text_range().start())..usize::from(token.text_range().end());
        let doc = self.add(Doc::Text(range));
        Part {
            doc,
            kind,
            first: Some(kind),
            last: Some(kind),
            leading: 0,
            trailing: 0,
            arguments: None,
        }
    }

    fn clean(&self, parts: Vec<Part>) -> Vec<Part> {
        let mut clean: Vec<Part> = Vec::new();
        let mut lines = 0;
        for mut part in parts {
            if part.first.is_none() {
                lines += part.leading;
                continue;
            }
            part.leading += lines;
            lines = part.trailing;
            clean.push(part);
        }
        if let Some(last) = clean.last_mut() {
            last.trailing = lines;
        }
        clean
    }

    fn node(&mut self, kind: K, parts: Vec<Part>, compact: bool) -> Part {
        if kind == K::ArgList {
            let parts = self.clean(parts);
            let mut result = Part::empty(kind, 0);
            result.first = parts.first().and_then(|part| part.first);
            result.last = parts.last().and_then(|part| part.last);
            result.leading = parts.first().map_or(0, |part| part.leading);
            result.arguments = Some(parts);
            return result;
        }
        let mut expanded = Vec::new();
        for mut part in parts {
            if let Some(arguments) = part.arguments.take() {
                expanded.extend(arguments);
            } else {
                expanded.push(part);
            }
        }
        let parts = self.clean(expanded);
        if parts.is_empty() {
            return Part::empty(kind, 0);
        }
        if matches!(kind, K::MemberExpr | K::CallExpr | K::IndexExpr) && parts.len() > 1 {
            let mut iter = parts.into_iter();
            let base = iter.next().unwrap();
            let first_tail = iter.as_slice().first().unwrap();
            let split = kind == K::MemberExpr
                && base.last != Some(K::RBrace)
                && !matches!(first_tail.first, Some(K::LineComment | K::BlockComment));
            let tail = self.sequence(kind, iter.collect(), false, false);
            let tail_doc = if base.last == Some(K::LineComment) {
                let line = self.add(Doc::HardLine);
                self.concat(vec![line, tail.doc])
            } else if (kind == K::CallExpr && tail.first == Some(K::LBrace))
                || matches!(tail.first, Some(K::LineComment | K::BlockComment))
            {
                let separator = if tail.leading > 0
                    && matches!(tail.first, Some(K::LineComment | K::BlockComment))
                {
                    Doc::HardLine
                } else {
                    Doc::Space
                };
                let separator = self.add(separator);
                self.concat(vec![separator, tail.doc])
            } else {
                tail.doc
            };
            let doc = self.add(Doc::Chain {
                base: base.doc,
                tail: tail_doc,
                split,
                call: kind == K::CallExpr && tail.first != Some(K::LBrace),
            });
            return Part {
                doc,
                kind,
                first: base.first,
                last: tail.last,
                leading: base.leading,
                trailing: tail.trailing,
                arguments: None,
            };
        }
        if matches!(kind, K::BinaryExpr | K::RangeExpr | K::UnionType) && parts.len() > 1 {
            let mut iter = parts.into_iter();
            let base = iter.next().unwrap();
            let tail = self.sequence(kind, iter.collect(), false, false);
            let doc = self.add(Doc::Flow {
                base: base.doc,
                tail: tail.doc,
                split: base.last != Some(K::RBrace),
            });
            return Part {
                doc,
                kind,
                first: base.first,
                last: tail.last,
                leading: base.leading,
                trailing: tail.trailing,
                arguments: None,
            };
        }
        let mut result = self.sequence(
            kind,
            parts,
            matches!(kind, K::Root | K::Block),
            compact && kind == K::Block,
        );
        if compact && kind == K::IfExpr {
            result.doc = self.add(Doc::Group(result.doc));
        }
        result
    }

    fn sequence(
        &mut self,
        kind: K,
        parts: Vec<Part>,
        statements: bool,
        compact_branch: bool,
    ) -> Part {
        let parts = self.clean(parts);
        let Some(first) = parts.first() else {
            return Part::empty(kind, 0);
        };
        let leading = first.leading;
        let first_kind = first.first;
        let last_kind = parts.last().unwrap().last;
        let trailing = parts.last().unwrap().trailing;
        let doc = if let Some(open) = parts
            .iter()
            .position(|part| matches!(part.kind, K::LParen | K::LBracket | K::LBrace))
        {
            let close = match parts[open].kind {
                K::LParen => K::RParen,
                K::LBracket => K::RBracket,
                _ => K::RBrace,
            };
            if let Some(end) = parts.iter().rposition(|part| part.kind == close) {
                let block = parts[open].kind == K::LBrace
                    && matches!(kind, K::Block | K::ClassDecl | K::EnumDecl | K::MatchExpr);
                let mut parts = parts;
                let after = parts.split_off(end + 1);
                let closing = parts.pop().unwrap();
                let inside = parts.split_off(open + 1);
                let opening = parts.pop().unwrap();
                let enclosed = self.enclosed(
                    opening,
                    inside,
                    closing,
                    block && !compact_branch,
                    !compact_branch,
                );
                parts.push(enclosed);
                parts.extend(after);
                self.join(&parts, statements, kind)
            } else {
                self.join(&parts, statements, kind)
            }
        } else {
            self.join(&parts, statements, kind)
        };
        Part {
            doc,
            kind,
            first: first_kind,
            last: last_kind,
            leading,
            trailing,
            arguments: None,
        }
    }

    fn enclosed(
        &mut self,
        opening: Part,
        inside: Vec<Part>,
        closing: Part,
        block: bool,
        group: bool,
    ) -> Part {
        let doc = if inside.is_empty() {
            self.concat(vec![opening.doc, closing.doc])
        } else if block {
            let start = self.add(Doc::HardLine);
            let content = self.join(&inside, true, K::Block);
            let content = self.concat(vec![start, content]);
            let content = self.add(Doc::Indent(content));
            let end = self.add(Doc::HardLine);
            self.concat(vec![opening.doc, content, end, closing.doc])
        } else {
            let padded = opening.kind == K::LBrace;
            let start = self.add(Doc::Line(padded));
            let content = self.fill(&inside);
            let content = self.concat(vec![start, content]);
            let content = self.add(Doc::Indent(content));
            let end = self.add(Doc::Line(padded));
            let all = self.concat(vec![opening.doc, content, end, closing.doc]);
            if group {
                self.add(Doc::Group(all))
            } else {
                all
            }
        };
        Part {
            doc,
            kind: opening.kind,
            first: opening.first,
            last: closing.last,
            leading: opening.leading,
            trailing: closing.trailing,
            arguments: None,
        }
    }

    fn fill(&mut self, parts: &[Part]) -> Id {
        let mut groups = Vec::new();
        let mut start = 0;
        for (index, part) in parts.iter().enumerate() {
            if part.kind == K::Comma {
                groups.push(self.join(&parts[start..=index], false, K::ArgList));
                start = index + 1;
            }
        }
        if start < parts.len() {
            groups.push(self.join(&parts[start..], false, K::ArgList));
        }
        self.add(Doc::Fill(groups))
    }

    fn join(&mut self, parts: &[Part], statements: bool, parent: K) -> Id {
        let mut docs = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            if let Some(previous) = index.checked_sub(1).map(|index| &parts[index]) {
                let left = previous.last.unwrap();
                let right = part.first.unwrap();
                let comment = matches!(right, K::LineComment | K::BlockComment);
                let hard = left == K::LineComment
                    || (comment && part.leading > 0)
                    || (left == K::BlockComment && part.leading > 0)
                    || (statements
                        && !(comment && part.leading == 0)
                        && !matches!(right, K::Semi | K::Comma));
                if hard {
                    docs.push(self.add(Doc::HardLine));
                    if part.leading > 1 {
                        docs.push(self.add(Doc::HardLine));
                    }
                } else if self.spaced(previous, part, parent) {
                    docs.push(self.add(Doc::Space));
                }
            }
            docs.push(part.doc);
        }
        self.concat(docs)
    }

    fn spaced(&self, left: &Part, right: &Part, parent: K) -> bool {
        let a = left.last.unwrap();
        let b = right.first.unwrap();
        if matches!(a, K::LineComment | K::BlockComment)
            || matches!(b, K::LineComment | K::BlockComment)
        {
            return true;
        }
        if matches!(
            b,
            K::Comma
                | K::Semi
                | K::Colon
                | K::RParen
                | K::RBracket
                | K::Dot
                | K::QuestionDot
                | K::ColonColon
        ) || matches!(
            a,
            K::LParen
                | K::LBracket
                | K::Dot
                | K::QuestionDot
                | K::ColonColon
                | K::QuestionBracket
                | K::QuestionParen
                | K::Ellipsis
        ) {
            return false;
        }
        if matches!(
            b,
            K::LParen | K::QuestionParen | K::LBracket | K::QuestionBracket
        ) {
            return !matches!(
                parent,
                K::CallExpr | K::IndexExpr | K::FnDecl | K::FunctionType
            );
        }
        if parent == K::PrefixExpr && matches!(a, K::Bang | K::Tilde | K::Plus | K::Minus) {
            return matches!(b, K::Plus | K::Minus);
        }
        if parent == K::LiteralPattern && a == K::Minus {
            return false;
        }
        if parent == K::ParamList && b == K::Pipe {
            return false;
        }
        if parent == K::ClosureExpr && (a == K::Pipe || a == K::OrOr || b == K::Pipe) {
            return a == K::OrOr || (a == K::Pipe && left.kind == K::ParamList);
        }
        true
    }

    fn print(&self, root: Id) -> String {
        enum Task {
            Doc(Id, usize, bool, usize),
            Newline(usize),
            Space,
            FillNext(Id, usize, bool, bool, bool, usize),
            FlowNext(Id, usize, bool, bool, usize),
        }
        let mut work = vec![Task::Doc(root, 0, false, 0)];
        let mut output = String::new();
        let mut column = 0usize;
        let mut pending_indent = None;
        let mut pending_space = false;
        let mut content_end = 0;
        while let Some(task) = work.pop() {
            match task {
                Task::Newline(indent) => {
                    pending_space = false;
                    output.push('\n');
                    column = 0;
                    pending_indent = Some(indent);
                }
                Task::Space => {
                    if !output.is_empty() && pending_indent.is_none() && !pending_space {
                        pending_space = true;
                        column += 1;
                    }
                }
                Task::FillNext(id, indent, flat, first, previous_multiline, reserve) => {
                    if !first {
                        if flat
                            || (!previous_multiline
                                && pending_indent.unwrap_or(column)
                                    + 1
                                    + self.docs[id].flat
                                    + reserve
                                    <= WIDTH)
                        {
                            work.push(Task::Doc(id, indent, flat, reserve));
                            work.push(Task::Space);
                        } else {
                            work.push(Task::Doc(id, indent, false, reserve));
                            work.push(Task::Newline(indent));
                        }
                    } else {
                        work.push(Task::Doc(id, indent, flat, reserve));
                    }
                }
                Task::FlowNext(id, indent, flat, split, reserve) => {
                    let newline = !flat
                        && split
                        && pending_indent.unwrap_or(column) + 1 + self.docs[id].flat + reserve
                            > WIDTH;
                    work.push(Task::Doc(id, indent, flat, reserve));
                    work.push(if newline {
                        Task::Newline(indent)
                    } else {
                        Task::Space
                    });
                }
                Task::Doc(id, indent, flat, reserve) => match &self.docs[id].doc {
                    Doc::Text(range) => {
                        if let Some(spaces) = pending_indent.take() {
                            output.extend(std::iter::repeat_n(' ', spaces));
                            column = spaces;
                        }
                        if pending_space {
                            output.push(' ');
                            pending_space = false;
                        }
                        let text = &self.source[range.clone()];
                        output.push_str(text);
                        content_end = output.len();
                        column = if let Some((_, last)) = text.rsplit_once(['\n', '\r']) {
                            last.chars().count()
                        } else {
                            column + text.chars().count()
                        };
                    }
                    Doc::Space => work.push(Task::Space),
                    Doc::Line(space) => {
                        if flat {
                            if *space {
                                work.push(Task::Space);
                            }
                        } else {
                            work.push(Task::Newline(indent));
                        }
                    }
                    Doc::HardLine => work.push(Task::Newline(indent)),
                    Doc::Concat(parts) => {
                        let mut suffix = reserve;
                        for &id in parts.iter().rev() {
                            work.push(Task::Doc(id, indent, flat, suffix));
                            suffix = if self.docs[id].unbroken {
                                suffix.saturating_add(self.docs[id].prefix).min(INFINITE)
                            } else {
                                self.docs[id].prefix
                            };
                        }
                    }
                    Doc::Indent(id) => work.push(Task::Doc(*id, indent + 4, flat, reserve)),
                    Doc::Group(id) => work.push(Task::Doc(
                        *id,
                        indent,
                        flat || pending_indent.unwrap_or(column) + self.docs[*id].flat + reserve
                            <= WIDTH,
                        reserve,
                    )),
                    Doc::Fill(parts) => {
                        for (index, &id) in parts.iter().enumerate().rev() {
                            work.push(Task::FillNext(
                                id,
                                indent,
                                flat,
                                index == 0,
                                index > 0 && self.docs[parts[index - 1]].flat == INFINITE,
                                if index + 1 == parts.len() { reserve } else { 0 },
                            ));
                        }
                    }
                    Doc::Chain { .. } => {
                        let broken = !flat
                            && pending_indent.unwrap_or(column) + self.docs[id].flat + reserve
                                > WIDTH;
                        let mut cursor = id;
                        let mut tails = Vec::new();
                        while let Doc::Chain {
                            base,
                            tail,
                            split,
                            call,
                        } = self.docs[cursor].doc
                        {
                            tails.push((tail, split, call));
                            cursor = base;
                        }
                        let mut continued = false;
                        let mut steps = Vec::with_capacity(tails.len());
                        let calls = tails.iter().filter(|step| step.2).count();
                        let mut tails = tails.into_iter().rev().peekable();
                        while let Some((tail, split, _)) = tails.next() {
                            let newline = broken
                                && split
                                && calls > 1
                                && tails.peek().is_some_and(|next| next.2);
                            continued |= newline;
                            steps.push((
                                tail,
                                if continued { indent + 4 } else { indent },
                                newline,
                            ));
                        }
                        let mut suffix = reserve;
                        for (tail, step_indent, newline) in steps.into_iter().rev() {
                            work.push(Task::Doc(tail, step_indent, !broken, suffix));
                            suffix = if self.docs[tail].unbroken {
                                suffix.saturating_add(self.docs[tail].prefix).min(INFINITE)
                            } else {
                                self.docs[tail].prefix
                            };
                            if newline {
                                work.push(Task::Newline(step_indent));
                                suffix = 0;
                            }
                        }
                        work.push(Task::Doc(cursor, indent, !broken, suffix));
                    }
                    Doc::Flow { .. } => {
                        let fits = flat
                            || pending_indent.unwrap_or(column) + self.docs[id].flat + reserve
                                <= WIDTH;
                        let mut cursor = id;
                        let mut suffix = reserve;
                        while let Doc::Flow { base, tail, split } = self.docs[cursor].doc {
                            work.push(Task::FlowNext(tail, indent + 4, fits, split, suffix));
                            suffix = 0;
                            cursor = base;
                        }
                        work.push(Task::Doc(cursor, indent, fits, suffix));
                    }
                },
            }
        }
        output.truncate(content_end);
        if !output.is_empty() {
            output.push('\n');
        }
        output
    }
}
