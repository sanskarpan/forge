# forge Phase 4 Optimizer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the safe (non-fast-math) optimizer for `forge`: constant folding, algebraic simplification, strength reduction (incl. magic-number division), dominator-tree-scoped GVN/CSE, DCE, and i64-only reassociation — with the core invariant `-O0 == -O2` bit-exact, verified after every single pass in debug builds.

**Architecture:** New real content in `crates/forge-opt/` (currently a stub), each transform as a small `Pass` implementor operating on `forge-ir::Function`, driven by a fixed-point loop capped at 10 rounds. One small addition to `forge-ir` itself (`replace_value_everywhere`, closing a gap Task 11 flagged). Every pass is validated against `forge_ir::interp::interpret()` — the correctness oracle from Phase 0-3.

**Tech Stack:** Rust, `forge-ir`/`forge-syntax` as dependencies, same TDD/testing conventions as Phase 0-3.

**Design doc:** `docs/superpowers/specs/2026-08-04-phase-4-optimizer-design.md` — read this first. It contains the full correctness reasoning (including two real bugs found and fixed in SPEC.md's own optimizer tables, and a third — f64 reassociation being unsound — found while writing the design). This plan implements what that doc specifies; don't re-derive the reasoning, just implement it carefully and verify empirically.

---

## Task 1: Driver infrastructure + `replace_value_everywhere`

**Files:**
- Create: `crates/forge-opt/src/lib.rs` (overwrites the Task-1-era one-line stub)
- Modify: `crates/forge-opt/Cargo.toml`
- Modify: `crates/forge-ir/src/ir.rs`

- [ ] **Step 1: Update `crates/forge-opt/Cargo.toml`**

```toml
[package]
name = "forge-opt"
version.workspace = true
edition.workspace = true

[dependencies]
forge-ir = { path = "../forge-ir" }
forge-syntax = { path = "../forge-syntax" }
rustc-hash.workspace = true

[dev-dependencies]
proptest.workspace = true
```

(`forge-syntax` is a real, not dev, dependency — tests in every subsequent task use the `lex→parse→resolve→typecheck→lower` pipeline to build test `Function`s from source, matching `forge-ir`'s own established test pattern. `proptest` is dev-only, used by Task 9.)

- [ ] **Step 2: Add `replace_value_everywhere` to `forge-ir`**

Append to `crates/forge-ir/src/ir.rs`, near the existing `uses_of`/`replace_in_inst`:

```rust
/// Redirects every use of `old` to `new`, across BOTH instruction operands
/// (via `replace_in_inst`) and terminator operands (`Return`'s value,
/// `Branch`'s condition) — the gap `uses_of`/`replace_in_inst` alone don't
/// cover (flagged during Task 11's review) and load-bearing for the
/// optimizer: GVN/DCE need to redirect a terminator's operand when the
/// value it referenced gets CSE'd away.
pub fn replace_value_everywhere(f: &mut Function, old: Value, new: Value) {
    for inst in &mut f.insts {
        replace_in_inst(inst, old, new);
    }
    for block in &mut f.blocks {
        match &mut block.term {
            Some(Terminator::Return(v)) => {
                if *v == old {
                    *v = new;
                }
            }
            Some(Terminator::Branch { cond, .. }) => {
                if *cond == old {
                    *cond = new;
                }
            }
            _ => {}
        }
    }
}
```

- [ ] **Step 3: Write a test for `replace_value_everywhere` (failing first)**

Add a `#[cfg(test)] mod tests` block to `crates/forge-ir/src/ir.rs` (it doesn't have one yet):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use forge_syntax::span::Span;

    fn hand_built_return_fn() -> Function {
        // v0 = ConstF64(1.0), v1 = ConstF64(2.0), term: Return(v0)
        Function {
            insts: vec![Inst::ConstF64(1.0f64.to_bits()), Inst::ConstF64(2.0f64.to_bits())],
            types: vec![Ty::F64, Ty::F64],
            spans: vec![Span::new(0, 0), Span::new(0, 0)],
            blocks: vec![BlockData {
                insts: vec![Value(0), Value(1)],
                term: Some(Terminator::Return(Value(0))),
                preds: Default::default(),
            }],
            entry: Block(0),
            params: vec![],
        }
    }

    #[test]
    fn replaces_return_terminator_operand() {
        let mut f = hand_built_return_fn();
        replace_value_everywhere(&mut f, Value(0), Value(1));
        assert!(matches!(f.blocks[0].term, Some(Terminator::Return(v)) if v == Value(1)));
    }

    #[test]
    fn replaces_branch_terminator_condition() {
        let mut f = hand_built_return_fn();
        f.blocks[0].term = Some(Terminator::Branch { cond: Value(0), then_: Block(0), else_: Block(0) });
        replace_value_everywhere(&mut f, Value(0), Value(1));
        assert!(matches!(f.blocks[0].term, Some(Terminator::Branch { cond, .. }) if cond == Value(1)));
    }

    #[test]
    fn still_replaces_instruction_operands_via_existing_replace_in_inst() {
        let mut f = hand_built_return_fn();
        f.insts.push(Inst::Add(Value(0), Value(1))); // v2 = v0 + v1
        replace_value_everywhere(&mut f, Value(0), Value(1));
        assert!(matches!(f.insts[2], Inst::Add(a, b) if a == Value(1) && b == Value(1)));
    }
}
```

- [ ] **Step 4: Run the tests to confirm they fail, then confirm they pass after Step 2's implementation**

Run: `cargo test -p forge-ir --lib ir:: 2>&1 | tail -20`
Expected: 3 tests pass (this is TDD in reverse order only in the sense that Step 2's code is already given above — write the test file, delete/comment the function temporarily to see it fail, then confirm, then restore — or simply verify the tests fail to compile before Step 2 is applied if you're applying both steps together; the important thing is you've seen it fail before trusting it passes).

- [ ] **Step 5: Write `crates/forge-opt/src/lib.rs`**

```rust
// crates/forge-opt/src/lib.rs

use forge_ir::Function;

/// One optimization transform. `run` returns whether it changed the IR —
/// the driver uses this to detect a fixed point.
pub trait Pass {
    fn name(&self) -> &'static str;
    fn run(&mut self, f: &mut Function) -> bool;
}

/// Runs `passes` in order, repeating until none of them report a change
/// (fixed point) or 10 rounds pass, whichever comes first. Re-verifies the
/// IR after EVERY individual pass in debug builds — not just once at the
/// end — per CHECKLIST.md: "catches an optimizer bug at the pass that
/// caused it, not three passes later."
pub fn run_passes(f: &mut Function, passes: &mut [Box<dyn Pass>]) {
    for round in 0..10 {
        let mut changed = false;
        for pass in passes.iter_mut() {
            let pass_changed = pass.run(f);
            changed |= pass_changed;
            #[cfg(debug_assertions)]
            if let Err(e) = forge_ir::verify::verify(f) {
                panic!("verifier failed after pass '{}' (round {round}): {e}", pass.name());
            }
        }
        if !changed {
            break;
        }
    }
}

/// The real optimization pipeline. Empty for now — Tasks 2-8 each add one
/// `Box::new(TheirPass)` to this vec as their pass lands, in the order
/// SPEC.md §6.5 specifies: fold, simplify, strength-reduce, GVN, reassoc, DCE.
pub fn optimize(f: &mut Function) {
    let mut passes: Vec<Box<dyn Pass>> = vec![
        // Box::new(fold::ConstFold),              <- Task 2
        // Box::new(simplify::AlgebraicSimplify),  <- Task 3
        // Box::new(strength::StrengthReduceShifts), Box::new(strength::MagicDivision), <- Tasks 4-5
        // Box::new(gvn::Gvn),                     <- Task 6
        // Box::new(reassoc::Reassociate),          <- Task 8 (yes, before DCE — reassoc can expose new DCE opportunities)
        // Box::new(dce::Dce),                     <- Task 7
    ];
    run_passes(f, &mut passes);
}
```

- [ ] **Step 6: Write driver tests using fake passes (TDD — write these first, confirm compile failure since `Pass`/`run_passes` don't exist yet is impossible since Step 5 already wrote them; instead confirm the tests below fail on their OWN assertions before you're confident `run_passes` is correct — i.e. run them once, read the actual pass/verify call counts, don't just assume)**

Append to `crates/forge-opt/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use forge_ir::*;
    use forge_syntax::span::Span;
    use std::cell::Cell;
    use std::rc::Rc;

    fn trivial_function() -> Function {
        Function {
            insts: vec![Inst::Param { index: 0, ty: Ty::F64 }],
            types: vec![Ty::F64],
            spans: vec![Span::new(0, 0)],
            blocks: vec![BlockData {
                insts: vec![Value(0)],
                term: Some(Terminator::Return(Value(0))),
                preds: Default::default(),
            }],
            entry: Block(0),
            params: vec![("x".to_string(), Ty::F64)],
        }
    }

    struct CountingPass {
        calls: Rc<Cell<u32>>,
        fire_times: u32,
    }
    impl Pass for CountingPass {
        fn name(&self) -> &'static str {
            "counting-test-pass"
        }
        fn run(&mut self, _f: &mut Function) -> bool {
            let n = self.calls.get();
            self.calls.set(n + 1);
            n < self.fire_times
        }
    }

    #[test]
    fn driver_runs_to_fixed_point_and_stops() {
        let mut f = trivial_function();
        let calls = Rc::new(Cell::new(0));
        let mut passes: Vec<Box<dyn Pass>> =
            vec![Box::new(CountingPass { calls: calls.clone(), fire_times: 3 })];
        run_passes(&mut f, &mut passes);
        // Pass reports "changed" on calls 0,1,2 (3 times), then false on
        // call 3 -> the round-3 call is what makes changed=false, ending
        // the loop after that same round. Total calls: 4.
        assert_eq!(calls.get(), 4);
    }

    #[test]
    fn driver_caps_at_ten_rounds_even_if_always_changed() {
        let mut f = trivial_function();
        let calls = Rc::new(Cell::new(0));
        let mut passes: Vec<Box<dyn Pass>> =
            vec![Box::new(CountingPass { calls: calls.clone(), fire_times: u32::MAX })];
        run_passes(&mut f, &mut passes);
        assert_eq!(calls.get(), 10);
    }

    struct BreakingPass;
    impl Pass for BreakingPass {
        fn name(&self) -> &'static str {
            "breaking-test-pass"
        }
        fn run(&mut self, f: &mut Function) -> bool {
            // Deliberately corrupt: point Return at a value that doesn't exist.
            f.blocks[0].term = Some(Terminator::Return(Value(999)));
            true
        }
    }

    #[test]
    #[should_panic(expected = "breaking-test-pass")]
    fn verifier_runs_after_every_pass_and_names_the_culprit() {
        let mut f = trivial_function();
        let mut passes: Vec<Box<dyn Pass>> = vec![Box::new(BreakingPass)];
        run_passes(&mut f, &mut passes);
    }

    #[test]
    fn empty_pipeline_leaves_function_unchanged() {
        let mut f = trivial_function();
        optimize(&mut f);
        assert_eq!(f.insts.len(), 1);
    }
}
```

Note: `driver_runs_to_fixed_point_and_stops`'s exact expected call count (4) is given as a worked example, but VERIFY it by actually running the test rather than trusting the comment's arithmetic — if the real semantics of "changed" checking produce a different number, trust the actual behavior and fix the assertion to match reality (with a corrected comment), don't force the test to match a possibly-wrong prediction.

- [ ] **Step 5: Run all new tests**

Run: `cargo test -p forge-opt --lib 2>&1 | tail -20` and `cargo test -p forge-ir --lib ir:: 2>&1 | tail -20`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/forge-opt/Cargo.toml crates/forge-opt/src/lib.rs crates/forge-ir/src/ir.rs
git commit -m "feat(forge-opt): Pass trait, fixed-point driver, verify-after-every-pass"
```

