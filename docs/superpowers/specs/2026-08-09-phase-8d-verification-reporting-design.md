# Design: forge Phase 8d — Verification & Reporting

**Status:** Approved for planning
**Scope:** Per `docs/superpowers/specs/2026-08-09-phase-8-decomposition-design.md`, CHECKLIST.md Phase 8 bullets 17-18: an independent register-allocation verifier (bullet 17), and a register-pressure-per-program-point report (bullet 18). Lives in `crates/forge-regalloc`.
**Input:** `Interval` (7a's shape, unchanged since 8a), `Location` and `allocate()`'s `(HashMap<Value, Location>, u32)` output (8b/8c). Both bullets consume the allocator's OUTPUT only — neither touches `LinearScan` or any of its private state.
**Out of scope**: wiring either function into a real "run on every compilation in debug builds" call site (that's Phase 11 — Differential Testing & Verification — per CHECKLIST.md's own phase split; this slice builds the two standalone functions and tests them directly, not their integration into a compile pipeline that doesn't exist yet) and the actual workbench UI panel that bullet 18 says the report "drives" (the workbench frontend, per PROMPT.md/SPEC.md, is a separate, not-yet-started React project — this slice's job ends at producing the data, not rendering it).

## Why this is two independent, narrowly-scoped functions, not one module

Bullets 17 and 18 read like a pair, but they check different things for different reasons and must not be merged into one pass:

- **The verifier (17) is a CORRECTNESS gate.** Its entire value comes from being unable to share a bug with the allocator it's checking — PROMPT.md's own comment on this exact function says so explicitly: *"An INDEPENDENT verifier, deliberately written without reference to the allocator's internals so it cannot share a bug with it."* Concretely: it must not call into `LinearScan`, must not import `pick_register`/`expire_old_intervals`/`spill`/anything from `linear_scan.rs`'s implementation, and must re-derive the overlap check from first principles (a plain pairwise scan over the input data), even though `LinearScan::run()` internally already tracks something similar via `active`. Sharing so much as a helper function with the allocator defeats the entire point of this bullet.
- **The reporting function (18) is a DIAGNOSTIC, not a correctness check.** It has no pass/fail outcome — it just counts. It's fine (expected, even) for it to share code with anything, since a bug in a pure reporting function can't silently corrupt a compiled program's behavior the way a bug in the verifier could.

Keeping them as two separate top-level `pub fn`s in two separate files makes this boundary structural, not just a comment: `crates/forge-regalloc/src/verify.rs` (bullet 17) has zero `use crate::linear_scan::*` anywhere, checkable by grep, not just by promise; `crates/forge-regalloc/src/pressure.rs` (bullet 18) has no such constraint.

## `verify_allocation` — the independent verifier

**The single most important fact this function must get right, already discovered and documented by Phase 8b's own review (SPEC.md's Phase 8b note, item 3), is that PROMPT.md's own sketch for this function is WRONG in two ways** — not a hypothetical risk, an already-proven one:

```rust
// PROMPT.md's literal sketch (crates/forge-regalloc, "verify_allocation"):
pub fn verify_allocation(intervals: &[Interval], alloc: &Allocation) -> Result<(), String> {
    for (i, a) in intervals.iter().enumerate() {
        for b in &intervals[i + 1..] {
            if !(a.start < b.end && b.start < a.end) { continue; }   // no overlap
            if let (Location::Reg(ra), Location::Reg(rb)) =
                (alloc.of(a.value), alloc.of(b.value))
            {
                if ra == rb {
                    return Err(format!("overlapping values ... both assigned {:?}", ra));
                }
            }
        }
    }
    Ok(())
}
```

