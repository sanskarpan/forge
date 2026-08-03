// crates/forge-syntax/src/typeck.rs

use rustc_hash::FxHashMap;

use crate::ast::{Ast, BinaryOp, Expr, ExprIdx, UnaryOp};
use crate::diagnostic::Diagnostic;
use crate::span::Span;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    F64,
    I64,
    Bool,
}

#[derive(Debug)]
pub struct TypedAst {
    pub ast: Ast,
    pub types: Vec<Ty>,
    pub params: Vec<(String, Ty)>,
}

pub fn typecheck(ast: Ast) -> Result<TypedAst, Vec<Diagnostic>> {
    let mut ctx = Ctx {
        ast: &ast,
        param_ty: FxHashMap::default(),
        param_order: Vec::new(),
        local_ty: FxHashMap::default(),
        types: vec![Ty::F64; ast.exprs.len()],
        diags: Vec::new(),
    };
    ctx.infer_expect(ast.root, None);
    ctx.check(ast.root);
    let diags = std::mem::take(&mut ctx.diags);
    let types = std::mem::take(&mut ctx.types);
    let params = ctx
        .param_order
        .iter()
        .map(|n| (n.clone(), ctx.param_ty[n]))
        .collect();
    drop(ctx);
    if diags.is_empty() {
        Ok(TypedAst { ast, types, params })
    } else {
        Err(diags)
    }
}

struct Ctx<'a> {
    ast: &'a Ast,
    param_ty: FxHashMap<String, Ty>,
    param_order: Vec<String>,
    local_ty: FxHashMap<String, Ty>,
    types: Vec<Ty>,
    diags: Vec<Diagnostic>,
}

impl<'a> Ctx<'a> {
    fn note_param(&mut self, name: &str) {
        if !self.param_ty.contains_key(name) {
            self.param_ty.insert(name.to_string(), Ty::F64);
            self.param_order.push(name.to_string());
        }
    }

    fn constrain_param(&mut self, name: &str, ty: Ty) {
        self.note_param(name);
        self.param_ty.insert(name.to_string(), ty);
    }

    /// Pass 1: seed every free parameter's type from any unambiguous
    /// constraint, propagated down through type-preserving nodes (unary
    /// negate, arithmetic) so `(n * 2654435761) >> 16` forces `n` to i64
    /// even though `n` isn't a direct operand of `>>`. Names containing `%`
    /// are let-locals (see `resolve.rs`), not parameters, and are skipped —
    /// their type comes directly from their value expression in `check`.
    fn infer_expect(&mut self, idx: ExprIdx, expected: Option<Ty>) {
        match self.ast.get(idx).clone() {
            Expr::Ident(name) => {
                if name.contains('%') {
                    return;
                }
                match expected {
                    Some(t) => self.constrain_param(&name, t),
                    None => self.note_param(&name),
                }
            }
            Expr::Unary { op, operand } => {
                let inner = match op {
                    UnaryOp::Neg => expected,
                    UnaryOp::BitNot => Some(Ty::I64),
                    UnaryOp::Not => Some(Ty::Bool),
                };
                self.infer_expect(operand, inner);
            }
            Expr::Binary { op, lhs, rhs } => {
                use BinaryOp::*;
                let inner = match op {
                    Add | Sub | Mul | Div | Rem => expected,
                    BitAnd | BitOr | BitXor | Shl | Shr => Some(Ty::I64),
                    And | Or => Some(Ty::Bool),
                    Eq | Ne | Lt | Le | Gt | Ge => None,
                };
                self.infer_expect(lhs, inner);
                self.infer_expect(rhs, inner);
            }
            Expr::Call { args, .. } => {
                for a in args {
                    self.infer_expect(a, Some(Ty::F64));
                }
            }
            Expr::If { cond, then_, else_ } => {
                self.infer_expect(cond, Some(Ty::Bool));
                self.infer_expect(then_, expected);
                self.infer_expect(else_, expected);
            }
            Expr::Let { value, body, .. } => {
                self.infer_expect(value, None);
                self.infer_expect(body, expected);
            }
            Expr::Float(_) | Expr::Int(_) | Expr::Bool(_) => {}
        }
    }

    /// Pass 2: real type-check against the parameter types pass 1 resolved.
    fn check(&mut self, idx: ExprIdx) -> Ty {
        let span = self.ast.span(idx);
        let ty = match self.ast.get(idx).clone() {
            Expr::Float(_) => Ty::F64,
            Expr::Int(_) => Ty::I64,
            Expr::Bool(_) => Ty::Bool,
            Expr::Ident(name) => {
                if let Some(t) = self.local_ty.get(&name) {
                    *t
                } else {
                    *self.param_ty.get(&name).expect("seeded in pass 1")
                }
            }
            Expr::Unary { op, operand } => {
                let t = self.check(operand);
                let ospan = self.ast.span(operand);
                match op {
                    UnaryOp::Neg => {
                        self.expect_numeric(t, ospan);
                        t
                    }
                    UnaryOp::Not => {
                        self.expect(t, Ty::Bool, ospan);
                        Ty::Bool
                    }
                    UnaryOp::BitNot => {
                        self.expect(t, Ty::I64, ospan);
                        Ty::I64
                    }
                }
            }
            Expr::Binary { op, lhs, rhs } => self.check_binary(op, lhs, rhs),
            Expr::Call { callee, args } => self.check_call(&callee, &args, span),
            Expr::If { cond, then_, else_ } => {
                let c = self.check(cond);
                self.expect(c, Ty::Bool, self.ast.span(cond));
                let t = self.check(then_);
                let e = self.check(else_);
                if t != e {
                    self.diags.push(
                        Diagnostic::error(
                            format!("if branches have different types: {t:?} vs {e:?}"),
                            span,
                            "branch type mismatch",
                        )
                        .with_secondary(self.ast.span(then_), format!("then branch is {t:?}"))
                        .with_secondary(self.ast.span(else_), format!("else branch is {e:?}")),
                    );
                }
                t
            }
            Expr::Let { name, value, body } => {
                let vt = self.check(value);
                self.local_ty.insert(name, vt);
                self.check(body)
            }
        };
        self.types[idx.index()] = ty;
        ty
    }

