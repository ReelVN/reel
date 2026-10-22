//! Source-owned string fragments shared by lowering and host compile extensions.
use crate::{SyntaxKind as K, TextRange, hir};

/// A string literal's significant source fragments in evaluation order.
#[derive(Clone, Copy)]
pub struct StringLiteral<'a> {
    hir: &'a hir::Hir,
    node: hir::NodeId,
    raw: bool,
    format: bool,
}

/// Static decoded text or an interpolation expression with its original span.
#[derive(Clone, Copy, Debug)]
pub enum StringPart<'a> {
    Text(StringText<'a>),
    Expression { node: hir::NodeId, span: TextRange },
}

/// A source slice whose decoded UTF-8 length never exceeds `text.len()`.
#[derive(Clone, Copy, Debug)]
pub struct StringText<'a> {
    pub text: &'a str,
    pub span: TextRange,
    raw: bool,
    format: bool,
}

/// Invalid input is possible only for HIR that did not pass source analysis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeError;

impl<'a> StringLiteral<'a> {
    /// Inspect an analyzed string without allocating or copying its source.
    pub fn new(hir: &'a hir::Hir, node: hir::NodeId) -> Option<Self> {
        if hir.node(node).kind != hir::HirKind::String {
            return None;
        }
        let first = hir.tokens(node).next()?;
        let prefix = hir.text(first.span);
        Some(Self {
            hir,
            node,
            raw: prefix.starts_with('r'),
            format: prefix.starts_with('f'),
        })
    }

    /// Visit literal text and interpolation holes in their source order.
    pub fn parts(self) -> impl Iterator<Item = StringPart<'a>> {
        self.hir
            .elements(self.node)
            .iter()
            .filter_map(move |part| match *part {
                hir::HirElement::Token(token) if token.kind == K::StringText => {
                    Some(StringPart::Text(StringText {
                        text: self.hir.text(token.span),
                        span: token.span,
                        raw: self.raw,
                        format: self.format,
                    }))
                }
                hir::HirElement::Node(node) => {
                    let hir::Form::Single { child } = self.hir.node(node).form else {
                        return None;
                    };
                    Some(StringPart::Expression {
                        node: child,
                        span: self.hir.node(node).span,
                    })
                }
                _ => None,
            })
    }
}

impl StringText<'_> {
    /// Append decoded UTF-8. Callers with allocation budgets reserve at most
    /// `text.len()` additional bytes before entering this shared decoder.
    pub fn append_to(self, bytes: &mut Vec<u8>) -> Result<(), DecodeError> {
        if self.raw {
            bytes.extend_from_slice(self.text.as_bytes());
            return Ok(());
        }
        let mut chars = self.text.chars();
        while let Some(ch) = chars.next() {
            let ch = if ch == '\\' {
                match chars.next() {
                    Some('n') => '\n',
                    Some('r') => '\r',
                    Some('t') => '\t',
                    Some('0') => '\0',
                    Some('"') => '"',
                    Some('\\') => '\\',
                    Some('u') => {
                        if chars.next() != Some('{') {
                            return Err(DecodeError);
                        }
                        let mut value = 0u32;
                        loop {
                            match chars.next() {
                                Some('}') => break,
                                Some(c) => {
                                    value = value
                                        .checked_mul(16)
                                        .and_then(|v| c.to_digit(16).and_then(|c| v.checked_add(c)))
                                        .ok_or(DecodeError)?;
                                }
                                None => return Err(DecodeError),
                            }
                        }
                        char::from_u32(value).ok_or(DecodeError)?
                    }
                    _ => return Err(DecodeError),
                }
            } else if self.format && matches!(ch, '{' | '}') {
                if chars.next() != Some(ch) {
                    return Err(DecodeError);
                }
                ch
            } else {
                ch
            };
            bytes.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
        }
        Ok(())
    }
}
