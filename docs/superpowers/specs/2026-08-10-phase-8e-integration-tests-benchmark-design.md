# Design: forge Phase 8e — Integration Tests & Benchmark

**Status:** Approved for planning
**Scope:** Per `docs/superpowers/specs/2026-08-09-phase-8-decomposition-design.md`, CHECKLIST.md Phase 8 bullets 19-24 — the six closing bullets of Phase 8, all "integration test" or "benchmark" in nature rather than new allocator logic. Lives in `crates/forge-regalloc` (new `tests/` and `benches/` directories — external, public-API-only test/bench crates, not `#[cfg(test)]` modules inside `src/`, since these are genuinely cross-function integration checks, not unit tests of one file).
**Input:** Every function Phase 8a-8d shipped (`build_intervals`, `excluded_registers`, `allocate`, `verify_allocation`, `register_pressure`), all already re-exported from `crates/forge-regalloc/src/lib.rs`.
**Out of scope**: everything requiring real byte emission or JIT execution — see "Why two of these six bullets can't be built as literally worded" below, the single most important section of this doc.

## Why two of these six bullets can't be built as literally worded, and what this design does instead

Four of the six bullets (19, 21, 23, 24) are directly buildable against what Phase 8a-8d shipped. Two (20, 22) are not, as literally worded, because they presuppose infrastructure that doesn't exist yet:

- **Bullet 20** says "40 simultaneously live values, 16 registers → CORRECT RESULTS with spills." "Correct results" can only mean one thing in a compiler context: run the compiled code and check its output against a known-good answer (the interpreter, per Phase 11's eventual differential-testing scope). That requires the "final code-emission pipeline" (task #68 in this project's tracking — MachineInst → real Assembler bytes), which is still `pending`/unbuilt (`crates/forge-x64/src/assembler.rs` has zero references to `MachineInst` as of this writing). Building it is explicitly out of scope for `crates/forge-regalloc`. **Correction to an earlier draft of this doc**: the mmap-based JIT execution HARNESS this would eventually run through is NOT missing — `crates/forge-mem` already ships a complete one (`ExecutableBuffer`, `CompiledExpr::{from_buffer, call1, call2, call_n}`, `CodeCache`, 14 passing tests). The only missing piece is task #68 itself (translating `MachineInst` to real bytes to hand that harness) — narrowing the claim to exactly that, since overstating what's missing would misdirect whoever picks up task #68 next.
- **Bullet 22** says "expression calling libm → caller-saved values are SPILLED AROUND THE CALL." This describes an EMISSION-time code sequence (push/save before the call, pop/restore after) — the exact same category of thing Phase 7e's design doc explicitly deferred ("real byte-level call sequence... explicitly deferred to... 'Final code-emission pipeline'") and Phase 8c's design doc independently reasoned about for `idiv`'s third-party clobber ("resolvable at emission time via ordinary stack `push`/`pop` for the displaced occupants... not the reload mechanism's scratch registers at all"). `CallLibm`'s XMM-clobber-everything is the SAME category of problem as `idiv`'s rax/rdx clobber, and this project has already, consistently, drawn the boundary: THIRD-PARTY clobber from one specific instruction is an emission-time concern, resolved by generating save/restore code around that instruction for whatever happens to be live, not by teaching the allocator a new constraint type. Building the real save/restore bytes needs the same missing emission pipeline as bullet 20.

**Both bullets get a version that's honestly buildable now, clearly labeled as a narrower thing than their literal wording, with the full literal version's dependency stated explicitly:**

