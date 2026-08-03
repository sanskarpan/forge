//! forge-syntax: source spans, diagnostics, tokens, and lexer for the
//! forge expression language.

pub mod span;
pub mod diagnostic;
pub mod token;
pub mod lexer;

pub use diagnostic::Diagnostic;
pub use span::Span;
