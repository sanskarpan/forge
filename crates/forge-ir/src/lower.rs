// crates/forge-ir/src/lower.rs

use smallvec::smallvec;

use forge_syntax::ast::{BinaryOp, Expr, ExprIdx, UnaryOp};
use forge_syntax::typeck::{Ty as AstTy, TypedAst};

use crate::builder::Builder;
use crate::ir::*;

/// Lowers a type-checked AST into SSA IR.
///
/// # Precondition
///
/// `typed` must have been produced by a successful [`forge_syntax::typeck::typecheck`]
/// call (typically via `typecheck(resolve(ast))`). This function does not
/// re-validate call arity or callee names — that's `typecheck`'s job.
/// Behavior on a hand-built or otherwise malformed `TypedAst` (e.g. an
/// `Expr::Call` with an unknown callee, or a known callee with the wrong
/// number of arguments) is unspecified: it may panic via `unreachable!()`
/// or an index-out-of-bounds in `lower_call`, rather than returning a
/// diagnostic.
pub fn lower(typed: &TypedAst) -> Function {
    let mut b = Builder::new();
    let entry = b.create_block();
    b.f.entry = entry;
    b.cur_block = entry;
    b.seal_block(entry);

    let root_span = typed.ast.span(typed.ast.root);
    for (i, (name, ty)) in typed.params.iter().enumerate() {
        let ty = lower_ty(*ty);
        let v = b.emit(
            entry,
            Inst::Param {
                index: i as u32,
                ty,
            },
            ty,
            root_span,
        );
        b.f.params.push((name.clone(), ty));
        b.write_variable(name, entry, v);
    }

    let (result, exit_block) = lower_expr(&mut b, typed, typed.ast.root);
    b.f.blocks[exit_block.0 as usize].term = Some(Terminator::Return(result));
    b.f
}

fn lower_ty(t: AstTy) -> Ty {
    match t {
        AstTy::F64 => Ty::F64,
        AstTy::I64 => Ty::I64,
        AstTy::Bool => Ty::Bool,
    }
}

/// Implicit i64 -> f64 widening (SPEC §3), applied at the one place it's
/// actually needed: `typeck`'s `check_binary`/`check_call` allow an i64
/// operand where f64 is expected, so lowering must insert the conversion
/// typeck implicitly promised. Only I64 needs coercion here -- Bool never
/// reaches this because typeck already rejected it upstream.
fn coerce_to_f64(
    b: &mut Builder,
    block: Block,
    val: Value,
    ty: Ty,
    span: forge_syntax::span::Span,
) -> Value {
    if ty == Ty::I64 {
        b.emit(block, Inst::IToF(val), Ty::F64, span)
    } else {
        val
    }
}

