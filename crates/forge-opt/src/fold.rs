// crates/forge-opt/src/fold.rs

use crate::Pass;
use forge_ir::*;

/// Both operands must be LITERAL constant instructions — this is different
/// from (and simpler than) algebraic simplification (`simplify.rs`, a later
/// task). When both operands are literal, computing the result now is
/// always safe for every op, including ones that produce NaN/Inf: folding
/// doesn't change what the program computes, it just computes it earlier.
/// `0.0 / 0.0 -> NaN` is exactly as correct at compile time as at runtime.
///
/// Deliberately NOT sharing logic with `forge_ir::interp` — structurally
/// similar per-op arithmetic, but reusing/refactoring the already
/// heavily-verified interpreter risked destabilizing it for this task's
/// sake. Correctness is instead pinned by `folding_never_changes_the_answer`
/// below, comparing against `interpret()` directly.
pub struct ConstFold;

impl Pass for ConstFold {
    fn name(&self) -> &'static str {
        "const-fold"
    }
    fn run(&mut self, f: &mut Function) -> bool {
        let mut changed = false;
        for i in 0..f.insts.len() {
            if let Some(folded) = try_fold(f, Value(i as u32)) {
                f.insts[i] = folded;
                changed = true;
            }
        }
        changed
    }
}

enum ConstVal {
    F64(f64),
    I64(i64),
    Bool(bool),
}

fn as_const(f: &Function, v: Value) -> Option<ConstVal> {
    match &f.insts[v.0 as usize] {
        Inst::ConstF64(bits) => Some(ConstVal::F64(f64::from_bits(*bits))),
        Inst::ConstI64(n) => Some(ConstVal::I64(*n)),
        Inst::ConstBool(b) => Some(ConstVal::Bool(*b)),
        _ => None,
    }
}

fn f64_inst(x: f64) -> Inst {
    Inst::ConstF64(x.to_bits())
}

