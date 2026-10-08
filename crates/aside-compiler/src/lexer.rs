use crate::diagnostic::span;
use crate::raw::RawToken;
use crate::{
    Diagnostic, DiagnosticCode as D, Limits, SourceTooLarge, SyntaxKind as K, TextRange, byte_range,
};
use logos::Logos;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: K,
    pub range: TextRange,
}
impl Token {
    pub fn text(self, source: &str) -> &str {
        &source[byte_range(self.range)]
    }
}
#[derive(Debug, Clone)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub diagnostics: Vec<Diagnostic>,
    pub exhausted: bool,
}
pub fn lex(source: &str) -> Result<Lexed, SourceTooLarge> {
    lex_with_limits(source, Limits::default())
}
pub fn lex_with_limits(source: &str, limits: Limits) -> Result<Lexed, SourceTooLarge> {
    let limits = limits.normalized();
    limits.check(source)?;
    Ok(Scanner {
        source,
        pos: 0,
        tokens: Vec::new(),
        diagnostics: Vec::new(),
        modes: Vec::new(),
        limits,
        exhausted: false,
        tail_start: None,
    }
    .run())
}
#[derive(Debug, Clone, Copy)]
enum Mode {
    String {
        triple: bool,
        raw: bool,
        format: bool,
        opening: usize,
    },
    Hole {
        braces: usize,
        opening: usize,
    },
}
struct Scanner<'a> {
    source: &'a str,
    pos: usize,
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
    modes: Vec<Mode>,
    limits: Limits,
    exhausted: bool,
    tail_start: Option<usize>,
}
impl Scanner<'_> {
    // Ordinary tokens are charged at emission, including every string boundary.
    // One separate ErrorToken tail may follow the ordinary token limit.
    fn emit(&mut self, kind: K, start: usize, end: usize) -> bool {
        if start < end {
            if self.tokens.len() >= self.limits.max_tokens {
                self.tail_start.get_or_insert(start);
                self.error(D::BudgetExceeded, start, end, "token budget exhausted");
                self.exhausted = true;
                return false;
            }
            self.tokens.push(Token {
                kind,
                range: span(start, end),
            });
        }
        true
    }
    fn error(&mut self, code: D, start: usize, end: usize, message: &str) {
        if self.exhausted {
            return;
        }
        if self.diagnostics.len() + 1 >= self.limits.max_diagnostics {
            self.diagnostics.push(Diagnostic::error(
                D::BudgetExceeded,
                span(start, end),
                "diagnostic budget exhausted",
            ));
            self.exhausted = true;
        } else {
            self.diagnostics
                .push(Diagnostic::error(code, span(start, end), message));
        }
    }
    fn push_mode(&mut self, mode: Mode, start: usize) {
        if self.modes.len() >= self.limits.max_depth {
            self.error(
                D::BudgetExceeded,
                start,
                self.pos,
                "interpolation nesting budget exhausted",
            );
            self.exhausted = true;
        } else {
            self.modes.push(mode);
        }
    }
    fn run(mut self) -> Lexed {
        while self.pos < self.source.len() && !self.exhausted {
            if self.tokens.len() >= self.limits.max_tokens {
                self.error(
                    D::BudgetExceeded,
                    self.pos,
                    self.source.len(),
                    "token budget exhausted",
                );
                self.exhausted = true;
                break;
            }
            match self.modes.last().copied() {
                Some(Mode::String {
                    triple,
                    raw,
                    format,
                    opening,
                }) => self.string(triple, raw, format, opening),
                _ => self.normal(),
            }
        }
        if !self.exhausted {
            while let Some(mode) = self.modes.pop() {
                let (code, opening, message) = match mode {
                    Mode::String { opening, .. } => {
                        (D::UnterminatedString, opening, "unterminated string")
                    }
                    Mode::Hole { opening, .. } => (
                        D::InvalidInterpolation,
                        opening,
                        "unterminated interpolation",
                    ),
                };
                self.error(code, opening, self.source.len(), message);
                if self.exhausted {
                    break;
                }
            }
        }
        if self.exhausted {
            let start = self.tail_start.unwrap_or(self.pos);
            if start < self.source.len() {
                self.tokens.push(Token {
                    kind: K::ErrorToken,
                    range: span(start, self.source.len()),
                });
            }
        }
        Lexed {
            tokens: self.tokens,
            diagnostics: self.diagnostics,
            exhausted: self.exhausted,
        }
    }
    fn normal(&mut self) {
        let start = self.pos;
        let mut lexer = RawToken::lexer(&self.source[start..]);
        let raw = lexer.next().expect("normal scanner is called before EOF");
        self.pos += lexer.span().end;
        let mut kind = raw.map_or(K::ErrorToken, RawToken::kind);
        if kind == K::BlockComment {
            let mut depth = 1usize;
            let bytes = self.source.as_bytes();
            while self.pos < bytes.len() && depth != 0 {
                if bytes[self.pos..].starts_with(b"/*") {
                    depth += 1;
                    self.pos += 2;
                } else if bytes[self.pos..].starts_with(b"*/") {
                    depth -= 1;
                    self.pos += 2;
                } else {
                    self.pos += 1;
                }
            }
            if depth != 0 {
                self.error(
                    D::UnterminatedComment,
                    start,
                    self.pos,
                    "unterminated block comment",
                );
            }
        }
        if kind == K::Ident {
            let text = &self.source[start..self.pos];
            kind = keyword(text);
            if kind == K::Ident && !valid_identifier(text) {
                self.error(
                    D::InvalidIdentifier,
                    start,
                    self.pos,
                    "identifier does not follow the pinned Unicode XID version",
                );
                kind = K::ErrorToken;
            }
        }
        if kind == K::RBrace {
            if let Some(Mode::Hole { braces, .. }) = self.modes.last_mut() {
                if *braces == 0 {
                    kind = K::InterpolationEnd;
                    self.modes.pop();
                } else {
                    *braces -= 1;
                }
            }
        } else if kind == K::LBrace
            && let Some(Mode::Hole { braces, .. }) = self.modes.last_mut()
        {
            *braces += 1;
        }
        if !self.emit(kind, start, self.pos) {
            return;
        }
        if kind == K::StringStart {
            let text = &self.source[start..self.pos];
            self.push_mode(
                Mode::String {
                    triple: text.ends_with("\"\"\""),
                    raw: text.starts_with('r'),
                    format: text.starts_with('f'),
                    opening: start,
                },
                start,
            );
        } else if kind == K::Number {
            self.number(start, self.pos);
        } else if kind == K::ErrorToken {
            self.error(
                D::InvalidCharacter,
                start,
                self.pos,
                "invalid source character",
            );
        }
    }
    fn number(&mut self, start: usize, end: usize) {
        let text: String = self.source[start..end]
            .chars()
            .filter(|&c| c != '_')
            .collect();
        let radix = if text.starts_with("0x") || text.starts_with("0X") {
            16
        } else if text.starts_with("0o") || text.starts_with("0O") {
            8
        } else if text.starts_with("0b") || text.starts_with("0B") {
            2
        } else {
            10
        };
        if radix == 10 && (text.contains('.') || text.contains(['e', 'E'])) {
            match text.parse::<f64>() {
                Ok(n) if n.is_finite() => (),
                Ok(_) => self.error(
                    D::FloatOverflow,
                    start,
                    end,
                    "floating literal exceeds float64; use an explicit nonfinite value",
                ),
                Err(_) => self.error(D::InvalidNumber, start, end, "invalid floating literal"),
            }
        } else {
            let digits = if radix == 10 { &text[..] } else { &text[2..] };
            if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
                self.error(
                    D::InvalidNumber,
                    start,
                    end,
                    "invalid digits in integer literal",
                );
            } else if u64::from_str_radix(digits, radix).map_or(true, |n| n > 1 << 63) {
                self.error(
                    D::IntegerOverflow,
                    start,
                    end,
                    "integer literal exceeds int64 magnitude",
                );
            }
        }
    }
    fn string(&mut self, triple: bool, raw: bool, format: bool, opening: usize) {
        let start = self.pos;
        let delimiter = if triple { "\"\"\"" } else { "\"" };
        while self.pos < self.source.len() {
            let rest = &self.source[self.pos..];
            if rest.starts_with(delimiter) {
                if !self.emit(K::StringText, start, self.pos) {
                    return;
                }
                let end_start = self.pos;
                self.pos += delimiter.len();
                if !self.emit(K::StringEnd, end_start, self.pos) {
                    return;
                }
                self.modes.pop();
                return;
            }
            if !triple && rest.starts_with(['\n', '\r']) {
                if !self.emit(K::StringText, start, self.pos) {
                    return;
                }
                self.error(
                    D::UnterminatedString,
                    opening,
                    self.pos,
                    "newline in a single-line string",
                );
                self.modes.pop();
                return;
            }
            if format && (rest.starts_with("{{") || rest.starts_with("}}")) {
                self.pos += 2;
                continue;
            }
            if format && rest.starts_with('{') {
                if !self.emit(K::StringText, start, self.pos) {
                    return;
                }
                let hole = self.pos;
                self.pos += 1;
                if !self.emit(K::InterpolationStart, hole, self.pos) {
                    return;
                }
                self.push_mode(
                    Mode::Hole {
                        braces: 0,
                        opening: hole,
                    },
                    hole,
                );
                return;
            }
            if format && rest.starts_with('}') {
                self.error(
                    D::InvalidInterpolation,
                    self.pos,
                    self.pos + 1,
                    "literal '}' in interpolation must be doubled",
                );
            }
            if !raw && rest.starts_with('\\') {
                self.escape();
            } else {
                self.pos += rest.chars().next().unwrap().len_utf8();
            }
        }
        self.emit(K::StringText, start, self.pos);
    }
    fn escape(&mut self) {
        let start = self.pos;
        self.pos += 1;
        let Some(c) = self.source[self.pos..].chars().next() else {
            self.error(D::InvalidEscape, start, self.pos, "incomplete escape");
            return;
        };
        self.pos += c.len_utf8();
        match c {
            '\\' | '"' | 'n' | 'r' | 't' | '0' => (),
            'u' => {
                if !self.source[self.pos..].starts_with('{') {
                    self.error(
                        D::InvalidUnicodeEscape,
                        start,
                        self.pos,
                        "Unicode escape requires braces",
                    );
                    return;
                }
                self.pos += 1;
                let digits = self.pos;
                while let Some(c) = self.source[self.pos..].chars().next() {
                    if matches!(c, '}' | '"' | '\n' | '\r' | '{') {
                        break;
                    }
                    self.pos += c.len_utf8();
                }
                let closed = self.source[self.pos..].starts_with('}');
                let value = &self.source[digits..self.pos];
                let valid = !value.is_empty()
                    && value.bytes().all(|b| b.is_ascii_hexdigit())
                    && value.len() <= 6
                    && u32::from_str_radix(value, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .is_some();
                if closed {
                    self.pos += 1;
                }
                if !closed || !valid {
                    self.error(
                        D::InvalidUnicodeEscape,
                        start,
                        self.pos,
                        "invalid Unicode scalar escape",
                    );
                }
            }
            _ => self.error(
                D::InvalidEscape,
                start,
                self.pos,
                "unsupported string escape",
            ),
        }
    }
}
fn valid_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || unicode_ident::is_xid_start(c))
        && chars.all(|c| c == '_' || unicode_ident::is_xid_continue(c))
}
fn keyword(text: &str) -> K {
    match text {
        "let" => K::Let,
        "mut" => K::Mut,
        "fn" => K::Fn,
        "async" => K::Async,
        "gen" => K::Gen,
        "class" => K::Class,
        "static" => K::Static,
        "pub" => K::Pub,
        "use" => K::Use,
        "as" => K::As,
        "if" => K::If,
        "else" => K::Else,
        "while" => K::While,
        "for" => K::For,
        "in" => K::In,
        "loop" => K::Loop,
        "match" => K::Match,
        "is" => K::Is,
        "await" => K::Await,
        "return" => K::Return,
        "break" => K::Break,
        "continue" => K::Continue,
        "throw" => K::Throw,
        "yield" => K::Yield,
        "try" => K::Try,
        "catch" => K::Catch,
        "finally" => K::Finally,
        "true" => K::True,
        "false" => K::False,
        "null" => K::Null,
        _ => K::Ident,
    }
}