---

## Task 2: Constant folding

**Files:**
- Create: `crates/forge-opt/src/fold.rs`
- Modify: `crates/forge-opt/src/lib.rs`

- [ ] **Step 1: Write the test module (failing first)**

```rust
// crates/forge-opt/src/fold.rs — append at the bottom

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
            "1.0 / 0.0", "0.0 / 0.0", "-1.0 * 0.0", "sqrt(4.0)", "3.0 % 2.0",
            "7 / 2", "-7 / 2", "min(1.0, 2.0)", "max(1.0, 2.0)", "1.0 == 1.0",
            "sin(0.0)", "pow(2.0, 3.0)",
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
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-opt --lib fold:: 2>&1 | head -20`
Expected: FAIL — `ConstFold` not defined.

- [ ] **Step 3: Write the implementation above the test module**

```rust
// crates/forge-opt/src/fold.rs — above the `#[cfg(test)]` module

use forge_ir::*;

/// Both operands must be LITERAL constant instructions — this is different
/// from (and simpler than) algebraic simplification (`simplify.rs`). When
/// both operands are literal, computing the result now is always safe for
/// every op, including ones that produce NaN/Inf: folding doesn't change
/// what the program computes, it just computes it earlier.
/// `0.0 / 0.0 -> NaN` is exactly as correct at compile time as at runtime.
///
/// Deliberately NOT sharing logic with `forge_ir::interp` — structurally
/// similar per-op arithmetic, but reusing/refactoring the already
/// heavily-verified interpreter risked destabilizing it for this task's
/// sake. Correctness is instead pinned by `folding_never_changes_the_answer`
/// below, comparing against `interpret()` directly.
pub struct ConstFold;