Two concrete defects in this text, both already identified (SPEC.md's Phase 8a and 8b notes) and both must NOT be re-introduced here:

1. **`a.start < b.end && b.start < a.end` is a HALF-OPEN overlap test.** This project's `Interval::[start, end]` is INCLUSIVE (8a's documented deviation from an earlier sketch) — `end` is the position of the value's last read, and the value is still live at that position. Two intervals `[0,2]` and `[2,4]` genuinely overlap (share position 2), but the half-open test above says `2 < 4 && 2 < 2` → `false`, wrongly reporting no overlap. The correct predicate, already used throughout `crates/forge-regalloc` (8a's `merge_phi_intervals`, 8b's `pick_register`/`expire_old_intervals`, 8c's `spill`), is `a.start <= b.end && b.start <= a.end`.
2. **The property this function checks — "no two overlapping intervals may share a register, ever" — is FALSE of the correct allocator's own real output**, and this is the more serious defect, though NOT for the reason an earlier draft of this doc claimed. Execution-based review measured the two defects' real, isolated impact on the actual corpus (18 programs, real `build_intervals` → `allocate` output): PROMPT.md's literal sketch (both defects present) rejects **0/18** — the two defects CANCEL, because the half-open test is exactly false for a handoff pair (`a=[0,2], b=[2,4]`: `2 < 2` fails), so it accidentally treats every touching pair as non-overlapping and never reaches the missing-exemption check at all. The inclusive-but-no-exemption variant (defect 1 fixed, defect 2 still present) is the one that actually rejects the corpus — measured **17/18** programs. So the literal sketch's real failure mode is the OPPOSITE of "too strict": it's a FALSE NEGATIVE — it silently ACCEPTS a genuine double-booking at a touching, non-handoff position, because the half-open test wrongly calls it "not overlapping" regardless of whether an exemption would even apply. Confirmed by execution: `a=[0,2], b=[2,4]`, `hint: None` (NOT a handoff), both assigned `Rax` — a real conflict at position 2 — and the literal sketch returns `Ok(())` on it. `pick_register`'s Case 2 deliberately gives one register to two intervals that touch at exactly one shared instruction (`lhs.end == dst.start`, `dst.hint == Some(lhs.value)`) — a legitimate coalesced handoff, not a conflict, because the donor's value dies at the exact instruction the recipient's is born; SPEC.md's Phase 8b note is careful to attribute the 17/18 rejection to "the plain INCLUSIVE predicate," not the half-open one, and states the corrected property explicitly: *"sharing a register is a violation UNLESS the ranges are disjoint (`a.end < b.start || b.end < a.start`) OR they touch at exactly one point that is a real handoff (`a.end == b.start && b.hint == Some(a.value)`, or symmetrically)."*

```rust
/// crates/forge-regalloc/src/verify.rs
use crate::interval::Interval;
use crate::linear_scan::Location; // the ONLY thing imported from linear_scan.rs --
                                    // a type definition, not a function or any
                                    // piece of allocator logic.
use forge_ir::Value;
use std::collections::HashMap;

/// An INDEPENDENT verifier (PROMPT.md), deliberately written without
/// reference to `LinearScan`'s internals so it cannot share a bug with the
/// allocator it checks. Re-derives the overlap check from the raw
/// `Interval`/assignment data via a plain pairwise scan -- the same shape
/// PROMPT.md's own sketch uses, corrected for two defects that sketch has
/// (see this file's design doc): the INCLUSIVE range convention, and the
/// legitimate-handoff exemption that makes "no two overlapping intervals
/// share a register" false of the real allocator's own correct output.
///
/// Also checks spill slots: `Location::Spill(n)` values sharing the same
/// slot number must be genuinely, STRICTLY disjoint -- unlike registers,
/// a stack slot has no same-instruction handoff mechanism (nothing ever
/// hands a slot from one interval to another mid-instruction the way a
/// register can be transferred), so no exemption applies on this side.
/// This extends bullet 17's literal wording ("share a register") to the
/// `Location` variant Phase 8c added after this bullet was written --
/// the same "extend the established property to a new Location variant"
/// step Phase 8c's own tests already took for its corpus checks.
///
/// NOT reachable from the current `spill()` (8c): its reuse condition
/// (`slot_end[s] < start`, STRICT) already guarantees every same-slot
/// pair is pairwise strictly disjoint -- execution-based review measured
/// zero touching same-slot pairs across 56 real same-slot pairs produced
/// under high register pressure. This branch is deliberately kept anyway
/// for two reasons, not because the case is a live risk today: (1) an
/// INDEPENDENT verifier must not lean on `spill()`'s own invariant --
/// doing so would reintroduce exactly the "shares a bug with the thing
/// it's checking" failure this whole function exists to avoid; (2)
/// defense-in-depth against OTHER future slot-assigning code this
/// verifier would also need to check (`evict_and_assign`'s still-deferred
/// victim-reassignment path, Phase 11's differential testing, any future
/// slot coalescer) -- none of which are guaranteed to preserve `spill()`'s
/// strict-inequality invariant just because `spill()` itself does.
pub fn verify_allocation(
    intervals: &[Interval],
    assignment: &HashMap<Value, Location>,
) -> Result<(), String> {
    for (i, a) in intervals.iter().enumerate() {
        for b in &intervals[i + 1..] {
            let overlaps = a.start <= b.end && b.start <= a.end;
            if !overlaps {
                continue;
            }
            match (assignment.get(&a.value), assignment.get(&b.value)) {
                (Some(Location::Reg(ra)), Some(Location::Reg(rb))) if ra == rb => {
                    let legit_handoff = (a.end == b.start && b.hint == Some(a.value))
                        || (b.end == a.start && a.hint == Some(b.value));
                    if !legit_handoff {
                        return Err(format!(
                            "overlapping values {:?} [{},{}] and {:?} [{},{}] both assigned \
                             {:?}, and neither end touches the other's start as a hinted \
                             handoff",
                            a.value, a.start, a.end, b.value, b.start, b.end, ra
                        ));
                    }
                }
                (Some(Location::Spill(sa)), Some(Location::Spill(sb))) if sa == sb => {
                    return Err(format!(
                        "overlapping values {:?} [{},{}] and {:?} [{},{}] both assigned spill \
                         slot {sa} -- spill slots have no legitimate-handoff exemption",
                        a.value, a.start, a.end, b.value, b.start, b.end
                    ));
                }
                _ => {}
            }
        }
    }
    Ok(())
}
```

