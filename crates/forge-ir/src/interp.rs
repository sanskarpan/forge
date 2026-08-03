// crates/forge-ir/src/interp.rs

use crate::ir::*;

/// Runtime value carried through the interpreter, and later into JIT calling
/// conventions. `Function.params` allows real, independently-typed f64/i64/
/// bool parameters, so a single `f64` slot can't represent every argument or
/// result — hence the enum. See SPEC §3 "Runtime value representation".
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RtValue {
    F64(f64),
    I64(i64),
    Bool(bool),
}

impl RtValue {
    pub fn as_f64(self) -> f64 {
        match self {
            RtValue::F64(x) => x,
            _ => panic!("expected f64"),
        }
    }
    pub fn as_i64(self) -> i64 {
        match self {
            RtValue::I64(x) => x,
            _ => panic!("expected i64"),
        }
    }
    pub fn as_bool(self) -> bool {
        match self {
            RtValue::Bool(x) => x,
            _ => panic!("expected bool"),
        }
    }
}

fn get(vals: &[Option<RtValue>], v: Value) -> RtValue {
    vals[v.0 as usize].expect("undefined value")
}

/// The correctness oracle for the entire project. Must implement IEEE-754
/// semantics EXACTLY — NaN propagation, signed zeros, infinities, subnormals.
/// No shortcuts. Every future differential test compares the JIT to this,
/// bit for bit, so any sloppiness here becomes a false failure (or worse,
/// masks a real JIT bug). Integer ops use Rust's wrapping arithmetic
/// throughout, matching the raw machine `add`/`sub`/`imul` the JIT will
/// eventually emit, which wrap on overflow with no trap.
pub fn interpret(f: &Function, args: &[RtValue]) -> RtValue {
    let mut vals: Vec<Option<RtValue>> = vec![None; f.insts.len()];
    let mut block = f.entry;
    let mut prev_block: Option<Block> = None;

    loop {
        for &v in &f.blocks[block.0 as usize].insts {
            let result = match &f.insts[v.0 as usize] {
                Inst::ConstF64(bits) => RtValue::F64(f64::from_bits(*bits)),
                Inst::ConstI64(n) => RtValue::I64(*n),
                Inst::ConstBool(b) => RtValue::Bool(*b),
                Inst::Param { index, .. } => args[*index as usize],

                Inst::Add(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::F64(x), RtValue::F64(y)) => RtValue::F64(x + y),
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x.wrapping_add(y)),
                    _ => unreachable!("type checker guarantees matching operand types"),
                },
                Inst::Sub(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::F64(x), RtValue::F64(y)) => RtValue::F64(x - y),
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x.wrapping_sub(y)),
                    _ => unreachable!(),
                },
                Inst::Mul(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::F64(x), RtValue::F64(y)) => RtValue::F64(x * y),
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x.wrapping_mul(y)),
                    _ => unreachable!(),
                },
                Inst::Div(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::F64(x), RtValue::F64(y)) => RtValue::F64(x / y),
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x.wrapping_div(y)),
                    _ => unreachable!(),
                },
                Inst::Rem(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::F64(x), RtValue::F64(y)) => RtValue::F64(x % y),
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x.wrapping_rem(y)),
                    _ => unreachable!(),
                },
                Inst::Neg(a) => match get(&vals, *a) {
                    RtValue::F64(x) => RtValue::F64(-x),
                    RtValue::I64(x) => RtValue::I64(x.wrapping_neg()),
                    _ => unreachable!(),
                },

                Inst::Sqrt(a) => RtValue::F64(get(&vals, *a).as_f64().sqrt()),
                Inst::Abs(a) => RtValue::F64(get(&vals, *a).as_f64().abs()),
                Inst::Floor(a) => RtValue::F64(get(&vals, *a).as_f64().floor()),
                Inst::Ceil(a) => RtValue::F64(get(&vals, *a).as_f64().ceil()),
                Inst::Round(a) => RtValue::F64(get(&vals, *a).as_f64().round()),
                Inst::Trunc(a) => RtValue::F64(get(&vals, *a).as_f64().trunc()),
                // CAREFUL: f64::min/max have DIFFERENT NaN semantics from
                // x86's minsd/maxsd (Rust returns the non-NaN operand; minsd
                // returns its second operand if either is NaN). We pick
                // Rust's semantics here — codegen must later emit an extra
                // compare rather than a bare minsd to match.
                Inst::Min(a, b) => {
                    RtValue::F64(get(&vals, *a).as_f64().min(get(&vals, *b).as_f64()))
                }
                Inst::Max(a, b) => {
                    RtValue::F64(get(&vals, *a).as_f64().max(get(&vals, *b).as_f64()))
                }
                Inst::Fma { a, b, c } => RtValue::F64(
                    get(&vals, *a)
                        .as_f64()
                        .mul_add(get(&vals, *b).as_f64(), get(&vals, *c).as_f64()),
                ),

                Inst::And(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x & y),
                    (RtValue::Bool(x), RtValue::Bool(y)) => RtValue::Bool(x & y),
                    _ => unreachable!(),
                },
                Inst::Or(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x | y),
                    (RtValue::Bool(x), RtValue::Bool(y)) => RtValue::Bool(x | y),
                    _ => unreachable!(),
                },
                Inst::Xor(a, b) => match (get(&vals, *a), get(&vals, *b)) {
                    (RtValue::I64(x), RtValue::I64(y)) => RtValue::I64(x ^ y),
                    (RtValue::Bool(x), RtValue::Bool(y)) => RtValue::Bool(x ^ y),
                    _ => unreachable!(),
                },
                Inst::Not(a) => match get(&vals, *a) {
                    RtValue::I64(x) => RtValue::I64(!x),
                    RtValue::Bool(x) => RtValue::Bool(!x),
                    _ => unreachable!(),
                },
                Inst::Shl(a, b) => RtValue::I64(
                    get(&vals, *a)
                        .as_i64()
                        .wrapping_shl(get(&vals, *b).as_i64() as u32),
                ),
                Inst::Shr(a, b) => RtValue::I64(
                    (get(&vals, *a).as_i64() as u64).wrapping_shr(get(&vals, *b).as_i64() as u32)
                        as i64,
                ),
                Inst::Sar(a, b) => RtValue::I64(
                    get(&vals, *a)
                        .as_i64()
                        .wrapping_shr(get(&vals, *b).as_i64() as u32),
                ),

                Inst::Cmp { op, lhs, rhs } => {
                    RtValue::Bool(eval_cmp(*op, get(&vals, *lhs), get(&vals, *rhs)))
                }

                Inst::Call {
                    func,
                    args: call_args,
                } => {
                    let a = get(&vals, call_args[0]).as_f64();
                    RtValue::F64(match func {
                        LibFunc::Sin => a.sin(),
                        LibFunc::Cos => a.cos(),
                        LibFunc::Tan => a.tan(),
                        LibFunc::Exp => a.exp(),
                        LibFunc::Log => a.ln(),
                        LibFunc::Pow => a.powf(get(&vals, call_args[1]).as_f64()),
                    })
                }

                Inst::IToF(a) => RtValue::F64(get(&vals, *a).as_i64() as f64),
                Inst::FToI(a) => RtValue::I64(get(&vals, *a).as_f64() as i64),

                Inst::Phi { incoming } => {
                    let from = prev_block.expect("phi in entry block");
                    let (_, val) = incoming
                        .iter()
                        .find(|(b, _)| *b == from)
                        .expect("phi missing operand for predecessor");
                    get(&vals, *val)
                }
            };
            vals[v.0 as usize] = Some(result);
        }

        match &f.blocks[block.0 as usize].term {
            Some(Terminator::Return(v)) => return get(&vals, *v),
            Some(Terminator::Jump(b)) => {
                prev_block = Some(block);
                block = *b;
            }
            Some(Terminator::Branch { cond, then_, else_ }) => {
                prev_block = Some(block);
                // Cond is always bool-typed (Cmp or a bool param) — no float
                // truthiness coercion to get wrong.
                block = if get(&vals, *cond).as_bool() {
                    *then_
                } else {
                    *else_
                };
            }
            None => panic!("block {block:?} has no terminator"),
        }
    }
}