/// Returns the value produced and the block that now holds it. `if` creates
/// new blocks, so every caller threads the returned block forward instead of
/// assuming `b.cur_block` is still what it was before the recursive call.
///
/// # Invariant
///
/// On return, `b.cur_block` must equal the returned `Block` — this is
/// checked with a `debug_assert_eq!` below. Any new block-creating construct
/// added to the match below must explicitly reassign `b.cur_block` before
/// recursing into (or emitting in) the new block; see the `If` arm for the
/// canonical pattern (assign `b.cur_block` immediately before each
/// `lower_expr` call into a freshly created block).
fn lower_expr(b: &mut Builder, typed: &TypedAst, idx: ExprIdx) -> (Value, Block) {
    let span = typed.ast.span(idx);
    let ty = lower_ty(typed.types[idx.index()]);
    let block = b.cur_block;

    let result = match typed.ast.get(idx).clone() {
        Expr::Float(v) => (b.emit(block, Inst::ConstF64(v.to_bits()), ty, span), block),
        Expr::Int(n) => (b.emit(block, Inst::ConstI64(n), ty, span), block),
        Expr::Bool(v) => (b.emit(block, Inst::ConstBool(v), ty, span), block),
        Expr::Ident(name) => (b.read_variable(&name, block, ty), block),

        Expr::Unary { op, operand } => {
            let (v, block) = lower_expr(b, typed, operand);
            b.cur_block = block;
            let inst = match op {
                UnaryOp::Neg => Inst::Neg(v),
                UnaryOp::Not | UnaryOp::BitNot => Inst::Not(v),
            };
            (b.emit(block, inst, ty, span), block)
        }

        Expr::Binary { op, lhs, rhs } => {
            let (l, block) = lower_expr(b, typed, lhs);
            b.cur_block = block;
            let (r, block) = lower_expr(b, typed, rhs);
            b.cur_block = block;
            // Implicit i64 -> f64 widening (SPEC §3): typeck's check_binary
            // allows (F64,F64), (I64,F64), (F64,I64), and (I64,I64), yielding
            // F64/F64/F64/I64 respectively. Only coerce when the overall
            // result is F64 -- (I64,I64) must NOT be coerced, or pure integer
            // arithmetic would silently become float arithmetic.
            let (l, r) = if matches!(
                op,
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem
            ) && ty == Ty::F64
            {
                let lty = lower_ty(typed.types[lhs.index()]);
                let rty = lower_ty(typed.types[rhs.index()]);
                (
                    coerce_to_f64(b, block, l, lty, span),
                    coerce_to_f64(b, block, r, rty, span),
                )
            } else {
                (l, r)
            };
            let inst = lower_binary(op, l, r);
            (b.emit(block, inst, ty, span), block)
        }

        Expr::Call { callee, args } => {
            let mut vals = Vec::new();
            let mut block = block;
            for a in &args {
                let (v, blk) = lower_expr(b, typed, *a);
                let arg_ty = lower_ty(typed.types[a.index()]);
                block = blk;
                b.cur_block = block;
                vals.push(coerce_to_f64(b, block, v, arg_ty, span));
            }
            let inst = lower_call(&callee, &vals);
            (b.emit(block, inst, ty, span), block)
        }

        Expr::If { cond, then_, else_ } => {
            let (c, block) = lower_expr(b, typed, cond);
            let then_block = b.create_block();
            let else_block = b.create_block();
            let merge_block = b.create_block();

            b.f.blocks[block.0 as usize].term = Some(Terminator::Branch {
                cond: c,
                then_: then_block,
                else_: else_block,
            });
            b.add_pred(then_block, block);
            b.add_pred(else_block, block);
            b.seal_block(then_block);
            b.seal_block(else_block);

            b.cur_block = then_block;
            let (then_val, then_exit) = lower_expr(b, typed, then_);
            b.f.blocks[then_exit.0 as usize].term = Some(Terminator::Jump(merge_block));
            b.add_pred(merge_block, then_exit);

            b.cur_block = else_block;
            let (else_val, else_exit) = lower_expr(b, typed, else_);
            b.f.blocks[else_exit.0 as usize].term = Some(Terminator::Jump(merge_block));
            b.add_pred(merge_block, else_exit);

            b.seal_block(merge_block);
            b.cur_block = merge_block;
            let incoming = smallvec![(then_exit, then_val), (else_exit, else_val)];
            (
                b.emit(merge_block, Inst::Phi { incoming }, ty, span),
                merge_block,
            )
        }

        // `name` was already alpha-renamed by forge_syntax::resolve to be
        // globally unique, so writing it into this block's SSA variable map
        // can never collide with (or need restoring after) any other
        // binding — see the design doc's "Resolved ambiguities".
        Expr::Let { name, value, body } => {
            let (v, block) = lower_expr(b, typed, value);
            b.cur_block = block;
            b.write_variable(&name, block, v);
            lower_expr(b, typed, body)
        }
    };

    debug_assert_eq!(
        b.cur_block, result.1,
        "lower_expr must leave b.cur_block equal to the block it returns"
    );
    result
}

