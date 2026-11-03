//! Position encoding is confined to the protocol boundary; source stays UTF-8.
use lsp_types::{Position, PositionEncodingKind, Range};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encoding {
    Utf8,
    Utf16,
}

impl Encoding {
    pub fn negotiate(encodings: Option<&[PositionEncodingKind]>) -> Self {
        if encodings.is_some_and(|values| values.contains(&PositionEncodingKind::UTF8)) {
            Self::Utf8
        } else {
            Self::Utf16
        }
    }
    pub fn kind(self) -> PositionEncodingKind {
        match self {
            Self::Utf8 => PositionEncodingKind::UTF8,
            Self::Utf16 => PositionEncodingKind::UTF16,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LineIndex {
    index: Arc<aside_compiler::LineIndex>,
}
impl LineIndex {
    pub fn new(source: &str) -> Self {
        Self {
            index: Arc::new(aside_compiler::LineIndex::new(source)),
        }
    }
    pub fn offset(&self, source: &str, position: Position, encoding: Encoding) -> Option<usize> {
        let range = self.index.line(position.line)?;
        let start = u32::from(range.start()) as usize;
        let end = u32::from(range.end()) as usize;
        let line = source[start..end].trim_end_matches(['\r', '\n']);
        let column = position.character as usize;
        match encoding {
            Encoding::Utf8 => {
                let column = column.min(line.len());
                line.is_char_boundary(column).then_some(start + column)
            }
            Encoding::Utf16 => {
                let end = self.index.to_wide(
                    aside_compiler::WideEncoding::Utf16,
                    aside_compiler::LineCol {
                        line: position.line,
                        col: line.len() as u32,
                    },
                )?;
                let native = self.index.to_utf8(
                    aside_compiler::WideEncoding::Utf16,
                    aside_compiler::WideLineCol {
                        line: position.line,
                        col: position.character.min(end.col),
                    },
                )?;
                line.is_char_boundary(native.col as usize)
                    .then_some(start + native.col as usize)
            }
        }
    }
    pub fn position(&self, source: &str, offset: usize, encoding: Encoding) -> Option<Position> {
        if offset > source.len() || !source.is_char_boundary(offset) {
            return None;
        }
        let mut native = self
            .index
            .try_line_col(u32::try_from(offset).ok()?.into())?;
        let range = self.index.line(native.line)?;
        let visible = source[u32::from(range.start()) as usize..u32::from(range.end()) as usize]
            .trim_end_matches(['\r', '\n']);
        native.col = native.col.min(visible.len() as u32);
        let column = match encoding {
            Encoding::Utf8 => native.col,
            Encoding::Utf16 => {
                self.index
                    .to_wide(aside_compiler::WideEncoding::Utf16, native)?
                    .col
            }
        };
        Some(Position::new(native.line, column))
    }
    pub fn range(
        &self,
        source: &str,
        span: aside_compiler::TextRange,
        encoding: Encoding,
    ) -> Range {
        Range {
            start: self
                .position(source, u32::from(span.start()) as usize, encoding)
                .expect("compiler span starts at a valid UTF-8 source boundary"),
            end: self
                .position(source, u32::from(span.end()) as usize, encoding)
                .expect("compiler span ends at a valid UTF-8 source boundary"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chinese_emoji_crlf_and_encoding_negotiation() {
        let source = "中文😀x\r\n下一行";
        let index = LineIndex::new(source);
        assert_eq!(Encoding::negotiate(None), Encoding::Utf16);
        assert_eq!(
            Encoding::negotiate(Some(&[
                PositionEncodingKind::UTF16,
                PositionEncodingKind::UTF8
            ])),
            Encoding::Utf8
        );
        for encoding in [Encoding::Utf8, Encoding::Utf16] {
            for offset in [0, 3, 6, 10, 11, 13, 16, 19, 22] {
                let position = index.position(source, offset, encoding).unwrap();
                assert_eq!(index.offset(source, position, encoding), Some(offset));
            }
        }
        assert_eq!(
            index.offset(source, Position::new(0, 3), Encoding::Utf16),
            None
        );
        assert_eq!(
            index.offset(source, Position::new(0, 7), Encoding::Utf8),
            None
        );
        assert_eq!(
            index.offset(source, Position::new(0, 99), Encoding::Utf16),
            Some(11)
        );
        assert_eq!(
            index.offset(source, Position::new(0, 99), Encoding::Utf8),
            Some(11)
        );
        assert_eq!(
            index.position(source, 12, Encoding::Utf16),
            Some(Position::new(0, 5))
        );
    }
}