impl crate::Pass for ConstFold {
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

enum C {
    F64(f64),
    I64(i64),
    Bool(bool),
}

fn as_const(f: &Function, v: Value) -> Option<C> {
    match &f.insts[v.0 as usize] {
        Inst::ConstF64(bits) => Some(C::F64(f64::from_bits(*bits))),
        Inst::ConstI64(n) => Some(C::I64(*n)),
        Inst::ConstBool(b) => Some(C::Bool(*b)),
        _ => None,
    }
}

fn f64_inst(x: f64) -> Inst {
    Inst::ConstF64(x.to_bits())
}

fn try_fold(f: &Function, v: Value) -> Option<Inst> {
    match &f.insts[v.0 as usize] {
        Inst::Add(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(C::F64(x)), Some(C::F64(y))) => Some(f64_inst(x + y)),
            (Some(C::I64(x)), Some(C::I64(y))) => Some(Inst::ConstI64(x.wrapping_add(y))),
            _ => None,
        },
        Inst::Sub(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(C::F64(x)), Some(C::F64(y))) => Some(f64_inst(x - y)),
            (Some(C::I64(x)), Some(C::I64(y))) => Some(Inst::ConstI64(x.wrapping_sub(y))),
            _ => None,
        },
        Inst::Mul(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(C::F64(x)), Some(C::F64(y))) => Some(f64_inst(x * y)),
            (Some(C::I64(x)), Some(C::I64(y))) => Some(Inst::ConstI64(x.wrapping_mul(y))),
            _ => None,
        },
        Inst::Div(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(C::F64(x)), Some(C::F64(y))) => Some(f64_inst(x / y)),
            (Some(C::I64(x)), Some(C::I64(y))) => Some(Inst::ConstI64(x.wrapping_div(y))),
            _ => None,
        },
        Inst::Rem(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(C::F64(x)), Some(C::F64(y))) => Some(f64_inst(x % y)),
            (Some(C::I64(x)), Some(C::I64(y))) => Some(Inst::ConstI64(x.wrapping_rem(y))),
            _ => None,
        },
        Inst::Neg(a) => match as_const(f, *a) {
            Some(C::F64(x)) => Some(f64_inst(-x)),
            Some(C::I64(x)) => Some(Inst::ConstI64(x.wrapping_neg())),
            _ => None,
        },
        Inst::Sqrt(a) => match as_const(f, *a) { Some(C::F64(x)) => Some(f64_inst(x.sqrt())), _ => None },
        Inst::Abs(a) => match as_const(f, *a) { Some(C::F64(x)) => Some(f64_inst(x.abs())), _ => None },
        Inst::Floor(a) => match as_const(f, *a) { Some(C::F64(x)) => Some(f64_inst(x.floor())), _ => None },
        Inst::Ceil(a) => match as_const(f, *a) { Some(C::F64(x)) => Some(f64_inst(x.ceil())), _ => None },
        Inst::Round(a) => match as_const(f, *a) { Some(C::F64(x)) => Some(f64_inst(x.round())), _ => None },
        Inst::Trunc(a) => match as_const(f, *a) { Some(C::F64(x)) => Some(f64_inst(x.trunc())), _ => None },
        Inst::Min(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(C::F64(x)), Some(C::F64(y))) => Some(f64_inst(x.min(y))),
            _ => None,
        },
        Inst::Max(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(C::F64(x)), Some(C::F64(y))) => Some(f64_inst(x.max(y))),
            _ => None,
        },
        Inst::Fma { a, b, c } => match (as_const(f, *a), as_const(f, *b), as_const(f, *c)) {
            (Some(C::F64(x)), Some(C::F64(y)), Some(C::F64(z))) => Some(f64_inst(x.mul_add(y, z))),
            _ => None,
        },
        Inst::And(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(C::I64(x)), Some(C::I64(y))) => Some(Inst::ConstI64(x & y)),
            (Some(C::Bool(x)), Some(C::Bool(y))) => Some(Inst::ConstBool(x & y)),
            _ => None,
        },
        Inst::Or(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(C::I64(x)), Some(C::I64(y))) => Some(Inst::ConstI64(x | y)),
            (Some(C::Bool(x)), Some(C::Bool(y))) => Some(Inst::ConstBool(x | y)),
            _ => None,
        },
        Inst::Xor(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(C::I64(x)), Some(C::I64(y))) => Some(Inst::ConstI64(x ^ y)),
            _ => None,
        },
        Inst::Not(a) => match as_const(f, *a) {
            Some(C::I64(x)) => Some(Inst::ConstI64(!x)),
            Some(C::Bool(x)) => Some(Inst::ConstBool(!x)),
            _ => None,
        },
        Inst::Shl(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(C::I64(x)), Some(C::I64(y))) => Some(Inst::ConstI64(x.wrapping_shl(y as u32))),
            _ => None,
        },
        Inst::Shr(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(C::I64(x)), Some(C::I64(y))) => {
                Some(Inst::ConstI64((x as u64).wrapping_shr(y as u32) as i64))
            }
            _ => None,
        },
        Inst::Sar(a, b) => match (as_const(f, *a), as_const(f, *b)) {
            (Some(C::I64(x)), Some(C::I64(y))) => Some(Inst::ConstI64(x.wrapping_shr(y as u32))),
            _ => None,
        },
        Inst::Cmp { op, lhs, rhs } => {
            let (l, r) = (as_const(f, *lhs)?, as_const(f, *rhs)?);
            eval_cmp_const(*op, l, r).map(Inst::ConstBool)
        }
        Inst::Call { func, args } => {
            let a = match as_const(f, args[0])? {
                C::F64(x) => x,
                _ => return None,
            };
            let result = match func {
                LibFunc::Sin => a.sin(),
                LibFunc::Cos => a.cos(),
                LibFunc::Tan => a.tan(),
                LibFunc::Exp => a.exp(),
                LibFunc::Log => a.ln(),
                LibFunc::Pow => match as_const(f, args[1])? {
                    C::F64(b) => a.powf(b),
                    _ => return None,
                },
            };
            Some(f64_inst(result))
        }
        Inst::IToF(a) => match as_const(f, *a) {
            Some(C::I64(x)) => Some(f64_inst(x as f64)),
            _ => None,
        },
        Inst::FToI(a) => match as_const(f, *a) {
            Some(C::F64(x)) => Some(Inst::ConstI64(x as i64)),
            _ => None,
        },
        _ => None,
    }
}

fn eval_cmp_const(op: CmpOp, l: C, r: C) -> Option<bool> {
    match (op, l, r) {
        (CmpOp::Eq, C::F64(x), C::F64(y)) => Some(x == y),
        (CmpOp::Ne, C::F64(x), C::F64(y)) => Some(x != y),
        (CmpOp::Lt, C::F64(x), C::F64(y)) => Some(x < y),
        (CmpOp::Le, C::F64(x), C::F64(y)) => Some(x <= y),
        (CmpOp::Gt, C::F64(x), C::F64(y)) => Some(x > y),
        (CmpOp::Ge, C::F64(x), C::F64(y)) => Some(x >= y),
        (CmpOp::Eq, C::I64(x), C::I64(y)) => Some(x == y),
        (CmpOp::Ne, C::I64(x), C::I64(y)) => Some(x != y),
        (CmpOp::Lt, C::I64(x), C::I64(y)) => Some(x < y),
        (CmpOp::Le, C::I64(x), C::I64(y)) => Some(x <= y),
        (CmpOp::Gt, C::I64(x), C::I64(y)) => Some(x > y),
        (CmpOp::Ge, C::I64(x), C::I64(y)) => Some(x >= y),
        (CmpOp::Eq, C::Bool(x), C::Bool(y)) => Some(x == y),
        (CmpOp::Ne, C::Bool(x), C::Bool(y)) => Some(x != y),
        _ => None,
    }
}
```

- [ ] **Step 4: Register the pass and add the module**

In `crates/forge-opt/src/lib.rs`: add `pub mod fold;` near the top, and uncomment/add `Box::new(fold::ConstFold),` in `optimize()`'s pass vec.

- [ ] **Step 5: Run tests**

Run: `cargo test -p forge-opt --lib fold:: 2>&1 | tail -20`
Expected: all pass (4 tests: 3 unit + the multi-case correctness-property test).

- [ ] **Step 6: Commit**

```bash
git add crates/forge-opt/src/fold.rs crates/forge-opt/src/lib.rs
git commit -m "feat(forge-opt): constant folding"
```

---

## Task 3: Algebraic simplification

**Files:**
- Create: `crates/forge-opt/src/simplify.rs`
- Modify: `crates/forge-opt/src/lib.rs`

- [ ] **Step 1: Write the test module (failing first)**

```rust
// crates/forge-opt/src/simplify.rs — append at the bottom

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
    fn x_plus_neg_zero_simplifies_to_x() {
        let mut f = lowered("x + -0.0");
        let changed = AlgebraicSimplify.run(&mut f);
        assert!(changed);
        // The root Add should have been replaced by the Param directly.
        assert!(matches!(f.blocks[f.entry.0 as usize].term, Some(Terminator::Return(v)) if matches!(f.insts[v.0 as usize], Inst::Param { .. })));
    }

    #[test]
    fn x_plus_pos_zero_is_not_simplified_for_f64() {
        // The bug this whole rule exists to avoid: x + 0.0 is NOT safe when
        // x could be -0.0 at runtime, so this direction must NOT fire.
        let mut f = lowered("x + 0.0");
        let changed = AlgebraicSimplify.run(&mut f);
        assert!(!changed, "x + 0.0 must not simplify for f64 -- see design doc");
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
        assert!(!AlgebraicSimplify.run(&mut f_float), "f64 x*0 must not fold -- NaN/Inf*0=NaN");

        let mut f_int = lowered("n * 0");
        assert!(AlgebraicSimplify.run(&mut f_int), "i64 x*0 should simplify to 0");
    }

    #[test]
    fn double_negation_cancels() {
        let mut f = lowered("- -x");
        assert!(AlgebraicSimplify.run(&mut f));
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
            ("n * 0", &[RtValue::I64(42)]),
            ("n - n", &[RtValue::I64(i64::MIN)]),
            ("n / n", &[RtValue::I64(7)]),
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
                        assert_eq!(e.to_bits(), a.to_bits(), "mismatch for {src:?} args={args:?}: {e} vs {a}");
                    }
                }
                (e, a) => assert_eq!(e, a, "mismatch for {src:?} args={args:?}"),
            }
        }
    }
}
```

Note: `"x + -0.0"` and `"- -x"` as source text — confirm the parser actually accepts unary minus directly before a literal/expression like this (it should, per Task 5's parser, unary `-` binds as a prefix operator at binding power 21). If either literal doesn't parse the way you expect, adjust the source string (e.g. use a `let`-bound negative literal) while preserving the test's intent, and note why in your report.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-opt --lib simplify:: 2>&1 | head -20`

