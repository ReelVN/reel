use crate::SyntaxKind;
use logos::Logos;

#[derive(Logos, Debug, Clone, Copy)]
pub(crate) enum RawToken {
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token(",")]
    Comma,
    #[token(";")]
    Semi,
    #[token(":")]
    Colon,
    #[token("::")]
    ColonColon,
    #[token(".")]
    Dot,
    #[token("...")]
    Ellipsis,
    #[token("..")]
    Range,
    #[token("..=")]
    RangeInclusive,
    #[token("->")]
    Arrow,
    #[token("=>")]
    FatArrow,
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("%")]
    Percent,
    #[token("!")]
    Bang,
    #[token("~")]
    Tilde,
    #[token("&")]
    Amp,
    #[token("|")]
    Pipe,
    #[token("^")]
    Caret,
    #[token("&&")]
    AndAnd,
    #[token("||")]
    OrOr,
    #[token("??")]
    QuestionQuestion,
    #[token("?.")]
    QuestionDot,
    #[token("?[")]
    QuestionBracket,
    #[token("?(")]
    QuestionParen,
    #[token("=")]
    Eq,
    #[token("==")]
    EqEq,
    #[token("!=")]
    NotEq,
    #[token("<")]
    Lt,
    #[token("<=")]
    Le,
    #[token(">")]
    Gt,
    #[token(">=")]
    Ge,
    #[token("<<")]
    Shl,
    #[token(">>")]
    Shr,
    #[token("+=")]
    PlusEq,
    #[token("-=")]
    MinusEq,
    #[token("*=")]
    StarEq,
    #[token("/=")]
    SlashEq,
    #[token("%=")]
    PercentEq,
    #[token("&=")]
    AmpEq,
    #[token("|=")]
    PipeEq,
    #[token("^=")]
    CaretEq,
    #[token("<<=")]
    ShlEq,
    #[token(">>=")]
    ShrEq,
    #[regex(r"[ \t\r\n\x0C]+")]
    Whitespace,
    #[regex(r"//[^\r\n]*", allow_greedy = true)]
    LineComment,
    #[token("/*")]
    BlockComment,
    #[regex(r"[A-Za-z_][A-Za-z0-9_]*")]
    Ident,
    #[regex(r"[^\x00-\x7F]")]
    UnicodeChar,
    #[regex(r"0[xXoObB][0-9A-Za-z_]*|[0-9][0-9_]*(\.[0-9][0-9_]*)?([eE][+-]?[0-9A-Za-z_]*)?")]
    Number,
    #[token("\"")]
    #[token("r\"")]
    #[token("f\"")]
    #[token("\"\"\"")]
    #[token("r\"\"\"")]
    #[token("f\"\"\"")]
    StringStart,
}
impl RawToken {
    pub fn kind(self) -> SyntaxKind {
        match self {
            Self::LParen => SyntaxKind::LParen,
            Self::RParen => SyntaxKind::RParen,
            Self::LBrace => SyntaxKind::LBrace,
            Self::RBrace => SyntaxKind::RBrace,
            Self::LBracket => SyntaxKind::LBracket,
            Self::RBracket => SyntaxKind::RBracket,
            Self::Comma => SyntaxKind::Comma,
            Self::Semi => SyntaxKind::Semi,
            Self::Colon => SyntaxKind::Colon,
            Self::ColonColon => SyntaxKind::ColonColon,
            Self::Dot => SyntaxKind::Dot,
            Self::Ellipsis => SyntaxKind::Ellipsis,
            Self::Range => SyntaxKind::Range,
            Self::RangeInclusive => SyntaxKind::RangeInclusive,
            Self::Arrow => SyntaxKind::Arrow,
            Self::FatArrow => SyntaxKind::FatArrow,
            Self::Plus => SyntaxKind::Plus,
            Self::Minus => SyntaxKind::Minus,
            Self::Star => SyntaxKind::Star,
            Self::Slash => SyntaxKind::Slash,
            Self::Percent => SyntaxKind::Percent,
            Self::Bang => SyntaxKind::Bang,
            Self::Tilde => SyntaxKind::Tilde,
            Self::Amp => SyntaxKind::Amp,
            Self::Pipe => SyntaxKind::Pipe,
            Self::Caret => SyntaxKind::Caret,
            Self::AndAnd => SyntaxKind::AndAnd,
            Self::OrOr => SyntaxKind::OrOr,
            Self::QuestionQuestion => SyntaxKind::QuestionQuestion,
            Self::QuestionDot => SyntaxKind::QuestionDot,
            Self::QuestionBracket => SyntaxKind::QuestionBracket,
            Self::QuestionParen => SyntaxKind::QuestionParen,
            Self::Eq => SyntaxKind::Eq,
            Self::EqEq => SyntaxKind::EqEq,
            Self::NotEq => SyntaxKind::NotEq,
            Self::Lt => SyntaxKind::Lt,
            Self::Le => SyntaxKind::Le,
            Self::Gt => SyntaxKind::Gt,
            Self::Ge => SyntaxKind::Ge,
            Self::Shl => SyntaxKind::Shl,
            Self::Shr => SyntaxKind::Shr,
            Self::PlusEq => SyntaxKind::PlusEq,
            Self::MinusEq => SyntaxKind::MinusEq,
            Self::StarEq => SyntaxKind::StarEq,
            Self::SlashEq => SyntaxKind::SlashEq,
            Self::PercentEq => SyntaxKind::PercentEq,
            Self::AmpEq => SyntaxKind::AmpEq,
            Self::PipeEq => SyntaxKind::PipeEq,
            Self::CaretEq => SyntaxKind::CaretEq,
            Self::ShlEq => SyntaxKind::ShlEq,
            Self::ShrEq => SyntaxKind::ShrEq,
            Self::Whitespace => SyntaxKind::Whitespace,
            Self::LineComment => SyntaxKind::LineComment,
            Self::BlockComment => SyntaxKind::BlockComment,
            Self::Ident => SyntaxKind::Ident,
            Self::UnicodeChar => SyntaxKind::ErrorToken,
            Self::Number => SyntaxKind::Number,
            Self::StringStart => SyntaxKind::StringStart,
        }
    }
}