**Known, deliberate limitation: this verifier currently REJECTS a correct φ-coalesced allocation, if one is ever produced.** `merge_phi_intervals` (8a) gives every φ-group member the IDENTICAL `[start, end]` range and mutual hints toward the group's anchor — co-locating them in one register is the entire point of the hint. But the exemption predicate above requires `a.end == b.start` (a touching, not identical, pair), so two φ-group members sharing a register — `[2,7]` and `[2,7]`, `hint = Some(anchor)` — fail the exemption (`a.end == 7 ≠ 2 == b.start`) and this verifier reports them as a conflict, even though honoring the hint here would be entirely correct (both intervals denote the same value at the join point). This is UNREACHABLE today only because 8b's `pick_register` structurally cannot honor a φ hint in the first place (SPEC.md's Phase 8b note, item 2: Case 1 needs the target's register already free, which it never is for an identical-range pair; Case 2 needs `target.end == this.start`, false when both ends are equal) — measured zero same-register φ-group pairs across the whole real corpus. The FIRST time φ coalescing is made to work (a plausible future allocator improvement), this verifier — meant to run on every debug compilation per PROMPT.md/SPEC.md — will reject that correct code. Recorded here as a known limitation to revisit if/when φ coalescing lands, not fixed now (building an exemption for a case with zero real producers would be exactly the kind of speculative generality this project avoids elsewhere).

An earlier draft of this doc instead argued the exemption's precondition "can only be satisfied by a genuine two-address chain, never by a φ-group masquerading as one," reasoning from "`a.end == b.start` requires two different positions." That reasoning is not sound as stated — a zero-length interval pair (`start == end == s` for both) satisfies `a.end == b.start` without being two different positions at all, and would exempt cleanly if such a pair existed with a matching hint. It happens to be harmless (exempting two same-value φ-group members is semantically correct, and no zero-length φ pair is currently producible either, for the same "φ dest is seeded at block start, its incoming values are defined in strictly earlier blocks" reason), so no fix is needed — but the argument itself should not be relied on, and is replaced by the paragraph above, which states the real, currently-relevant limitation instead.

