//! Aside lossless syntax trees, shared immutable ownership, and typed cursors.
#![forbid(
    // missing_debug_implementations,
    unconditional_recursion,
    future_incompatible,
    // missing_docs,
)]
#![deny(unsafe_code)]

#[allow(unsafe_code, unsafe_op_in_unsafe_fn)]
pub mod cursor;
#[allow(unsafe_code, unsafe_op_in_unsafe_fn)]
mod green;

pub mod api;
mod syntax_text;
mod utility_types;

#[allow(unsafe_code, unsafe_op_in_unsafe_fn)]
mod arc;
pub mod ast;
mod cow_mut;
#[cfg(feature = "serde1")]
mod serde_impls;

pub use text_size::{TextLen, TextRange, TextSize};

pub use crate::{
    api::{
        Language, SyntaxElement, SyntaxElementChildren, SyntaxNode, SyntaxNodeChildren, SyntaxToken,
    },
    green::{
        Checkpoint, Children, GreenNode, GreenNodeBuilder, GreenNodeData, GreenToken,
        GreenTokenData, NodeCache, SyntaxKind,
    },
    syntax_text::SyntaxText,
    utility_types::{Direction, NodeOrToken, TokenAtOffset, WalkEvent},
};
