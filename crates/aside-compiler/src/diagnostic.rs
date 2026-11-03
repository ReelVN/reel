//! Structured byte-range diagnostics and editor/source coordinates.
use crate::{TextRange, TextSize};
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticCode {
    InvalidCharacter,
    InvalidIdentifier,
    UnterminatedComment,
    UnterminatedString,
    InvalidEscape,
    InvalidUnicodeEscape,
    InvalidInterpolation,
    InvalidNumber,
    IntegerOverflow,
    FloatOverflow,
    ExpectedToken,
    ExpectedExpression,
    ExpectedPattern,
    ExpectedType,
    ExpectedDeclaration,
    MissingSemicolon,
    ChainedComparison,
    ChainedRange,
    InvalidAssignment,
    MissingElse,
    InvalidRest,
    InvalidArgumentOrder,
    DuplicateArgument,
    ChildrenConflict,
    BudgetExceeded,
    InvalidHir,
    UnknownName,
    DuplicateBinding,
    UninitializedBinding,
    ImmutableBinding,
    PatternBindings,
    InvalidControl,
    TypeMismatch,
    InvalidReceiver,
    InvalidDefault,
    NoWait,
    InvalidCall,
    ModuleResolution,
    InvalidType,
    InvalidEffect,
    UnusedResult,
    UnknownMember,
}
impl DiagnosticCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidCharacter => "ASL001",
            Self::InvalidIdentifier => "ASL002",
            Self::UnterminatedComment => "ASL003",
            Self::UnterminatedString => "ASL004",
            Self::InvalidEscape => "ASL005",
            Self::InvalidUnicodeEscape => "ASL006",
            Self::InvalidInterpolation => "ASL007",
            Self::InvalidNumber => "ASL008",
            Self::IntegerOverflow => "ASL009",
            Self::FloatOverflow => "ASL010",
            Self::ExpectedToken => "ASP001",
            Self::ExpectedExpression => "ASP002",
            Self::ExpectedPattern => "ASP003",
            Self::ExpectedType => "ASP004",
            Self::ExpectedDeclaration => "ASP005",
            Self::MissingSemicolon => "ASP006",
            Self::ChainedComparison => "ASP007",
            Self::ChainedRange => "ASP008",
            Self::InvalidAssignment => "ASP009",
            Self::MissingElse => "ASP010",
            Self::InvalidRest => "ASP011",
            Self::InvalidArgumentOrder => "ASP012",
            Self::DuplicateArgument => "ASP013",
            Self::ChildrenConflict => "ASP014",
            Self::BudgetExceeded => "ASR001",
            Self::InvalidHir => "ASS001",
            Self::UnknownName => "ASS002",
            Self::DuplicateBinding => "ASS003",
            Self::UninitializedBinding => "ASS004",
            Self::ImmutableBinding => "ASS005",
            Self::PatternBindings => "ASS006",
            Self::InvalidControl => "ASS007",
            Self::TypeMismatch => "ASS008",
            Self::InvalidReceiver => "ASS009",
            Self::InvalidDefault => "ASS010",
            Self::NoWait => "ASS011",
            Self::InvalidCall => "ASS012",
            Self::ModuleResolution => "ASS013",
            Self::InvalidType => "ASS014",
            Self::InvalidEffect => "ASS015",
            Self::UnusedResult => "ASS016",
            Self::UnknownMember => "ASS017",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedRange {
    pub range: TextRange,
    pub message: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    pub range: TextRange,
    pub replacement: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub severity: Severity,
    pub range: TextRange,
    pub message: String,
    pub related: Vec<RelatedRange>,
    pub fix: Option<Fix>,
}
impl Diagnostic {
    pub(crate) fn error(
        code: DiagnosticCode,
        range: TextRange,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity: Severity::Error,
            range,
            message: message.into(),
            related: Vec::new(),
            fix: None,
        }
    }
    pub fn to_codespan<FileId: Clone>(
        &self,
        file: FileId,
    ) -> codespan_reporting::diagnostic::Diagnostic<FileId> {
        use codespan_reporting::diagnostic::{Diagnostic as D, Label, Severity as S};
        let mut labels =
            vec![Label::primary(file.clone(), byte_range(self.range)).with_message(&self.message)];
        labels.extend(
            self.related.iter().map(|r| {
                Label::secondary(file.clone(), byte_range(r.range)).with_message(&r.message)
            }),
        );
        D::new(match self.severity {
            Severity::Error => S::Error,
            Severity::Warning => S::Warning,
        })
        .with_code(self.code.as_str())
        .with_message(&self.message)
        .with_labels(labels)
    }
}
pub fn byte_range(range: TextRange) -> Range<usize> {
    usize::from(range.start())..usize::from(range.end())
}
pub(crate) fn span(start: usize, end: usize) -> TextRange {
    TextRange::new(TextSize::from(start as u32), TextSize::from(end as u32))
}
/// All fields are zero based. Columns explicitly state their unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePosition {
    pub line: usize,
    pub byte_column: usize,
    pub scalar_column: usize,
    pub utf16_column: usize,
}
/// Borrows the source; indexes LF/CRLF lines once. Invalid byte boundaries return None.
#[derive(Debug)]
pub struct SourceMap<'a> {
    _source: &'a str,
    lines: line_index::LineIndex,
}
impl<'a> SourceMap<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            _source: source,
            lines: line_index::LineIndex::new(source),
        }
    }
    pub fn position(&self, offset: usize) -> Option<SourcePosition> {
        let position = self
            .lines
            .try_line_col(u32::try_from(offset).ok()?.into())?;
        Some(SourcePosition {
            line: position.line as usize,
            byte_column: position.col as usize,
            scalar_column: self
                .lines
                .to_wide(line_index::WideEncoding::Utf32, position)?
                .col as usize,
            utf16_column: self
                .lines
                .to_wide(line_index::WideEncoding::Utf16, position)?
                .col as usize,
        })
    }
}