**Why `Result<(), String>` and not something richer (e.g. a `Vec<Conflict>` collecting every violation, not just the first)**: matches PROMPT.md's own sketch exactly, and this project's established practice (8a-8c) is to build exactly what's needed and no more — a verifier's job in a debug-build gate (the actual "run on every compilation" wiring, Phase 11's job, not this slice's) is to fail loudly the first time something is wrong, not to produce a comprehensive report of every violation for a JIT compiler's single-expression inputs, where "which is the first bug" and "are there more" carry the same practical weight (the program is either correct or it's already time to stop). If Phase 11 needs multi-violation output later, that's a Phase 11 concern to design against real requirements, not something to speculatively build here.

## `register_pressure` — the diagnostic report

```rust
/// crates/forge-regalloc/src/pressure.rs
use crate::interval::{Interval, RegClass};

/// Register pressure (simultaneously live interval count) at a single
/// linearized instruction position, split by class -- GPR and XMM are
/// allocated from wholly separate pools throughout this allocator (8a-8c),
/// so a combined count would conflate two numbers that are never compared
/// against the same budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PressurePoint {
    pub gpr: u32,
    pub xmm: u32,
}

/// Computes register pressure at EVERY position `0..program_length`
/// (dense, not just at positions where pressure changes) via a standard
/// sweep-line: +1 at each interval's `start`, -1 at `end + 1` (the first
/// position it's no longer live, given INCLUSIVE `[start, end]`), then a
/// running prefix sum. `program_length` must be passed explicitly rather
/// than inferred from `intervals.iter().map(|iv| iv.end).max()` -- a
/// function's real instruction count can legitimately exceed every
/// interval's own `end` (e.g. trailing instructions after the last value
/// read, or a function with dead code the optimizer left in place before
/// this pass runs), and silently under-sizing the report would leave
/// those trailing positions unrepresented rather than correctly reported
/// as zero pressure. Callers pass `selected.insts.len()`.
///
/// Dense (one entry per position, not a sparse step-function) because the
/// stated consumer is a workbench chart plotting pressure across the
/// instruction-index axis directly -- a sparse representation would just
/// push the same expansion work onto every future caller instead of
/// doing it once here.
pub fn register_pressure(intervals: &[Interval], program_length: usize) -> Vec<PressurePoint> {
    let mut gpr_delta = vec![0i32; program_length + 1];
    let mut xmm_delta = vec![0i32; program_length + 1];
    for iv in intervals {
        let delta = match iv.reg_class {
            RegClass::Gpr => &mut gpr_delta,
            RegClass::Xmm => &mut xmm_delta,
        };
        let start = iv.start as usize;
        let end_exclusive = (iv.end as usize + 1).min(program_length);
        if start < program_length {
            delta[start] += 1;
        }
        if end_exclusive < program_length {
            delta[end_exclusive] -= 1;
        }
    }
    let mut out = Vec::with_capacity(program_length);
    let (mut gpr_running, mut xmm_running) = (0i32, 0i32);
    for pos in 0..program_length {
        gpr_running += gpr_delta[pos];
        xmm_running += xmm_delta[pos];
        out.push(PressurePoint {
            gpr: gpr_running as u32,
            xmm: xmm_running as u32,
        });
    }
    out
}
```

**Why the delta arrays are sized `program_length + 1`**: this is defensive headroom, not a position that ever actually gets written — the `.min(program_length)` clamp on `end_exclusive`, combined with the `if end_exclusive < program_length` guard before the decrement, means index `program_length` itself is never written by any real or malformed input (confirmed by execution: re-deriving the delta arrays for real corpus programs shows the final index is always unused). The `+1` costs two wasted `i32` slots and is kept only as cheap insurance against a future edit to the clamp logic that might need the headroom — not because the current code relies on it.