fn lower_binary(op: BinaryOp, l: Value, r: Value) -> Inst {
    use BinaryOp::*;
    match op {
        Add => Inst::Add(l, r),
        Sub => Inst::Sub(l, r),
        Mul => Inst::Mul(l, r),
        Div => Inst::Div(l, r),
        Rem => Inst::Rem(l, r),
        BitAnd | And => Inst::And(l, r),
        BitOr | Or => Inst::Or(l, r),
        BitXor => Inst::Xor(l, r),
        Shl => Inst::Shl(l, r),
        Shr => Inst::Shr(l, r),
        Eq => Inst::Cmp {
            op: CmpOp::Eq,
            lhs: l,
            rhs: r,
        },
        Ne => Inst::Cmp {
            op: CmpOp::Ne,
            lhs: l,
            rhs: r,
        },
        Lt => Inst::Cmp {
            op: CmpOp::Lt,
            lhs: l,
            rhs: r,
        },
        Le => Inst::Cmp {
            op: CmpOp::Le,
            lhs: l,
            rhs: r,
        },
        Gt => Inst::Cmp {
            op: CmpOp::Gt,
            lhs: l,
            rhs: r,
        },
        Ge => Inst::Cmp {
            op: CmpOp::Ge,
            lhs: l,
            rhs: r,
        },
    }
}

