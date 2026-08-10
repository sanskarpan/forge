# Phase 8e — Integration Tests & Benchmark Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close out CHECKLIST.md Phase 8's final 6 bullets (19-24): 3 new external integration tests, 2 existing-test cross-references, and a new criterion benchmark that also requires a real, scoped performance fix to get close to its stated target.

**Architecture:** One new external test file (`crates/forge-regalloc/tests/integration.rs`, public-API-only), one new benchmark file (`crates/forge-regalloc/benches/allocation.rs`), a small internal `LinearScan` hashing swap (`std::collections::HashMap`/`HashSet` → `rustc_hash::FxHashMap`/`FxHashSet` for exactly 3 internal fields and their feeders — added ADDITIVELY alongside the existing `std` import, not replacing it, so the ~8 unrelated `HashSet<PhysReg>` usages already in `linear_scan.rs`'s own test module need zero changes), and CHECKLIST.md annotations.

**Tech Stack:** Rust, `crates/forge-regalloc`, adds `criterion` (already a workspace dependency, unused until now) and `rustc-hash` (already a workspace dependency, used by 3 other crates, not yet this one) as new dev-/regular dependencies of this one crate.

**Design doc:** `docs/superpowers/specs/2026-08-10-phase-8e-integration-tests-benchmark-design.md` — execution-verified through two full review rounds (the first found bullet 19's original test was impossible to build as described, a vacuous non-strict predicate in bullet 22, a factually wrong description of an existing test for bullet 23, and a real ~3x perf gap in bullet 24; the second re-ran every corrected piece and reproduced its exact numbers, including the real before/after benchmark measurement: 134.6 µs → 56.8 µs after the hashing swap — closes most but not all of the gap to 50 µs, recorded honestly rather than force-fit). Treat every code block and every measured number below as verified, not merely proposed.

---

## Before you start

Read `crates/forge-regalloc/src/linear_scan.rs` and `crates/forge-regalloc/src/interval.rs` in full — Task 3 modifies `linear_scan.rs`'s internals; Tasks 1 and 2 only add new files / touch one stale comment. Confirm the baseline: `cargo test -p forge-regalloc` currently shows 80 passing tests (Phase 8d's shipped state), and neither a `tests/` nor a `benches/` directory exists yet under `crates/forge-regalloc/`.

All `Interval` fields are `pub` (confirmed in `interval.rs`), and `forge_regalloc::{build_intervals, excluded_registers, allocate, verify_allocation, Interval, RegClass, Location, SPILL_AWARE_ALLOCATABLE_GPR}` are all re-exported from `crates/forge-regalloc/src/lib.rs` — everything Tasks 1 and 3 need from this crate's own public surface.

---

### Task 1: `tests/integration.rs` — bullets 19, 20, 22

**Files:**
- Create: `crates/forge-regalloc/tests/integration.rs`

- [ ] **Step 1: Write bullet 19's test (hand-built I64 function, 3 GPR values, no spills)**

```rust
// crates/forge-regalloc/tests/integration.rs
use forge_ir::Value;
use forge_regalloc::{allocate, build_intervals, excluded_registers, verify_allocation, Location};
use forge_syntax::span::Span;

/// CHECKLIST Phase 8 bullet 19: "Test: 3 values, 16 registers -> no
/// spills". "16 registers" is stale CHECKLIST wording from before Phase
/// 8c introduced SCRATCH_GPR/XMM reservation -- the real pool this test
/// runs against is SPILL_AWARE_ALLOCATABLE_GPR (12), via the real
/// allocate() the crate actually ships. A real 3-VARIABLE source program
/// cannot produce exactly 3 values (3 Params + >=1 combining op is always
/// >=4 values, and untyped surface arithmetic lowers to F64/XMM anyway,
/// not GPR) -- confirmed by execution during design review. A hand-built
/// I64 function (2 Params + 1 Add = exactly 3 values) goes through the
/// SAME real select -> build_intervals -> allocate pipeline; only the
/// front-end source-text stage is bypassed, not any part of what this
/// bullet is actually testing.
#[test]
fn bullet_19_three_values_no_spills() {
    let mut b = forge_ir::builder::Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        forge_ir::Inst::Param { index: 0, ty: forge_ir::Ty::I64 },
        forge_ir::Ty::I64,
        Span::new(0, 0),
    );
    let y = b.emit(
        entry,
        forge_ir::Inst::Param { index: 1, ty: forge_ir::Ty::I64 },
        forge_ir::Ty::I64,
        Span::new(0, 0),
    );
    let sum = b.emit(
        entry,
        forge_ir::Inst::Add(x, y),
        forge_ir::Ty::I64,
        Span::new(0, 0),
    );
    b.f.blocks[entry.0 as usize].term = Some(forge_ir::Terminator::Return(sum));

    let selected = forge_x64::select(&b.f);
    let intervals = build_intervals(&b.f, &selected);
    let excluded = excluded_registers(&b.f, &selected);

    assert_eq!(intervals.len(), 3, "2 Params + 1 Add must produce exactly 3 values");
    for iv in &intervals {
        assert_eq!(
            iv.reg_class,
            forge_regalloc::RegClass::Gpr,
            "I64 params/results must be GPR-class"
        );
    }

    let (assignment, bytes) = allocate(intervals.clone(), &excluded, &selected);

    for iv in &intervals {
        assert!(
            matches!(assignment.get(&iv.value), Some(Location::Reg(_))),
            "3 values into a 12-register pool must never spill: {:?}",
            iv.value
        );
    }
    assert_eq!(bytes, 0);
    assert!(verify_allocation(&intervals, &assignment).is_ok());
}
```

- [ ] **Step 2: Run to verify it passes**

Run: `cargo test -p forge-regalloc --test integration bullet_19`
Expected: PASS (1 test). If it fails to compile, double-check the exact `Builder`/`Inst`/`Terminator` API shape against `crates/forge-regalloc/src/linear_scan.rs`'s own `run_allocates_a_straight_line_chain_via_transfers` test, which uses the identical pattern (just via `crate::` internal paths instead of the `forge_regalloc::` public re-exports this external test file must use).

- [ ] **Step 3: Write bullet 20's test (40 forced-overlapping GPR intervals, forced spilling, independently verified)**

Add to `crates/forge-regalloc/tests/integration.rs`:

```rust
/// CHECKLIST Phase 8 bullet 20: "Test: 40 simultaneously live values, 16
/// registers -> correct results with spills". "Correct RESULTS" (i.e.
/// verified against real program execution) needs the not-yet-built
/// MachineInst-to-bytes emission pipeline (task #68) and is out of scope
/// here -- what's checkable now is that the ALLOCATION itself is sound
/// (independently verified via verify_allocation), which is the
/// load-bearing precondition for execution correctness once emission
/// exists. On this specific fixture (every interval hint: None, so the
/// handoff exemption never fires, and every interval shares one
/// identical range so no two spilled values can ever land in the same
/// slot) verify_allocation's Ok is a real but narrow regression guard --
/// it can only fail if allocate() double-books a register outright.
#[test]
fn bullet_20_forty_live_values_forces_spilling_and_stays_valid() {
    let intervals: Vec<forge_regalloc::Interval> = (0..40)
        .map(|n| forge_regalloc::Interval {
            value: Value(n),
            start: 0,
            end: 50,
            reg_class: forge_regalloc::RegClass::Gpr,
            hint: None,
            fixed: None,
            spill_weight: 0.0,
        })
        .collect();

    let selected = forge_x64::SelectedFunction {
        insts: Vec::new(),
        synthetic_types: std::collections::HashMap::new(),
        coalescing_hints: std::collections::HashMap::new(),
        pool: forge_x64::ConstantPool::default(),
        block_starts: Vec::new(),
    };
    let (assignment, bytes) = allocate(
        intervals.clone(),
        &std::collections::HashMap::new(),
        &selected,
    );

    let spilled = intervals
        .iter()
        .filter(|iv| matches!(assignment.get(&iv.value), Some(Location::Spill(_))))
        .count();
    assert_eq!(
        spilled, 28,
        "40 intervals into a 12-register pool must spill exactly 28"
    );
    assert_eq!(bytes, 224, "28 spills that can never reuse a slot must need exactly 224 bytes");
    assert_eq!(assignment.len(), 40, "every interval must get SOME Location");
    assert!(verify_allocation(&intervals, &assignment).is_ok());
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p forge-regalloc --test integration bullet_20`
Expected: PASS (1 test).

- [ ] **Step 5: Write bullet 22's test (real libm program, strict cross-call liveness, call-clobber hazard confirmed real)**

Add to `crates/forge-regalloc/tests/integration.rs`:

```rust
/// CHECKLIST Phase 8 bullet 22: "Test: expression calling libm ->
/// caller-saved values are spilled around the call". "Spilled around the
/// call" describes an EMISSION-time save/restore sequence -- the exact
/// same category of problem as `idiv`'s third-party rax/rdx clobber
/// (Phase 8c's design doc: "resolvable at emission time via ordinary
/// stack push/pop for the displaced occupants"), deferred to the
/// not-yet-built emission pipeline (task #68), same as bullet 20. What's
/// checkable and worth checking NOW: (1) verify_allocation returns Ok,
/// confirming the CURRENT, documented scope boundary (this allocator
/// does not model call clobbers -- see verify.rs's own doc comment,
/// added in Phase 8d's holistic review, commit 53193fb); (2) at least
/// one XMM interval's range STRICTLY contains a real CallLibm's
/// position, proving the hazard is REAL on this program, not a
/// hypothetical the test can't actually trigger. The strict predicate
/// (`iv.start < pos && pos < iv.end`) is required, not the inclusive
/// `<=`/`<=` form -- confirmed by execution during design review that
/// the inclusive form is trivially satisfiable by any libm call's own
/// argument/result intervals with ZERO genuine cross-call liveness
/// (e.g. `sin(x)` alone scores 2 hits under the inclusive form and 0
/// under the strict one -- the strict form is what actually distinguishes
/// "genuinely live across the call" from "merely borders the call").
#[test]
fn bullet_22_libm_call_clobber_hazard_is_real_and_currently_unverified() {
    let src = "sin(x) + cos(y) + x + y";
    let (tokens, diags) = forge_syntax::lexer::lex(src);
    assert!(diags.is_empty(), "lex errors: {diags:?}");
    let (ast, diags) = forge_syntax::parser::parse(&tokens);
    assert!(diags.is_empty(), "parse errors: {diags:?}");
    let typed = forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast))
        .unwrap_or_else(|e| panic!("type errors: {e:?}"));
    let func = forge_ir::lower::lower(&typed);

    let selected = forge_x64::select(&func);
    let intervals = build_intervals(&func, &selected);
    let excluded = excluded_registers(&func, &selected);
    let (assignment, _bytes) = allocate(intervals.clone(), &excluded, &selected);

    assert!(
        verify_allocation(&intervals, &assignment).is_ok(),
        "current, documented scope boundary: this allocator doesn't model call clobbers"
    );

    let call_positions: Vec<usize> = selected
        .insts
        .iter()
        .enumerate()
        .filter(|(_, inst)| matches!(inst, forge_x64::MachineInst::CallLibm { .. }))
        .map(|(pos, _)| pos)
        .collect();
    assert!(!call_positions.is_empty(), "this program must contain at least one CallLibm");

    let hazard_is_real = intervals.iter().any(|iv| {
        iv.reg_class == forge_regalloc::RegClass::Xmm
            && call_positions
                .iter()
                .any(|&pos| (iv.start as usize) < pos && pos < (iv.end as usize))
    });
    assert!(
        hazard_is_real,
        "no XMM interval is genuinely live across a CallLibm on this program -- the test is \
         vacuous and doesn't actually exercise the hazard it claims to"
    );
}
```

- [ ] **Step 6: Run to verify it passes**

Run: `cargo test -p forge-regalloc --test integration bullet_22`
Expected: PASS (1 test).

- [ ] **Step 7: Run the whole new test file together**

Run: `cargo test -p forge-regalloc --test integration`
Expected: PASS (3 tests).

- [ ] **Step 8: Commit**

```bash
cd /Users/sanskar/dev/Research/Projects/JIT-Compiler
git add crates/forge-regalloc/tests/integration.rs
git commit -m "test(forge-regalloc): add bullet 19/20/22 integration tests"
```

---

### Task 2: Bullets 21 and 23 — cross-references, plus one stale comment fix

**Files:**
- Modify: `crates/forge-regalloc/src/linear_scan.rs`

- [ ] **Step 1: Confirm bullet 21 needs no new code**

`crates/forge-regalloc/src/verify.rs`'s `catches_a_deliberately_broken_allocation` test (Phase 8d) already does exactly what CHECKLIST bullet 21 asks. Run: `cargo test -p forge-regalloc --lib verify::tests::catches_a_deliberately_broken_allocation` — Expected: PASS (already passing, shipped in Phase 8d; this step is a confirmation, not new work). No file changes for this step.

- [ ] **Step 2: Fix `run_allocates_a_straight_line_chain_via_transfers`'s stale doc comment**

This test (Phase 8b) already fully satisfies bullet 23's substance, but its own doc comment is factually wrong (traced as the source of an error an earlier draft of the Phase 8e design doc copied from it). Find the comment above `run_allocates_a_straight_line_chain_via_transfers` in `crates/forge-regalloc/src/linear_scan.rs` — it currently reads something like:

```rust
    #[test]
    fn run_allocates_a_straight_line_chain_via_transfers() {
        // a = x + 1; b = a + 1; c = b + 1 -- three successive two-address
        // handoffs, all through the same register.
```

Change the comment to accurately describe what the test actually builds and asserts (2 `Add`s, not 3; `x`/`a`/`c` share one register via two Case 2 handoffs; the constant `one` correctly does NOT share it, though the test itself doesn't separately assert that distinction):

```rust
    #[test]
    fn run_allocates_a_straight_line_chain_via_transfers() {
        // x = param; one = 1; a = x + one; c = a + one -- two successive
        // two-address handoffs (x -> a, a -> c), all through the same
        // register. `one` (read twice, live across both adds) correctly
        // gets a DIFFERENT register in the real allocator's output --
        // this test doesn't separately assert that distinction, only that
        // x/a/c share one register, which is the coalescing property
        // bullet 23 ("coalescing eliminates redundant mov for a
        // two-address chain") is actually about.
```

Do not change any code below the comment — only the comment text. Run: `cargo test -p forge-regalloc --lib linear_scan::tests::run_allocates_a_straight_line_chain_via_transfers` — Expected: PASS (unchanged behavior, comment-only edit).

- [ ] **Step 3: Commit**

```bash
git add crates/forge-regalloc/src/linear_scan.rs
git commit -m "docs(forge-regalloc): correct stale two-address-chain test comment"
```

---

### Task 3: Bullet 24 — benchmark, baseline measurement, hashing swap, re-measurement

**Files:**
- Modify: `crates/forge-regalloc/Cargo.toml`
- Create: `crates/forge-regalloc/benches/allocation.rs`
- Modify: `crates/forge-regalloc/src/linear_scan.rs`

- [ ] **Step 1: Add the benchmark dependency and target**

Change `crates/forge-regalloc/Cargo.toml`'s `[dev-dependencies]` section:

```toml
[dev-dependencies]
forge-syntax = { path = "../forge-syntax" }
smallvec.workspace = true
criterion.workspace = true

[[bench]]
name = "allocation"
harness = false
```

- [ ] **Step 2: Write the benchmark**

Create `crates/forge-regalloc/benches/allocation.rs`:

```rust
use criterion::{criterion_group, criterion_main, Criterion};
use forge_ir::Value;
use forge_regalloc::{allocate, Interval, RegClass};
use std::collections::HashMap;

/// 1000 values, staggered SHORT-lived ranges (NOT all-overlapping -- the
/// all-overlapping shape belongs to bullet 20's correctness stress test
/// in tests/integration.rs, not this performance benchmark, which should
/// reflect realistic throughput: mostly short-lived SSA values with
/// localized overlap, not one maximally-adversarial all-live-at-once
/// block). Split evenly GPR/XMM to exercise both of allocate()'s passes.
fn thousand_value_intervals() -> Vec<Interval> {
    (0..1000)
        .map(|n| Interval {
            value: Value(n),
            start: n,
            end: n + 4,
            reg_class: if n % 2 == 0 { RegClass::Gpr } else { RegClass::Xmm },
            hint: None,
            fixed: None,
            spill_weight: 0.0,
        })
        .collect()
}

fn bench_allocate(c: &mut Criterion) {
    let intervals = thousand_value_intervals();
    let selected = forge_x64::SelectedFunction {
        insts: Vec::new(), // spill_weight isn't what this benchmark measures
        synthetic_types: HashMap::new(),
        coalescing_hints: HashMap::new(),
        pool: forge_x64::ConstantPool::default(),
        block_starts: Vec::new(),
    };
    c.bench_function("allocate_1000_values", |b| {
        b.iter(|| allocate(intervals.clone(), &HashMap::new(), &selected))
    });
}

criterion_group!(benches, bench_allocate);
criterion_main!(benches);
```

- [ ] **Step 3: Run the BASELINE benchmark (before the hashing swap) and record the number**

Run: `cargo bench -p forge-regalloc --bench allocation`
Expected: reports a real timing number for `allocate_1000_values`. Execution-based design review measured ~130-145 µs on this exact benchmark against a `std::collections::HashMap`/`HashSet`-based `LinearScan` — confirm you observe something in that neighborhood (machine-dependent, won't be identical, but should be the same order of magnitude, not e.g. 10x different). Write down the exact number you observe; it goes into CHECKLIST in Step 8.

- [ ] **Step 4: Add the `rustc-hash` dependency**

Add to `crates/forge-regalloc/Cargo.toml`'s `[dependencies]` section (a REGULAR dependency, not dev — `LinearScan` itself, not just its tests/benches, uses it):

```toml
[dependencies]
forge-ir = { path = "../forge-ir" }
forge-x64 = { path = "../forge-x64" }
rustc-hash.workspace = true
```

- [ ] **Step 5: Swap `LinearScan`'s three internal fields to `FxHashMap`/`FxHashSet` — ADDITIVELY, not replacing the existing `std` import**

In `crates/forge-regalloc/src/linear_scan.rs`, change the top-level imports from:

```rust
use crate::interval::{Interval, RegClass};
use crate::liveness::reads_of;
use forge_ir::Value;
use forge_x64::{PhysReg, SelectedFunction};
use std::collections::{HashMap, HashSet};
```

to (ADD the `rustc_hash` import, KEEP the `std::collections` one exactly as-is — this is deliberate: ~8 existing tests in this file's own `#[cfg(test)] mod tests` construct plain `HashSet<PhysReg>` for unrelated pool-membership checks, e.g. `scratch_and_spill_aware_pools_are_disjoint_and_union_complete_gpr`, and must keep meaning `std::collections::HashSet` unchanged — only the specific declaration sites touched below switch to the `Fx`-prefixed names explicitly):

```rust
use crate::interval::{Interval, RegClass};
use crate::liveness::reads_of;
use forge_ir::Value;
use forge_x64::{PhysReg, SelectedFunction};
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::{HashMap, HashSet};
```

Change `precompute_excluded`'s signature and body from:

```rust
fn precompute_excluded(
    excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>,
) -> HashMap<Value, HashSet<PhysReg>> {
    let mut out: HashMap<Value, HashSet<PhysReg>> = HashMap::new();
    for (&(_, value), regs) in excluded_registers {
        out.entry(value).or_default().extend(regs.iter().copied());
    }
    out
}
```
to (the PARAMETER type stays `std::collections::HashMap` — it's `allocate()`'s public `excluded_registers` argument type, untouched by this swap; only the RETURN type and its internal accumulator change):
```rust
fn precompute_excluded(
    excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>,
) -> FxHashMap<Value, FxHashSet<PhysReg>> {
    let mut out: FxHashMap<Value, FxHashSet<PhysReg>> = FxHashMap::default();
    for (&(_, value), regs) in excluded_registers {
        out.entry(value).or_default().extend(regs.iter().copied());
    }
    out
}
```

Change `EMPTY_EXCLUSION_SET` from:
```rust
// `HashSet::new()` is not `const fn`, so this needs a lazily-initialized
// static, not a plain `static _: HashSet<_> = HashSet::new();` (a compile
// error).
static EMPTY_EXCLUSION_SET: std::sync::LazyLock<HashSet<PhysReg>> =
    std::sync::LazyLock::new(HashSet::new);
```
to:
```rust
// `FxHashSet::default()` is not `const fn`, so this needs a
// lazily-initialized static, not a plain
// `static _: FxHashSet<_> = FxHashSet::default();` (a compile error).
static EMPTY_EXCLUSION_SET: std::sync::LazyLock<FxHashSet<PhysReg>> =
    std::sync::LazyLock::new(FxHashSet::default);
```

Change the `LinearScan` struct's three fields from:
```rust
pub struct LinearScan<'a> {
    intervals: Vec<Interval>,
    active: Vec<usize>,
    free_regs: HashSet<PhysReg>,
    assignment: HashMap<Value, Location>,
    excluded: HashMap<Value, HashSet<PhysReg>>,
    allocatable: &'a [PhysReg],
    slot_end: Vec<u32>,
}
```
to:
```rust
pub struct LinearScan<'a> {
    intervals: Vec<Interval>,
    active: Vec<usize>,
    free_regs: FxHashSet<PhysReg>,
    assignment: FxHashMap<Value, Location>,
    excluded: FxHashMap<Value, FxHashSet<PhysReg>>,
    allocatable: &'a [PhysReg],
    slot_end: Vec<u32>,
}
```

Change `LinearScan::new`'s `assignment: HashMap::new(),` line to `assignment: FxHashMap::default(),` (the `free_regs: allocatable.iter().copied().collect(),` line needs NO change — `.collect()` infers its target type from the now-`FxHashSet<PhysReg>` field type automatically; `excluded: precompute_excluded(excluded_registers),` also needs no change, since `precompute_excluded`'s return type already changed above).

Change `excluded_at`'s return type from `-> &HashSet<PhysReg>` to `-> &FxHashSet<PhysReg>` (its body, `self.excluded.get(&value).unwrap_or(&EMPTY_EXCLUSION_SET)`, needs no change).

- [ ] **Step 6: Fix the one existing test that reads `precompute_excluded`'s output directly**

Find `precompute_excluded_unions_per_value_across_positions` in `linear_scan.rs`'s test module. It currently has:

```rust
        let v1: HashSet<PhysReg> = excluded[&Value(1)].clone();
```

Change to remove the now-stale explicit type (let it infer, since `excluded`'s type changed):

```rust
        let v1 = excluded[&Value(1)].clone();
```

- [ ] **Step 7: Run the full crate test suite**

Run: `cargo test -p forge-regalloc 2>&1 | tail -30`
Expected: PASS, all 83 tests (80 pre-existing + 3 new from Task 1's `tests/integration.rs`, which is a SEPARATE test binary from `--lib` — `cargo test -p forge-regalloc` without `--lib`/`--test` runs both together; confirm the combined total). If anything in the ~8 untouched pool-membership tests (`scratch_and_spill_aware_pools_are_disjoint_and_union_complete_gpr` etc.) fails to compile, you've accidentally removed or shadowed the `std::collections::HashSet` import — re-check Step 5 added the `rustc_hash` import ADDITIVELY rather than replacing the `std::collections` one.

- [ ] **Step 8: Re-run the benchmark (AFTER the swap) and record the real number**

Run: `cargo bench -p forge-regalloc --bench allocation`
Expected: a real number, meaningfully lower than Step 3's baseline. Execution-based design review measured **56.8 µs** after this exact swap (down from a 134.6 µs baseline, a 58% reduction) — still short of the 50 µs target by roughly 14%, NOT a full pass. Record whatever real number you observe (machine-dependent) — do NOT adjust the benchmark's workload or add further optimizations to force a specific number; this step's job is an honest measurement, not hitting a target by any means necessary. If your number is close to 56.8 µs (say, within 20-30%), that's the expected, correct outcome — proceed. If it's wildly different in either direction, note it in your final report but still proceed (machine variance is real and not a reason to loop on this step).

- [ ] **Step 9: Run clippy and fmt**

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -60`
Expected: clean (this now covers the new `tests/` and `benches/` targets too).

Run: `cargo fmt --check`
Expected: clean. If it reports diffs, run `cargo fmt` and re-check.

- [ ] **Step 10: Commit**

```bash
git add crates/forge-regalloc/Cargo.toml crates/forge-regalloc/benches/allocation.rs crates/forge-regalloc/src/linear_scan.rs
git commit -m "perf(forge-regalloc): add allocation benchmark, swap LinearScan to FxHashMap/FxHashSet"
```

---

### Task 4: CHECKLIST.md annotations and final workspace verification

**Files:**
- Modify: `CHECKLIST.md`

- [ ] **Step 1: Annotate bullets 19-24**

Find CHECKLIST.md's Phase 8 section (bullets 19-24, currently reading "Test: 3 values, 16 registers → no spills" through "Benchmark: allocation of 1000 values < 50 µs"). Following the exact convention established by every prior Phase 8 sub-slice's annotations (append `— **note (Phase 8e):** ...` to each bullet, ending with a `Details:` pointer to this phase's design doc where non-trivial), add:

- Bullet 19: note that "16 registers" is stale (real pool is `SPILL_AWARE_ALLOCATABLE_GPR`, 12) and the test uses a hand-built I64 function since no real 3-variable source program produces exactly 3 GPR values.
- Bullet 20: note that "correct results" (execution-verified) needs task #68 and is out of scope; what's built is allocation-level validity via `verify_allocation`.
- Bullet 21: note this was already satisfied by Phase 8d's `catches_a_deliberately_broken_allocation` test — no new code.
- Bullet 22: note "spilled around the call" is an emission-time concern (task #68, same category as `idiv`'s clobber, per Phase 8c's precedent) and is out of scope; what's built confirms the hazard is real via a strict cross-call-liveness check, and that `verify_allocation` doesn't currently catch it (a documented, not accidental, gap — see `verify.rs`'s own doc comment from Phase 8d).
- Bullet 23: note this was already satisfied by Phase 8b's `run_allocates_a_straight_line_chain_via_transfers` test (its stale doc comment corrected in this phase) — no new test code.
- Bullet 24: record the REAL measured before/after numbers from Task 3 Steps 3 and 8 (your actual observed numbers, not the design doc's example numbers if yours differ) — state plainly whether the 50 µs target was met or not. If not met, say so as a known, honest, scoped limitation (consistent with how `evict_and_assign`'s deferred victim case and reload/store insertion are already recorded elsewhere in this file), not as a success.

Use `Details: docs/superpowers/specs/2026-08-10-phase-8e-integration-tests-benchmark-design.md` for bullets whose reasoning is non-trivial (20, 22, 24 at minimum).

- [ ] **Step 2: Commit the CHECKLIST update**

```bash
git add CHECKLIST.md
git commit -m "docs: Phase 8e CHECKLIST annotations"
```

- [ ] **Step 3: Final full-workspace verification**

Run: `cargo test --workspace 2>&1 | tail -40`
Expected: all green.

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -60`
Expected: clean.

Run: `cargo fmt --check`
Expected: clean.

---

## Self-review notes (already applied above, recorded for the implementer's context)

- **Spec coverage**: every one of the 6 bullets (19-24) has a corresponding task step above — 3 new tests (Task 1), 2 cross-references plus 1 stale-comment fix (Task 2), 1 new benchmark plus a real, measured performance fix (Task 3), and CHECKLIST annotations (Task 4).
- **Type consistency check**: `allocate(intervals: Vec<Interval>, excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>, selected: &SelectedFunction) -> (HashMap<Value, Location>, u32)` (the crate's PUBLIC signature) is used identically across Task 1's three tests and Task 3's benchmark — confirmed unaffected by Task 3's internal `FxHashMap`/`FxHashSet` swap, which is scoped to `LinearScan`'s three private fields and never touches this public signature.
- **Placeholder scan**: no task above contains a TBD, a "handle appropriately," or an unshown code block. Task 3 Step 8 deliberately does NOT prescribe a required outcome number (only reports what's actually measured) — this is intentional honesty, not an unfinished step.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-08-10-phase-8e-integration-tests-benchmark-plan.md`. Per this project's established cadence, this plan is next sent to a dispatched subagent for its own execution-based review before subagent-driven implementation begins.
