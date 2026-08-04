// crates/forge-opt/src/strength.rs

use forge_ir::*;
use forge_syntax::span::Span;

/// Strength-reduces three i64-only patterns where the second (or, for `Mul`,
/// either) operand is a literal power of two:
///
/// - `x * 2^k -> x << k`
/// - `x / 2^k -> (x + ((x >> 63) & (2^k - 1))) >> k`   (signed rounding fixup)
/// - `x % 2^k -> x - (q << k)`, reusing the corrected `q` from the division
///   rule above
///
/// All three are naturally scoped to i64: the power-of-two check below only
/// matches when the relevant operand is a literal `Inst::ConstI64`, which an
/// f64-typed `Div`/`Mul`/`Rem` never has (its operands are `ConstF64` or
/// something that resolves to f64) — no explicit `Ty` check is needed.
///
/// ## Why division needs a fixup at all
///
/// Truncating division (what this language's `/` is, matching Rust/C) rounds
/// toward zero. Arithmetic shift right rounds toward negative infinity.
/// These agree for non-negative `x` but disagree for negative `x`:
/// `-7 / 2 == -3` (truncating) but `-7 >> 1 == -4` (arithmetic shift, floors
/// toward -inf). The fix (the standard sequence GCC/LLVM emit): add a bias
/// of `2^k - 1` before shifting, but ONLY when `x` is negative — otherwise
/// the bias would incorrectly perturb an already-exact or positive result.
/// `x >> 63` (arithmetic) is exactly that conditional: it's all-1-bits
/// (-1i64) when `x < 0` and all-0-bits (0i64) when `x >= 0`, so ANDing it
/// with `2^k - 1` yields the bias only in the negative case, and zero
/// otherwise. This is provably correct (not just empirically observed) —
/// see the design doc's "Strength reduction" section — but is exactly the
/// kind of arithmetic that's easy to get subtly wrong, which is why the test
/// module below exhaustively checks both signs and `i64::MIN`/`i64::MAX`
/// rather than trusting the derivation alone.
///
/// ## Why remainder is NOT `x & (2^k - 1)`
///
/// That mask computes the EUCLIDEAN remainder (always non-negative), but
/// this language's `%` is truncating (remainder's sign follows the
/// dividend), matching `/`'s rounding. `-7 % 4 == -3` (truncating) but
/// `-7 & 3 == 1` (masking) — these disagree for every negative `x` that
/// isn't an exact multiple. SPEC.md §6.3 documents this exact bug. Instead
/// we compute the remainder from the identity `x == q*d + r` using the
/// ALREADY-CORRECTED `q` from the division rule: `r = x - (q << k)`. This is
/// correct by construction (it's just that defining identity, rearranged),
/// not a separately-derived bit trick, at the cost of a couple more
/// instructions than the naive mask.
pub struct StrengthReduceShifts;

impl crate::Pass for StrengthReduceShifts {
    fn name(&self) -> &'static str {
        "strength-reduce-shifts"
    }
    fn run(&mut self, f: &mut Function) -> bool {
        let mut changed = false;
        for block_idx in 0..f.blocks.len() {
            let block = Block(block_idx as u32);
            let mut pos = 0usize;
            while pos < f.blocks[block.0 as usize].insts.len() {
                let v = f.blocks[block.0 as usize].insts[pos];
                let span = f.spans[v.0 as usize];
                let next_pos = match f.insts[v.0 as usize].clone() {
                    Inst::Mul(a, b) => mul_pow2(f, block, pos, v, a, b, span),
                    Inst::Div(a, b) => div_pow2(f, block, pos, v, a, b, span),
                    Inst::Rem(a, b) => rem_pow2(f, block, pos, v, a, b, span),
                    _ => None,
                };
                if next_pos.is_some() {
                    changed = true;
                }
                pos = next_pos.unwrap_or(pos + 1);
            }
        }
        changed
    }
}

/// `n == 2^k` for some `k` in `1..63`? `k == 0` (`n == 1`) and `n == 0` are
/// deliberately excluded — both are already handled by `simplify.rs`'s
/// `x * 1 -> x` / `x * 0 -> 0` rules, and this pass only needs `k >= 1`.
fn power_of_two_k(n: i64) -> Option<u32> {
    if n <= 1 {
        return None;
    }
    let u = n as u64;
    if u.is_power_of_two() {
        let k = u.trailing_zeros();
        if (1..63).contains(&k) {
            return Some(k);
        }
    }
    None
}

/// `Some(k)` if `v` is a literal `ConstI64` holding an exact power of two in
/// `1..63`. Returning `None` for anything else (including a `ConstF64`) is
/// exactly what keeps this pass naturally scoped to i64 — an f64 `Mul`/`Div`/
/// `Rem` never has a `ConstI64` operand by construction.
fn const_pow2_k(f: &Function, v: Value) -> Option<u32> {
    match f.insts[v.0 as usize] {
        Inst::ConstI64(n) => power_of_two_k(n),
        _ => None,
    }
}

