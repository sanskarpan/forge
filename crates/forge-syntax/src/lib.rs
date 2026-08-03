//! forge-syntax: source spans, diagnostics, tokens, and lexer for the
//! forge expression language.

pub mod ast;
pub mod diagnostic;
pub mod lexer;
pub mod parser;
pub mod span;
pub mod token;

pub use diagnostic::Diagnostic;
pub use span::Span;