fn try_fold(f: &Function, v: Value) -> Option<Inst> {
    match &f.insts[v.0 as usize] {
        Inst::Add(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(ConstVal::F64(x)), Some(ConstVal::F64(y))) => Some(f64_inst(x + y)),
            (Some(ConstVal::I64(x)), Some(ConstVal::I64(y))) => {
                Some(Inst::ConstI64(x.wrapping_add(y)))
            }
            _ => None,
        },
        Inst::Sub(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(ConstVal::F64(x)), Some(ConstVal::F64(y))) => Some(f64_inst(x - y)),
            (Some(ConstVal::I64(x)), Some(ConstVal::I64(y))) => {
                Some(Inst::ConstI64(x.wrapping_sub(y)))
            }
            _ => None,
        },
        Inst::Mul(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(ConstVal::F64(x)), Some(ConstVal::F64(y))) => Some(f64_inst(x * y)),
            (Some(ConstVal::I64(x)), Some(ConstVal::I64(y))) => {
                Some(Inst::ConstI64(x.wrapping_mul(y)))
            }
            _ => None,
        },
        Inst::Div(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(ConstVal::F64(x)), Some(ConstVal::F64(y))) => Some(f64_inst(x / y)),
            // Unlike f64 division, i64 division by zero has no well-defined
            // result (`wrapping_div` panics on it — it only guards the
            // i64::MIN / -1 overflow case, not divisor == 0). This code may
            // be unreachable at runtime (e.g. behind a dead branch), so we
            // must not crash the optimizer over it: just decline to fold
            // and leave the Div instruction as-is.
            (Some(ConstVal::I64(_)), Some(ConstVal::I64(0))) => None,
            (Some(ConstVal::I64(x)), Some(ConstVal::I64(y))) => {
                Some(Inst::ConstI64(x.wrapping_div(y)))
            }
            _ => None,
        },
        Inst::Rem(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(ConstVal::F64(x)), Some(ConstVal::F64(y))) => Some(f64_inst(x % y)),
            // Same reasoning as Div above: rem-by-zero has no well-defined
            // i64 result, so decline to fold rather than panic.
            (Some(ConstVal::I64(_)), Some(ConstVal::I64(0))) => None,
            (Some(ConstVal::I64(x)), Some(ConstVal::I64(y))) => {
                Some(Inst::ConstI64(x.wrapping_rem(y)))
            }
            _ => None,
        },
        Inst::Neg(a) => match as_const(f, *a) {
            Some(ConstVal::F64(x)) => Some(f64_inst(-x)),
            Some(ConstVal::I64(x)) => Some(Inst::ConstI64(x.wrapping_neg())),
            _ => None,
        },
        Inst::Sqrt(a) => match as_const(f, *a) {
            Some(ConstVal::F64(x)) => Some(f64_inst(x.sqrt())),
            _ => None,
        },
        Inst::Abs(a) => match as_const(f, *a) {
            Some(ConstVal::F64(x)) => Some(f64_inst(x.abs())),
            _ => None,
        },
        Inst::Floor(a) => match as_const(f, *a) {
            Some(ConstVal::F64(x)) => Some(f64_inst(x.floor())),
            _ => None,
        },
        Inst::Ceil(a) => match as_const(f, *a) {
            Some(ConstVal::F64(x)) => Some(f64_inst(x.ceil())),
            _ => None,
        },
        Inst::Round(a) => match as_const(f, *a) {
            Some(ConstVal::F64(x)) => Some(f64_inst(x.round())),
            _ => None,
        },
        Inst::Trunc(a) => match as_const(f, *a) {
            Some(ConstVal::F64(x)) => Some(f64_inst(x.trunc())),
            _ => None,
        },
        Inst::Min(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(ConstVal::F64(x)), Some(ConstVal::F64(y))) => Some(f64_inst(x.min(y))),
            _ => None,
        },
        Inst::Max(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(ConstVal::F64(x)), Some(ConstVal::F64(y))) => Some(f64_inst(x.max(y))),
            _ => None,
        },
        Inst::Fma { a, b, c } => match (as_const(f, *a), as_const(f, *b), as_const(f, *c)) {
            (Some(ConstVal::F64(x)), Some(ConstVal::F64(y)), Some(ConstVal::F64(z))) => {
                Some(f64_inst(x.mul_add(y, z)))
            }
            _ => None,
        },
        Inst::And(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(ConstVal::I64(x)), Some(ConstVal::I64(y))) => Some(Inst::ConstI64(x & y)),
            (Some(ConstVal::Bool(x)), Some(ConstVal::Bool(y))) => Some(Inst::ConstBool(x & y)),
            _ => None,
        },
        Inst::Or(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(ConstVal::I64(x)), Some(ConstVal::I64(y))) => Some(Inst::ConstI64(x | y)),
            (Some(ConstVal::Bool(x)), Some(ConstVal::Bool(y))) => Some(Inst::ConstBool(x | y)),
            _ => None,
        },
        Inst::Xor(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(ConstVal::I64(x)), Some(ConstVal::I64(y))) => Some(Inst::ConstI64(x ^ y)),
            _ => None,
        },
        Inst::Not(a) => match as_const(f, *a) {
            Some(ConstVal::I64(x)) => Some(Inst::ConstI64(!x)),
            Some(ConstVal::Bool(x)) => Some(Inst::ConstBool(!x)),
            _ => None,
        },
        Inst::Shl(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(ConstVal::I64(x)), Some(ConstVal::I64(y))) => {
                Some(Inst::ConstI64(x.wrapping_shl(y as u32)))
            }
            _ => None,
        },
        Inst::Shr(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(ConstVal::I64(x)), Some(ConstVal::I64(y))) => {
                Some(Inst::ConstI64((x as u64).wrapping_shr(y as u32) as i64))
            }
            _ => None,
        },
        Inst::Sar(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(ConstVal::I64(x)), Some(ConstVal::I64(y))) => {
                Some(Inst::ConstI64(x.wrapping_shr(y as u32)))
            }
            _ => None,
        },
        Inst::Cmp { op, lhs, rhs } => {
            let (l, r) = (as_const(f, *lhs)?, as_const(f, *rhs)?);
            eval_cmp_const(*op, l, r).map(Inst::ConstBool)
        }
        Inst::Call { func, args } => {
            let a = match as_const(f, args[0])? {
                ConstVal::F64(x) => x,
                _ => return None,
            };
            let result = match func {
                LibFunc::Sin => a.sin(),
                LibFunc::Cos => a.cos(),
                LibFunc::Tan => a.tan(),
                LibFunc::Exp => a.exp(),
                LibFunc::Log => a.ln(),
                LibFunc::Pow => match as_const(f, args[1])? {
                    ConstVal::F64(b) => a.powf(b),
                    _ => return None,
                },
            };
            Some(f64_inst(result))
        }
        Inst::IToF(a) => match as_const(f, *a) {
            Some(ConstVal::I64(x)) => Some(f64_inst(x as f64)),
            _ => None,
        },
        Inst::FToI(a) => match as_const(f, *a) {
            Some(ConstVal::F64(x)) => Some(Inst::ConstI64(x as i64)),
            _ => None,
        },
        _ => None,
    }
}