/// Insert a new instruction into `block` at position `pos` (shifting
/// everything at/after `pos` — including the value that used to sit there —
/// one slot to the right) and return its freshly allocated `Value`. Unlike
/// `Vec::push`, this lets us splice helper instructions in BEFORE an
/// existing instruction whose own `Value` identity must be preserved (later
/// code may already reference it), which is exactly what the div/rem
/// sign-fixup sequences need: several new instructions have to be defined,
/// in program order, before the position the original `Div`/`Rem` occupied.
///
/// ## Chaining several calls in a row
///
/// Each call only shifts things right by one slot, so inserting a whole
/// SEQUENCE of `n` new instructions before some original position requires
/// threading a mutable `pos` through `n` calls, incrementing it by one after
/// each: the first call inserts at the original `pos`; every following call
/// must insert at the now-updated `pos` too, since that's where the
/// still-pending original instruction (and everything after it) has been
/// pushed to. After all `n` calls, the original instruction's own new
/// position is `pos` (i.e. `old_pos + n`) — not `old_pos + n + 1`, since
/// `pos` was already advanced past the last inserted instruction; callers
/// that also rewrite the original instruction's own slot afterward, and then
/// need to know where to resume scanning the block, use `pos + 1` for that
/// (skip past the rewritten original too). See `emit_div_bias` for the
/// reference implementation of this pattern (six calls in a row, `pos`
/// incremented after each), and `div_pow2`/`rem_pow2` for how its returned
/// `pos` is subsequently used both to insert further instructions and to
/// compute the caller's resume position.
fn insert_before(
    f: &mut Function,
    block: Block,
    pos: usize,
    inst: Inst,
    ty: Ty,
    span: Span,
) -> Value {
    let v = Value(f.insts.len() as u32);
    f.insts.push(inst);
    f.types.push(ty);
    f.spans.push(span);
    f.blocks[block.0 as usize].insts.insert(pos, v);
    v
}

fn mul_pow2(
    f: &mut Function,
    block: Block,
    pos: usize,
    v: Value,
    a: Value,
    b: Value,
    span: Span,
) -> Option<usize> {
    let (x, k) = if let Some(k) = const_pow2_k(f, b) {
        (a, k)
    } else {
        let k = const_pow2_k(f, a)?;
        (b, k)
    };
    let k_const = insert_before(f, block, pos, Inst::ConstI64(k as i64), Ty::I64, span);
    f.insts[v.0 as usize] = Inst::Shl(x, k_const);
    Some(pos + 2) // the inserted k_const, plus the rewritten original
}

/// Emits the shared sign-fixup prefix used by both `x / 2^k` and `x % 2^k`:
///
/// ```text
/// c63       = 63
/// sign_mask = x >> 63           (arithmetic -- all-1s if x<0, all-0s else)
/// mask      = 2^k - 1
/// bias      = sign_mask & mask
/// biased    = x + bias
/// k_const   = k
/// ```
///
/// Returns `(biased, k_const, pos)` where `pos` is the block position the
/// ORIGINAL instruction (the one at the call site's `pos`) now occupies,
/// after being shifted right by every instruction inserted here.
fn emit_div_bias(
    f: &mut Function,
    block: Block,
    mut pos: usize,
    x: Value,
    k: u32,
    span: Span,
) -> (Value, Value, usize) {
    let c63 = insert_before(f, block, pos, Inst::ConstI64(63), Ty::I64, span);
    pos += 1;
    let sign_mask = insert_before(f, block, pos, Inst::Sar(x, c63), Ty::I64, span);
    pos += 1;
    let mask = (1i64 << k) - 1;
    let mask_const = insert_before(f, block, pos, Inst::ConstI64(mask), Ty::I64, span);
    pos += 1;
    let bias = insert_before(
        f,
        block,
        pos,
        Inst::And(sign_mask, mask_const),
        Ty::I64,
        span,
    );
    pos += 1;
    let biased = insert_before(f, block, pos, Inst::Add(x, bias), Ty::I64, span);
    pos += 1;
    let k_const = insert_before(f, block, pos, Inst::ConstI64(k as i64), Ty::I64, span);
    pos += 1;
    (biased, k_const, pos)
}

fn div_pow2(
    f: &mut Function,
    block: Block,
    pos: usize,
    v: Value,
    a: Value,
    b: Value,
    span: Span,
) -> Option<usize> {
    let k = const_pow2_k(f, b)?;
    let (biased, k_const, pos_after) = emit_div_bias(f, block, pos, a, k, span);
    f.insts[v.0 as usize] = Inst::Sar(biased, k_const);
    Some(pos_after + 1)
}

