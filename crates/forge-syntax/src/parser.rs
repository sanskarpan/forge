// crates/forge-syntax/src/parser.rs

use crate::ast::{Ast, BinaryOp, Expr, ExprIdx, Idx, UnaryOp};
use crate::diagnostic::Diagnostic;
use crate::span::Span;
use crate::token::{Token, TokenKind};

pub fn parse(tokens: &[Token]) -> (Ast, Vec<Diagnostic>) {
    let mut p = Parser {
        tokens,
        pos: 0,
        exprs: Vec::new(),
        spans: Vec::new(),
        diags: Vec::new(),
    };
    let root = p.parse_expr(0);
    if p.peek().kind != TokenKind::Eof {
        let tok = p.peek().clone();
        p.diags.push(Diagnostic::error(
            format!("unexpected trailing token {:?}", tok.kind),
            tok.span,
            "expected end of expression",
        ));
    }
    (
        Ast {
            exprs: p.exprs,
            spans: p.spans,
            root,
        },
        p.diags,
    )
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    exprs: Vec<Expr>,
    spans: Vec<Span>,
    diags: Vec<Diagnostic>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn advance(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn expect(&mut self, kind: TokenKind) -> Token {
        if self.peek().kind == kind {
            self.advance()
        } else {
            let tok = self.peek().clone();
            self.diags.push(Diagnostic::error(
                format!("expected {kind:?}, found {:?}", tok.kind),
                tok.span,
                "unexpected token",
            ));
            tok
        }
    }

    fn push(&mut self, expr: Expr, span: Span) -> ExprIdx {
        self.exprs.push(expr);
        self.spans.push(span);
        Idx::new((self.exprs.len() - 1) as u32)
    }

    fn parse_expr(&mut self, min_bp: u8) -> ExprIdx {
        let mut lhs = self.parse_prefix();
        while let Some((op, l_bp, r_bp)) = self.infix_binding_power() {
            if l_bp < min_bp {
                break;
            }
            self.advance();
            let rhs = self.parse_expr(r_bp);
            let span = self.spans[lhs.index()].join(self.spans[rhs.index()]);
            lhs = self.push(Expr::Binary { op, lhs, rhs }, span);
        }
        lhs
    }

    /// Precedence per SPEC §3, lowest to highest; each level is left-assoc
    /// via `(bp, bp + 1)`.
    fn infix_binding_power(&self) -> Option<(BinaryOp, u8, u8)> {
        use TokenKind::*;
        Some(match self.peek().kind {
            OrOr => (BinaryOp::Or, 1, 2),
            AndAnd => (BinaryOp::And, 3, 4),
            Pipe => (BinaryOp::BitOr, 5, 6),
            Caret => (BinaryOp::BitXor, 7, 8),
            Amp => (BinaryOp::BitAnd, 9, 10),
            EqEq => (BinaryOp::Eq, 11, 12),
            NotEq => (BinaryOp::Ne, 11, 12),
            Lt => (BinaryOp::Lt, 13, 14),
            Le => (BinaryOp::Le, 13, 14),
            Gt => (BinaryOp::Gt, 13, 14),
            Ge => (BinaryOp::Ge, 13, 14),
            Shl => (BinaryOp::Shl, 15, 16),
            Shr => (BinaryOp::Shr, 15, 16),
            Plus => (BinaryOp::Add, 17, 18),
            Minus => (BinaryOp::Sub, 17, 18),
            Star => (BinaryOp::Mul, 19, 20),
            Slash => (BinaryOp::Div, 19, 20),
            Percent => (BinaryOp::Rem, 19, 20),
            _ => return None,
        })
    }

    /// Unary operators bind at 21 — tighter than every binary operator above.
    const UNARY_BP: u8 = 21;

    fn parse_prefix(&mut self) -> ExprIdx {
        let tok = self.advance();
        match tok.kind {
            TokenKind::Float => {
                let v: f64 = tok
                    .text
                    .parse()
                    .expect("lexer only produces valid float text");
                self.push(Expr::Float(v), tok.span)
            }
            TokenKind::Int => {
                let v: i64 = tok
                    .text
                    .parse()
                    .expect("lexer only produces valid int text");
                self.push(Expr::Int(v), tok.span)
            }
            TokenKind::True => self.push(Expr::Bool(true), tok.span),
            TokenKind::False => self.push(Expr::Bool(false), tok.span),
            TokenKind::Ident => {
                if self.peek().kind == TokenKind::LParen {
                    self.advance();
                    let mut args = Vec::new();
                    if self.peek().kind != TokenKind::RParen {
                        loop {
                            args.push(self.parse_expr(0));
                            if self.peek().kind == TokenKind::Comma {
                                self.advance();
                            } else {
                                break;
                            }
                        }
                    }
                    let end = self.peek().span;
                    self.expect(TokenKind::RParen);
                    self.push(
                        Expr::Call {
                            callee: tok.text,
                            args,
                        },
                        tok.span.join(end),
                    )
                } else {
                    self.push(Expr::Ident(tok.text), tok.span)
                }
            }
            TokenKind::LParen => {
                let inner = self.parse_expr(0);
                self.expect(TokenKind::RParen);
                inner
            }
            TokenKind::Minus => {
                let operand = self.parse_expr(Self::UNARY_BP);
                let span = tok.span.join(self.spans[operand.index()]);
                self.push(
                    Expr::Unary {
                        op: UnaryOp::Neg,
                        operand,
                    },
                    span,
                )
            }
            TokenKind::Bang => {
                let operand = self.parse_expr(Self::UNARY_BP);
                let span = tok.span.join(self.spans[operand.index()]);
                self.push(
                    Expr::Unary {
                        op: UnaryOp::Not,
                        operand,
                    },
                    span,
                )
            }
            TokenKind::Tilde => {
                let operand = self.parse_expr(Self::UNARY_BP);
                let span = tok.span.join(self.spans[operand.index()]);
                self.push(
                    Expr::Unary {
                        op: UnaryOp::BitNot,
                        operand,
                    },
                    span,
                )
            }
            TokenKind::If => {
                let cond = self.parse_expr(0);
                self.expect(TokenKind::Then);
                let then_ = self.parse_expr(0);
                self.expect(TokenKind::Else);
                let else_ = self.parse_expr(0);
                let span = tok.span.join(self.spans[else_.index()]);
                self.push(Expr::If { cond, then_, else_ }, span)
            }
            TokenKind::Let => {
                let name_tok = self.expect(TokenKind::Ident);
                self.expect(TokenKind::Assign);
                let value = self.parse_expr(0);
                self.expect(TokenKind::In);
                let body = self.parse_expr(0);
                let span = tok.span.join(self.spans[body.index()]);
                self.push(
                    Expr::Let {
                        name: name_tok.text,
                        value,
                        body,
                    },
                    span,
                )
            }
            _ => {
                self.diags.push(Diagnostic::error(
                    format!("unexpected token {:?}", tok.kind),
                    tok.span,
                    "expected an expression",
                ));
                // Error-recovery placeholder: the parser must still return
                // *some* ExprIdx so callers (and any enclosing binary/call/
                // if/let parse in progress) can keep going instead of
                // panicking or aborting the whole parse. `0.0` has no
                // semantic meaning here — it is never a real literal from
                // the source — and this node's presence is only ever
                // discoverable via the diagnostic already pushed above.
                // Downstream consumers (typeck, IR lowering, etc.) must not
                // treat it as a genuine `Expr::Float`; a caller that only
                // proceeds on `diags.is_empty()` will never observe it.
                self.push(Expr::Float(0.0), tok.span)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    fn parse_ok(src: &str) -> Ast {
        let (tokens, diags) = lex(src);
        assert!(diags.is_empty(), "lex diagnostics: {diags:?}");
        let (ast, diags) = parse(&tokens);
        assert!(diags.is_empty(), "parse diagnostics: {diags:?}");
        ast
    }

    #[test]
    fn precedence_multiplicative_over_additive() {
        // 1 + 2 * 3 must parse as 1 + (2 * 3): root is Add, whose rhs is Mul.
        let ast = parse_ok("1 + 2 * 3");
        match ast.get(ast.root) {
            Expr::Binary {
                op: BinaryOp::Add,
                rhs,
                ..
            } => {
                assert!(matches!(
                    ast.get(*rhs),
                    Expr::Binary {
                        op: BinaryOp::Mul,
                        ..
                    }
                ));
            }
            other => panic!("expected top-level Add, got {other:?}"),
        }
    }

    #[test]
    fn left_associative_subtraction() {
        // 10 - 3 - 2 must parse as (10 - 3) - 2: root's lhs is a Binary.
        let ast = parse_ok("10 - 3 - 2");
        match ast.get(ast.root) {
            Expr::Binary {
                op: BinaryOp::Sub,
                lhs,
                ..
            } => {
                assert!(matches!(
                    ast.get(*lhs),
                    Expr::Binary {
                        op: BinaryOp::Sub,
                        ..
                    }
                ));
            }
            other => panic!("expected top-level Sub, got {other:?}"),
        }
    }

    #[test]
    fn unary_binds_tighter_than_multiplicative() {
        // -x * y must parse as (-x) * y.
        let ast = parse_ok("-x * y");
        match ast.get(ast.root) {
            Expr::Binary {
                op: BinaryOp::Mul,
                lhs,
                ..
            } => {
                assert!(matches!(
                    ast.get(*lhs),
                    Expr::Unary {
                        op: UnaryOp::Neg,
                        ..
                    }
                ));
            }
            other => panic!("expected top-level Mul, got {other:?}"),
        }
    }

    #[test]
    fn bitwise_shift_precedence_matches_spec_example() {
        // (n * 2654435761) >> 16 must parse with Shr at the root and Mul as its lhs.
        let ast = parse_ok("n * 2654435761 >> 16");
        match ast.get(ast.root) {
            Expr::Binary {
                op: BinaryOp::Shr,
                lhs,
                ..
            } => {
                assert!(matches!(
                    ast.get(*lhs),
                    Expr::Binary {
                        op: BinaryOp::Mul,
                        ..
                    }
                ));
            }
            other => panic!("expected top-level Shr, got {other:?}"),
        }
    }

    #[test]
    fn if_then_else_and_let_in() {
        let ast = parse_ok("let t = x * x in if t > 0.0 then sqrt(t) else 0.0");
        assert!(matches!(ast.get(ast.root), Expr::Let { .. }));
    }

    #[test]
    fn call_with_multiple_args() {
        let ast = parse_ok("max(a, b)");
        match ast.get(ast.root) {
            Expr::Call { callee, args } => {
                assert_eq!(callee, "max");
                assert_eq!(args.len(), 2);
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn logical_not_unary() {
        let ast = parse_ok("!x");
        assert!(matches!(
            ast.get(ast.root),
            Expr::Unary {
                op: UnaryOp::Not,
                ..
            }
        ));
    }

    #[test]
    fn bitwise_not_unary() {
        let ast = parse_ok("~x");
        assert!(matches!(
            ast.get(ast.root),
            Expr::Unary {
                op: UnaryOp::BitNot,
                ..
            }
        ));
    }

    #[test]
    fn parens_override_default_precedence() {
        // (1 + 2) * 3 must parse as Mul at the root, with an Add on the lhs
        // (the parens force the addition to bind before the multiplication,
        // the opposite of the default precedence).
        let ast = parse_ok("(1 + 2) * 3");
        match ast.get(ast.root) {
            Expr::Binary {
                op: BinaryOp::Mul,
                lhs,
                ..
            } => {
                assert!(matches!(
                    ast.get(*lhs),
                    Expr::Binary {
                        op: BinaryOp::Add,
                        ..
                    }
                ));
            }
            other => panic!("expected top-level Mul, got {other:?}"),
        }
    }

    #[test]
    fn boolean_literals() {
        let ast = parse_ok("true");
        assert!(matches!(ast.get(ast.root), Expr::Bool(true)));

        let ast = parse_ok("false");
        assert!(matches!(ast.get(ast.root), Expr::Bool(false)));
    }

    #[test]
    fn unclosed_paren_produces_diagnostic() {
        // Missing closing paren: `expect(RParen)` fails and records a
        // diagnostic instead of panicking.
        let (tokens, diags) = lex("(1 + 2");
        assert!(diags.is_empty(), "lex diagnostics: {diags:?}");
        let (_ast, diags) = parse(&tokens);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("RParen"), "{:?}", diags[0]);
    }

    #[test]
    fn trailing_garbage_produces_diagnostic() {
        // A complete expression followed by an extra token must trip the
        // "unexpected trailing token" check in `parse`.
        let (tokens, diags) = lex("1 + 2 )");
        assert!(diags.is_empty(), "lex diagnostics: {diags:?}");
        let (_ast, diags) = parse(&tokens);
        assert_eq!(diags.len(), 1);
        assert!(
            diags[0].message.contains("unexpected trailing token"),
            "{:?}",
            diags[0]
        );
    }

    #[test]
    fn bare_unexpected_token_hits_prefix_catch_all() {
        // A token that can never start an expression (`)` alone) must be
        // caught by `parse_prefix`'s wildcard arm, not panic.
        let (tokens, diags) = lex(")");
        assert!(diags.is_empty(), "lex diagnostics: {diags:?}");
        let (_ast, diags) = parse(&tokens);
        assert_eq!(diags.len(), 1);
        assert!(
            diags[0].message.contains("unexpected token"),
            "{:?}",
            diags[0]
        );

        // `+` alone: same catch-all, from a different unexpected token kind.
        let (tokens, diags) = lex("+");
        assert!(diags.is_empty(), "lex diagnostics: {diags:?}");
        let (_ast, diags) = parse(&tokens);
        assert_eq!(diags.len(), 1);
        assert!(
            diags[0].message.contains("unexpected token"),
            "{:?}",
            diags[0]
        );
    }
}