- [ ] **Step 3: Write the implementation above the test module**

```rust
// crates/forge-opt/src/simplify.rs — above the `#[cfg(test)]` module

use forge_ir::*;

/// Rewrites an instruction to a cheaper equivalent when ONE operand is a
/// literal identity/absorbing element, or the two operands are the same
/// `Value` (structural equality) — NOT both-literal (that's `fold.rs`'s
/// job). See SPEC.md §6.2 and the design doc for the full correctness
/// reasoning behind each rule, especially the f64 sign-of-zero trap that
/// makes `x + 0` asymmetric with `x - 0`.
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

fn is_f64_neg_zero(f: &Function, v: Value) -> bool {
    matches!(&f.insts[v.0 as usize], Inst::ConstF64(bits)
        if { let x = f64::from_bits(*bits); x == 0.0 && x.is_sign_negative() })
}
fn is_f64_pos_zero(f: &Function, v: Value) -> bool {
    matches!(&f.insts[v.0 as usize], Inst::ConstF64(bits)
        if { let x = f64::from_bits(*bits); x == 0.0 && x.is_sign_positive() })
}
fn is_i64_zero(f: &Function, v: Value) -> bool {
    matches!(&f.insts[v.0 as usize], Inst::ConstI64(0))
}
fn is_f64_one(f: &Function, v: Value) -> bool {
    matches!(&f.insts[v.0 as usize], Inst::ConstF64(bits) if f64::from_bits(*bits) == 1.0)
}
fn is_i64_one(f: &Function, v: Value) -> bool {
    matches!(&f.insts[v.0 as usize], Inst::ConstI64(1))
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
                return None; // x - x for f64: NaN - NaN = NaN, unsafe (IntOnly below)
            }
            let zero = if is_f64(f, a) { is_f64_pos_zero(f, b) } else { is_i64_zero(f, b) };
            if zero {
                return Some(Rewrite::UseExisting(a));
            }
            if a == b && !is_f64(f, a) {
                return Some(Rewrite::BecomeConst(Inst::ConstI64(0))); // x - x -> 0, IntOnly
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
            let one = |x: Value| if is_f64(f, a) { is_f64_one(f, x) } else { is_i64_one(f, x) };
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
            if a == b {
                // x / x -> 1, IntOnly (f64: 0/0=NaN, NaN/NaN=NaN).
                return if !is_f64(f, a) { Some(Rewrite::BecomeConst(Inst::ConstI64(1))) } else { None };
            }
            let one = if is_f64(f, a) { is_f64_one(f, b) } else { is_i64_one(f, b) };
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
            Some(Rewrite::BecomeConst(Inst::ConstI64(0)))
        }
        Inst::Neg(a) => match &f.insts[a.0 as usize] {
            Inst::Neg(inner) => Some(Rewrite::UseExisting(*inner)), // -(-x) -> x, exact both domains
            _ => None,
        },
        _ => None,
    }
}
```

- [ ] **Step 4: Register the pass**

In `crates/forge-opt/src/lib.rs`: add `pub mod simplify;`, uncomment `Box::new(simplify::AlgebraicSimplify),`.

- [ ] **Step 5: Run tests, fix the `x + -0.0` / `- -x` source-text issue if it arises (see note in Step 1), and confirm all pass**

Run: `cargo test -p forge-opt --lib simplify:: 2>&1 | tail -20`

- [ ] **Step 6: Commit**

```bash
git add crates/forge-opt/src/simplify.rs crates/forge-opt/src/lib.rs
git commit -m "feat(forge-opt): algebraic simplification"
```

---

## Task 4: Strength reduction — shifts, signed division/remainder by power of 2

**Files:**
- Create: `crates/forge-opt/src/strength.rs`
- Modify: `crates/forge-opt/src/lib.rs`

**This is one of the trickiest tasks in this plan — read the design doc's "Strength reduction" section again before starting, and treat empirical verification (not just code review) as mandatory before considering this done.**

- [ ] **Step 1: Understand the required transforms (design, not literal code — you derive the exact instruction sequences with TDD)**

Three rewrites, all i64-only (verify each is only reachable when the operand's type is `Ty::I64` — the type checker already guarantees `Shl`/`Shr`/`And` etc. only apply to i64, so this should fall out naturally rather than needing explicit gating, but confirm this by testing):

1. **`x * C` where `C` is an exact power of 2 (`C == 2^k` for some `k` in `1..63`) → `x << k`.** Straightforward: replace the `Mul` with a `Shl` against a new `ConstI64(k)`. Skip `C == 1` (already handled by `simplify.rs`) and `C == 0` (already handled by `simplify.rs`); this pass only needs to handle `k >= 1`.

2. **`x / C` where `C` is an exact power of 2 → arithmetic-shift-right with a sign fixup.** Truncating division rounds toward zero; arithmetic shift rounds toward negative infinity — these disagree for negative `x` (`-7/2 == -3` truncating, but `-7>>1 == -4`). The correct sequence (matching what GCC/LLVM emit for signed division by a power of 2):
   ```
   sign_mask = x >> 63                      (arithmetic shift: all-1s if x<0, all-0s if x>=0)
   bias      = sign_mask & (2^k - 1)
   biased    = x + bias
   result    = biased >> k                  (arithmetic shift)
   ```
   You'll need to emit several new instructions into the IR (the sign mask, the bias constant, the AND, the ADD, and the k constant) before rewriting the original `Div` instruction's own slot into the final `Sar`. Since these run on IR that's already past construction (no `Builder` in scope here), you need a small helper to insert a new instruction into a specific block's instruction list at a specific position (not just append to the end — the new helper instructions must be defined BEFORE the position where the original `Div` was, in program order, since later code may already reference the `Div`'s `Value` and expects it to still resolve to the final result). Write this helper yourself; a `fn insert_before(f: &mut Function, block: Block, pos: usize, inst: Inst, ty: Ty, span: Span) -> Value` that pushes a new instruction onto `f.insts`/`f.types`/`f.spans` and inserts its `Value` into `f.blocks[block].insts` at `pos` (shifting subsequent entries, including the original instruction's own position, right by one) is the natural shape — you'll call it several times in sequence, tracking how `pos` needs to advance for each subsequent insertion.

3. **`x % C` where `C` is an exact power of 2 → `x - (q << k)`, reusing the corrected `q` from rule 2.** Do NOT implement this as `x & (2^k - 1)` — that computes the Euclidean remainder, which is WRONG for negative `x` (see SPEC.md §6.3's documented bug fix). Instead, literally reuse the division rewrite's mechanism: compute the same corrected `q` (steps above), then emit `q << k` and `x - (q << k)`, replacing the original `Rem`'s slot with the final `Sub`. This costs more instructions than the naive masked form, deliberately traded for provable correctness (the result is correct by construction from the identity `x = q*d + r`, not a separately-derived bit-trick).

- [ ] **Step 2: Write tests FIRST, including the case that would catch a wrong implementation**

```rust
// crates/forge-opt/src/strength.rs — test module, write before the implementation

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

    fn run_both(src: &str, n: i64) -> (RtValue, RtValue) {
        let unreduced = lowered(src);
        let expected = interpret(&unreduced, &[RtValue::I64(n)]);
        let mut reduced = lowered(src);
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
    fn div_by_power_of_two_matches_wrapping_div_including_negative_dividends() {
        // THE test that catches a naive (unfixed) shift implementation:
        // truncating division and arithmetic shift disagree here.
        for n in [0i64, 1, -1, 7, -7, 8, -8, 15, -15, i64::MIN, i64::MAX, -100_000] {
            let (e, a) = run_both("n / 8", n);
            assert_eq!(e, a, "n={n}: expected {e:?}, got {a:?}");
        }
    }

    #[test]
    fn rem_by_power_of_two_matches_wrapping_rem_including_negative_dividends() {
        // THE test that catches the SPEC.md-documented masking bug: a naive
        // `x & (2^k - 1)` implementation would fail exactly here.
        for n in [0i64, 1, -1, 7, -7, 8, -8, 15, -15, i64::MIN, i64::MAX, -100_000] {
            let (e, a) = run_both("n % 8", n);
            assert_eq!(e, a, "n={n}: expected {e:?}, got {a:?}");
        }
    }

    #[test]
    fn does_not_fire_on_non_power_of_two_divisor() {
        let mut f = lowered("n / 7");
        let changed = StrengthReduceShifts.run(&mut f);
        assert!(!changed, "7 is not a power of 2 -- magic division handles this, not shifts (Task 5)");
    }
}
```

- [ ] **Step 3: Implement, running the tests continuously as you derive the exact instruction sequence**

Write `StrengthReduceShifts` implementing `crate::Pass`, using the algorithm description from Step 1. Iterate: write your best attempt at the `insert_before` helper and the three rewrites, run `cargo test -p forge-opt --lib strength:: -- --nocapture`, and fix based on actual failures — especially `div_by_power_of_two_matches_wrapping_div_including_negative_dividends` and the rem equivalent, which are specifically designed to fail loudly if the sign-fixup logic is wrong. Do not consider this done until BOTH of those pass for every value in their test arrays, including `i64::MIN`.

- [ ] **Step 4: Register the pass**

`pub mod strength;` in `lib.rs`, `Box::new(strength::StrengthReduceShifts),` in the pipeline (position: after `simplify`, before GVN — matches SPEC.md §6.5's pipeline order).

- [ ] **Step 5: Run the full test suite for this crate to confirm no regressions**

Run: `cargo test -p forge-opt --lib 2>&1 | tail -30`

- [ ] **Step 6: Commit**

```bash
git add crates/forge-opt/src/strength.rs crates/forge-opt/src/lib.rs
git commit -m "feat(forge-opt): strength-reduce multiply/divide/remainder by power of 2"
```

---

## Task 5: Strength reduction — magic-number division and `pow` rules

**Files:**
- Modify: `crates/forge-opt/src/strength.rs`

- [ ] **Step 1: Port the magic-number division algorithm from PROMPT.md verbatim**

PROMPT.md's "Phase 4 — The Optimizer's Floating-Point Trap" section has a complete, already-correct `magic_signed`/`apply_magic` implementation (Granlund & Montgomery, PLDI '94) with its own test covering `i64::MIN` and 100,000 random samples per divisor. Read that section of PROMPT.md now. Port `magic_signed` and `apply_magic` into `strength.rs` as private helper functions (or a small `MagicNumber { multiplier: i64, shift: u32 }` struct), and port the test almost verbatim — it already specifies exactly the coverage needed (`[3i64, 5, 7, 10, 100, 1000, -3, -7, -100]` as divisors, `[0, 1, -1, 42, -42, i64::MAX, i64::MIN, i64::MAX-1]` plus 100,000 random `i64` values per divisor, comparing against `n.wrapping_div(d)`).

- [ ] **Step 2: Wire magic division into a `Pass`**

Add `MagicDivision` (or fold it into `StrengthReduceShifts` if you judge that cleaner — your call, document which you chose and why) implementing `crate::Pass`, firing on `Inst::Div(x, c)` where `c` is a literal `ConstI64` that is NOT `0`, `1`, `-1`, or an exact power of 2 (those are handled by `simplify.rs`/Task 4 already — don't double-handle). Emit the 3-instruction magic-multiply sequence (`imul` 128-bit-widening multiply by the magic constant, then the appropriate shift, per PROMPT.md's `apply_magic`). This requires the same "insert instructions before the original position, then rewrite the original slot" mechanism from Task 4 — reuse that helper if you wrote it generically enough, or adapt it.

Note: our IR has no instruction for a 128-bit-widening multiply (x86's `imul` producing a 128-bit result needs two registers) — `apply_magic`'s reference implementation is written assuming that hardware primitive exists. Since we have no codegen yet in this slice, you need to decide how to represent this at the IR level. The simplest correct option: since Rust's `i64::wrapping_mul` combined with taking only the HIGH 64 bits of a full 128-bit product isn't directly expressible with our current `Mul`/`Shr` instructions (which only operate on 64-bit values with 64-bit results), you have two reasonable choices — (a) use `i128` arithmetic in the OPTIMIZER PASS ITSELF to compute the magic multiply's effect as a compile-time-verified rewrite into instructions that ARE representable in our 64-bit IR (this likely means the IR-level rewrite can't be a literal 3-instruction sequence the way PROMPT.md's pseudocode implies, since we lack a widening-multiply IR instruction), or (b) treat this as a signal that a true `imul`-128 IR instruction is needed and belongs in a LATER phase once codegen can actually emit the hardware instruction, and SCOPE THIS TASK to just building and correctness-testing the `magic_signed`/`apply_magic` MATH (which Phase 6/7's instruction selection will later consult when choosing to emit `imul r64, r64` + shift instead of `idiv`), without actually wiring it into the IR-rewriting optimizer pass yet.

**Stop and think about which of these is right before implementing** — this is exactly the kind of "the literal task description doesn't quite fit reality" situation earlier tasks in this project have hit (e.g. Task 12's printer test, Task 9's builder). Read `uses_of`/`Inst`'s variant list yourself and confirm whether a widening multiply is or isn't representable. If it genuinely isn't, implement option (b): the magic-number MATH lives in `strength.rs` fully tested, but the IR-rewriting `Pass` for it is a documented stub/TODO for Phase 6/7 rather than something this task force-fits into a lossy 64-bit-only IR. Report back your finding either way — this is a legitimate scope question, not a mistake to hide.

- [ ] **Step 3: `pow` simplification rules, with mandatory empirical verification**

Add rules firing on `Inst::Call { func: LibFunc::Pow, args }` where `args[1]` is a literal `ConstF64` bit-exactly matching `2.0`, `0.5`, or `-1.0`:
- `pow(x, 2.0) → x * x`
- `pow(x, 0.5) → sqrt(x)`
- `pow(x, -1.0) → 1.0 / x`

**Before trusting any of these, write a test that empirically checks bit-exactness against the UNMODIFIED `pow` call, across a reasonably large random sample of finite, NaN, Inf, and zero inputs, on THIS platform.** Something like:

```rust
#[test]
fn pow_x_2_matches_x_times_x_bit_exact() {
    use rand::{Rng, SeedableRng};
    let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(0xC0FFEE);
    let mut mismatches = 0;
    for _ in 0..100_000 {
        let x: f64 = rng.gen_range(-1e6..1e6);
        if x.powf(2.0).to_bits() != (x * x).to_bits() { mismatches += 1; }
    }
    assert_eq!(mismatches, 0, "pow(x,2.0) is not bit-identical to x*x on this platform -- do NOT implement this rule");
}
```

(If `rand`/`rand_chacha` aren't already workspace dependencies, either add them as dev-dependencies to `forge-opt/Cargo.toml` or write an equivalent check using a simple deterministic PRNG / a fixed large array of test values — your call on the mechanism, but the empirical check itself is mandatory, not optional.) **If this test fails on this platform, do NOT implement the `pow(x,2)→x*x` rule** — report DONE_WITH_CONCERNS explaining what you found, and skip that specific rule (implement whichever of the three DO verify bit-exact, dropping any that don't). Do the same empirical check for `pow(x,0.5)` vs `sqrt(x)` and `pow(x,-1.0)` vs `1.0/x` before trusting either.

- [ ] **Step 4: Register whichever passes you ended up with, run full test suite**

- [ ] **Step 5: Commit**

```bash
git add crates/forge-opt/src/strength.rs crates/forge-opt/src/lib.rs crates/forge-opt/Cargo.toml
git commit -m "feat(forge-opt): magic-number division math, empirically-verified pow() rules"
```

---

## Task 6: Dominator-tree-scoped GVN/CSE

**Files:**
- Create: `crates/forge-opt/src/gvn.rs`
- Modify: `crates/forge-opt/src/lib.rs`
- Modify: `crates/forge-ir/src/ir.rs` (add `Hash`/`Eq` derives)

**This is the second trickiest task in this plan.**

- [ ] **Step 1: Add `PartialEq, Eq, Hash` to `Inst`, `Ty`, `CmpOp`, `LibFunc` in `crates/forge-ir/src/ir.rs`**

These currently derive `Clone, Debug` (`Inst`) or `Clone, Copy, PartialEq, Eq, Debug` (`Ty`/`CmpOp`/`LibFunc` — missing `Hash`). Add `Hash` to the latter three, and `PartialEq, Eq, Hash` to `Inst` (it needs all three to be usable as a `HashMap` key; `SmallVec<[Value;2]>`/`SmallVec<[(Block,Value);2]>` already implement these conditionally on their element types, which now all do). This is purely additive — confirm `cargo check --workspace` still succeeds and `forge-ir`'s existing 92+ tests are unaffected (they don't depend on `Inst` NOT having these derives).

- [ ] **Step 2: Understand the algorithm (design, you implement with TDD)**

A flat whole-function hash table would be UNSOUND: it would incorrectly CSE two structurally-identical instructions in non-dominating sibling blocks (e.g. `then` and `else` arms of an `if`) — the `then`-block instruction doesn't dominate a use in `else`, so redirecting `else`'s copy to reuse it would violate SSA dominance (and the verifier, correctly, would catch this after the pass runs — but better to never produce it).

The standard approach (what LLVM's GVN does): walk the **dominator tree** in preorder, maintaining a hash table scoped to the current path from the root. Build `Block → Vec<Block>` (dominator-tree children) from the `idom` array `forge_ir::dominance::compute_dominators` already gives you (a block `c`'s dom-tree parent is `idom[c]`; invert that relationship once to get children lists). Recursive preorder walk:

```
visit(block, table):
    inserted = []
    for each instruction v in block (in order):
        key = canonicalize(insts[v])          # commutative-operand-sorted for Add/Mul/And/Or/Xor/Cmp{Eq,Ne}
        if table contains key:
            replace_value_everywhere(f, v, table[key])   # CSE hit
        else:
            table.insert(key, v)
            inserted.push(key)
    for each dom-tree child of block:
        visit(child, table)
    for key in inserted:
        table.remove(key)          # leaving this subtree -- these entries no longer dominate siblings
