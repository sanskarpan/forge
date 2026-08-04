// crates/forge-opt/src/simplify.rs

use forge_ir::*;

/// Rewrites an instruction to a cheaper equivalent when ONE operand is a
/// literal identity/absorbing element, or the two operands are the same
/// `Value` (structural equality) — NOT both-literal (that's `fold.rs`'s
/// job). See SPEC.md §6.2 for the full correctness reasoning behind each
/// rule, especially the f64 sign-of-zero trap that makes `x + 0` asymmetric
/// with `x - 0`.
pub struct AlgebraicSimplify;

impl crate::Pass for AlgebraicSimplify {
    fn name(&self) -> &'static str {
        "algebraic-simplify"
    }
    fn run(&mut self, f: &mut Function) -> bool {
        let mut changed = false;
        for i in 0..f.insts.len() {
            let v = Value(i as u32);
            match try_simplify(f, v) {
                Some(Rewrite::UseExisting(existing)) => {
                    forge_ir::replace_value_everywhere(f, v, existing);
                    changed = true;
                }
                Some(Rewrite::BecomeConst(inst)) => {
                    f.insts[i] = inst;
                    changed = true;
                }
                None => {}
            }
        }
        changed
    }
}

enum Rewrite {
    /// The instruction's result equals an already-existing Value (an
    /// operand, or something reachable through one) — redirect all uses to
    /// it via `replace_value_everywhere` and let DCE clean up the dead
    /// instruction.
    UseExisting(Value),
    /// The instruction's result is a constant that ISN'T already present as
    /// a Value in the IR — rewrite this instruction's own slot in place
    /// (same pattern as `fold.rs`).
    BecomeConst(Inst),
}

/// True if `v` is EXACTLY f64 negative zero — either a literal `ConstF64`
/// with the sign bit set, or `Neg` of something that is (recursively,
/// through any number of chained `Neg`s) a positive-zero. The latter
/// matters because the surface language has no negative float literal
/// syntax: `-0.0` always parses as `Unary(Neg, Float(0.0))` and lowers to
/// `Inst::Neg(ConstF64(+0.0))`, not a literal negative-zero constant —
/// `ConstFold` would collapse that to a literal, but this pass must also
/// work standalone (it's driven directly in tests, and nothing requires
/// `ConstFold` to have run first). Seeing through `Neg` here is exact, not
/// approximate: negating a float constant is just a sign-bit flip, so
/// `Neg(+0.0)` IS `-0.0`, bit for bit — and the mutual recursion with
/// `is_f64_pos_zero` below correctly toggles through an arbitrary-depth
/// chain of `Neg`s (each recursive call strictly decreases the operand's
/// SSA index, which is acyclic in valid IR, so this always terminates).
fn is_f64_neg_zero(f: &Function, v: Value) -> bool {
    match &f.insts[v.0 as usize] {
        Inst::ConstF64(bits) => {
            let x = f64::from_bits(*bits);
            x == 0.0 && x.is_sign_negative()
        }
        Inst::Neg(inner) => is_f64_pos_zero(f, *inner),
        _ => false,
    }
}
fn is_f64_pos_zero(f: &Function, v: Value) -> bool {
    match &f.insts[v.0 as usize] {
        Inst::ConstF64(bits) => {
            let x = f64::from_bits(*bits);
            x == 0.0 && x.is_sign_positive()
        }
        Inst::Neg(inner) => is_f64_neg_zero(f, *inner),
        _ => false,
    }
}
fn is_i64_zero(f: &Function, v: Value) -> bool {
    matches!(f.insts[v.0 as usize], Inst::ConstI64(0))
}
fn is_f64_one(f: &Function, v: Value) -> bool {
    matches!(&f.insts[v.0 as usize], Inst::ConstF64(bits) if f64::from_bits(*bits) == 1.0)
}
fn is_i64_one(f: &Function, v: Value) -> bool {
    matches!(f.insts[v.0 as usize], Inst::ConstI64(1))
}
fn is_f64(f: &Function, v: Value) -> bool {
    f.types[v.0 as usize] == Ty::F64
}

