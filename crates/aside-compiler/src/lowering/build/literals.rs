//! One literal decoder for ordinary expressions and direct parameter defaults.
use super::*;

impl super::constants::Evaluator<'_, '_> {
    pub(super) fn literal_constant(
        &mut self,
        mut node: h::NodeId,
    ) -> Result<Option<bc::Const>, Box<Error>> {
        loop {
            self.b.work(1)?;
            match (self.m.hir().node(node).kind, self.m.hir().node(node).form) {
                (H::Group, Form::Single { child }) => node = child,
                (H::Literal, Form::Literal { token }) => {
                    return Ok(Some(match token.kind {
                        K::Null => bc::Const::Null,
                        K::True => bc::Const::Bool(true),
                        K::False => bc::Const::Bool(false),
                        K::Number => self.number(self.m.hir().text(token.span), false)?,
                        _ => return Ok(None),
                    }));
                }
                (H::Prefix, Form::Unary { operator, value })
                    if matches!(operator.kind, K::Minus | K::Plus) =>
                {
                    let Form::Literal { token } = self.m.hir().node(value).form else {
                        return Ok(None);
                    };
                    return if token.kind == K::Number {
                        Ok(Some(self.number(
                            self.m.hir().text(token.span),
                            operator.kind == K::Minus,
                        )?))
                    } else {
                        Ok(None)
                    };
                }
                (H::String, _) if self.m.hir().children(node).next().is_none() => {
                    use crate::strings::{StringLiteral, StringPart};
                    let literal = StringLiteral::new(self.m.hir(), node).expect("analyzed string");
                    let mut bytes = Vec::new();
                    for part in literal.parts() {
                        let StringPart::Text(text) = part else {
                            unreachable!("static string")
                        };
                        let piece = self.string_piece(text)?;
                        self.b.reserve(&mut bytes, piece.len())?;
                        self.b.work(piece.len() as u64)?;
                        bytes.extend_from_slice(piece.as_bytes());
                        self.b.release_bytes(piece.capacity());
                    }
                    return Ok(Some(bc::Const::Utf8(
                        String::from_utf8(bytes).expect("decoded unicode"),
                    )));
                }
                _ => return Ok(None),
            }
        }
    }

    pub(super) fn number(&mut self, text: &str, negative: bool) -> Result<bc::Const, Box<Error>> {
        let mut bytes = Vec::new();
        self.b.work(text.len() as u64)?;
        self.b.reserve(&mut bytes, text.len())?;
        bytes.extend(text.bytes().filter(|&c| c != b'_'));
        let text = std::str::from_utf8(&bytes).expect("numeric source UTF-8");
        let prefix = text.get(..2).unwrap_or("");
        let (radix, digits) = match prefix {
            "0x" | "0X" => (16, &text[2..]),
            "0o" | "0O" => (8, &text[2..]),
            "0b" | "0B" => (2, &text[2..]),
            _ => (10, text),
        };
        let value = if radix == 10 && text.bytes().any(|c| matches!(c, b'.' | b'e' | b'E')) {
            let float: f64 = text
                .parse()
                .map_err(|_| self.b.error(ErrorKind::InvalidIr, "analyzed float literal"))?;
            bc::Const::FloatBits(if negative { -float } else { float }.to_bits())
        } else {
            let value = i128::from_str_radix(digits, radix).map_err(|_| {
                self.b
                    .error(ErrorKind::InvalidIr, "analyzed integer literal")
            })?;
            let value = if negative { -value } else { value };
            bc::Const::Int(i64::try_from(value).map_err(|_| {
                self.b
                    .error(ErrorKind::InvalidIr, "integer literal exceeds int64")
            })?)
        };
        self.b.release(bytes);
        Ok(value)
    }

    pub(super) fn string_piece(
        &mut self,
        text: crate::strings::StringText<'_>,
    ) -> Result<String, Box<Error>> {
        let mut bytes = Vec::new();
        self.b.reserve(&mut bytes, text.text.len())?;
        self.b.work(text.text.len() as u64)?;
        text.append_to(&mut bytes)
            .map_err(|_| self.b.error(ErrorKind::InvalidIr, "analyzed string escape"))?;
        Ok(String::from_utf8(bytes).expect("decoded unicode"))
    }
}

impl Lowerer<'_> {
    pub(super) fn literal_constant(
        &mut self,
        node: h::NodeId,
    ) -> Result<Option<bc::Const>, Box<Error>> {
        super::constants::Evaluator {
            m: self.m,
            b: &mut self.b,
        }
        .literal_constant(node)
    }
    pub(super) fn number(&mut self, text: &str, negative: bool) -> Result<bc::Const, Box<Error>> {
        super::constants::Evaluator {
            m: self.m,
            b: &mut self.b,
        }
        .number(text, negative)
    }
    pub(super) fn string_piece(
        &mut self,
        text: crate::strings::StringText<'_>,
    ) -> Result<String, Box<Error>> {
        super::constants::Evaluator {
            m: self.m,
            b: &mut self.b,
        }
        .string_piece(text)
    }
}
