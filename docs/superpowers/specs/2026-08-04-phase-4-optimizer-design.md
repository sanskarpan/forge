# Design: forge Phase 4 — Optimizer (safe passes only)

**Status:** Approved for planning
**Scope:** CHECKLIST.md Phase 4 — constant folding, algebraic simplification, strength reduction (incl. magic-number division), GVN/CSE, DCE, reassociation, driver/pass infrastructure. All bit-exact (no `--fast-math`).
**Out of scope:** fast-math mode (`sqrt(x*x)→abs(x)`, FMA contraction, `exp(x)*exp(y)→exp(x+y)`) — deferred to a follow-up slice once the safe optimizer is solid. LICM (no array/loop mode exists yet — N/A). Per-pass statistics UI (🟡, no CLI/workbench to show them yet).

This slice adds a real optimizer to `forge-opt` (currently a stub), operating on the `forge-ir` types and validated against the `interp::interpret()` oracle built in Phase 0-3. The core correctness property this slice must establish and keep passing forever after: **`-O0 == -O2`, bit-exact, for every expression** — an optimizer that changes what a program computes is a compiler bug, full stop.

## Two real bugs found in SPEC.md's own optimization tables (fixed as part of this design)

1. **`x + 0 → x` is not universally valid for f64.** IEEE-754 defines `(-0.0) + (+0.0) = +0.0`. So if `x` is `-0.0` and the literal being added is `+0.0`, the "simplification" flips the sign of zero — a real, silent answer change. The only unconditionally-safe direction is `x + (-0.0) → x` (adding *negative* zero never changes sign, for any `x`). `x - 0 → x` has no such problem (subtracting `+0.0` is definitionally the same as adding `-0.0`) — the asymmetry between `+` and `-` here is real, not a typo.
2. **`x % 2^k → x & (2^k-1)` is only correct for non-negative `x`.** Our `%` (`Inst::Rem`, matching Rust's `wrapping_rem`) is *truncating* — remainder sign follows the dividend. `x & (2^k-1)` computes the *Euclidean* remainder, always non-negative. These disagree for negative `x` (`-7 % 4 == -3`, but `-7 & 3 == 1`). The general signed case needs the same rounding-fixup machinery as `x / 2^k`.

Both are now documented in SPEC.md §6.2/§6.3 with the corrected rule. This slice implements the corrected versions, not the literal (buggy) ones.

## Architecture

New real content in `crates/forge-opt/` (currently a one-line stub):

```
forge-opt/src/
├── lib.rs        # Pass trait, driver
├── fold.rs       # constant folding
├── simplify.rs   # algebraic simplification (rule table w/ Validity)
├── strength.rs   # x*2^k, x/2^k, x%2^k, magic-number division
├── gvn.rs        # dominator-tree-scoped GVN/CSE
├── dce.rs        # dead code elimination
└── reassoc.rs    # reassociation (dependency-depth reduction)
```

### Pass trait and driver

```rust
pub trait Pass {
    fn name(&self) -> &'static str;
    fn run(&mut self, f: &mut Function) -> bool; // true if it changed anything
}

pub fn optimize(f: &mut Function) {
    let mut passes: Vec<Box<dyn Pass>> = vec![
        Box::new(ConstFold), Box::new(AlgebraicSimplify), Box::new(StrengthReduce),
        Box::new(Gvn), Box::new(Reassociate), Box::new(Dce),
    ];
    for round in 0..10 {
        let mut changed = false;
        for pass in &mut passes {
            let pass_changed = pass.run(f);
            changed |= pass_changed;
            #[cfg(debug_assertions)]
            if let Err(e) = forge_ir::verify::verify(f) {
                panic!("verifier failed after pass '{}' (round {round}): {e}", pass.name());
            }
        }
        if !changed { break; }
    }
}
```

Per CHECKLIST's own instruction ("run the verifier after every pass in debug builds — catches optimizer bugs at the pass that caused them, not three passes later"), the driver runs `verify()` after *every single pass*, not just per round. This is cheap (the IR is small) and is exactly the safety net that makes bugs in this phase find-able instead of "wrong answer three passes later."

No separate copy-propagation pass. This IR has no `Copy` instruction — the SSA builder's trivial-φ removal and this slice's own simplify/GVN passes all redirect uses directly via `replace_all_uses` rather than inserting an intermediate copy to later eliminate. A copy-propagation pass would have structurally nothing to do. (Flagging this explicitly since it's a literal deviation from the checklist's phrasing, not an oversight.)

No `x*3/5/9 → lea` forms. This is a codegen/instruction-selection concern — there's no encoder yet to benefit from choosing a `lea` over separate shift+add, and doing the IR rewrite now doesn't simplify anything (it's a *different* shape, not a simpler one, until an encoder can pattern-match it into one instruction). Deferred to Phase 6/7.

### `Terminator` operands and GVN/DCE

Task 11's code review flagged that `uses_of`/`replace_in_inst` (the `Inst`-level exhaustiveness guard) don't cover `Terminator::Return`/`Branch`'s operands. This phase is exactly where that gap becomes load-bearing: GVN must redirect a `Return`'s or `Branch`'s operand if the value it referenced got CSE'd away, and DCE's liveness must seed from those same operands. This slice adds the missing piece — a `replace_value_everywhere(f: &mut Function, old: Value, new: Value)` helper in `forge-ir` (not `forge-opt`, since it's a general IR operation) that covers `Inst` operands (via existing `replace_in_inst`) *and* `Terminator::Return`/`Branch` operands, used by both GVN and any future pass that needs it.

## Constant folding (`fold.rs`)

Both operands must be *literal* constant instructions (`ConstF64`/`ConstI64`/`ConstBool`) — this is different from, and simpler than, algebraic simplification below. When both operands are literal, computing the result at compile time is **always safe**, for every op, including the ones that produce NaN/Inf — folding doesn't change what the program computes, it just computes it earlier. `0.0 / 0.0 → NaN` is exactly as correct at compile time as at runtime.

Deliberate design choice: `fold.rs` does **not** reuse `interp.rs`'s per-instruction arithmetic (they're structurally similar but operate on different types — `RtValue` for the interpreter vs. constructing a new `Inst` here). Rather than risk destabilizing the already-heavily-verified interpreter with a refactor, this slice adds its own small, self-contained per-op folding logic (wrapping i64, IEEE f64, matching the interpreter's semantics), and closes the "did I really match `interp.rs`" gap with a property test: **for every foldable expression, `interpret(unfolded) == interpret(folded)`**, generated across the same random/edge-case value corpus Task 13 used. This gives the correctness guarantee via testing rather than shared code — a reasonable trade given `interp.rs`'s existing test investment shouldn't be touched to serve this task.

## Algebraic simplification (`simplify.rs`)

`Validity::Always | IntOnly` (no `FastMathOnly` rules implemented this slice — the enum has the variant for forward compatibility, but nothing constructs it yet). One operand is a literal identity/absorbing element, or the two operands are the same `Value` (structural equality, not just "would evaluate the same") — NOT both-literal (that's constant folding's job).

Correctness-verified rule set for this slice:

| Rule | Validity | Why |
|---|---|---|
| `x + (-0.0) → x` | Always (f64); trivially always for i64 (`x + 0 → x`, no signed zero) | See "two real bugs" above |
| `x - 0 → x` | Always | Subtracting `+0.0` ≡ adding `-0.0`; always safe |
| `x * 1 → x` | Always | Exact identity in both domains |
| `x / 1 → x` | Always | Exact identity in both domains (i64: no overflow risk, `MIN/1=MIN`) |
| `-(-x) → x` | Always | Negation is involutive and exact in both wrapping-i64 (`MIN` maps to itself, so double-negating `MIN` returns `MIN`) and IEEE-f64 (sign-bit flip, exact) |
| `x * 0 → 0` | IntOnly | f64: `NaN*0=NaN`, `Inf*0=NaN` |
| `x - x → 0` | IntOnly | f64: `NaN-NaN=NaN` |
| `x / x → 1` | IntOnly | f64: `0/0=NaN`, `NaN/NaN=NaN` |
| `x & x → x` | Always for i64/bool (bitwise ops never reach f64 by construction — type checker enforces this, so there's no "which domain" ambiguity to worry about here) |
| `x ^ x → 0` | Always for i64/bool, same reasoning |

Commutative canonicalization (lower `Value` index operand first) is shared infrastructure used by both this pass (to match `0.0 + x` as well as `x + 0.0`) and GVN below.

## Strength reduction (`strength.rs`)

- `x * 2^k → x << k` — **i64 only** (f64 has no integer-shift representation of scaling by a power of 2; the type checker guarantees `Shl`'s operands are i64, so this rule is naturally scoped by construction — no explicit type check needed beyond "is this a `Mul` with an i64-typed power-of-2 constant operand").
- `x / 2^k → x >> k` with the classic signed rounding fixup: truncating division rounds toward zero, but arithmetic shift rounds toward negative infinity — these differ for negative dividends (`-7/2 = -3` truncating, but `-7>>1 = -4`). Correct sequence (matching what GCC/LLVM emit): `q = (x + ((x >> 63) & (2^k - 1))) >> k`, where `x >> 63` is all-1s if `x < 0` else all-0s, giving a `(2^k-1)` bias only when `x` is negative before the shift.
- `x % 2^k → x - (q << k)`, reusing the corrected `q` from the division rule above — provably correct by construction from `q` (the defining relationship of truncating division: `x = q*d + r`), rather than a separately-derived bit-trick. This costs 2 more instructions than the naive `x & mask`, deliberately traded for provable correctness (see the SPEC.md fix above).
- **Magic-number division** (Granlund-Montgomery) for `x / C` where `C` is a non-power-of-2 constant — the flagship optimization. PROMPT.md's `magic_signed`/`apply_magic` implementation is already correct and has its own exhaustive/property test (including `i64::MIN`) — port it directly into `strength.rs`, don't reimplement from scratch.
- `pow(x, 2) → x * x`, `pow(x, 0.5) → sqrt(x)`, `pow(x, -1) → 1 / x` when the exponent argument is a literal `ConstF64` bit-exactly matching `2.0`/`0.5`/`-1.0`. **Correctness risk to verify empirically, not assume**: Rust's `f64::powf` is not guaranteed by IEEE-754 or the language to be bit-identical to the direct-op replacement in every case (e.g. `x.powf(2.0)` vs `x*x` — most libm implementations special-case small integer/half exponents for accuracy, but this is a quality-of-implementation guarantee, not a spec guarantee). The implementing task must empirically differential-test this (compare `interpret()` before/after the rewrite across a large random+edge-case sample on this actual platform) before trusting it — if it doesn't hold bit-exact, the rule must be dropped or the differential test's tolerance question escalated, not silently accepted.

## GVN/CSE (`gvn.rs`)

Dominator-tree-scoped hash-consing (the standard LLVM-style approach), not a flat whole-function table — a flat table would incorrectly CSE two structurally-identical instructions in non-dominating sibling blocks (e.g. the `then` and `else` arms of an `if`), which is unsound (the `then`-block instruction doesn't dominate a use in `else`).

Walk the dominator tree in preorder (build `Block → Vec<Block>` children from the existing `idom` array from `forge_ir::dominance`). Maintain a `HashMap<Inst, Value>` scoped per dominator subtree: insert entries for the current block's instructions, recurse into dominator-tree children, then **remove** this block's own entries before returning to the parent — a save/restore discipline identical in spirit to the type checker's scope handling from Phase 0-3, just over a hash map instead of a `Vec` stack.

Key: the canonicalized `Inst` itself (commutative-operand-sorted for `Add/Mul/And/Or/Xor/Cmp{Eq,Ne}`; order preserved for everything else). Requires adding `#[derive(PartialEq, Eq, Hash)]` to `Inst`, and (transitively) to `Ty`/`CmpOp`/`LibFunc`, which currently only derive `PartialEq, Eq` — a small, purely additive change (no behavior change to any existing code). Every `Inst` variant is CSE-safe to key on structurally: every instruction in this IR is side-effect-free (even `Call` — libm functions are pure), so there's no variant that needs excluding from the hash-cons table.

On a hit, `replace_value_everywhere(f, this_value, existing_value)` — the earlier value's users get redirected; the now-redundant instruction becomes dead and is swept by DCE in the same fixed-point round (no separate cleanup needed here).

## Dead code elimination (`dce.rs`)

Worklist-based reachability, seeded from every block's terminator operand (`Return`'s value, `Branch`'s condition — `Jump` has none), transitively following `uses_of` backward through the def-use graph. Anything not reached is dead; sweep by filtering `block.insts` (the underlying `f.insts` Vec keeps the now-unreferenced entries — consistent with how the SSA builder already leaves dead trivial-phis in place after `replace_all_uses`, no renumbering needed).

## Reassociation (`reassoc.rs`, 🟡 but included — cheap and directly testable)

**Scoped to `i64` chains only — a real correctness constraint this design found, not present in the original CHECKLIST wording.** Associativity itself is not generally valid for f64 arithmetic: `(a+b)+c` and `a+(b+c)` can differ in the last few ULPs for finite floats due to rounding, which is exactly why real compilers (GCC/Clang) gate reassociation behind `--ffast-math`. Wrapping i64 addition/multiplication, by contrast, is a true associative ring operation (mod 2^64, no rounding) — genuinely safe to reassociate bit-exact. Since this slice is explicitly bit-exact/no-fast-math, reassociation fires only on `i64` `Add`/`Mul` chains; f64 reassociation is deferred alongside fast-math, flagged the same way as the two SPEC.md bugs above.

Rebalances `((a+b)+c)+d` into `(a+b)+(c+d)`, reducing dependency-chain depth from 3 to 2 — measurably faster on a superscalar CPU once codegen exists (not measurable yet without an encoder, but the IR-shape depth-reduction test from CHECKLIST is meaningful today).

## Testing plan

- **The core invariant**: `-O0 == -O2`, bit-exact, across a differential corpus reusing Task 13's `arb_interesting_f64`-style value generation and a small `arb_expression` generator (a lighter version of what Phase 11 will eventually build in full — build only what this slice needs, not the full differential-testing infrastructure).
- `x*0` folds for i64, does NOT fold for f64 (both directions tested).
- `x + 0.0` does NOT get simplified when the *variable* could be `-0.0` (test: interpret both the "simplified" and real IR for `x + 0.0` with `x = RtValue::F64(-0.0)`, confirm the pass does NOT fire this direction — i.e., confirm the implementation only implements the safe `x + (-0.0) → x` direction, not the unsafe one).
- `(a+b)*(a+b)` CSEs to one `Add` (2 total Mul/Add-family instructions instead of 3).
- `a+b` and `b+a` CSE together (commutative canonicalization).
- GVN does NOT CSE across non-dominating sibling blocks (a `then`/`else` pair with structurally identical instructions must NOT merge — write this as an explicit negative test, since it's the one way a naive flat-table GVN silently breaks SSA soundness).
- Magic division matches `wrapping_div` for a large random sample including `i64::MIN` and small divisors (2, 3, 7, 100).
- Signed `x % 2^k` matches `wrapping_rem` for negative and positive dividends (the exact case the SPEC.md bug fix is about — this test must include negative dividends, not just positive ones).
- DCE removes an unreferenced subexpression entirely (instruction count check).
- Reassociation reduces dependency depth on an 8-term i64 sum, and is confirmed to NOT fire on an f64 chain (regression test for the associativity constraint above).
- `pow(x,2)`/`pow(x,0.5)`/`pow(x,-1)` rules: empirically verified bit-exact against unoptimized `interpret()` across a random sample before being trusted; if the platform's `powf` doesn't match, the implementing task must escalate rather than silently ship a rule that breaks the core `-O0==-O2` invariant.

## Exit criteria

1. `cargo test --workspace` passes, including the new `-O0==-O2` differential property test.
2. `forge-opt` is no longer a stub — `cargo check -p forge-opt` succeeds with real content, and `forge-ir`'s existing 92 tests still pass unmodified (this slice must not touch `forge-ir`'s existing verified logic, only add the `replace_value_everywhere` helper as new, additive surface).
3. Verifier runs after every pass in debug builds (not just once at the end) — confirmed by a test that deliberately breaks a pass's output and checks the driver panics with a message naming the specific pass.
4. `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
5. SPEC.md's two corrected rules (`x+(-0.0)→x`, signed `x%2^k`) match what's actually implemented — no more drift between the top-level docs and the code, the exact class of gap the Phase 0-3 final review caught.