fn rem_pow2(
    f: &mut Function,
    block: Block,
    pos: usize,
    v: Value,
    a: Value,
    b: Value,
    span: Span,
) -> Option<usize> {
    let k = const_pow2_k(f, b)?;
    let (biased, k_const, mut pos_after) = emit_div_bias(f, block, pos, a, k, span);
    let q = insert_before(
        f,
        block,
        pos_after,
        Inst::Sar(biased, k_const),
        Ty::I64,
        span,
    );
    pos_after += 1;
    let shifted = insert_before(f, block, pos_after, Inst::Shl(q, k_const), Ty::I64, span);
    pos_after += 1;
    f.insts[v.0 as usize] = Inst::Sub(a, shifted);
    Some(pos_after + 1)
}

// ─────────────────────────────────────────────────────────────────────────
// Magic-number division (Granlund & Montgomery, PLDI '94)
// ─────────────────────────────────────────────────────────────────────────
//
// PROMPT.md's "Phase 4 — The Optimizer's Floating-Point Trap" section gives
// a complete, already-correct `magic_signed` (ported ~verbatim below) plus a
// test skeleton for it. It does NOT give `apply_magic`'s body, nor does it
// give the IR-rewriting `Pass` that would use it. Both of those had to be
// worked out here, and the second one turned out not to be buildable as
// described — see the doc comment on `apply_magic` and the one below it for
// why.
//
// ## Scope decision: no IR-rewriting Pass for magic division in this task
//
// The task description offers two options: (a) express the "3-instruction
// magic-multiply sequence" using `i128` arithmetic INSIDE the optimizer pass
// while still emitting only real 64-bit IR instructions, or (b) treat the
// missing widening multiply as a signal that the IR-rewriting pass belongs
// in a later phase (once codegen can actually emit `imul` producing a
// 128-bit result), and scope this task to the MATH only.
//
// Checking `forge_ir::ir::Inst`'s variant list (and `interp.rs`'s evaluator
// for it) settles which: `Inst::Mul(Value, Value)` is defined as a 64-bit
// TRUNCATING multiply -- `interp.rs` evaluates it as
// `x.wrapping_mul(y)`, keeping only the low 64 bits of the mathematical
// product, discarding the high 64 bits entirely. There is no `Mulh`/widening
// variant anywhere in `Inst`, and nothing else in the enum (a pair-of-`Mul`s
// trick, a 128-bit-result marker, etc.) can recover the discarded high bits
// from two 64-bit-result multiplies without already having them. So option
// (a) is not actually available: "use `i128` arithmetic to compute the
// magic multiply's effect as a compile-time-verified rewrite into
// instructions that ARE representable in our 64-bit IR" is not possible for
// the "keep only the high 64 bits of a 128-bit product" step specifically,
// because no combination of our 64-bit-result IR instructions computes that
// value. (Concretely: `apply_magic` below needs `((n as i128) *
// (m.multiplier as i128)) >> 64` -- there is no way to build that number out
// of `Inst::Mul`/`Inst::Shl`/`Inst::Sar`/etc, all of which only ever see or
// produce the low 64 bits of any product.)
//
// That is exactly the situation option (b) describes: a genuine hardware
// primitive (`imul r64,r64` writing a 128-bit result across two registers)
// is missing at the IR level, not just inconvenient to reach. So this task
// implements and exhaustively tests the `magic_signed`/`apply_magic` MATH
// (below), fully verified against `wrapping_div` including `i64::MIN`, and
// leaves the actual IR-rewriting pass as a documented TODO for the phase
// that adds a widening-multiply IR instruction (or, per the task's own
// framing, for Phase 6/7's instruction *selection* to consult directly when
// choosing between `idiv` and `imul`+shift for constant divisors, which
// doesn't need an IR-level rewrite at all -- it can call `magic_signed`
// itself when lowering `Div` to machine code). No `MagicDivision` pass is
// registered in `lib.rs` in this slice; see the comment there.

/// The magic multiplier and shift produced by `magic_signed`, applied by
/// `apply_magic` to replace `n / d` (`d` fixed at compile time) with a
/// multiply and a shift.
///
/// `#[allow(dead_code)]` throughout this section: as explained above, this
/// math has no caller yet in this slice (only the test module exercises
/// it) -- that's the deliberate scope decision, not an oversight.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MagicNumber {
    multiplier: i64,
    shift: u32,
}