fn eval_cmp(op: CmpOp, l: RtValue, r: RtValue) -> bool {
    match (op, l, r) {
        (CmpOp::Eq, RtValue::F64(x), RtValue::F64(y)) => x == y,
        (CmpOp::Ne, RtValue::F64(x), RtValue::F64(y)) => x != y,
        (CmpOp::Lt, RtValue::F64(x), RtValue::F64(y)) => x < y,
        (CmpOp::Le, RtValue::F64(x), RtValue::F64(y)) => x <= y,
        (CmpOp::Gt, RtValue::F64(x), RtValue::F64(y)) => x > y,
        (CmpOp::Ge, RtValue::F64(x), RtValue::F64(y)) => x >= y,
        (CmpOp::Eq, RtValue::I64(x), RtValue::I64(y)) => x == y,
        (CmpOp::Ne, RtValue::I64(x), RtValue::I64(y)) => x != y,
        (CmpOp::Lt, RtValue::I64(x), RtValue::I64(y)) => x < y,
        (CmpOp::Le, RtValue::I64(x), RtValue::I64(y)) => x <= y,
        (CmpOp::Gt, RtValue::I64(x), RtValue::I64(y)) => x > y,
        (CmpOp::Ge, RtValue::I64(x), RtValue::I64(y)) => x >= y,
        (CmpOp::Eq, RtValue::Bool(x), RtValue::Bool(y)) => x == y,
        (CmpOp::Ne, RtValue::Bool(x), RtValue::Bool(y)) => x != y,
        _ => unreachable!("type checker guarantees comparable operand types"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_syntax::lexer::lex;
    use forge_syntax::parser::parse;
    use forge_syntax::resolve::resolve;
    use forge_syntax::typeck::typecheck;

    fn run(src: &str, args: &[RtValue]) -> RtValue {
        let (tokens, _) = lex(src);
        let (ast, _) = parse(&tokens);
        let typed = typecheck(resolve(ast)).unwrap();
        let f = crate::lower::lower(&typed);
        interpret(&f, args)
    }

    #[test]
    fn sqrt_of_three_four_five_triangle() {
        let r = run(
            "sqrt(x * x + y * y)",
            &[RtValue::F64(3.0), RtValue::F64(4.0)],
        );
        assert_eq!(r, RtValue::F64(5.0));
    }

    #[test]
    fn known_intrinsic_values() {
        assert_eq!(run("abs(x)", &[RtValue::F64(-2.5)]), RtValue::F64(2.5));
        assert_eq!(run("floor(x)", &[RtValue::F64(2.7)]), RtValue::F64(2.0));
        assert_eq!(run("ceil(x)", &[RtValue::F64(2.1)]), RtValue::F64(3.0));
        assert_eq!(
            run("min(x, y)", &[RtValue::F64(3.0), RtValue::F64(5.0)]),
            RtValue::F64(3.0)
        );
        assert_eq!(
            run("max(x, y)", &[RtValue::F64(3.0), RtValue::F64(5.0)]),
            RtValue::F64(5.0)
        );
        assert_eq!(run("sin(x)", &[RtValue::F64(0.0)]), RtValue::F64(0.0));
    }

    #[test]
    fn nan_propagates_through_arithmetic() {
        let r = run("x + 1.0", &[RtValue::F64(f64::NAN)]);
        assert!(matches!(r, RtValue::F64(v) if v.is_nan()));
    }

    #[test]
    fn nan_comparison_takes_else_branch() {
        // All comparisons with NaN are false, so `if NaN > 0.0` must take else.
        let r = run("if x > 0.0 then 1.0 else 2.0", &[RtValue::F64(f64::NAN)]);
        assert_eq!(r, RtValue::F64(2.0));
    }

    #[test]
    fn i64_arithmetic_wraps_on_overflow() {
        let r = run("x + y", &[RtValue::I64(i64::MAX), RtValue::I64(1)]);
        assert_eq!(r, RtValue::I64(i64::MIN));
    }

    #[test]
    fn bitwise_shift_matches_spec_example() {
        // (n * 2654435761) >> 16, evaluated with a concrete n.
        let r = run("(n * 2654435761) >> 16", &[RtValue::I64(12345)]);
        let expected = (12345i64.wrapping_mul(2654435761)) >> 16;
        assert_eq!(r, RtValue::I64(expected));
    }

    // --- Extended self-review probes: IEEE-754 edge cases beyond the 6
    // baseline tests above (negative zero, infinity, i64::MIN, Min/Max NaN
    // semantics, and a combined if/let/intrinsics/i64 pipeline). These exist
    // because Task 13 is the correctness oracle every future JIT backend is
    // differential-tested against — see interpret()'s doc comment. ---

    #[test]
    fn negative_zero_division_matches_ieee754() {
        let r = run("1.0 / x", &[RtValue::F64(-0.0)]);
        match r {
            RtValue::F64(v) => {
                assert!(v.is_infinite() && v.is_sign_negative(), "got {v}");
            }
            _ => panic!("expected f64"),
        }
    }

    #[test]
    fn infinity_propagates_through_arithmetic() {
        let r = run("x / y", &[RtValue::F64(1.0), RtValue::F64(0.0)]);
        assert_eq!(r, RtValue::F64(f64::INFINITY));
        let r2 = run("x - x", &[RtValue::F64(f64::INFINITY)]);
        assert!(matches!(r2, RtValue::F64(v) if v.is_nan()));
    }

    #[test]
    fn i64_min_negation_wraps_to_itself() {
        let r = run("-x", &[RtValue::I64(i64::MIN)]);
        assert_eq!(r, RtValue::I64(i64::MIN));
    }

    #[test]
    fn i64_min_div_by_neg_one_wraps_without_panicking() {
        let r = run("x / y", &[RtValue::I64(i64::MIN), RtValue::I64(-1)]);
        assert_eq!(r, RtValue::I64(i64::MIN));
    }

    #[test]
    fn min_max_return_non_nan_operand_regardless_of_order() {
        // Rust's f64::min/max return the non-NaN operand regardless of
        // argument order -- confirm this holds through the full pipeline
        // for both operand orders.
        let r1 = run("min(x, y)", &[RtValue::F64(f64::NAN), RtValue::F64(1.0)]);
        assert_eq!(r1, RtValue::F64(1.0));
        let r2 = run("min(x, y)", &[RtValue::F64(1.0), RtValue::F64(f64::NAN)]);
        assert_eq!(r2, RtValue::F64(1.0));
        let r3 = run("max(x, y)", &[RtValue::F64(f64::NAN), RtValue::F64(1.0)]);
        assert_eq!(r3, RtValue::F64(1.0));
        let r4 = run("max(x, y)", &[RtValue::F64(1.0), RtValue::F64(f64::NAN)]);
        assert_eq!(r4, RtValue::F64(1.0));
    }

    #[test]
    fn combined_if_let_intrinsics_and_i64_pipeline() {
        // if/let/intrinsics/i64 all together in one expression: `n`'s only
        // uses are bitwise/comparison against int literals, so it's
        // unambiguously inferred I64; `x`/`y` flow into f64 intrinsics.
        // Params are numbered by first source appearance, confirmed via the
        // printed IR: n=index0 (I64), x=index1 (F64), y=index2 (F64).
        let src = "let m = n & 1 in if m == 1 then sqrt(x * x + y * y) else abs(x)";

        // n = 3 is odd -> m == 1 -> then-branch -> sqrt(3^2 + 4^2) = 5.0
        let r_then = run(
            src,
            &[RtValue::I64(3), RtValue::F64(3.0), RtValue::F64(4.0)],
        );
        assert_eq!(r_then, RtValue::F64(5.0));

        // n = 4 is even -> m == 0 -> else-branch -> abs(-6.0) = 6.0
        let r_else = run(
            src,
            &[RtValue::I64(4), RtValue::F64(-6.0), RtValue::F64(4.0)],
        );
        assert_eq!(r_else, RtValue::F64(6.0));
    }
}