```

Canonicalization: for `Add`/`Mul`/`And`/`Or`/`Xor` and `Cmp` with `op` in `{Eq, Ne}` (fully commutative), sort the two operand `Value`s by index (lower first) before using the `Inst` as a key, so `a+b` and `b+a` hash identically. Every other op (including `Cmp{Lt,Le,Gt,Ge}`, which are NOT commutative) keeps operand order as-is.

- [ ] **Step 3: Write tests FIRST**

```rust
// crates/forge-opt/src/gvn.rs — test module

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
        forge_ir::lower::lower(&typed)
    }

    fn count_op<F: Fn(&Inst) -> bool>(f: &Function, pred: F) -> usize {
        f.insts.iter().filter(|i| pred(i)).count()
    }

    #[test]
    fn repeated_subexpression_cses_to_one_add() {
        // (a+b)*(a+b): the two (a+b) subexpressions must become one Add.
        let mut f = lowered("(a + b) * (a + b)");
        let before = count_op(&f, |i| matches!(i, Inst::Add(_, _)));
        assert_eq!(before, 2, "sanity: two separate Adds before GVN");
        Gvn.run(&mut f);
        let after = count_op(&f, |i| matches!(i, Inst::Add(_, _)));
        assert_eq!(after, 1, "GVN should have merged the two identical Adds into one");
    }

    #[test]
    fn commutative_operands_cse_together() {
        // a+b and b+a must be recognized as the same value.
        let mut f = lowered("(a + b) + (b + a)");
        Gvn.run(&mut f);
        let after = count_op(&f, |i| matches!(i, Inst::Add(_, _)));
        // (a+b) appears twice (once as a+b, once as b+a) plus the outer add
        // = 3 Adds total before CSE; after CSE the two inner ones merge, so
        // 2 remain (the shared a+b, and the outer add of it with itself).
        assert_eq!(after, 2, "b+a should CSE with a+b via commutative canonicalization");
    }

    #[test]
    fn does_not_cse_across_non_dominating_sibling_blocks() {
        // then and else each independently compute `x * x` -- these must
        // NOT be merged into one instruction, since neither branch
        // dominates the other. A flat (non-dominator-scoped) GVN would
        // incorrectly merge these; this is the test that catches that bug.
        let mut f = lowered("if x > 0.0 then x * x else x * x");
        let before = count_op(&f, |i| matches!(i, Inst::Mul(_, _)));
        assert_eq!(before, 2, "sanity: one Mul per branch before GVN");
        Gvn.run(&mut f);
        let after = count_op(&f, |i| matches!(i, Inst::Mul(_, _)));
        assert_eq!(after, 2, "the two branch-local Muls must NOT be CSE'd across non-dominating blocks");
        assert!(forge_ir::verify::verify(&f).is_ok(), "result must still be valid SSA");
    }

    #[test]
    fn cse_result_still_passes_the_verifier() {
        let mut f = lowered("(a + b) * (a + b) + sqrt(a + b)");
        Gvn.run(&mut f);
        assert!(forge_ir::verify::verify(&f).is_ok());
    }
}
```

- [ ] **Step 4: Implement `Gvn`, iterating against the tests**

Run `cargo test -p forge-opt --lib gvn:: -- --nocapture` continuously while developing. `does_not_cse_across_non_dominating_sibling_blocks` is the single most important test here — if your implementation passes every other test but fails this one, you've built a flat table, not a dominator-scoped one, and the fix is architectural (add the scoping), not incremental.

- [ ] **Step 5: Register the pass**

`pub mod gvn;` in `lib.rs`, `Box::new(gvn::Gvn),` in the pipeline (after strength reduction, per SPEC.md §6.5).

- [ ] **Step 6: Run full test suite, including `forge-ir`'s (confirm the new `Hash`/`Eq` derives didn't break anything)**

Run: `cargo test --workspace 2>&1 | tail -40`

- [ ] **Step 7: Commit**

```bash
git add crates/forge-opt/src/gvn.rs crates/forge-opt/src/lib.rs crates/forge-ir/src/ir.rs
git commit -m "feat(forge-opt): dominator-tree-scoped GVN/CSE"
```

---

## Task 7: Dead code elimination

**Files:**
- Create: `crates/forge-opt/src/dce.rs`
- Modify: `crates/forge-opt/src/lib.rs`

- [ ] **Step 1: Write the test module (failing first)**

```rust
// crates/forge-opt/src/dce.rs — test module

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
        forge_ir::lower::lower(&typed)
    }

    #[test]
    fn removes_an_unused_subexpression() {
        // `let t = x * x in y` -- t is computed but never used in the body.
        let mut f = lowered("let t = x * x in y");
        let before = f.blocks[f.entry.0 as usize].insts.len();
        let changed = Dce.run(&mut f);
        assert!(changed);
        let after = f.blocks[f.entry.0 as usize].insts.len();
        assert!(after < before, "the dead `x*x` should have been swept from the block's instruction list");
        assert!(forge_ir::verify::verify(&f).is_ok());
    }

    #[test]
    fn does_not_remove_a_value_used_by_the_return() {
        let mut f = lowered("x + y");
        let before = f.blocks[f.entry.0 as usize].insts.len();
        Dce.run(&mut f);
        assert_eq!(f.blocks[f.entry.0 as usize].insts.len(), before);
    }

    #[test]
    fn does_not_remove_a_value_used_only_by_a_branch_condition() {
        // The condition of an `if` is a Terminator operand, not a normal
        // Inst use -- confirms DCE's liveness seeding covers Branch.cond
        // (this is exactly the gap `replace_value_everywhere`/Task 1 fixed
        // for rewriting; DCE's OWN liveness-seed must independently cover
        // it too, via `uses_of`-style reasoning over Terminators).
        let mut f = lowered("if x > 0.0 then 1.0 else 2.0");
        Dce.run(&mut f);
        assert!(forge_ir::verify::verify(&f).is_ok());
        // The comparison feeding the branch must still be present.
        assert!(f.insts.iter().any(|i| matches!(i, Inst::Cmp { .. })));
    }
}
```

- [ ] **Step 2: Run to confirm failure**

- [ ] **Step 3: Implement**

```rust
// crates/forge-opt/src/dce.rs — above the test module