/// Computes the magic multiplier/shift pair for dividing by the fixed
/// divisor `d`. Ported verbatim from PROMPT.md's "Phase 4 — The Optimizer's
/// Floating-Point Trap" section (Granlund & Montgomery, PLDI '94).
///
/// `idiv` has 20-40 cycle latency on modern x86 and is NOT pipelined -- the
/// whole divider stalls. The magic-number sequence is 3 instructions, ~5
/// cycles, fully pipelined: a 5-10x win on one instruction (once codegen can
/// emit it -- see the module-level comment above on why that's not this
/// task).
#[allow(dead_code)]
// `q1 = 2 * q1` (etc) instead of `q1 *= 2`: kept exactly as PROMPT.md's
// reference algorithm writes it, per the task's own "port verbatim"
// instruction -- deviating stylistically here would make the port harder to
// diff against its source, for a purely cosmetic clippy preference.
#[allow(clippy::assign_op_pattern)]
fn magic_signed(d: i64) -> MagicNumber {
    assert!(d != 0 && d != 1 && d != -1);
    let ad = d.unsigned_abs();
    let t = (1u64 << 63) + if d > 0 { 0 } else { 1 };
    let anc = t - 1 - t % ad;

    let mut p = 63u32;
    let (mut q1, mut r1) = ((1u64 << 63) / anc, (1u64 << 63) % anc);
    let (mut q2, mut r2) = ((1u64 << 63) / ad, (1u64 << 63) % ad);

    loop {
        p += 1;
        q1 = 2 * q1;
        r1 = 2 * r1;
        if r1 >= anc {
            q1 += 1;
            r1 -= anc;
        }
        q2 = 2 * q2;
        r2 = 2 * r2;
        if r2 >= ad {
            q2 += 1;
            r2 -= ad;
        }
        let delta = ad - r2;
        if !(q1 < delta || (q1 == delta && r1 == 0)) {
            break;
        }
    }

    let mut m = (q2 + 1) as i64;
    if d < 0 {
        m = -m;
    }
    MagicNumber {
        multiplier: m,
        shift: p - 64,
    }
}

/// Applies `m` (as produced by `magic_signed(d)`) to `n`, reproducing
/// `n.wrapping_div(d)` exactly. Takes `d` itself as well as `m` -- see below
/// for why that's not optional, despite PROMPT.md's test snippet writing
/// the call as the 2-argument `apply_magic(n, &m)` (that snippet's own
/// `apply_magic` body isn't shown anywhere in PROMPT.md; only its use inside
/// the test is).
///
/// This is the piece PROMPT.md's excerpt doesn't spell out, and it turned
/// out NOT to be a plain transcription: an initial 2-argument version
/// (`apply_magic(n, &m)`, no `d`) FAILED the exhaustive test below for real
/// -- concretely, `9223372036854775807 / 100` -- which is exactly the kind
/// of "the reference material's own example doesn't actually check out"
/// trap this project has hit before (a printer test whose own assertion
/// didn't match its example; a builder algorithm needing real understanding
/// instead of transcription). Root cause, worked out from that failure:
///
/// `magic_signed`'s loop repeatedly DOUBLES `q2` (`q2 = 2 * q2 ...`, several
/// times) before returning `multiplier = (q2 + 1) as i64`. So `q2 + 1` is
/// NOT bounded by the loop's small starting value (`(1u64 << 63) / ad`) the
/// way it looks at a glance -- it grows past `1u64 << 63` for perfectly
/// ordinary divisors (`d = 100` above is one). When that happens, casting to
/// `i64` wraps the stored `multiplier`'s SIGN relative to `d`'s own sign:
/// positive `d` can still produce a negative `multiplier`, and negative `d`
/// a positive one. Recovering the correct high-64-bits-of-the-product value
/// from the wrapped `multiplier` alone is provably impossible in general:
/// `multiplier < 0` is consistent with BOTH "d>0 and the true magic number
/// overflowed i64" (needs a `+n` correction) and "d<0 and it didn't overflow"
/// (needs no correction) -- these are indistinguishable from `(n, m)` alone,
/// confirmed by hand-deriving both cases against the divisor list. A real
/// compiler doesn't hit this ambiguity because `d` is a compile-time literal
/// already in scope wherever the multiply-by-magic-number sequence gets
/// emitted -- so this signature just makes that same fact explicit here too
/// (Hacker's Delight §10-4's reference "compiled code": `if d>0 && M<0:
/// q+=n`, `if d<0 && M>0: q-=n`).
///
/// The final `+= 1`-if-negative step is unrelated to the above and IS exactly
/// as PROMPT.md's algorithm implies: `MULHS(n, M) >> s` rounds toward
/// negative infinity (it's an arithmetic shift), but truncating division
/// rounds toward zero -- they disagree exactly when the true quotient is
/// negative and inexact, the same reasoning as the power-of-two sign-fixup
/// above (extracting the sign bit and adding it back is Hacker's Delight's
/// `q + ((unsigned)q >> 31)`, widened to 64 bits here).
#[allow(dead_code)]
fn apply_magic(n: i64, d: i64, m: &MagicNumber) -> i64 {
    let mut q = (((n as i128) * (m.multiplier as i128)) >> 64) as i64;
    if d > 0 && m.multiplier < 0 {
        q = q.wrapping_add(n);
    }
    if d < 0 && m.multiplier > 0 {
        q = q.wrapping_sub(n);
    }
    if m.shift > 0 {
        q >>= m.shift;
    }
    q.wrapping_add(((q as u64) >> 63) as i64)
}

