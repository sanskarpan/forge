// crates/forge-syntax/src/resolve.rs

use crate::ast::{Ast, Expr, ExprIdx};

/// Alpha-renames every `let`-bound name to a globally unique identifier
/// (`{name}%{counter}`). `%` never appears in a source-level identifier (the
/// lexer only accepts alphanumeric + `_`), so a renamed name can never
/// collide with a real parameter, and two different `let`s that happen to
/// reuse a name get distinct renamed forms. Everything downstream —
/// type-checking, IR lowering — can therefore treat a bare name as
/// unambiguous: always the same parameter, or always the same local.
///
/// This exists because a naive scoped-lookup approach (walk enclosing
/// scopes at use-sites, without ever renaming) gets `(let x = 1 in x) + x`
/// wrong under the SSA-style variable resolution the IR builder uses
/// downstream: the trailing `x` must resolve to the outer parameter, not
/// to the let's `1`, and only physically distinct names make that
/// unambiguous everywhere the name is used.
pub fn resolve(mut ast: Ast) -> Ast {
    let mut counter = 0u32;
    let root = ast.root;
    rename(&mut ast, root, &mut Vec::new(), &mut counter);
    ast
}

fn rename(ast: &mut Ast, idx: ExprIdx, scope: &mut Vec<(String, String)>, counter: &mut u32) {
    // `.clone()` here (and on `unique` below) is only to release the borrow
    // on `ast.exprs` before recursing into a call that needs `&mut ast`;
    // it carries no correctness weight of its own.
    match ast.exprs[idx.index()].clone() {
        Expr::Ident(name) => {
            if let Some((_, unique)) = scope.iter().rev().find(|(orig, _)| *orig == name) {
                ast.exprs[idx.index()] = Expr::Ident(unique.clone());
            }
        }
        Expr::Unary { operand, .. } => rename(ast, operand, scope, counter),
        Expr::Binary { lhs, rhs, .. } => {
            rename(ast, lhs, scope, counter);
            rename(ast, rhs, scope, counter);
        }
        Expr::Call { args, .. } => {
            for a in args {
                rename(ast, a, scope, counter);
            }
        }
        Expr::If { cond, then_, else_ } => {
            rename(ast, cond, scope, counter);
            rename(ast, then_, scope, counter);
            rename(ast, else_, scope, counter);
        }
        Expr::Let { name, value, body } => {
            rename(ast, value, scope, counter);
            *counter += 1;
            let unique = format!("{name}%{counter}");
            scope.push((name, unique.clone()));
            rename(ast, body, scope, counter);
            scope.pop();
            // `value` and `body` are unchanged `Copy` indices (only the
            // nodes they point to were mutated above), so this write-back
            // is infallible by construction — a direct struct literal
            // instead of a re-match-and-patch avoids a silent no-op if
            // that invariant is ever broken by a future refactor.
            ast.exprs[idx.index()] = Expr::Let {
                name: unique,
                value,
                body,
            };
        }
        Expr::Float(_) | Expr::Int(_) | Expr::Bool(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{BinaryOp, Expr};
    use crate::lexer::lex;
    use crate::parser::parse;

    fn resolved(src: &str) -> Ast {
        let (tokens, _) = lex(src);
        let (ast, _) = parse(&tokens);
        resolve(ast)
    }

    #[test]
    fn let_bound_name_is_renamed() {
        let ast = resolved("let x = 1 in x + 2");
        match ast.get(ast.root) {
            Expr::Let { name, body, .. } => {
                assert!(
                    name.contains('%'),
                    "let-bound name should be renamed, got {name:?}"
                );
                match ast.get(*body) {
                    Expr::Binary { lhs, .. } => {
                        let Expr::Ident(ident_name) = ast.get(*lhs) else {
                            panic!("expected Ident")
                        };
                        assert_eq!(ident_name, name);
                    }
                    other => panic!("expected Binary body, got {other:?}"),
                }
            }
            other => panic!("expected Let, got {other:?}"),
        }
    }

    #[test]
    fn outer_reference_after_let_is_not_renamed() {
        // (let x = 1 in x) + x — the trailing x is the free parameter,
        // untouched by the let's renaming.
        let ast = resolved("(let x = 1 in x) + x");
        match ast.get(ast.root) {
            Expr::Binary {
                op: BinaryOp::Add,
                rhs,
                ..
            } => {
                assert_eq!(ast.get(*rhs), &Expr::Ident("x".to_string()));
            }
            other => panic!("expected top-level Add, got {other:?}"),
        }
    }

    #[test]
    fn nested_shadowing_uses_distinct_names() {
        let ast = resolved("let x = 1 in let x = 2 in x");
        let Expr::Let {
            name: outer_name,
            body: outer_body,
            ..
        } = ast.get(ast.root)
        else {
            panic!()
        };
        let Expr::Let {
            name: inner_name,
            body: inner_body,
            ..
        } = ast.get(*outer_body)
        else {
            panic!()
        };
        assert_ne!(outer_name, inner_name);
        assert_eq!(ast.get(*inner_body), &Expr::Ident(inner_name.clone()));
    }

    #[test]
    fn let_inside_call_arg_does_not_leak_to_sibling_arg() {
        // f(let x = 1 in x, x) — the let's binding must only be visible in
        // its own body, not in the second call argument, and the callee
        // itself is a function name, never renamed.
        let ast = resolved("f(let x = 1 in x, x)");
        match ast.get(ast.root) {
            Expr::Call { callee, args } => {
                assert_eq!(callee, "f");
                assert_eq!(args.len(), 2);
                let Expr::Let { name, body, .. } = ast.get(args[0]) else {
                    panic!("expected Let")
                };
                assert!(name.contains('%'));
                assert_eq!(ast.get(*body), &Expr::Ident(name.clone()));
                // second arg is the free parameter `x`, untouched
                assert_eq!(ast.get(args[1]), &Expr::Ident("x".to_string()));
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn let_inside_if_condition_does_not_leak_to_branches() {
        // if (let x = 1 in x) > 0 then x else x — both branches reference
        // the outer free parameter `x`, not the condition's local let.
        let ast = resolved("if (let x = 1 in x) > 0 then x else x");
        match ast.get(ast.root) {
            Expr::If { then_, else_, .. } => {
                assert_eq!(ast.get(*then_), &Expr::Ident("x".to_string()));
                assert_eq!(ast.get(*else_), &Expr::Ident("x".to_string()));
            }
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn sibling_lets_shadowing_same_name_are_independent() {
        // (let x = 1 in x) + (let x = 2 in x) — both lets rename `x`, but
        // to distinct names, and each body resolves to its own let.
        let ast = resolved("(let x = 1 in x) + (let x = 2 in x)");
        match ast.get(ast.root) {
            Expr::Binary { lhs, rhs, .. } => {
                let Expr::Let {
                    name: lhs_name,
                    body: lhs_body,
                    ..
                } = ast.get(*lhs)
                else {
                    panic!()
                };
                let Expr::Let {
                    name: rhs_name,
                    body: rhs_body,
                    ..
                } = ast.get(*rhs)
                else {
                    panic!()
                };
                assert_ne!(lhs_name, rhs_name);
                assert_eq!(ast.get(*lhs_body), &Expr::Ident(lhs_name.clone()));
                assert_eq!(ast.get(*rhs_body), &Expr::Ident(rhs_name.clone()));
            }
            other => panic!("expected top-level Binary, got {other:?}"),
        }
    }

    #[test]
    fn let_value_referencing_same_name_binds_to_outer_scope() {
        // let x = x + 1 in x — the x inside `value` must resolve to whatever
        // x meant BEFORE this let (a free parameter here), not to the new
        // binding being created, since the binding doesn't exist yet while
        // its own initializer is evaluated.
        let ast = resolved("let x = x + 1 in x");
        let Expr::Let { name, value, body } = ast.get(ast.root) else {
            panic!()
        };
        match ast.get(*value) {
            Expr::Binary { lhs, .. } => {
                // lhs must be the ORIGINAL "x" (the free param), not `name`.
                assert_eq!(ast.get(*lhs), &Expr::Ident("x".to_string()));
            }
            other => panic!("expected Binary, got {other:?}"),
        }
        // body's x, by contrast, DOES refer to the new binding.
        assert_eq!(ast.get(*body), &Expr::Ident(name.clone()));
    }

    #[test]
    fn nested_let_value_referencing_same_name_binds_to_immediately_outer_let() {
        // let x = 1 in let x = x + 1 in x — the inner let's `value` refers
        // to `x` as bound by the OUTER let (whose value is `1`), not the
        // free parameter and not the inner binding being created.
        let ast = resolved("let x = 1 in let x = x + 1 in x");
        let Expr::Let {
            name: outer_name,
            body: outer_body,
            ..
        } = ast.get(ast.root)
        else {
            panic!()
        };
        let Expr::Let {
            name: inner_name,
            value: inner_value,
            body: inner_body,
        } = ast.get(*outer_body)
        else {
            panic!()
        };
        assert_ne!(outer_name, inner_name);
        match ast.get(*inner_value) {
            Expr::Binary { lhs, .. } => {
                assert_eq!(ast.get(*lhs), &Expr::Ident(outer_name.clone()));
            }
            other => panic!("expected Binary, got {other:?}"),
        }
        assert_eq!(ast.get(*inner_body), &Expr::Ident(inner_name.clone()));
    }
}
