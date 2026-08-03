// crates/forge-syntax/src/token.rs

use crate::span::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Float, Int, Ident, True, False,
    If, Then, Else, Let, In,
    LParen, RParen, Comma, At, Assign,
    OrOr, AndAnd,
    Pipe, Caret, Amp,
    EqEq, NotEq,
    Lt, Le, Gt, Ge,
    Shl, Shr,
    Plus, Minus,
    Star, Slash, Percent,
    Bang, Tilde,
    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// Literal text for Float/Int/Ident (with `_` separators stripped for
    /// numbers); empty for everything else.
    pub text: String,
}