use rustc_hash::FxHashSet;

use forge_ir::*;

/// Worklist-based reachability from every block's terminator operand
/// (`Return`'s value, `Branch`'s condition -- `Jump` has none), following
/// `uses_of` backward transitively. Anything never reached is dead. Sweeps
/// by filtering each block's `insts` list -- the underlying `f.insts` Vec
/// keeps now-unreferenced entries in place (consistent with how the SSA
/// builder already leaves dead trivial-phis after `replace_all_uses`; no
/// renumbering needed).
pub struct Dce;

impl crate::Pass for Dce {
    fn name(&self) -> &'static str {
        "dce"
    }
    fn run(&mut self, f: &mut Function) -> bool {
        let mut used: FxHashSet<Value> = FxHashSet::default();
        let mut worklist: Vec<Value> = Vec::new();

        for block in &f.blocks {
            match &block.term {
                Some(Terminator::Return(v)) => worklist.push(*v),
                Some(Terminator::Branch { cond, .. }) => worklist.push(*cond),
                _ => {}
            }
        }

        while let Some(v) = worklist.pop() {
            if used.insert(v) {
                for operand in uses_of(&f.insts[v.0 as usize]) {
                    worklist.push(operand);
                }
            }
        }

        let mut changed = false;
        for block in &mut f.blocks {
            let before = block.insts.len();
            block.insts.retain(|v| used.contains(v));
            if block.insts.len() != before {
                changed = true;
            }
        }
        changed
    }
}
```

- [ ] **Step 4: Register the pass**

`pub mod dce;` in `lib.rs`. Position it LAST in the pipeline (per the `optimize()` skeleton's comment from Task 1 — after reassociation, since reassociation can expose new dead code, and DCE should run after everything else has had a chance to make something dead).

- [ ] **Step 5: Run tests**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-opt/src/dce.rs crates/forge-opt/src/lib.rs
git commit -m "feat(forge-opt): dead code elimination"
```