fn try_simplify(f: &Function, v: Value) -> Option<Rewrite> {
    match &f.insts[v.0 as usize] {
        Inst::Add(a, b) => {
            let (a, b) = (*a, *b);
            // ⚠ f64: ONLY "+ (-0.0)" is safe (see SPEC.md §6.2 -- adding a
            // literal +0.0 to x=-0.0 gives +0.0, changing the sign). i64 has
            // no signed zero, so plain "+ 0" is fine in either position.
            if is_f64(f, a) {
                if is_f64_neg_zero(f, b) {
                    return Some(Rewrite::UseExisting(a));
                }
                if is_f64_neg_zero(f, a) {
                    return Some(Rewrite::UseExisting(b));
                }
            } else {
                if is_i64_zero(f, b) {
                    return Some(Rewrite::UseExisting(a));
                }
                if is_i64_zero(f, a) {
                    return Some(Rewrite::UseExisting(b));
                }
            }
            None
        }
        Inst::Sub(a, b) => {
            let (a, b) = (*a, *b);
            // x - 0 -> x IS always safe in both domains: subtracting +0.0 is
            // definitionally the same as adding -0.0. No sign trap here,
            // unlike Add above -- this asymmetry is real, don't "fix" it.
            if a == b && is_f64(f, a) {
                return None; // x - x for f64: NaN - NaN = NaN, unsafe.
            }
            let zero = if is_f64(f, a) {
                is_f64_pos_zero(f, b)
            } else {
                is_i64_zero(f, b)
            };
            if zero {
                return Some(Rewrite::UseExisting(a));
            }
            if a == b {
                // x - x -> 0. Safe for i64 unconditionally: subtraction
                // never traps (unlike Div below), and wrapping_sub(x, x) is
                // exactly 0 for every x, including i64::MIN. (The f64 case
                // was already handled and rejected above.)
                return Some(Rewrite::BecomeConst(Inst::ConstI64(0)));
            }
            None
        }
        Inst::Mul(a, b) => {
            let (a, b) = (*a, *b);
            if !is_f64(f, a) {
                // x * 0 -> 0, IntOnly: the zero is already an existing
                // Value (the literal operand itself) -- reuse it directly.
                if is_i64_zero(f, b) {
                    return Some(Rewrite::UseExisting(b));
                }
                if is_i64_zero(f, a) {
                    return Some(Rewrite::UseExisting(a));
                }
            }
            let one = |x: Value| {
                if is_f64(f, a) {
                    is_f64_one(f, x)
                } else {
                    is_i64_one(f, x)
                }
            };
            if one(b) {
                return Some(Rewrite::UseExisting(a));
            }
            if one(a) {
                return Some(Rewrite::UseExisting(b));
            }
            None
        }
        Inst::Div(a, b) => {
            let (a, b) = (*a, *b);
            // NOTE: SPEC.md §6.2 lists "x / x -> 1" as Validity::IntOnly
            // (only flagging the f64 0/0=NaN hazard). That table describes
            // mathematical validity assuming x is some fixed known value --
            // it does NOT account for what "IntOnly" means operationally
            // here: `a` and `b` are the same arbitrary SSA `Value`, not a
            // literal, so we cannot rule out x == 0 at runtime.
            //
            // The ORIGINAL `Div(x, x)` traps whenever x == 0 (confirmed
            // empirically: `i64::wrapping_div` still panics on a zero
            // divisor -- `wrapping_div` only guards the i64::MIN / -1
            // overflow case, exactly like the `ConstFold` div-by-zero bug
            // fixed in the previous task). If we rewrote this to a literal
            // `1`, a program that used to trap on x == 0 would instead
            // silently "succeed" with the wrong answer -- strictly worse
            // than either "both trap" or "both compute the same value",
            // and the same failure class as Task 2's dead-code panic bug
            // (just inverted: instead of the OPTIMIZER panicking on a value
            // that's dead, the optimizer would make a runtime trap vanish
            // for a value that's very much live). So this rule is
            // deliberately NOT implemented, unlike Sub's `x - x -> 0`
            // (subtraction never traps, so that one really is safe).
            let one = if is_f64(f, a) {
                is_f64_one(f, b)
            } else {
                is_i64_one(f, b)
            };
            if one {
                Some(Rewrite::UseExisting(a))
            } else {
                None
            }
        }
        Inst::And(a, b) if a == b => Some(Rewrite::UseExisting(*a)), // idempotent, both i64 and bool
        Inst::Xor(a, b) if a == b => {
            // Only reachable as i64 -- there's no logical-xor surface
            // operator (see lower.rs), so Bool-typed Xor never occurs today.
            // XOR never traps, so this is unconditionally safe.
            Some(Rewrite::BecomeConst(Inst::ConstI64(0)))
        }
        Inst::Neg(a) => match &f.insts[a.0 as usize] {
            Inst::Neg(inner) => Some(Rewrite::UseExisting(*inner)), // -(-x) -> x, exact both domains
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pass;
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

    /// `typeck`'s parameter-type inference (see `forge_syntax::typeck::Ctx::
    /// infer_expect`) only forces a bare identifier to `I64` when it's used
    /// somewhere that DEMANDS `I64` -- the bitwise/shift operators, or
    /// (transitively) an arithmetic operator's operand when the SURROUNDING
    /// context already expects `I64`. A sibling `I64` literal does NOT do
    /// it: `n * 0` alone infers `n: F64` (the default), silently widening
    /// the `0` via `IToF` instead of giving `n` the `I64` type the test
    /// intends. `& -1` (bitwise AND with all-ones, a no-op) forces `I64`
    /// top-down through `Sub`/`Div`/`Mul`'s pass-through inference without
    /// disturbing which `Value` each bare `n` resolves to, so both
    /// occurrences of `n` in e.g. `(n - n) & -1` still refer to the exact
    /// same SSA value -- required for the `a == b` structural-equality
    /// rules under test here.
    fn lowered_i64(src: &str) -> Function {
        lowered(&format!("({src}) & -1"))
    }

    #[test]
    fn x_plus_neg_zero_simplifies_to_x() {
        // "x + -0.0" parses as Add(x, Unary(Neg, Float(0.0))) -- there's no
        // negative-float-literal syntax, so this lowers to
        // Add(Param, Neg(ConstF64(+0.0))), not a literal ConstF64(-0.0).
        // `is_f64_neg_zero` sees through that `Neg` (exact, not folding),
        // so the rule fires without needing `ConstFold` to run first.
        let mut f = lowered("x + -0.0");
        let changed = AlgebraicSimplify.run(&mut f);
        assert!(changed);
        // The root Add should have been replaced by the Param directly.
        assert!(
            matches!(f.blocks[f.entry.0 as usize].term, Some(Terminator::Return(v)) if matches!(f.insts[v.0 as usize], Inst::Param { .. }))
        );
    }

    #[test]
    fn x_plus_pos_zero_is_not_simplified_for_f64() {
        // The bug this whole rule exists to avoid: x + 0.0 is NOT safe when
        // x could be -0.0 at runtime, so this direction must NOT fire.
        let mut f = lowered("x + 0.0");
        let changed = AlgebraicSimplify.run(&mut f);
        assert!(
            !changed,
            "x + 0.0 must not simplify for f64 -- see design doc"
        );
    }

    #[test]
    fn x_minus_zero_simplifies_for_f64() {
        // Unlike the addition case, x - 0.0 IS always safe.
        let mut f = lowered("x - 0.0");
        let changed = AlgebraicSimplify.run(&mut f);
        assert!(changed);
    }

    #[test]
    fn x_times_zero_is_not_simplified_for_f64_but_is_for_i64() {
        let mut f_float = lowered("x * 0.0");
        assert!(
            !AlgebraicSimplify.run(&mut f_float),
            "f64 x*0 must not fold -- NaN/Inf*0=NaN"
        );

        let mut f_int = lowered_i64("n * 0");
        assert!(
            AlgebraicSimplify.run(&mut f_int),
            "i64 x*0 should simplify to 0"
        );
    }

    #[test]
    fn zero_times_x_is_also_simplified_for_i64_zero_on_the_left() {
        // Commutative-operand coverage: `n * 0` (zero on the right) is
        // covered above, but the Mul arm has a separate `is_i64_zero(f, a)`
        // branch for zero on the LEFT that nothing was exercising.
        let mut f_int = lowered_i64("0 * n");
        assert!(
            AlgebraicSimplify.run(&mut f_int),
            "i64 0*x should simplify to 0 (zero on the left)"
        );
    }

    #[test]
    fn double_negation_cancels() {
        let mut f = lowered("- -x");
        assert!(AlgebraicSimplify.run(&mut f));
    }

    #[test]
    fn x_times_one_and_x_div_one_actually_fire_for_f64() {
        // The differential-sample test below confirms these rules don't
        // change the ANSWER, but never asserts they actually FIRE -- it
        // would pass identically even if both rules silently stopped
        // simplifying anything. Dedicated `changed` coverage here closes
        // that gap.
        let mut f_mul = lowered("x * 1.0");
        assert!(AlgebraicSimplify.run(&mut f_mul), "x * 1.0 should simplify");

        let mut f_div = lowered("x / 1.0");
        assert!(AlgebraicSimplify.run(&mut f_div), "x / 1.0 should simplify");
    }

    #[test]
    fn and_of_same_value_simplifies_to_that_value() {
        // `&` forces I64 directly (no `lowered_i64` workaround needed), and
        // both occurrences of `n` resolve to the same SSA Param value.
        let mut f = lowered("n & n");
        let changed = AlgebraicSimplify.run(&mut f);
        assert!(changed, "x & x should simplify to x");
        // Redirected to the existing Param value, not a new constant.
        assert!(
            matches!(f.blocks[f.entry.0 as usize].term, Some(Terminator::Return(v)) if matches!(f.insts[v.0 as usize], Inst::Param { .. }))
        );
    }

    #[test]
    fn xor_of_same_value_simplifies_to_zero() {
        let mut f = lowered("n ^ n");
        let changed = AlgebraicSimplify.run(&mut f);
        assert!(changed, "x ^ x should simplify to 0");
        assert!(
            matches!(f.blocks[f.entry.0 as usize].term, Some(Terminator::Return(v)) if matches!(f.insts[v.0 as usize], Inst::ConstI64(0)))
        );
    }

    #[test]
    fn x_div_x_is_not_simplified_for_i64() {
        // See the long comment on the Div arm of `try_simplify`: x is an
        // arbitrary SSA value here, not a proven-nonzero literal, and the
        // original Div(x, x) traps at runtime whenever x == 0. Folding this
        // to a literal 1 would silently erase that trap. Regression test
        // for that soundness issue.
        let mut f = lowered_i64("n / n");
        let changed = AlgebraicSimplify.run(&mut f);
        assert!(
            !changed,
            "x / x must not simplify to 1 for i64 -- x may be 0 at runtime, and the \
             original Div(x, x) traps in that case; folding away the trap is unsound"
        );
    }

    #[test]
    #[should_panic(expected = "divide by zero")]
    fn confirms_div_x_x_actually_traps_at_runtime_when_x_is_zero() {
        // Empirical confirmation backing the test above: the UNSIMPLIFIED
        // program really does panic (mirroring a real hardware trap) when
        // x == 0, so declining to simplify Div(x, x) is preserving real
        // behavior, not being overly conservative about nothing.
        let f = lowered_i64("n / n");
        let _ = interpret(&f, &[RtValue::I64(0)]);
    }

    #[test]
    fn simplification_never_changes_the_answer_for_a_representative_sample() {
        // Broader correctness net: for a handful of expressions where a
        // rule SHOULD fire, confirm interpret() agrees before and after.
        let cases: &[(&str, &[RtValue])] = &[
            ("x - 0.0", &[RtValue::F64(-0.0)]),
            ("x - 0.0", &[RtValue::F64(3.5)]),
            ("x * 1.0", &[RtValue::F64(f64::NAN)]),
            ("x / 1.0", &[RtValue::F64(f64::INFINITY)]),
            ("- -x", &[RtValue::F64(-0.0)]),
            // `& -1` forces `n` to I64 -- see `lowered_i64`'s doc comment.
            ("(n * 0) & -1", &[RtValue::I64(42)]),
            ("(n - n) & -1", &[RtValue::I64(i64::MIN)]),
            ("(n / n) & -1", &[RtValue::I64(7)]),
        ];
        for (src, args) in cases {
            let unfolded = lowered(src);
            let expected = interpret(&unfolded, args);
            let mut simplified = lowered(src);
            AlgebraicSimplify.run(&mut simplified);
            let actual = interpret(&simplified, args);
            match (expected, actual) {
                (RtValue::F64(e), RtValue::F64(a)) => {
                    if e.is_nan() {
                        assert!(a.is_nan(), "NaN-ness mismatch for {src:?} args={args:?}");
                    } else {
                        assert_eq!(
                            e.to_bits(),
                            a.to_bits(),
                            "mismatch for {src:?} args={args:?}: {e} vs {a}"
                        );
                    }
                }
                (e, a) => assert_eq!(e, a, "mismatch for {src:?} args={args:?}"),
            }
        }
    }
}
