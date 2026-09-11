//! Parsing for the source-level array form described by SPEC §12.
//!
//! Array programs deliberately have a small envelope around the existing
//! expression grammar: the envelope names the output column and induction
//! variable, while the right-hand side remains an ordinary Forge expression
//! with indexed column loads such as `a[i]`.

use crate::ast::Ast;
use crate::diagnostic::Diagnostic;
use crate::parser;
use crate::span::Span;
use crate::token::{Token, TokenKind};

#[derive(Clone, Debug)]
pub struct ArrayProgram {
    pub output: String,
    pub index: String,
    pub body: Ast,
}

/// Parses `@vectorize output[index] = expression`.
pub fn parse(tokens: &[Token]) -> (Option<ArrayProgram>, Vec<Diagnostic>) {
    let mut p = HeaderParser {
        tokens,
        pos: 0,
        diags: Vec::new(),
    };
    if p.peek().kind != TokenKind::At {
        return (None, Vec::new());
    }
    p.advance();
    let keyword = p.expect(TokenKind::Ident);
    if keyword.text != "vectorize" {
        p.diags.push(Diagnostic::error(
            format!("expected `vectorize`, found `{}`", keyword.text),
            keyword.span,
            "unknown array declaration",
        ));
    }
    let output = p.expect(TokenKind::Ident);
    p.expect(TokenKind::LBracket);
    let index = p.expect(TokenKind::Ident);
    p.expect(TokenKind::RBracket);
    p.expect(TokenKind::Assign);

    let body_tokens = if p.pos < tokens.len() {
        &tokens[p.pos..]
    } else {
        &tokens[tokens.len()..]
    };
    let (body, body_diags) = parser::parse(body_tokens);
    p.diags.extend(body_diags);
    if p.diags.is_empty() {
        (
            Some(ArrayProgram {
                output: output.text,
                index: index.text,
                body,
            }),
            p.diags,
        )
    } else {
        (None, p.diags)
    }
}

struct HeaderParser<'a> {
    tokens: &'a [Token],
    pos: usize,
    diags: Vec<Diagnostic>,
}

impl<'a> HeaderParser<'a> {
    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or_else(|| {
            self.tokens
                .last()
                .expect("array parser requires an EOF token")
        })
    }

    fn advance(&mut self) -> Token {
        let token = self.peek().clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        token
    }

    fn expect(&mut self, kind: TokenKind) -> Token {
        if self.peek().kind == kind {
            return self.advance();
        }
        let token = self.peek().clone();
        self.diags.push(Diagnostic::error(
            format!("expected {kind:?}, found {:?}", token.kind),
            token.span,
            "invalid vectorize declaration",
        ));
        token
    }
}

#[allow(dead_code)]
fn _span_for_program(program: &ArrayProgram) -> Span {
    program.body.span(program.body.root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Expr;
    use crate::lexer::lex;

    #[test]
    fn parses_documented_vectorize_form() {
        let (tokens, lex_diags) = lex("@vectorize result[i] = a[i] * b[i] + c[i]");
        assert!(lex_diags.is_empty(), "{lex_diags:?}");
        let (program, diags) = parse(&tokens);
        let program = program.expect("array program");
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(program.output, "result");
        assert_eq!(program.index, "i");
        assert!(matches!(
            program.body.get(program.body.root),
            Expr::Binary { .. }
        ));
    }

    #[test]
    fn non_array_source_is_not_claimed_by_array_parser() {
        let (tokens, diags) = lex("x + 1");
        assert!(diags.is_empty());
        let (program, parse_diags) = parse(&tokens);
        assert!(program.is_none());
        assert!(parse_diags.is_empty());
    }
}