---

## Task 8: Reassociation (i64 only)

**Files:**
- Create: `crates/forge-opt/src/reassoc.rs`
- Modify: `crates/forge-opt/src/lib.rs`

- [ ] **Step 1: Write the test module (failing first)**

```rust
// crates/forge-opt/src/reassoc.rs — test module

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

    /// Longest chain of Add/Mul instructions ending at the function's
    /// return value, counted by walking operand-of-operand as far as it
    /// stays within the same associative op.
    fn dependency_depth(f: &Function, v: Value) -> u32 {
        match &f.insts[v.0 as usize] {
            Inst::Add(a, b) | Inst::Mul(a, b) => 1 + dependency_depth(f, *a).max(dependency_depth(f, *b)),
            _ => 0,
        }
    }

    #[test]
    fn reduces_dependency_depth_on_an_i64_sum_chain() {
        let mut f = lowered("((((((a + b) + c) + d) + e) + g) + h) + i");
        let Terminator::Return(root) = f.blocks[f.entry.0 as usize].term.clone().unwrap() else { panic!() };
        let before = dependency_depth(&f, root);
        assert_eq!(before, 7, "sanity: a fully left-leaning 8-term chain has depth 7");
        Reassociate.run(&mut f);
        let Terminator::Return(new_root) = f.blocks[f.entry.0 as usize].term.clone().unwrap() else { panic!() };
        let after = dependency_depth(&f, new_root);
        assert!(after < before, "reassociation should reduce dependency depth (got {after}, was {before})");
    }

    #[test]
    fn does_not_reassociate_f64_chains() {
        // Floating-point addition is not associative under rounding --
        // reassociating without --fast-math would be a real correctness
        // bug (a different, if very close, answer). Confirm this pass
        // leaves an f64 chain's shape untouched.
        let mut f = lowered("((((a + b) + c) + d) + e)");
        let before_insts = f.insts.len();
        let changed = Reassociate.run(&mut f);
        assert!(!changed, "f64 chains must not be reassociated without --fast-math");
        assert_eq!(f.insts.len(), before_insts);
    }

    #[test]
    fn reassociation_never_changes_the_i64_answer() {
        let src = "((((((n + n) + n) + n) + n) + n) + n) + n";
        let unreassoc = lowered(src);
        let expected = interpret(&unreassoc, &[RtValue::I64(7)]);
        let mut reassoc = lowered(src);
        Reassociate.run(&mut reassoc);
        let actual = interpret(&reassoc, &[RtValue::I64(7)]);
        assert_eq!(expected, actual);
    }
}
```

