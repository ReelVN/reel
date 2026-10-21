//! Source documentation queries for editor hovers and checked examples.
use crate::{SyntaxKind as K, SyntaxNode, TextRange};

/// Markdown attached to a declaration by adjacent `///` lines.
/// A blank source line, an ordinary comment, or another declaration ends the group.
/// The query reads the lossless syntax tree and allocates only the requested text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Documentation {
    pub markdown: String,
    pub range: TextRange,
}

pub fn declaration(node: &SyntaxNode) -> Option<Documentation> {
    let outer = node
        .parent()
        .filter(|parent| parent.kind() == K::ModifiedDecl);
    let node = outer.as_ref().unwrap_or(node);
    let mut first = node.first_token()?;
    while first.kind().is_trivia() {
        first = first.next_token()?;
    }
    let mut cursor = first.prev_token();
    let mut lines = Vec::new();
    let mut range = None;
    while let Some(token) = cursor {
        match token.kind() {
            K::Whitespace if token.text().bytes().filter(|&b| b == b'\n').count() <= 1 => {}
            K::LineComment
                if token.text().starts_with("///") && !token.text().starts_with("////") =>
            {
                let line = &token.text()[3..];
                lines.push(line.strip_prefix(' ').unwrap_or(line).to_owned());
                range = Some(match range {
                    None => token.text_range(),
                    Some(range) => token.text_range().cover(range),
                });
            }
            _ => break,
        }
        cursor = token.prev_token();
    }
    lines.reverse();
    Some(Documentation {
        markdown: lines.join("\n"),
        range: range?,
    })
}

/// An independently compilable Aside module fenced inside API documentation.
#[derive(Debug, Clone, Copy)]
pub struct Example<'a> {
    pub source: &'a str,
    /// One-based line of the first code line in the Markdown text.
    pub line: usize,
}

/// Read `aside` fences without copying their source. Fences for other languages
/// remain available to Markdown renderers and are excluded from Aside validation.
pub fn examples(markdown: &str) -> Vec<Example<'_>> {
    let mut examples = Vec::new();
    let mut offset = 0;
    let mut open = None;
    for (line, text) in markdown.split_inclusive('\n').enumerate() {
        let trimmed = text.trim();
        if let Some((start, first_line)) = open {
            if trimmed == "```" {
                examples.push(Example {
                    source: &markdown[start..offset],
                    line: first_line,
                });
                open = None;
            }
        } else if trimmed == "```aside" {
            open = Some((offset + text.len(), line + 2));
        }
        offset += text.len();
    }
    examples
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_docs_follow_declarations_and_preserve_markdown() {
        let source = "/// Draw a label.\r\n///\r\n/// Use **UTF-8** text.\r\npub fn label() {}\n\n/// Detached.\n\nfn other() {}\n/// Interrupted.\n// Ordinary comment.\nfn plain() {}\n";
        let parsed = crate::parse(source).unwrap();
        assert!(parsed.is_ok());
        let declarations: Vec<_> = parsed
            .syntax()
            .descendants()
            .filter(|node| node.kind() == K::FnDecl)
            .collect();
        let docs = declaration(&declarations[0]).unwrap();
        assert_eq!(docs.markdown, "Draw a label.\n\nUse **UTF-8** text.");
        assert!(source[crate::byte_range(docs.range)].starts_with("/// Draw"));
        assert!(declaration(&declarations[1]).is_none());
        assert!(declaration(&declarations[2]).is_none());
    }

    #[test]
    fn examples_keep_source_lines_and_skip_other_fences() {
        let markdown = "Text\n\n```aside\nfn main() {}\n```\n```rust\nlet x = 1;\n```\n";
        let samples = examples(markdown);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].line, 4);
        assert_eq!(samples[0].source, "fn main() {}\n");
    }
}