- Bullet 20 becomes: 40 simultaneously live values against the REAL pool sizes (not "16 registers" — see below) forces real spills, AND the resulting allocation is independently verified valid (`verify_allocation` returns `Ok`) — "correct" in the one sense checkable without an execution pipeline: the allocation itself is sound. The test's own doc comment states plainly that execution-level "correct results" is deferred to when task #68 and Phase 11's differential testing exist. **Honest caveat, found by execution-based review**: on this specific hand-built fixture (every interval `hint: None`, so the handoff exemption can never fire, and 28 spills landing in 28 distinct slots by construction since every interval shares one identical range), `verify_allocation`'s `Ok` is close to tautological — it can only fail if `allocate()` double-books a register, which is still a real, worth-having regression guard, just a narrower one than "confirms correctness" implies. Stated plainly rather than oversold.
- Bullet 22 becomes: a real libm-calling program is compiled through the full pipeline, its allocation is confirmed independently valid (same `verify_allocation` check), AND — the load-bearing check that makes this test non-vacuous rather than just re-running bullet 21's assertion again — at least one XMM interval is confirmed to genuinely span the `CallLibm` instruction's position, proving the exact hazard Phase 8d's holistic review found (`verify_allocation`'s `Ok` doesn't currently model call clobbers) is REAL on this program, not hypothetical. **Correction to an earlier draft of this doc, found by execution-based review**: "genuinely span the position" must be checked with a STRICT containment (`iv.start < call_pos && call_pos < iv.end`), not the inclusive `iv.start <= call_pos && call_pos <= iv.end` an earlier draft used. Every `CallLibm` trivially has its OWN argument interval ending exactly at it and its OWN result interval starting exactly at it, so the inclusive form is satisfiable on `sin(x)` alone — a program where NOTHING is actually live across the call — which is exactly the vacuity this check exists to rule out. Confirmed by execution: the inclusive predicate scores 2 "hits" on `sin(x)` (zero genuine cross-call liveness); the strict predicate correctly scores 0 there and 5 on `sin(x) + cos(y) + x + y` (where `x` and `y` genuinely are live across both calls). The test's doc comment states plainly that the real save/restore bytes are task #68's job, matching the `idiv`-clobber precedent, and points at `verify.rs`'s own documented blind-spot note (added in Phase 8d's holistic review, commit `53193fb`) for the full reasoning.

**Bullets 19 and 20's "16 registers" is stale wording, not a requirement to satisfy literally.** CHECKLIST.md was written before Phase 8c introduced `SCRATCH_GPR`/`SCRATCH_XMM` reservation — the real pools `allocate()` scans against are `SPILL_AWARE_ALLOCATABLE_GPR` (12) and `SPILL_AWARE_ALLOCATABLE_XMM` (14), confirmed by Phase 8d's holistic review as a known, already-flagged staleness. This design tests against the REAL pool sizes (via the actual exported constants, never a hardcoded `16`), which is what "no spills" / "forces spills" actually depend on.

## Bullet 19 — 3 values, real pool size → no spills

**Correction to an earlier draft of this doc**: "a real compiled 3-variable program" does not exist for GPR — execution-based review confirmed every plain-arithmetic source program lowers untyped surface variables to `F64`/XMM, and a 3-*variable* program always needs at least 2 combining ops, producing ≥5 values (3 `Param`s + ≥2 op results), never exactly 3. Two real options were tried by execution; this design picks the GPR one, for consistency with bullet 20's GPR framing and this design's title bullets both discussing "the real pool":