**Why `end_exclusive` is clamped to `program_length` before the decrement**: `iv.end` is guaranteed `< program_length` by construction (every real interval's `end` is a real instruction position `build_intervals` derived from `selected.insts`, and `program_length` is that same `insts.len()`), so this never fires for real allocator output. The `.min(program_length)` clamp is defensive against a caller passing a `program_length` inconsistent with the `intervals` it hands in (e.g. a stale or hand-built count in a test) — it keeps the function total (no panic, no `u32` wraparound from an unmatched decrement) rather than correct-only-when-inputs-agree, at zero cost in the expected case where they do agree. Confirmed by execution against three deliberately-inconsistent shapes (an interval's `end` past `program_length`; an interval's `start` past `program_length`; `program_length == 0`): no panic and no wraparound-to-huge-`u32` in any case, because the clamp guarantees every `+1` has a matching `-1` or neither happens at all.

**No `RegClass` boundary crossing risk**: unlike the verifier, this function has no correctness property to preserve across a boundary — it's a pure count, and a wrong count is a diagnostic inconvenience, not a silently wrong compiled program. This is exactly why it's fine for `register_pressure` (but never `verify_allocation`) to live in the same crate section as, and even eventually be called from, allocator-adjacent code.

## Testing

- `verify_allocation`: a hand-built valid allocation (2 disjoint intervals, different registers) → `Ok`. A hand-built INVALID allocation (2 genuinely overlapping intervals, same register, no hint relationship) → `Err`, and the error message names both values. A hand-built LEGITIMATE handoff (`a.end == b.start`, `b.hint == Some(a.value)`, same register) → `Ok` — this is the test that would fail if the half-open PROMPT.md sketch or the naive "no exemption" version were shipped instead. The symmetric handoff direction (`b.end == a.start`, `a.hint == Some(b.value)`) → `Ok`, tested separately (the predicate is asymmetric in how it's written even though the property is symmetric — a real transcription bug could get one direction right and the other wrong, so both need independent coverage, not just one with a comment claiming symmetry). **Construction note, confirmed by execution**: the natural way to write this second test — keep the SAME slice order as the first test (`a` before `b`) and just flip which one has the hint (`a.hint = Some(b.value)` instead of `b.hint = Some(a.value)`) — does NOT exercise the second disjunct and produces `Err`, not `Ok`, because the second disjunct needs `b.end == a.start`, which requires `b` to come BEFORE `a` in the range ordering (i.e. genuinely swap which interval is which, not just which one holds the hint). A plan or implementer who writes the "obvious" version here will get a failing test and must not "fix" it by weakening the assertion — the fix is reversing the two intervals' actual ranges, not the test's expectations. Separately: this second-disjunct case is UNREACHABLE from the real corpus (`build_intervals` sorts by `(start, end, value)`, and for two intervals `a` before `b` in that order, `b.end == a.start` would force `b` to sort before `a` — a contradiction), so the corpus-wide `Ok`-on-every-program test below cannot exercise it; only this hand-built fixture can, and it must not be dropped as "redundant with the corpus test" for that reason. Two intervals TOUCHING but sharing a register WITHOUT a matching hint (`a.end == b.start`, `b.hint != Some(a.value)`) → `Err` — confirms the exemption is conditioned on the hint, not merely on the positions touching. Two `Location::Spill` values with the same slot number, genuinely overlapping → `Err`. Two `Location::Spill` values with the same slot number, touching at one point (`a.end == b.start`) → `Err` (no handoff exemption for spills, unlike registers) — this is the test that would fail if someone copy-pasted the register exemption onto the spill branch without checking whether the exemption even applies there. Two `Location::Spill` values, genuinely disjoint, same slot number → `Ok`.
- Re-run the ENTIRE existing 8a-8c corpus (the shared `test_corpus()` list already duplicated across `intervals.rs` and `linear_scan.rs`) through `build_intervals` → `allocate` → `verify_allocation`, asserting `Ok` on every program — this is the verifier's real job (confirm the SHIPPED allocator's real output is accepted, not just that hand-built fixtures pass/fail correctly in isolation) and doubles as a regression guard: if some future change to `LinearScan` produces an invalid allocation, this is the test that catches it, run independently of `linear_scan.rs`'s own tests.
- A deliberately-broken-allocation test (CHECKLIST bullet 21, arguably 8e's, but a MINIMAL version belongs here as `verify_allocation`'s own unit test regardless of which slice's corpus-level integration test bullet 21 becomes): take a real corpus program's real `assignment` output, mutate it in-memory to reassign two genuinely-overlapping, non-handoff values to the same register, confirm `verify_allocation` catches it. This is distinct from the hand-built fixture tests above because it exercises the verifier against REAL `Interval` shapes (φ-merged ranges, real hints) that a purely hand-built fixture might not accidentally match the shape of.
- `register_pressure`: 3 non-overlapping single-class intervals at different positions → pressure is 1 at each of their individual ranges, 0 elsewhere. 3 fully-overlapping same-class intervals → pressure 3 throughout their shared range. Mixed GPR+XMM intervals overlapping the SAME positions → confirms `gpr`/`xmm` counts are independent (a GPR interval must never inflate the `xmm` count or vice versa). A position past every interval's `end` but before `program_length` → pressure 0 (confirms the dense array's tail is correctly zeroed, not left as stale accumulator state). `program_length` shorter than some interval's `end` (a deliberately inconsistent test input) → does not panic (confirms the clamp).
- Re-run the corpus through `build_intervals` → `register_pressure`, asserting the output length equals `selected.insts.len()` for every program, and that the peak pressure never exceeds the class's **`SPILL_AWARE_ALLOCATABLE_GPR`/`SPILL_AWARE_ALLOCATABLE_XMM`** pool size (12/14 — NOT the wider `ALLOCATABLE_GPR`/`ALLOCATABLE_XMM`, 14/16, which is `allocate()`'s pre-scratch-reservation constant and no longer the one `LinearScan` actually scans against) PLUS however many spills that program produced. Execution-based review confirmed this distinction is load-bearing, not pedantic: measured against 200 randomized spilling programs, the bound `peak <= SPILL_AWARE pool size + spills` is TIGHT (zero slack) in 186/200 cases — a real, falsifiable cross-check between this report and the allocator's actual behavior. Using the wider, pre-8c constant makes every one of those 186 cases slack-by-2 and the assertion effectively unfalsifiable on any program this allocator can currently produce (a basic sanity cross-check between this new report and the existing allocator output, not a redundant re-test of allocation correctness itself).