// ─────────────────────────────────────────────────────────────────────────
// `pow()` strength reduction -- investigated, NOT implemented
// ─────────────────────────────────────────────────────────────────────────
//
// The task asks for three rules -- `pow(x,2.0) -> x*x`, `pow(x,0.5) ->
// sqrt(x)`, `pow(x,-1.0) -> 1.0/x` -- but ONLY after empirically confirming
// each is bit-for-bit identical to the platform's real `pow` first, dropping
// any that aren't. All three empirical checks are implemented as tests below
// (`pow_x_2_vs_x_times_x_diverges_on_this_platform`,
// `pow_x_neg1_vs_one_over_x_diverges_on_this_platform`,
// `pow_x_half_vs_sqrt_diverges_on_this_platform`) and, on this platform,
// **all three diverge from the real `pow`** -- so NONE of the three rules
// are implemented here. That outcome deserves its own explanation, because
// getting to it took an extra round of debugging the TEST itself, not just
// the rules:
//
// A first attempt compared against `x.powf(2.0)` / `x.powf(-1.0)` as the
// "unmodified pow" oracle and got a bit-exact PASS under `cargo test
// --release` but a real, substantial FAILURE under plain `cargo test`
// (debug) -- for ordinary inputs like `x = 978892.85`, not just exotic edge
// cases. The two build profiles disagreeing on a supposedly platform-level
// empirical fact was the tell that the test itself was unsound: at higher
// optimization levels, LLVM's own libcall-simplification recognizes
// `f64::powf(x, 2.0)`/`f64::powf(x, -1.0)` (and, it turns out, even a
// hand-written `extern "C" fn pow(...)` call, purely by matching the
// function's NAME against its table of known libm functions) and rewrites
// it to `fmul`/`fdiv` itself, before this test ever runs. Comparing against
// that oracle under `--release` was circular -- "does x*x equal x*x" -- and
// told us nothing about the platform's actual `pow`.
//
// The fix was to route the oracle call through an indirect function pointer
// wrapped in `std::hint::black_box` (see `libm_pow` in the test module),
// which hides the callee's identity from LLVM's optimizer and forces a
// genuine call to the dynamically-linked `pow` symbol. With THAT oracle,
// debug and release builds agree, and the answer is unambiguous: on this
// platform, `pow(x, 2.0)` and `x*x` differ by 1 ULP for a large fraction of
// ordinary-magnitude random inputs (e.g. `pow(978892.85, 2.0) ==
// 958231212390.852` vs `978892.85 * 978892.85 == 958231212390.8519`), and
// likewise for `pow(x, -1.0)` vs `1.0/x`. `pow(x, 0.5)` vs `sqrt(x)` was
// already known to diverge at `-0.0` and `-Inf` even under the flawed
// oracle. So this system's libm evidently does NOT special-case these
// exponents to be bit-identical to the "obvious" arithmetic equivalent --
// exactly the kind of platform-specific numerical trap this task's mandatory
// empirical-verification step exists to catch, rather than trusting the
// "obviously true" algebraic identity.
//
// Net result: no `PowStrengthReduce` pass exists in this slice. If a future
// platform's libm (or a `fast_math`-gated version of this pass, where a
// 1-ULP difference would be an accepted tradeoff -- unlike here, where none
// of the passes in this crate are fast-math-gated yet) makes some of these
// rules attractive again, the empirical tests below are exactly what should
// be re-run first.

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

    /// `n * 8` / `n / 8` / `n % 8` alone do NOT force `n: I64` -- a sibling
    /// I64 literal doesn't constrain the OTHER Add/Sub/Mul/Div/Rem operand
    /// (see `typeck::Ctx::infer_expect`'s `Add | Sub | Mul | Div | Rem =>
    /// expected` passthrough with `expected == None` at the top level), so
    /// `n` defaults to F64 and the literal gets widened via `IToF` instead.
    /// Confirmed empirically (see `bare_n_without_forcing_infers_as_f64`
    /// below) -- same issue Task 3 (`AlgebraicSimplify`) hit. `& -1` forces
    /// I64 top-down through the pass-through inference without disturbing
    /// which SSA `Value` `n` resolves to.
    fn lowered_i64(src: &str) -> Function {
        lowered(&format!("({src}) & -1"))
    }

    #[test]
    fn bare_n_without_forcing_infers_as_f64() {
        // Confirms the type-inference gotcha before trusting any other test
        // in this module: `n * 8` unforced really does type `n` as F64 (its
        // sole param comes back F64), so feeding it an `RtValue::I64` would
        // panic in `interpret` -- exactly the trap the `lowered_i64` helper
        // above exists to avoid.
        let f = lowered("n * 8");
        assert_eq!(f.params, vec![("n".to_string(), Ty::F64)]);

        let forced = lowered_i64("n * 8");
        assert_eq!(forced.params, vec![("n".to_string(), Ty::I64)]);
    }

    fn run_both(src: &str, n: i64) -> (RtValue, RtValue) {
        let unreduced = lowered_i64(src);
        let expected = interpret(&unreduced, &[RtValue::I64(n)]);
        let mut reduced = lowered_i64(src);
        StrengthReduceShifts.run(&mut reduced);
        let actual = interpret(&reduced, &[RtValue::I64(n)]);
        (expected, actual)
    }

    #[test]
    fn mul_by_power_of_two_matches_for_positive_and_negative_n() {
        for n in [0i64, 1, -1, 7, -7, i64::MIN, i64::MAX] {
            let (e, a) = run_both("n * 8", n);
            assert_eq!(e, a, "n={n}");
        }
    }

    #[test]
    fn mul_fires_and_becomes_a_shift() {
        let mut f = lowered_i64("n * 8");
        let changed = StrengthReduceShifts.run(&mut f);
        assert!(changed);
        assert!(f.insts.iter().any(|i| matches!(i, Inst::Shl(..))));
        assert!(!f.insts.iter().any(|i| matches!(i, Inst::Mul(..))));
    }

    #[test]
    fn mul_fires_with_constant_on_the_left_too() {
        let mut f = lowered_i64("8 * n");
        let changed = StrengthReduceShifts.run(&mut f);
        assert!(
            changed,
            "constant-on-the-left Mul should also strength-reduce"
        );
        assert!(f.insts.iter().any(|i| matches!(i, Inst::Shl(..))));
    }

    #[test]
    fn div_by_power_of_two_matches_wrapping_div_including_negative_dividends() {
        // THE test that catches a naive (unfixed) shift implementation:
        // truncating division and arithmetic shift disagree here.
        for n in [
            0i64,
            1,
            -1,
            7,
            -7,
            8,
            -8,
            15,
            -15,
            i64::MIN,
            i64::MAX,
            -100_000,
        ] {
            let (e, a) = run_both("n / 8", n);
            assert_eq!(e, a, "n={n}: expected {e:?}, got {a:?}");
        }
    }

    #[test]
    fn rem_by_power_of_two_matches_wrapping_rem_including_negative_dividends() {
        // THE test that catches the SPEC.md-documented masking bug: a naive
        // `x & (2^k - 1)` implementation would fail exactly here.
        for n in [
            0i64,
            1,
            -1,
            7,
            -7,
            8,
            -8,
            15,
            -15,
            i64::MIN,
            i64::MAX,
            -100_000,
        ] {
            let (e, a) = run_both("n % 8", n);
            assert_eq!(e, a, "n={n}: expected {e:?}, got {a:?}");
        }
    }

    #[test]
    fn div_fires_and_removes_the_original_div() {
        let mut f = lowered_i64("n / 8");
        let changed = StrengthReduceShifts.run(&mut f);
        assert!(changed);
        assert!(!f.insts.iter().any(|i| matches!(i, Inst::Div(..))));
        assert!(f.insts.iter().any(|i| matches!(i, Inst::Sar(..))));
    }

    #[test]
    fn rem_fires_and_removes_the_original_rem() {
        let mut f = lowered_i64("n % 8");
        let changed = StrengthReduceShifts.run(&mut f);
        assert!(changed);
        assert!(!f.insts.iter().any(|i| matches!(i, Inst::Rem(..))));
        assert!(f.insts.iter().any(|i| matches!(i, Inst::Sub(..))));
    }

    #[test]
    fn does_not_fire_on_non_power_of_two_divisor() {
        let mut f = lowered_i64("n / 7");
        let changed = StrengthReduceShifts.run(&mut f);
        assert!(
            !changed,
            "7 is not a power of 2 -- magic division handles this, not shifts (a later task)"
        );
    }

    #[test]
    fn does_not_fire_on_non_power_of_two_divisor_for_rem() {
        let mut f = lowered_i64("n % 7");
        let changed = StrengthReduceShifts.run(&mut f);
        assert!(!changed);
    }

    #[test]
    fn does_not_fire_on_f64_division() {
        // f64 Div/Rem/Mul never have a ConstI64 operand, so this pass must
        // decline naturally -- no explicit Ty check needed. Regression test
        // for that "falls out naturally" claim.
        let mut f = lowered("x / 8.0");
        let changed = StrengthReduceShifts.run(&mut f);
        assert!(
            !changed,
            "f64 division must never be strength-reduced by this pass"
        );
    }

    #[test]
    fn does_not_fire_on_c_equal_to_one_or_zero() {
        // Already handled by simplify.rs; this pass should leave them alone
        // (power_of_two_k excludes n<=1 by construction).
        let mut f1 = lowered_i64("n * 1");
        assert!(!StrengthReduceShifts.run(&mut f1));
        let mut f0 = lowered_i64("n * 0");
        assert!(!StrengthReduceShifts.run(&mut f0));
    }

    #[test]
    fn verifies_after_rewrite() {
        // The rewritten IR must still pass `verify()` -- but note `verify()`
        // only checks BLOCK-granularity dominance (see verify.rs's own
        // doc comment), not same-block instruction order, so this test alone
        // would NOT catch an `insert_before` position bug that puts a helper
        // instruction after the value that uses it. Ordering itself is
        // caught by the interpret()-based tests above (they panic on
        // "undefined value" if a use precedes its def in `block.insts`),
        // not this one -- this test is only confirming dominance stays
        // intact, a real but separate property.
        for src in ["n * 8", "n / 8", "n % 8"] {
            let mut f = lowered_i64(src);
            StrengthReduceShifts.run(&mut f);
            assert!(
                forge_ir::verify::verify(&f).is_ok(),
                "verify failed for {src}"
            );
        }
    }

    #[test]
    fn magic_division_is_exact() {
        // Ported from PROMPT.md's "Phase 4 — The Optimizer's Floating-Point
        // Trap" section almost verbatim (that section names `StdRng`; this
        // uses `rand_chacha::ChaCha8Rng` seeded the same way, since `StdRng`
        // isn't guaranteed-reproducible across rand versions and this crate
        // doesn't otherwise depend on `rand`'s default backend). Must be
        // exact for EVERY input, including `i64::MIN` -- the case that
        // breaks naive implementations, because `|i64::MIN|` is not
        // representable as an `i64`.
        use rand::{Rng, SeedableRng};
        for d in [3i64, 5, 7, 10, 100, 1000, -3, -7, -100] {
            let m = magic_signed(d);
            for n in [0i64, 1, -1, 42, -42, i64::MAX, i64::MIN, i64::MAX - 1] {
                assert_eq!(
                    apply_magic(n, d, &m),
                    n.wrapping_div(d),
                    "magic division wrong for {n} / {d}"
                );
            }
            let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(0xC0FFEE);
            for _ in 0..100_000 {
                let n: i64 = rng.random();
                assert_eq!(
                    apply_magic(n, d, &m),
                    n.wrapping_div(d),
                    "magic division wrong for {n} / {d}"
                );
            }
        }
    }

    // Raw FFI declaration for the platform's real libm `pow`, deliberately
    // NOT `f64::powf`. This matters a great deal: an earlier version of this
    // test used `x.powf(2.0)`/`x.powf(-1.0)` as the "unmodified pow" oracle
    // and got DIFFERENT verdicts under `cargo test` (debug) vs
    // `cargo test --release` -- because at higher optimization levels LLVM
    // recognizes `f64::powf`'s underlying `llvm.pow.f64` intrinsic called
    // with a compile-time-constant exponent of `2.0`/`-1.0` and rewrites it
    // to `fmul`/`fdiv` ITSELF, internally, before this test ever runs.
    //
    // Switching to a raw `extern "C" fn pow(...)` call was NOT enough to fix
    // this by itself, and re-confirmed the same debug/release disagreement:
    // LLVM's `LibCallSimplifier` recognizes calls to a function literally
    // NAMED `pow`/`sqrt`/etc as "the standard libm function" by name+
    // signature and applies the same algebraic rewrite, independent of
    // whether the call came from `f64::powf` or a hand-written `extern "C"`
    // declaration. The only way to get an oracle immune to this is to hide
    // the call behind an indirect function pointer that `std::hint::
    // black_box` marks opaque -- LLVM can no longer identify the callee by
    // name once the call is indirect, so it can't apply the libcall
    // simplification, and this reports the platform's actual libm behavior
    // identically regardless of optimization level (confirmed below by
    // running under both `cargo test` and `cargo test --release`). That's
    // also the more meaningful oracle for this project specifically: a real
    // JIT backend lowers `LibFunc::Pow` to an actual call instruction
    // targeting libm, not something further optimized away, so this is what
    // production code will really call.
    extern "C" {
        fn pow(x: f64, y: f64) -> f64;
    }
    fn libm_pow(x: f64, y: f64) -> f64 {
        type PowFn = unsafe extern "C" fn(f64, f64) -> f64;
        let f: PowFn = std::hint::black_box(pow as PowFn);
        unsafe { f(x, y) }
    }

    /// Shared driver for the three empirical `pow`-rule checks below:
    /// compares `actual` (what the rewrite would produce) against `oracle`
    /// (unmodified `pow`, always `libm_pow` above -- see its doc comment for
    /// why NOT `f64::powf`) across a fixed set of special values PLUS random
    /// samples covering the full range of `f64` bit patterns (not just
    /// "ordinary" magnitudes -- a uniform `-1e6..1e6` sampler as suggested
    /// by the task alone would essentially never hit `NaN`/`Inf`/subnormals/
    /// signed zero, exactly the values most likely to disagree). Returns
    /// the mismatches found (empty means bit-exact across the whole sample).
    fn pow_rule_mismatches(
        actual: impl Fn(f64) -> f64,
        oracle: impl Fn(f64) -> f64,
    ) -> Vec<(f64, f64, f64)> {
        use rand::{Rng, SeedableRng};
        let specials: [f64; 15] = [
            0.0,
            -0.0,
            1.0,
            -1.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MIN_POSITIVE,
            f64::MIN_POSITIVE / 2.0, // subnormal
            f64::MAX,
            f64::MIN,
            2.0,
            -2.0,
            0.5,
            -0.5,
        ];
        let mut mismatches = Vec::new();
        let check = |x: f64, mismatches: &mut Vec<(f64, f64, f64)>| {
            let a = actual(x);
            let o = oracle(x);
            let same = if o.is_nan() {
                a.is_nan()
            } else {
                a.to_bits() == o.to_bits()
            };
            if !same {
                mismatches.push((x, o, a));
            }
        };
        for &x in &specials {
            check(x, &mut mismatches);
        }
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(0xC0FFEE);
        for _ in 0..100_000 {
            let x: f64 = rng.random_range(-1e6..1e6);
            check(x, &mut mismatches);
        }
        // Also hammer the FULL f64 bit-pattern space (catches NaN payload
        // quirks, subnormals, and extreme magnitudes a magnitude-bounded
        // sampler would essentially never produce).
        for _ in 0..100_000 {
            let bits: u64 = rng.random();
            check(f64::from_bits(bits), &mut mismatches);
        }
        mismatches
    }

    #[test]
    fn pow_x_2_vs_x_times_x_diverges_on_this_platform() {
        // This is expected to FIND a mismatch -- i.e. the candidate rule
        // `pow(x,2.0) -> x*x` is NOT implemented, and this test is the
        // actual evidence for why, not a restatement of the claim. See the
        // module comment above for the full story, including why an early
        // version of this exact check gave the opposite (wrong) answer.
        let mismatches = pow_rule_mismatches(|x| x * x, |x| libm_pow(x, 2.0));
        assert!(
            !mismatches.is_empty(),
            "pow(x,2.0) and x*x agreed everywhere in this run -- if this holds up on a clean \
             re-run, reconsider implementing the pow(x,2.0)->x*x rule"
        );
    }

    #[test]
    fn pow_x_neg1_vs_one_over_x_diverges_on_this_platform() {
        let mismatches = pow_rule_mismatches(|x| 1.0 / x, |x| libm_pow(x, -1.0));
        assert!(
            !mismatches.is_empty(),
            "pow(x,-1.0) and 1.0/x agreed everywhere in this run -- if this holds up on a \
             clean re-run, reconsider implementing the pow(x,-1.0)->1.0/x rule"
        );
    }

    #[test]
    fn pow_x_half_vs_sqrt_diverges_on_this_platform() {
        let mismatches = pow_rule_mismatches(|x| x.sqrt(), |x| libm_pow(x, 0.5));
        assert!(
            !mismatches.is_empty(),
            "pow(x,0.5) and sqrt(x) agreed everywhere in this run -- if this holds up on a \
             clean re-run, reconsider implementing the pow(x,0.5)->sqrt(x) rule"
        );
        // The two divergences already known from the (flawed-oracle) first
        // pass at this check, still present with the corrected oracle:
        // -0.0's sign, and -Inf's NaN-ness.
        assert!(
            mismatches
                .iter()
                .any(|(x, ..)| *x == 0.0 && x.is_sign_negative()),
            "expected -0.0 to be among the mismatches: {mismatches:?}"
        );
        assert!(
            mismatches
                .iter()
                .any(|(x, ..)| x.is_infinite() && x.is_sign_negative()),
            "expected -Inf to be among the mismatches: {mismatches:?}"
        );
    }
}