**A hand-built `forge_ir::builder::Builder` function** — two real `Ty::I64` `Param`s plus one `Add` — produces exactly 3 values, all `RegClass::Gpr`, through the REAL `select` → `build_intervals` → `allocate` pipeline (only the front-end source-text stage is bypassed, not any part of the allocator pipeline this bullet is actually testing — `Builder`, `forge_ir::Terminator`, and `b.f` are all reachable from an external test crate, confirmed by compiling). Confirm all 3 get `Location::Reg`, never `Location::Spill`, against the real `SPILL_AWARE_ALLOCATABLE_GPR` pool (12). Trivially true given the pool vastly exceeds 3, but CHECKLIST wants this specific, named scenario checkable on its own — not just implied by the much larger corpus-wide "never spills" test Phase 8c already ships (`run_produces_only_reg_locations_never_spill_for_the_corpus`, which covers 18 real programs but isn't phrased as "3 values" specifically).

## Bullet 20 — 40 simultaneously live values → forced spilling, independently verified valid

Hand-built (not corpus-derived, matching Phase 8c's own established "synthetic values, not corpus-derived" pattern for pressure scenarios beyond the real corpus's reach — the real corpus tops out at 4 GPR / 7 XMM simultaneous liveness): 40 GPR intervals all sharing one wide overlapping range, forcing `40 - 12 = 28` spills. Assert: every interval gets SOME `Location` (completeness), spill count is exactly 28 (deterministic given identical, maximally-overlapping ranges — same reasoning Phase 8c's own `allocate_spills_under_pressure_with_a_valid_frame_size_and_no_overlapping_slot_reuse` test used for its exact-64-bytes assertion), frame byte count is `28 * 8` (also exact, for the same no-reuse-possible reason), and — the check that makes "correct" meaningful at this layer — `verify_allocation(&intervals, &assignment)` returns `Ok`.

## Bullet 21 — verifier catches a deliberately broken allocation

**Already built.** `crates/forge-regalloc/src/verify.rs`'s `catches_a_deliberately_broken_allocation` test (Phase 8d) does exactly this: takes a real corpus program's real allocation, mutates it to force a genuine conflict, confirms `verify_allocation` returns `Err`. This bullet needs no new code — only a CHECKLIST annotation pointing at the existing test, matching how bullets 1-18 have already cross-referenced work done under a differently-named task where applicable (e.g. bullet 1's "satisfied without a numbering pass" note).

## Bullet 22 — expression calling libm → the call-clobber hazard is real and currently unhandled, confirmed on real data

As scoped in the "why two bullets can't be built as literally worded" section above: compile a real program with an XMM-heavy libm call and enough surrounding register pressure that at least one XMM value's interval genuinely spans the `CallLibm` position (`sin(x) + cos(y) + x + y` — confirmed by execution: `x`/`y` are real, live-across-both-calls values with ranges `[0,5]`/`[1,6]` against `CallLibm`s at positions 2 and 3). Assert two things: (1) `verify_allocation` returns `Ok` (confirms the CURRENT, documented scope boundary — this allocator doesn't model call clobbers, by design, matching the `idiv` precedent, and this is not itself a test failure); (2) at least one XMM-class interval's `[start, end]` STRICTLY contains at least one real `CallLibm` instruction's position (`iv.start < pos && pos < iv.end` — NOT the inclusive `<=`/`<=` form, which is trivially satisfiable by any libm call's own argument/result intervals even with zero genuine cross-call liveness; see the correction above). This is the non-vacuousness check confirming the hazard is REAL on this program, not a hypothetical the test can't actually trigger — in the same spirit as Phase 8c/8d's `checked > 0` assertions elsewhere in this crate.

## Bullet 23 — coalescing eliminates redundant `mov` for a two-address chain

**Already built — but described incorrectly in an earlier draft of this doc, corrected here.** `crates/forge-regalloc/src/linear_scan.rs`'s `run_allocates_a_straight_line_chain_via_transfers` test (Phase 8b) builds `x = Param; one = ConstI64(1); a = x + one; c = a + one` (its own doc comment's `a`/`b`/`c` naming is stale — the Rust variable named `b` is the `Builder` itself, never a chained value; there is no third `Add`, only two: `a = x + one`, `c = a + one`). It confirms `x`, `a`, `c` — the genuinely two-address-CHAINED values (`x → a → c`, two successive `pick_register` Case 2 handoffs) — share ONE physical register, which IS the allocation-level precondition for eliminating a `mov` at each handoff. `one` (the constant operand, read twice, not part of the chain) correctly gets a DIFFERENT register in practice, since it's live across both adds — but the shipped test only asserts `x_loc == a_loc`, `a_loc == c_loc`, and `matches!(x_loc, Reg(_))`; it never separately asserts `one`'s register DIFFERS from the chain's, so this fact is true of the real allocator's output but not itself pinned by an assertion. This design doc's ENGLISH description of the test ("all four values to the SAME physical register") was flatly wrong either way, corrected here regardless. The literal "eliminates a `mov`" claim is about emitted BYTES, which (same as bullets 20/22) needs the not-yet-built emission pipeline to check directly; what's checkable now, and already is, is that the allocator makes the decision that WOULD let emission skip the `mov` for the chained values specifically. This bullet needs no new test code — only a CHECKLIST annotation, PLUS fixing the stale `a`/`b`/`c` doc comment at `linear_scan.rs`'s own `run_allocates_a_straight_line_chain_via_transfers` (a one-line comment fix, not a logic change, bundled into this phase's implementation since this design doc's own error was traced directly to copying it).

## Bullet 24 — Benchmark: allocation of 1000 values < 50 µs

The one bullet in this phase requiring genuinely new infrastructure: this workspace has `criterion = "0.5"` as a workspace dependency (`Cargo.toml`) but NO crate has used it yet — no `benches/` directory, no `[[bench]]` target anywhere in the repo. This design adds the first one.

```toml
# crates/forge-regalloc/Cargo.toml — new additions
[dev-dependencies]
criterion.workspace = true

[[bench]]
name = "allocation"
harness = false
```

```rust
// crates/forge-regalloc/benches/allocation.rs
use criterion::{criterion_group, criterion_main, Criterion};
use forge_regalloc::{allocate, Interval, RegClass};
use forge_ir::Value;
use std::collections::HashMap;

fn thousand_value_intervals() -> Vec<Interval> {
    // 1000 values, staggered short-lived ranges (NOT all-overlapping --
    // realistic allocator workloads are mostly short-lived SSA values with
    // localized overlap, not one maximally-adversarial all-live-at-once
    // block; the all-overlapping shape belongs to bullet 20's correctness
    // stress test, not this performance benchmark, which should reflect
    // realistic throughput). Split evenly GPR/XMM to exercise both passes.
    (0..1000)
        .map(|n| Interval {
            value: Value(n),
            start: n,
            end: n + 4, // short, staggered, mildly overlapping neighbors
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

`intervals.clone()` inside the closure IS measured (criterion's `iter` closure is the thing timed) — this is intentional: `allocate()`'s real signature takes `Vec<Interval>` by value, so any real caller pays this clone cost too, and excluding it would benchmark a function `allocate()` doesn't actually expose. (Measured separately by execution-based review: cloning 1000 intervals alone costs ~0.8 µs — under 1% of the total — so this doesn't meaningfully distort the number either way.)

**The target is currently MISSED by roughly 3x, measured by execution during design review — this must be addressed within this phase, not silently reported.** Baseline measurement of the exact benchmark above: **~130-145 µs**, not the target's 50 µs (confirmed roughly linear at ~130 ns/interval across 250/500/1000/2000-interval runs, i.e. a constant-factor cost, not an algorithmic complexity problem — `LinearScan::run()` is already linear-ish in the number of intervals for a fixed, small pool size). Shipping a benchmark that visibly misses its own stated target on its first run, with no acknowledgment or attempted fix, would be dishonest by this project's own standard (every prior phase has treated "found a real gap while building this slice, within this slice's own bounds" as something to fix, not silently document and walk past — e.g. Phase 8d's holistic review fixing a stale SPEC.md claim it found, Phase 8c's design review fixing its own point-in-time-vs-lifetime bugs before shipping).

**The fix this design proposes is narrow and low-risk, not an architecture change, and is scoped to `LinearScan`'s INTERNAL fields only**: `free_regs: HashSet<PhysReg>`, `assignment: HashMap<Value, Location>`, and `excluded: HashMap<Value, HashSet<PhysReg>>` — the three hot-path containers `LinearScan` owns and mutates internally on every `run()`. Rust's default hasher (SipHash) is deliberately DoS-resistant and correspondingly slow for the small, non-adversarial, integer-keyed maps this allocator actually uses. `rustc-hash = "2"` (the `FxHashMap`/`FxHashSet` family) is ALREADY a workspace dependency, already used by `forge-ir`, `forge-opt`, and `forge-syntax` for exactly this reason — `forge-regalloc` is the one crate in this allocator's hot path that never adopted it. This is a mechanical, behavior-preserving swap (`std::collections::HashMap<K,V>` → `rustc_hash::FxHashMap<K,V>` at these three field declarations; `HashSet` → `FxHashSet` likewise, plus every local variable/constructor that feeds them), not a redesign — `FxHashMap`/`FxHashSet` are drop-in type aliases over the same `std` map/set shape with a different `BuildHasher`, so no call-site logic changes, only the type annotations and constructor calls (`HashMap::new()` → `FxHashMap::default()`, since `FxHashMap` has no inherent `::new()`).

**Explicitly OUT of this swap's scope**: `allocate()`'s own public parameter `excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>` and `intervals.rs`'s `excluded_registers()` function that produces it both stay `std::collections::HashMap` — this is a cross-crate-facing public API type (`crates/forge-regalloc/src/lib.rs` doesn't currently re-export it as a type alias, and several already-shipped tests across `verify.rs`/`pressure.rs`/`linear_scan.rs`'s own test module construct it directly as `std::collections::HashMap`), and widening this swap to include it would be a real, unjustified breaking-API-shape change far beyond what a benchmark-driven perf fix warrants. Only `precompute_excluded`'s OUTPUT (`HashMap<Value, HashSet<PhysReg>>`, fully internal to `linear_scan.rs`) is in scope — it consumes the public `excluded_registers` parameter and immediately re-shapes it into the internal, swappable type, so the public boundary is untouched by construction.

**This is proposed as a measure-first, honest-regardless-of-outcome step, not a promise that 50 µs will be hit**: the plan measures the real baseline, applies the swap, re-measures, and reports whatever the real number is — matching this project's consistent refusal to assert an unverified claim (e.g. 8c's frame-size test using `>=` instead of a false `==` once first-fit was shown non-optimal). If the swap closes the gap to under 50 µs, bullet 24 is genuinely, honestly satisfied. If it doesn't fully close the gap, that's recorded plainly in CHECKLIST as a known, scoped follow-up (the same honest-limitation pattern already used for `evict_and_assign`'s deferred victim case, reload/store insertion, and the φ-coalescing verifier gap) — NOT silently ignored, and NOT force-fit by weakening the benchmark's own workload to make the number look better.

**Why the target itself is still NOT asserted in code even after the fix**: criterion benchmarks report timing, they don't fail a build on a threshold by default (that needs `criterion::Criterion::bench_function` plus a separate `--baseline`/CI-integration step this project doesn't have set up, and building that CI wiring is out of scope for a single benchmark bullet). The bullet is satisfied by having a REAL, runnable benchmark (`cargo bench -p forge-regalloc`) that reports the actual, honestly-measured number — a human (or a future CI step) reads the reported time against the 50 µs target, the same as any other criterion benchmark in a project without automated perf-regression gating.

## Testing (bullets 19, 20, 22 — the three with new test code)

- Bullet 19: a hand-built 2-`Param`-plus-1-`Add` `Ty::I64` function via `forge_ir::builder::Builder`, run through the real `select`/`build_intervals`/`allocate` pipeline, exactly 3 GPR intervals, all `Location::Reg`.
- Bullet 20: 40 hand-built maximally-overlapping GPR intervals, exactly 28 `Location::Spill`, exactly 224 total frame bytes, `verify_allocation` returns `Ok` (a real but narrow regression guard on this fixture — see the honest caveat above).
- Bullet 22: real compiled program (`sin(x) + cos(y) + x + y`) with a libm call and cross-call XMM liveness pressure, `verify_allocation` returns `Ok`, AND at least one XMM interval's range STRICTLY contains a real `CallLibm` position (non-vacuousness, strict containment — see the correction above).
- Bullet 24: baseline-measure `cargo bench -p forge-regalloc` (expect ~130-145 µs), apply the `HashMap`/`HashSet` → `FxHashMap`/`FxHashSet` swap in `linear_scan.rs`, re-measure, report the real resulting number honestly in CHECKLIST regardless of whether it clears 50 µs.

## Exit criteria

1. `crates/forge-regalloc/tests/integration.rs` exists with real tests for bullets 19, 20, and 22, each using the crate's public API only. The full set of public items these tests need (beyond the obvious `allocate`, `verify_allocation`, `Interval`, `RegClass`, `Location`, `SPILL_AWARE_ALLOCATABLE_GPR`) also includes: `forge_regalloc::{build_intervals, excluded_registers}` (bullets 19 and 22 both need the real pipeline, not just `allocate` in isolation), `forge_ir::{Value, builder::Builder, Ty, Inst, Terminator, Function, lower}`, `forge_x64::{select, MachineInst, SelectedFunction}`, and `forge_syntax::{lexer, parser, resolve, typeck, span::Span}` — needed by BOTH bullet 19 (its hand-built `Builder::emit` calls each take a `Span`) and bullet 22 (needs the real front-end to compile `sin(x) + cos(y) + x + y` from source), not bullet 22 alone. `forge_syntax` is already a `forge-regalloc` dev-dependency, everything else is a normal dependency of `forge-regalloc` or `forge-x64` and reachable without new `Cargo.toml` entries.
2. `crates/forge-regalloc/benches/allocation.rs` exists, `cargo bench -p forge-regalloc` runs to completion and reports a real timing number for `allocate()` on 1000 intervals, measured AFTER the `FxHashMap`/`FxHashSet` swap (not the pre-swap baseline).
3. `crates/forge-regalloc/Cargo.toml` gains `criterion` as a dev-dependency (via `.workspace = true`) and a `[[bench]]` target.
4. `crates/forge-regalloc/src/linear_scan.rs`'s THREE INTERNAL `LinearScan` fields (`free_regs`, `assignment`, `excluded`) and their feeders (`precompute_excluded`'s return type, `EMPTY_EXCLUSION_SET`, `excluded_at`'s return type) are swapped to `rustc_hash::FxHashMap`/`FxHashSet` (a new `forge-regalloc` dependency on `rustc-hash`, already a workspace dependency) — NOT `allocate()`'s public `excluded_registers` parameter or return type, and NOT the ~8 unrelated `HashSet`/`HashMap` usages already present in `linear_scan.rs`'s own `#[cfg(test)] mod tests` (pool-disjointness tests, etc.), which stay `std::collections::HashSet`/`HashMap` and need their `use super::*`-inherited names requalified once the module-level `use` changes. Exactly one existing test (the one reading `precompute_excluded`'s output directly) needs its own local type updated to `FxHashSet` to match the new return type. The resulting benchmark number is recorded honestly in CHECKLIST whether or not it clears 50 µs. `run_allocates_a_straight_line_chain_via_transfers`'s stale `a`/`b`/`c` doc comment (traced as the source of this design doc's own bullet-23 error) is corrected in the same pass, along with the now-stale "`HashSet::new()` is not `const fn`" comment on `EMPTY_EXCLUSION_SET` once it becomes `FxHashSet::default`.
5. CHECKLIST.md bullets 19-24 all get `— **note (Phase 8e):** ...` annotations, including explicit cross-references for bullets 21 and 23 (already satisfied by 8d's and 8b's existing tests, respectively — no new test code for those two, only the stale-comment fix for 23) and an honest statement of what bullets 20 and 22 could NOT literally satisfy and why (task #68, the missing `MachineInst`-to-bytes emission pipeline specifically — NOT a missing execution harness, which already exists in `crates/forge-mem`), matching this project's established annotation convention.
6. `cargo test --workspace` green, `cargo clippy --workspace --all-targets -- -D warnings` clean (including the new `benches/` and `tests/` targets), `cargo fmt --check` clean.