**Scope boundary, recorded deliberately rather than left implicit**: `verify_allocation` is narrower than `linear_scan.rs`'s own existing tests in two specific ways, both falling out of the `_ => {}` catch-all arm in its `match`: it does not flag a `Value` that's missing from `assignment` entirely (`linear_scan.rs`'s own `assignment.len() == intervals.len()` check already covers this), and it does not flag a `RegClass::Gpr` interval assigned an XMM register or vice versa (also already checked in `linear_scan.rs`'s own corpus tests). This is a deliberate scope decision, not an oversight: CHECKLIST bullet 17's literal wording is "no two overlapping intervals share a register," and this function does exactly that, independently, which is its whole reason to exist. Completeness and class-pool checks are properties of `allocate()`'s CONTRACT rather than of interval overlap specifically, and adding them here would blur why this function is independent in the first place (a completeness check has nothing to independently re-derive — it's just counting). If Phase 11's real "run on every debug compilation" integration wants a single call that checks everything, that composition belongs in Phase 11, calling multiple narrow checks — not built by widening this one.

## Exit criteria

1. `crates/forge-regalloc/src/verify.rs` exists, exports `pub fn verify_allocation(intervals: &[Interval], assignment: &HashMap<Value, Location>) -> Result<(), String>`, and contains no `use` of anything from `linear_scan.rs` except the `Location` type itself (checkable by grep, not just review).
2. `verify_allocation` implements the CORRECTED overlap property (inclusive ranges, register handoff exemption, no exemption for spill slots), matching SPEC.md's Phase 8b note precisely, not PROMPT.md's literal (known-wrong) sketch.
3. `crates/forge-regalloc/src/pressure.rs` exists, exports `pub fn register_pressure(intervals: &[Interval], program_length: usize) -> Vec<PressurePoint>` and the `PressurePoint { gpr: u32, xmm: u32 }` struct.
4. Both functions are re-exported from `crates/forge-regalloc/src/lib.rs`.
5. The full existing 8a-8c corpus passes `verify_allocation` when run through the real `build_intervals` → `allocate` pipeline (confirms the shipped allocator is actually sound by this independently-written check, not just self-consistent by its own tests).
6. All Testing section items covered by real tests, including both handoff-exemption directions and the spill-slot no-exemption case.
7. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