- [ ] **Step 2: Run to confirm failure**

- [ ] **Step 3: Implement**

Scope: only `Inst::Add`/`Inst::Mul` chains where every value in the chain is `Ty::I64` — verify this type gate explicitly rather than assuming it, since getting it wrong (reassociating an f64 chain) is a real correctness bug, not just a missed optimization. A reasonably simple correct approach for this scale (expression trees, not huge basic blocks): collect a maximal chain of the same associative op (e.g. walk `Add(Add(Add(a,b),c),d)` and flatten to the leaf operand list `[a,b,c,d]`), then rebuild a balanced tree from that flat list (e.g. pair them up: `(a+b)+(c+d)` for 4 leaves, recursively for more) instead of the original left-leaning chain. Emit the new balanced instructions, then redirect the original root's uses to the new balanced root via `replace_value_everywhere` (leaving the old chain for DCE to sweep).

Design your own exact flattening/rebalancing code — this is a well-scoped, self-contained tree transform; use the tests above (especially the depth-reduction and the f64-must-not-fire tests) to verify correctness as you build it, the same way you would for any other pass in this plan.

- [ ] **Step 4: Register the pass**

`pub mod reassoc;` in `lib.rs`. Position: after GVN, before DCE (matches the `optimize()` skeleton comment from Task 1 and SPEC.md §6.5's pipeline order).

- [ ] **Step 5: Run tests**

- [ ] **Step 6: Commit**

```bash
git add crates/forge-opt/src/reassoc.rs crates/forge-opt/src/lib.rs
git commit -m "feat(forge-opt): i64-only reassociation"
```

---

## Task 9: `-O0 == -O2` differential property test

**Files:**
- Create: `crates/forge-opt/tests/differential.rs`

- [ ] **Step 1: Write the property test**

This is the core correctness invariant for the whole optimizer, exercised broadly rather than case-by-case. Reuse the small `arb_expr`-style generator pattern from `crates/forge-syntax/tests/roundtrip.rs` (Phase 0-3), extended to cover the ops this optimizer actually touches (arithmetic, comparisons, `if`, intrinsics, and — importantly — integer literals/bitwise ops, since strength reduction only fires on i64).

```rust
// crates/forge-opt/tests/differential.rs

use forge_ir::interp::{interpret, RtValue};
use forge_syntax::lexer::lex;
use forge_syntax::parser::parse;
use forge_syntax::resolve::resolve;
use forge_syntax::typeck::typecheck;
use proptest::prelude::*;

fn arb_expr() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        (0.1f64..1000.0).prop_map(|f| format!("{f:.3}")),
        (1i64..1000).prop_map(|n| n.to_string()),
        Just("x".to_string()),
        Just("y".to_string()),
        Just("n".to_string()),
    ];
    leaf.prop_recursive(4, 64, 8, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} + {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} - {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} * {b})")),
            (inner.clone(), inner.clone(), inner.clone())
                .prop_map(|(c, t, e)| format!("(if {c} > 0.0 then {t} else {e})")),
            inner.clone().prop_map(|a| format!("sqrt({a} * {a})")),
            inner.clone().prop_map(|a| format!("abs({a})")),
        ]
    })
}

fn params_for(f: &forge_ir::Function) -> Vec<RtValue> {
    f.params
        .iter()
        .map(|(_, ty)| match ty {
            forge_ir::Ty::F64 => RtValue::F64(2.5),
            forge_ir::Ty::I64 => RtValue::I64(7),
            forge_ir::Ty::Bool => RtValue::Bool(true),
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn optimized_matches_unoptimized(src in arb_expr()) {
        let (tokens, diags) = lex(&src);
        prop_assume!(diags.is_empty());
        let (ast, diags) = parse(&tokens);
        prop_assume!(diags.is_empty());
        let Ok(typed) = typecheck(resolve(ast)) else { return Ok(()); };
        let unoptimized = forge_ir::lower::lower(&typed);
        let mut optimized = forge_ir::lower::lower(&typed);
        forge_opt::optimize(&mut optimized);

        prop_assert!(forge_ir::verify::verify(&optimized).is_ok(), "optimized IR failed verification for {src:?}");

        let args = params_for(&unoptimized);
        let expected = interpret(&unoptimized, &args);
        let actual = interpret(&optimized, &args);
        match (expected, actual) {
            (RtValue::F64(e), RtValue::F64(a)) => {
                if e.is_nan() {
                    prop_assert!(a.is_nan(), "NaN-ness mismatch for {src:?}");
                } else {
                    prop_assert_eq!(e.to_bits(), a.to_bits(), "mismatch for {src:?}: {} vs {}", e, a);
                }
            }
            (e, a) => prop_assert_eq!(e, a, "mismatch for {src:?}"),
        }
    }
}
```

Note: `params_for` uses fixed values (`2.5`, `7`, `true`) rather than varying per-case — this is a deliberate simplification (proptest's `arb_expr` already varies the EXPRESSION shape across 2000 cases, which is the dimension most likely to expose an optimizer bug; varying argument values too would be a nice-to-have but isn't required for this task). If you want to also vary argument values, that's a reasonable enhancement, not required.

- [ ] **Step 2: Run the test**

Run: `cargo test -p forge-opt --test differential 2>&1 | tail -30`
Expected: passes 2000 cases. If it finds a genuine counterexample, that's a REAL BUG in one of Tasks 2-8's passes — do not weaken the test to make it pass; go find and fix the actual bug in the pass that produced the wrong answer (proptest will shrink the failing case to a minimal reproduction, which will tell you which construct is involved).

- [ ] **Step 3: Commit**

```bash
git add crates/forge-opt/tests/differential.rs
git commit -m "test(forge-opt): -O0 == -O2 differential property test"
```

---

## Task 10: Final verification pass

**Files:** none created — this task only runs checks.

- [ ] **Step 1: Full workspace test run**

Run: `cargo test --workspace 2>&1 | tail -50`
Expected: every test across `forge-syntax`, `forge-ir`, and now `forge-opt` passes. No regressions in the 92 pre-existing tests.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace -- -D warnings`

- [ ] **Step 3: Format check**

Run: `cargo fmt --check` (or `cargo fmt` then re-check, matching the pattern established in Phase 0-3 where this was easy to forget)

- [ ] **Step 4: Confirm the day-one spike still works** (sanity check that nothing in this slice touched `forge-mem`)

Run: `make spike`

- [ ] **Step 5: Confirm SPEC.md's two corrected rules match what's implemented**

Read SPEC.md §6.2 (the `x + (-0.0) -> x` rule) and §6.3 (the signed `x % 2^k` caveat) and confirm `simplify.rs`/`strength.rs` actually implement exactly what those sections now say — this is the exact class of drift the Phase 0-3 final review caught (docs promising something the code doesn't do). If anything drifted during implementation (e.g. Task 5's magic-division scope finding), update SPEC.md/CHECKLIST.md to match reality rather than leaving stale claims.

- [ ] **Step 6: Report exit criteria status**

Confirm all 5 exit criteria from the design doc are met, and summarize any scope findings from Task 5 (magic division wiring) and Task 9 (whether the differential test found and required fixing any real bugs) for the final report.