    fn check_binary(&mut self, op: BinaryOp, lhs: ExprIdx, rhs: ExprIdx) -> Ty {
        let lt = self.check(lhs);
        let rt = self.check(rhs);
        let (lspan, rspan) = (self.ast.span(lhs), self.ast.span(rhs));
        use BinaryOp::*;
        match op {
            Add | Sub | Mul | Div | Rem => {
                self.expect_numeric(lt, lspan);
                self.expect(rt, lt, rspan);
                lt
            }
            BitAnd | BitOr | BitXor | Shl | Shr => {
                self.expect(lt, Ty::I64, lspan);
                self.expect(rt, Ty::I64, rspan);
                Ty::I64
            }
            And | Or => {
                self.expect(lt, Ty::Bool, lspan);
                self.expect(rt, Ty::Bool, rspan);
                Ty::Bool
            }
            Eq | Ne => {
                self.expect(rt, lt, rspan);
                Ty::Bool
            }
            Lt | Le | Gt | Ge => {
                self.expect_numeric(lt, lspan);
                self.expect(rt, lt, rspan);
                Ty::Bool
            }
        }
    }

    fn expect(&mut self, actual: Ty, expected: Ty, span: Span) {
        if actual != expected {
            self.diags.push(Diagnostic::error(
                format!("expected {expected:?}, found {actual:?}"),
                span,
                "type mismatch",
            ));
        }
    }

    fn expect_numeric(&mut self, ty: Ty, span: Span) {
        if ty != Ty::F64 && ty != Ty::I64 {
            self.diags.push(Diagnostic::error(
                format!("expected a numeric type, found {ty:?}"),
                span,
                "not numeric",
            ));
        }
    }

    fn check_call(&mut self, callee: &str, args: &[ExprIdx], span: Span) -> Ty {
        let sig: &[(&str, usize, Ty)] = &[
            ("sqrt", 1, Ty::F64),
            ("abs", 1, Ty::F64),
            ("floor", 1, Ty::F64),
            ("ceil", 1, Ty::F64),
            ("round", 1, Ty::F64),
            ("trunc", 1, Ty::F64),
            ("sin", 1, Ty::F64),
            ("cos", 1, Ty::F64),
            ("tan", 1, Ty::F64),
            ("exp", 1, Ty::F64),
            ("log", 1, Ty::F64),
            ("min", 2, Ty::F64),
            ("max", 2, Ty::F64),
            ("pow", 2, Ty::F64),
            ("fma", 3, Ty::F64),
        ];
        match sig.iter().find(|(name, _, _)| *name == callee) {
            Some((_, arity, ret)) => {
                if args.len() != *arity {
                    self.diags.push(Diagnostic::error(
                        format!("{callee}() takes {arity} argument(s), got {}", args.len()),
                        span,
                        "arity mismatch",
                    ));
                }
                for &a in args {
                    let t = self.check(a);
                    self.expect(t, Ty::F64, self.ast.span(a));
                }
                *ret
            }
            None => {
                self.diags.push(Diagnostic::error(
                    format!("unknown intrinsic `{callee}`"),
                    span,
                    "not a known function",
                ));
                for &a in args {
                    self.check(a);
                }
                Ty::F64
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use crate::parser::parse;
    use crate::resolve::resolve;

    fn typed(src: &str) -> Result<TypedAst, Vec<crate::diagnostic::Diagnostic>> {
        let (tokens, _) = lex(src);
        let (ast, _) = parse(&tokens);
        typecheck(resolve(ast))
    }

    #[test]
    fn int_plus_bool_is_a_type_error() {
        let err = typed("1 + true").unwrap_err();
        assert_eq!(err.len(), 1);
    }

    #[test]
    fn if_branch_type_mismatch() {
        let err = typed("if true then 1.0 else true").unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(err[0].message.contains("branch"));
    }

    #[test]
    fn intrinsic_arity_mismatch() {
        let err = typed("sqrt(1.0, 2.0)").unwrap_err();
        assert_eq!(err.len(), 1);
        assert!(err[0].message.contains("takes"));
    }

    #[test]
    fn param_inferred_i64_through_nested_arithmetic() {
        // The canonical SPEC example: n is not a *direct* operand of `>>`,
        // it's wrapped in a Mul first — inference must propagate through it.
        let t = typed("(n * 2654435761) >> 16").unwrap();
        assert_eq!(t.params, vec![("n".to_string(), Ty::I64)]);
    }

    #[test]
    fn param_defaults_to_f64() {
        let t = typed("sqrt(x * x + y * y)").unwrap();
        assert_eq!(
            t.params,
            vec![("x".to_string(), Ty::F64), ("y".to_string(), Ty::F64)]
        );
    }

    #[test]
    fn let_shadowing_does_not_leak_into_outer_scope() {
        // (let x = 1 in x) + x — outer x stays a free f64 parameter even
        // though a let-local also happens to be named x.
        let t = typed("(let x = 1.0 in x) + x").unwrap();
        assert_eq!(t.params, vec![("x".to_string(), Ty::F64)]);
    }
}