/// # Precondition
///
/// `callee` must be a known intrinsic name and `args` must have exactly the
/// arity `typecheck`'s `check_call` validated for it (1 for sqrt/abs/floor/
/// ceil/round/trunc/sin/cos/tan/exp/log, 2 for min/max/pow, 3 for fma) — see
/// `lower`'s doc comment. The `debug_assert!`s below turn a violation into a
/// clear panic message in debug builds instead of a raw index-out-of-bounds.
fn lower_call(callee: &str, args: &[Value]) -> Inst {
    match callee {
        "sqrt" => {
            debug_assert!(args.len() == 1);
            Inst::Sqrt(args[0])
        }
        "abs" => {
            debug_assert!(args.len() == 1);
            Inst::Abs(args[0])
        }
        "floor" => {
            debug_assert!(args.len() == 1);
            Inst::Floor(args[0])
        }
        "ceil" => {
            debug_assert!(args.len() == 1);
            Inst::Ceil(args[0])
        }
        "round" => {
            debug_assert!(args.len() == 1);
            Inst::Round(args[0])
        }
        "trunc" => {
            debug_assert!(args.len() == 1);
            Inst::Trunc(args[0])
        }
        "min" => {
            debug_assert!(args.len() == 2);
            Inst::Min(args[0], args[1])
        }
        "max" => {
            debug_assert!(args.len() == 2);
            Inst::Max(args[0], args[1])
        }
        "fma" => {
            debug_assert!(args.len() == 3);
            Inst::Fma {
                a: args[0],
                b: args[1],
                c: args[2],
            }
        }
        "sin" => {
            debug_assert!(args.len() == 1);
            Inst::Call {
                func: LibFunc::Sin,
                args: args.iter().copied().collect(),
            }
        }
        "cos" => {
            debug_assert!(args.len() == 1);
            Inst::Call {
                func: LibFunc::Cos,
                args: args.iter().copied().collect(),
            }
        }
        "tan" => {
            debug_assert!(args.len() == 1);
            Inst::Call {
                func: LibFunc::Tan,
                args: args.iter().copied().collect(),
            }
        }
        "exp" => {
            debug_assert!(args.len() == 1);
            Inst::Call {
                func: LibFunc::Exp,
                args: args.iter().copied().collect(),
            }
        }
        "log" => {
            debug_assert!(args.len() == 1);
            Inst::Call {
                func: LibFunc::Log,
                args: args.iter().copied().collect(),
            }
        }
        "pow" => {
            debug_assert!(args.len() == 2);
            Inst::Call {
                func: LibFunc::Pow,
                args: args.iter().copied().collect(),
            }
        }
        other => unreachable!("type checker already rejected unknown intrinsic `{other}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_syntax::lexer::lex;
    use forge_syntax::parser::parse;
    use forge_syntax::resolve::resolve;
    use forge_syntax::typeck::typecheck;

    fn lowered(src: &str) -> Function {
        let (tokens, _) = lex(src);
        let (ast, _) = parse(&tokens);
        let typed = typecheck(resolve(ast)).expect("should type-check");
        lower(&typed)
    }

    #[test]
    fn sqrt_of_sum_of_squares_is_exactly_six_instructions() {
        // param x, param y, mul, mul, add, sqrt — matches SPEC §15's example.
        let f = lowered("sqrt(x * x + y * y)");
        assert_eq!(f.insts.len(), 6);
    }

    #[test]
    fn if_produces_four_blocks_with_a_phi() {
        let f = lowered("if x > 0.0 then x else -x");
        assert_eq!(f.blocks.len(), 4); // entry, then, else, merge
        let merge = &f.blocks[3];
        let phi_count = merge
            .insts
            .iter()
            .filter(|&&v| matches!(f.insts[v.0 as usize], Inst::Phi { .. }))
            .count();
        assert_eq!(phi_count, 1);
    }

    #[test]
    fn outer_reference_after_shadowing_let_resolves_to_the_parameter() {
        // (let x = 1.0 in x) + x must add the let's value to the PARAMETER x,
        // not to itself — this is the shadowing bug the resolve pass fixes.
        let f = lowered("(let x = 1.0 in x) + x");
        let add = f
            .insts
            .iter()
            .find(|i| matches!(i, Inst::Add(_, _)))
            .expect("an Add exists");
        let Inst::Add(l, r) = add else { unreachable!() };
        // Both operands trace back to the single Param instruction — if the
        // shadow leaked, one operand would instead be the ConstF64(1.0).
        let param_idx = f
            .insts
            .iter()
            .position(|i| matches!(i, Inst::Param { .. }))
            .unwrap() as u32;
        assert_eq!(r.0, param_idx, "the trailing `x` must be the parameter");
        let _ = l; // lhs is the let's inner (shadowed) value — not asserted here
    }

    #[test]
    fn every_block_has_a_terminator() {
        let f = lowered("if x > 0.0 then x else -x");
        for b in &f.blocks {
            assert!(b.term.is_some());
        }
    }

    #[test]
    fn int_literal_lowers_to_const_i64() {
        let f = lowered("1");
        assert!(f.insts.iter().any(|i| matches!(i, Inst::ConstI64(1))));
    }

    #[test]
    fn bool_literal_lowers_to_const_bool() {
        let f = lowered("true");
        assert!(f.insts.iter().any(|i| matches!(i, Inst::ConstBool(true))));
    }

    #[test]
    fn logical_not_lowers_to_inst_not() {
        let f = lowered("!true");
        assert!(f.insts.iter().any(|i| matches!(i, Inst::Not(_))));
    }

    #[test]
    fn bitwise_not_lowers_to_inst_not() {
        let f = lowered("~n");
        assert!(f.insts.iter().any(|i| matches!(i, Inst::Not(_))));
    }

    #[test]
    fn sub_lowers_to_inst_sub() {
        let f = lowered("x - 1.0");
        assert!(f.insts.iter().any(|i| matches!(i, Inst::Sub(_, _))));
    }

    #[test]
    fn div_lowers_to_inst_div() {
        let f = lowered("x / 2.0");
        assert!(f.insts.iter().any(|i| matches!(i, Inst::Div(_, _))));
    }

    #[test]
    fn bitand_lowers_to_inst_and() {
        let f = lowered("n & 1");
        assert!(f.insts.iter().any(|i| matches!(i, Inst::And(_, _))));
    }

    #[test]
    fn eq_lowers_to_cmp_eq() {
        let f = lowered("x == 1.0");
        assert!(f
            .insts
            .iter()
            .any(|i| matches!(i, Inst::Cmp { op: CmpOp::Eq, .. })));
    }

    #[test]
    fn min_lowers_to_inst_min_with_both_args() {
        let f = lowered("min(x, y)");
        let min = f
            .insts
            .iter()
            .find_map(|i| match i {
                Inst::Min(a, b) => Some((*a, *b)),
                _ => None,
            })
            .expect("a Min inst exists");
        assert_ne!(min.0, min.1, "min's two args must be distinct params");
    }

    #[test]
    fn max_lowers_to_inst_max_with_both_args() {
        let f = lowered("max(x, y)");
        let max = f
            .insts
            .iter()
            .find_map(|i| match i {
                Inst::Max(a, b) => Some((*a, *b)),
                _ => None,
            })
            .expect("a Max inst exists");
        assert_ne!(max.0, max.1, "max's two args must be distinct params");
    }

    #[test]
    fn fma_lowers_to_inst_fma_with_three_distinct_args() {
        let f = lowered("fma(x, y, z)");
        let fma = f
            .insts
            .iter()
            .find_map(|i| match i {
                Inst::Fma { a, b, c } => Some((*a, *b, *c)),
                _ => None,
            })
            .expect("an Fma inst exists");
        assert_ne!(fma.0, fma.1);
        assert_ne!(fma.1, fma.2);
        assert_ne!(fma.0, fma.2);
    }

    #[test]
    fn sin_lowers_to_libm_call() {
        let f = lowered("sin(x)");
        assert!(f.insts.iter().any(|i| matches!(
            i,
            Inst::Call {
                func: LibFunc::Sin,
                ..
            }
        )));
    }

    #[test]
    fn mixed_arithmetic_inserts_itof_for_the_integer_operand() {
        let f = lowered("1 + 2.0");
        let itof_count = f
            .insts
            .iter()
            .filter(|i| matches!(i, Inst::IToF(_)))
            .count();
        assert_eq!(
            itof_count, 1,
            "exactly one operand (the int literal) needs widening"
        );
        let add = f
            .insts
            .iter()
            .find(|i| matches!(i, Inst::Add(_, _)))
            .expect("an Add exists");
        let Inst::Add(l, r) = add else { unreachable!() };
        // Both operands feeding the Add must be f64-typed post-coercion.
        assert_eq!(f.types[l.0 as usize], Ty::F64);
        assert_eq!(f.types[r.0 as usize], Ty::F64);
    }

    #[test]
    fn pure_i64_arithmetic_inserts_no_itof() {
        let f = lowered("1 + 2");
        assert!(!f.insts.iter().any(|i| matches!(i, Inst::IToF(_))));
    }

    #[test]
    fn intrinsic_call_with_int_literal_arg_inserts_itof() {
        let f = lowered("sqrt(4)");
        assert!(f.insts.iter().any(|i| matches!(i, Inst::IToF(_))));
        assert!(f.insts.iter().any(|i| matches!(i, Inst::Sqrt(_))));
    }

    #[test]
    fn let_bound_i64_widens_with_one_itof_when_used_with_f64() {
        let f = lowered("let t = 1 in t + 2.0");
        let itof_count = f
            .insts
            .iter()
            .filter(|i| matches!(i, Inst::IToF(_)))
            .count();
        assert_eq!(
            itof_count, 1,
            "the let-bound i64 local must be widened exactly once"
        );
        let add = f
            .insts
            .iter()
            .find(|i| matches!(i, Inst::Add(_, _)))
            .expect("an Add exists");
        let Inst::Add(l, r) = add else { unreachable!() };
        assert_eq!(f.types[l.0 as usize], Ty::F64);
        assert_eq!(f.types[r.0 as usize], Ty::F64);
    }
}