fn eval_cmp_const(op: CmpOp, l: ConstVal, r: ConstVal) -> Option<bool> {
    match (op, l, r) {
        (CmpOp::Eq, ConstVal::F64(x), ConstVal::F64(y)) => Some(x == y),
        (CmpOp::Ne, ConstVal::F64(x), ConstVal::F64(y)) => Some(x != y),
        (CmpOp::Lt, ConstVal::F64(x), ConstVal::F64(y)) => Some(x < y),
        (CmpOp::Le, ConstVal::F64(x), ConstVal::F64(y)) => Some(x <= y),
        (CmpOp::Gt, ConstVal::F64(x), ConstVal::F64(y)) => Some(x > y),
        (CmpOp::Ge, ConstVal::F64(x), ConstVal::F64(y)) => Some(x >= y),
        (CmpOp::Eq, ConstVal::I64(x), ConstVal::I64(y)) => Some(x == y),
        (CmpOp::Ne, ConstVal::I64(x), ConstVal::I64(y)) => Some(x != y),
        (CmpOp::Lt, ConstVal::I64(x), ConstVal::I64(y)) => Some(x < y),
        (CmpOp::Le, ConstVal::I64(x), ConstVal::I64(y)) => Some(x <= y),
        (CmpOp::Gt, ConstVal::I64(x), ConstVal::I64(y)) => Some(x > y),
        (CmpOp::Ge, ConstVal::I64(x), ConstVal::I64(y)) => Some(x >= y),
        (CmpOp::Eq, ConstVal::Bool(x), ConstVal::Bool(y)) => Some(x == y),
        (CmpOp::Ne, ConstVal::Bool(x), ConstVal::Bool(y)) => Some(x != y),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_ir::interp::{interpret, RtValue};
    use forge_syntax::lexer::lex;
    use forge_syntax::parser::parse;
    use forge_syntax::resolve::resolve;
    use forge_syntax::typeck::typecheck;

    fn lowered(src: &str) -> Function {
        let (tokens, _) = lex(src);
        let (ast, _) = parse(&tokens);
        let typed = typecheck(resolve(ast)).expect("should type-check");
        forge_ir::lower::lower(&typed)
    }

    #[test]
    fn folds_f64_constant_arithmetic() {
        let mut f = lowered("1.0 + 2.0");
        ConstFold.run(&mut f);
        let root = f.insts.last().unwrap();
        assert!(matches!(root, Inst::ConstF64(bits) if f64::from_bits(*bits) == 3.0));
    }

    #[test]
    fn folds_i64_constant_arithmetic_with_wrapping() {
        let mut f = lowered("9223372036854775807 + 1"); // i64::MAX + 1
        ConstFold.run(&mut f);
        let root = f.insts.last().unwrap();
        assert!(matches!(root, Inst::ConstI64(n) if *n == i64::MIN));
    }

    #[test]
    fn does_not_panic_on_i64_div_by_zero_even_in_dead_code() {
        let mut f = lowered("if false then 5 / 0 else 1");
        // Must not panic; folding div-by-zero should simply decline to fire.
        ConstFold.run(&mut f);
    }

    #[test]
    fn folds_i64_sar_hand_built() {
        // `Sar` (arithmetic shift right) has no surface syntax — only `Shr`
        // is ever lowered from `>>` (see forge_ir::lower.rs). Build the IR
        // by hand, same pattern as forge-ir/src/ir.rs's test module, so the
        // Sar arm of try_fold isn't left completely unverified.
        use forge_syntax::span::Span;
        let f = Function {
            insts: vec![
                Inst::ConstI64(-8),            // v0 = -8
                Inst::ConstI64(2),             // v1 = 2
                Inst::Sar(Value(0), Value(1)), // v2 = -8 >> 2 (arithmetic)
            ],
            types: vec![Ty::I64, Ty::I64, Ty::I64],
            spans: vec![Span::new(0, 0), Span::new(0, 0), Span::new(0, 0)],
            blocks: vec![BlockData {
                insts: vec![Value(0), Value(1), Value(2)],
                term: Some(Terminator::Return(Value(2))),
                preds: Default::default(),
            }],
            entry: Block(0),
            params: vec![],
        };
        let mut folded = f;
        ConstFold.run(&mut folded);
        // -8 >> 2 arithmetic == -2
        assert!(matches!(folded.insts[2], Inst::ConstI64(n) if n == -2));
    }

    #[test]
    fn does_not_fold_when_an_operand_is_not_literal() {
        let mut f = lowered("x + 1.0");
        let before = f.insts.len();
        let changed = ConstFold.run(&mut f);
        assert!(!changed);
        assert_eq!(f.insts.len(), before);
    }

    #[test]
    fn folding_never_changes_the_answer() {
        // The core correctness property for this pass: comparing the
        // interpreted result of the UNFOLDED vs FOLDED IR must always agree,
        // bit-exact (NaN-ness only for NaN cases) — folding must never be
        // an approximation, only an early computation of the same value.
        for src in [
            "1.0 / 0.0",
            "0.0 / 0.0",
            "-1.0 * 0.0",
            "sqrt(4.0)",
            "3.0 % 2.0",
            "7 / 2",
            "-7 / 2",
            "min(1.0, 2.0)",
            "max(1.0, 2.0)",
            "1.0 == 1.0",
            "sin(0.0)",
            "pow(2.0, 3.0)",
            "5 & 3",
            "5 | 2",
            "5 ^ 1",
            "~5",
            "1 << 4",
            "256 >> 4",
        ] {
            let unfolded = lowered(src);
            let expected = interpret(&unfolded, &[]);
            let mut folded = lowered(src);
            ConstFold.run(&mut folded);
            let actual = interpret(&folded, &[]);
            match (expected, actual) {
                (RtValue::F64(e), RtValue::F64(a)) => {
                    if e.is_nan() {
                        assert!(a.is_nan(), "NaN-ness mismatch for {src:?}: got {a}");
                    } else {
                        assert_eq!(e.to_bits(), a.to_bits(), "mismatch for {src:?}: {e} vs {a}");
                    }
                }
                (e, a) => assert_eq!(e, a, "mismatch for {src:?}"),
            }
        }
    }
}
