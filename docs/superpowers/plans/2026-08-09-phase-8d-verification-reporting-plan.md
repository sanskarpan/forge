# Phase 8d — Verification & Reporting Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add two standalone, independent functions to `crates/forge-regalloc`: `verify_allocation` (CHECKLIST bullet 17 — an independent register-allocation correctness check) and `register_pressure` (bullet 18 — a diagnostic report of simultaneously-live interval counts per program point).

**Architecture:** Two new files, each with its own module and test suite, neither depending on the other: `crates/forge-regalloc/src/verify.rs` (imports only `Interval` and the `Location` type — nothing else from `linear_scan.rs` — so it structurally cannot share a bug with the allocator it checks) and `crates/forge-regalloc/src/pressure.rs` (a pure counting function with no correctness constraint to preserve). Both consume only the allocator's output (`Vec<Interval>`, `HashMap<Value, Location>`), never its internals.

**Tech Stack:** Rust, `crates/forge-regalloc` (this crate), depends on `forge-ir` (`Value`) — no other new dependencies.

**Design doc:** `docs/superpowers/specs/2026-08-09-phase-8d-verification-reporting-design.md` — execution-verified (an earlier review built both functions verbatim in a scratch harness and ran them against the real 18-program corpus plus 900+ randomized `allocate()` outputs; the code blocks are confirmed correct and shippable, and the doc's prose was corrected in one follow-up round to fix several claims about WHY the code is correct that execution proved were wrong or overstated). Treat the code blocks below as verified.

---

## Before you start

Read `crates/forge-regalloc/src/linear_scan.rs` in full (current shipped state — `Location`, `allocate()`'s real 3-arg/tuple-return signature, `SPILL_AWARE_ALLOCATABLE_GPR`/`_XMM`, and the existing `test_corpus()`/`front_end()` test helpers, which this plan's new test modules will duplicate verbatim — this project's established convention, already used between `intervals.rs` and `linear_scan.rs`, is to copy this exact helper pair into each new test module that needs the corpus rather than share it via `pub(crate)` plumbing across files). Also read `crates/forge-regalloc/src/interval.rs` (`Interval`, `RegClass`) and `crates/forge-regalloc/src/lib.rs` (current re-export list, which both tasks below extend).

Both new files are ADDITIVE — neither task modifies `linear_scan.rs`, `intervals.rs`, `interval.rs`, or `liveness.rs` at all, beyond `lib.rs`'s `mod`/`pub use` lines. Confirm before starting: `cargo test -p forge-regalloc` should currently show 61 passing tests (Phase 8c's shipped state).

---

### Task 1: `verify_allocation` — the independent verifier

**Files:**
- Create: `crates/forge-regalloc/src/verify.rs`
- Modify: `crates/forge-regalloc/src/lib.rs`

- [ ] **Step 1: Wire the new module in (empty file first, so later steps compile incrementally)**

Create `crates/forge-regalloc/src/verify.rs` with just:

```rust
use crate::interval::Interval;
use crate::linear_scan::Location;
use forge_ir::Value;
use std::collections::HashMap;
```

Add to `crates/forge-regalloc/src/lib.rs`:

```rust
mod verify;
```
(add this line alongside the existing `mod interval;` / `mod intervals;` / `mod linear_scan;` / `mod liveness;` lines — alphabetical-ish grouping, doesn't matter which exact position)

- [ ] **Step 2: Write the failing hand-built fixture tests**

Add to `crates/forge-regalloc/src/verify.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::interval::RegClass;

    fn iv(value: u32, start: u32, end: u32, class: RegClass) -> Interval {
        Interval {
            value: Value(value),
            start,
            end,
            reg_class: class,
            hint: None,
            fixed: None,
            spill_weight: 0.0,
        }
    }

    #[test]
    fn accepts_a_valid_disjoint_allocation() {
        let a = iv(0, 0, 2, RegClass::Gpr);
        let b = iv(1, 3, 5, RegClass::Gpr);
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Reg(forge_x64::PhysReg::Rax));
        assignment.insert(b.value, Location::Reg(forge_x64::PhysReg::Rbx));

        assert!(verify_allocation(&[a, b], &assignment).is_ok());
    }

    #[test]
    fn rejects_overlapping_same_register_without_handoff() {
        let a = iv(0, 0, 5, RegClass::Gpr);
        let b = iv(1, 2, 8, RegClass::Gpr); // genuinely overlaps a, no hint relationship
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Reg(forge_x64::PhysReg::Rax));
        assignment.insert(b.value, Location::Reg(forge_x64::PhysReg::Rax));

        let err = verify_allocation(&[a, b], &assignment).unwrap_err();
        assert!(
            err.contains("Value(0)"),
            "error must name the first value: {err}"
        );
        assert!(
            err.contains("Value(1)"),
            "error must name the second value: {err}"
        );
    }

    #[test]
    fn accepts_a_legitimate_handoff() {
        let a = iv(0, 0, 2, RegClass::Gpr);
        let mut b = iv(1, 2, 4, RegClass::Gpr);
        b.hint = Some(a.value); // a.end == b.start, b hints at a -- a real two-address handoff
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Reg(forge_x64::PhysReg::Rax));
        assignment.insert(b.value, Location::Reg(forge_x64::PhysReg::Rax));

        assert!(verify_allocation(&[a, b], &assignment).is_ok());
    }

    #[test]
    fn accepts_the_symmetric_handoff_direction() {
        // The second disjunct (`b.end == a.start && a.hint == Some(b.value)`)
        // needs b's range to come BEFORE a's, not merely a flipped hint on
        // the same slice order as the test above -- confirmed by execution
        // during design review. Naively keeping a=[0,2],b=[2,4] and just
        // setting a.hint = Some(b.value) does NOT exercise this branch and
        // produces Err, not Ok (the design doc's Testing section explains
        // why). This test genuinely swaps which interval has the earlier
        // range instead.
        let mut a = iv(0, 2, 4, RegClass::Gpr); // now the LATER range
        let b = iv(1, 0, 2, RegClass::Gpr); // now the EARLIER range
        a.hint = Some(b.value); // b.end == a.start, a hints at b
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Reg(forge_x64::PhysReg::Rax));
        assignment.insert(b.value, Location::Reg(forge_x64::PhysReg::Rax));

        assert!(verify_allocation(&[a, b], &assignment).is_ok());
    }

    #[test]
    fn rejects_touching_intervals_sharing_a_register_without_a_matching_hint() {
        let a = iv(0, 0, 2, RegClass::Gpr);
        let b = iv(1, 2, 4, RegClass::Gpr); // touches a, but no hint at all
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Reg(forge_x64::PhysReg::Rax));
        assignment.insert(b.value, Location::Reg(forge_x64::PhysReg::Rax));

        assert!(
            verify_allocation(&[a, b], &assignment).is_err(),
            "touching positions alone must not be enough -- the hint must actually match"
        );
    }

    #[test]
    fn rejects_overlapping_spill_slots() {
        let a = iv(0, 0, 5, RegClass::Gpr);
        let b = iv(1, 2, 8, RegClass::Gpr);
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Spill(0));
        assignment.insert(b.value, Location::Spill(0));

        assert!(verify_allocation(&[a, b], &assignment).is_err());
    }

    #[test]
    fn rejects_touching_spill_slots_even_though_touching_registers_can_be_exempt() {
        // Spill slots have NO handoff exemption, unlike registers -- this
        // is the test that would fail if someone copy-pasted the register
        // exemption onto the spill branch.
        let a = iv(0, 0, 2, RegClass::Gpr);
        let mut b = iv(1, 2, 4, RegClass::Gpr);
        b.hint = Some(a.value); // even WITH a matching hint --
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Spill(0));
        assignment.insert(b.value, Location::Spill(0));

        assert!(
            verify_allocation(&[a, b], &assignment).is_err(),
            "spill slots must never be exempted, even when the hint would exempt a register"
        );
    }

    #[test]
    fn accepts_disjoint_spill_slots_sharing_a_slot_number() {
        let a = iv(0, 0, 2, RegClass::Gpr);
        let b = iv(1, 3, 5, RegClass::Gpr); // genuinely disjoint from a
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Spill(0));
        assignment.insert(b.value, Location::Spill(0));

        assert!(verify_allocation(&[a, b], &assignment).is_ok());
    }

    #[test]
    fn accepts_overlapping_values_in_different_spill_slots() {
        // Distinguishes the `sa == sb` check from "any two overlapping
        // Spill locations are an error regardless of slot number" -- the
        // disjoint-same-slot test above short-circuits on the `!overlaps`
        // guard before ever reaching the slot-number comparison, so it
        // can't tell these two behaviors apart on its own; this test can.
        let a = iv(0, 0, 5, RegClass::Gpr);
        let b = iv(1, 2, 8, RegClass::Gpr); // genuinely OVERLAPS a
        let mut assignment = HashMap::new();
        assignment.insert(a.value, Location::Spill(0));
        assignment.insert(b.value, Location::Spill(1)); // DIFFERENT slots -- fine

        assert!(verify_allocation(&[a, b], &assignment).is_ok());
    }
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo build -p forge-regalloc --tests 2>&1 | tail -20`
Expected: fails to compile — `verify_allocation` doesn't exist yet.

- [ ] **Step 4: Implement `verify_allocation`**

Add to `crates/forge-regalloc/src/verify.rs`, above the `#[cfg(test)]` block:

```rust
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
/// a stack slot has no same-instruction handoff mechanism, so no
/// exemption applies on this side. Not reachable from the current
/// `spill()` (its strict `slot_end[s] < start` reuse condition already
/// guarantees same-slot pairs are disjoint) but kept anyway: an
/// independent verifier must not lean on the allocator's own invariant,
/// and this is defense-in-depth against other future slot-assigning code
/// (`evict_and_assign`'s still-deferred victim path, a future coalescer).
///
/// Deliberately does NOT check for a `Value` missing from `assignment`
/// entirely, or a `RegClass`/register-class mismatch -- both are already
/// checked by `linear_scan.rs`'s own tests, and bullet 17's literal scope
/// is the overlap property specifically. See this file's design doc for
/// the full reasoning.
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

- [ ] **Step 5: Run to verify the hand-built tests pass**

Run: `cargo test -p forge-regalloc --lib verify::tests`
Expected: PASS (9 tests: `accepts_a_valid_disjoint_allocation`, `rejects_overlapping_same_register_without_handoff`, `accepts_a_legitimate_handoff`, `accepts_the_symmetric_handoff_direction`, `rejects_touching_intervals_sharing_a_register_without_a_matching_hint`, `rejects_overlapping_spill_slots`, `rejects_touching_spill_slots_even_though_touching_registers_can_be_exempt`, `accepts_disjoint_spill_slots_sharing_a_slot_number`, `accepts_overlapping_values_in_different_spill_slots`).

- [ ] **Step 6: Add the corpus-wide tests**

Add to the SAME `#[cfg(test)] mod tests` block in `verify.rs`, after the 9 hand-built tests above:

```rust
    #[test]
    fn accepts_every_real_corpus_allocation() {
        for src in test_corpus() {
            let func = front_end(src);
            let selected = forge_x64::select(&func);
            let intervals = crate::intervals::build_intervals(&func, &selected);
            let excluded = crate::intervals::excluded_registers(&func, &selected);
            let (assignment, _bytes) =
                crate::linear_scan::allocate(intervals.clone(), &excluded, &selected);

            assert!(
                verify_allocation(&intervals, &assignment).is_ok(),
                "{src:?}: the real, shipped allocator's own output must pass this independent \
                 check -- if it doesn't, either the allocator or this verifier has a bug"
            );
        }
    }

    #[test]
    fn catches_a_deliberately_broken_allocation() {
        let mut checked = 0;
        for src in test_corpus() {
            let func = front_end(src);
            let selected = forge_x64::select(&func);
            let intervals = crate::intervals::build_intervals(&func, &selected);
            let excluded = crate::intervals::excluded_registers(&func, &selected);
            let (mut assignment, _bytes) =
                crate::linear_scan::allocate(intervals.clone(), &excluded, &selected);

            // Find a genuinely-overlapping, non-handoff, same-class pair in
            // this program's REAL intervals (phi-merged ranges, real hints
            // included) -- not every corpus program has one.
            let mut broken = None;
            'outer: for i in 0..intervals.len() {
                for j in (i + 1)..intervals.len() {
                    let (a, b) = (&intervals[i], &intervals[j]);
                    if a.reg_class != b.reg_class {
                        continue;
                    }
                    let overlaps = a.start <= b.end && b.start <= a.end;
                    let legit_handoff = (a.end == b.start && b.hint == Some(a.value))
                        || (b.end == a.start && a.hint == Some(b.value));
                    if overlaps && !legit_handoff {
                        broken = Some((a.value, b.value));
                        break 'outer;
                    }
                }
            }
            let Some((va, vb)) = broken else { continue };
            checked += 1;

            // Force vb onto va's real location, creating a genuine conflict.
            let loc_a = assignment[&va];
            assignment.insert(vb, loc_a);

            assert!(
                verify_allocation(&intervals, &assignment).is_err(),
                "{src:?}: {va:?} and {vb:?} genuinely overlap without a legitimate handoff but \
                 were forced onto the same location -- verify_allocation must catch this"
            );
        }
        assert!(
            checked > 0,
            "corpus must contain at least one genuinely-overlapping non-handoff pair to \
             exercise this test -- if this fails, the corpus changed and needs a hand-built \
             fallback fixture instead"
        );
    }

    /// Shared corpus, copied VERBATIM from `crates/forge-regalloc/src/linear_scan.rs`'s
    /// `test_corpus()` -- this project's established convention (already used between
    /// `intervals.rs` and `linear_scan.rs`) is to duplicate this exact list into each new
    /// test module that needs it, rather than thread `pub(crate)` plumbing across files.
    fn test_corpus() -> Vec<&'static str> {
        vec![
            "3.14159 * r * r",
            "sin(x) + cos(y)",
            "(n * 2654435761) >> 16",
            "x / y",
            "x + 1",
            "fma(a, b, c)",
            "base + i * 8",
            "let t = a - b in if t > 0.0 then t else -t",
            "if a > b then (if a > c then a else c) else b",
            "(if a > b then a else b) + a",
            "sqrt(x * x + y * y)",
            "abs(x) + floor(y) + ceil(z)",
            "(n >> 1) % 7 + (n >> 1) / 7",
            "if a > b then (a * c) + (b * c) else a - b",
            "if a > b then (a - b) - (a + b) else c - a",
            "if a > b then fma(a, b, c) else a * c",
            "if x > y then (x * y) + (x - y) else x / y",
            "if x > y then fma(x, y, z) * x else fma(y, x, z) - y",
        ]
    }

    /// Same front_end helper shape as linear_scan.rs's own test module (lex ->
    /// parse -> resolve -> typecheck -> lower).
    fn front_end(src: &str) -> forge_ir::Function {
        let (tokens, diags) = forge_syntax::lexer::lex(src);
        assert!(diags.is_empty(), "lex errors for {src:?}: {diags:?}");
        let (ast, diags) = forge_syntax::parser::parse(&tokens);
        assert!(diags.is_empty(), "parse errors for {src:?}: {diags:?}");
        let typed = forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast))
            .unwrap_or_else(|e| panic!("type errors for {src:?}: {e:?}"));
        forge_ir::lower::lower(&typed)
    }
```

- [ ] **Step 7: Run to verify everything passes**

Run: `cargo test -p forge-regalloc --lib verify::tests`
Expected: PASS (11 tests total).

- [ ] **Step 8: Re-export from `lib.rs`**

Change `crates/forge-regalloc/src/lib.rs`'s existing re-export block to add `verify_allocation`:

```rust
pub use verify::verify_allocation;
```
(add as a new line, alongside the existing `pub use interval::{...}`, `pub use intervals::{...}`, etc. lines)

- [ ] **Step 9: Run the full crate suite**

Run: `cargo test -p forge-regalloc 2>&1 | tail -20`
Expected: PASS, 72 tests total (61 existing + 11 new).

- [ ] **Step 10: Commit**

```bash
cd /Users/sanskar/dev/Research/Projects/JIT-Compiler
git add crates/forge-regalloc/src/verify.rs crates/forge-regalloc/src/lib.rs
git commit -m "feat(forge-regalloc): add verify_allocation, an independent allocation verifier"
```

---

### Task 2: `register_pressure` — the diagnostic report

**Files:**
- Create: `crates/forge-regalloc/src/pressure.rs`
- Modify: `crates/forge-regalloc/src/lib.rs`

- [ ] **Step 1: Wire the new module in**

Create `crates/forge-regalloc/src/pressure.rs` with just:

```rust
use crate::interval::{Interval, RegClass};
```

Add to `crates/forge-regalloc/src/lib.rs`:

```rust
mod pressure;
```

- [ ] **Step 2: Write the failing tests**

Add to `crates/forge-regalloc/src/pressure.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use forge_ir::Value;

    fn iv(value: u32, start: u32, end: u32, class: RegClass) -> Interval {
        Interval {
            value: Value(value),
            start,
            end,
            reg_class: class,
            hint: None,
            fixed: None,
            spill_weight: 0.0,
        }
    }

    #[test]
    fn reports_disjoint_intervals_correctly() {
        let intervals = vec![
            iv(0, 0, 1, RegClass::Gpr),
            iv(1, 3, 4, RegClass::Gpr),
            iv(2, 6, 7, RegClass::Gpr),
        ];
        let pressure = register_pressure(&intervals, 10);

        assert_eq!(pressure.len(), 10);
        assert_eq!(pressure[0], PressurePoint { gpr: 1, xmm: 0 });
        assert_eq!(pressure[1], PressurePoint { gpr: 1, xmm: 0 });
        assert_eq!(pressure[2], PressurePoint { gpr: 0, xmm: 0 }); // gap between intervals
        assert_eq!(pressure[3], PressurePoint { gpr: 1, xmm: 0 });
        assert_eq!(pressure[5], PressurePoint { gpr: 0, xmm: 0 });
        assert_eq!(pressure[6], PressurePoint { gpr: 1, xmm: 0 });
        assert_eq!(pressure[8], PressurePoint { gpr: 0, xmm: 0 });
    }

    #[test]
    fn reports_overlapping_intervals_correctly() {
        let intervals = vec![
            iv(0, 0, 5, RegClass::Gpr),
            iv(1, 0, 5, RegClass::Gpr),
            iv(2, 0, 5, RegClass::Gpr),
        ];
        let pressure = register_pressure(&intervals, 6);

        for (pos, p) in pressure.iter().enumerate() {
            assert_eq!(
                *p,
                PressurePoint { gpr: 3, xmm: 0 },
                "position {pos} must show all 3 intervals live"
            );
        }
    }

    #[test]
    fn keeps_gpr_and_xmm_independent() {
        let intervals = vec![
            iv(0, 0, 5, RegClass::Gpr),
            iv(1, 0, 5, RegClass::Gpr),
            iv(2, 0, 5, RegClass::Xmm),
        ];
        let pressure = register_pressure(&intervals, 6);

        assert_eq!(pressure[2], PressurePoint { gpr: 2, xmm: 1 });
    }

    #[test]
    fn is_zero_past_every_intervals_end() {
        let intervals = vec![iv(0, 0, 2, RegClass::Gpr)];
        let pressure = register_pressure(&intervals, 10);

        for (pos, p) in pressure.iter().enumerate().skip(3) {
            assert_eq!(
                *p,
                PressurePoint { gpr: 0, xmm: 0 },
                "position {pos} is past the only interval's end -- must be zero, not stale"
            );
        }
    }

    #[test]
    fn does_not_panic_when_program_length_is_smaller_than_an_intervals_end() {
        // A deliberately inconsistent input -- program_length shorter than
        // a real interval's end. Must not panic and must not wrap around
        // to a huge u32 from an unmatched decrement.
        let intervals = vec![iv(0, 1, 10, RegClass::Gpr), iv(1, 0, 100, RegClass::Xmm)];
        let pressure = register_pressure(&intervals, 4);

        assert_eq!(pressure.len(), 4);
        for p in &pressure {
            assert!(
                p.gpr < 1000 && p.xmm < 1000,
                "must not wrap around to a huge count: {p:?}"
            );
        }
    }

    #[test]
    fn handles_program_length_zero() {
        let intervals = vec![iv(0, 0, 5, RegClass::Gpr)];
        let pressure = register_pressure(&intervals, 0);

        assert!(pressure.is_empty());
    }
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo build -p forge-regalloc --tests 2>&1 | tail -20`
Expected: fails to compile — `register_pressure`/`PressurePoint` don't exist yet.

- [ ] **Step 4: Implement `PressurePoint` and `register_pressure`**

Add to `crates/forge-regalloc/src/pressure.rs`, above the `#[cfg(test)]` block:

```rust
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
/// interval's own `end`, and silently under-sizing the report would leave
/// those trailing positions unrepresented rather than correctly reported
/// as zero pressure. Callers pass `selected.insts.len()`.
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

- [ ] **Step 5: Run to verify the hand-built tests pass**

Run: `cargo test -p forge-regalloc --lib pressure::tests`
Expected: PASS (6 tests).

- [ ] **Step 6: Add the corpus-wide cross-check test**

Add to the SAME `#[cfg(test)] mod tests` block in `pressure.rs`, after the 6 hand-built tests above:

```rust
    #[test]
    fn matches_program_length_and_stays_within_pool_plus_spills_for_the_corpus() {
        for src in test_corpus() {
            let func = front_end(src);
            let selected = forge_x64::select(&func);
            let intervals = crate::intervals::build_intervals(&func, &selected);
            let excluded = crate::intervals::excluded_registers(&func, &selected);
            let (assignment, _bytes) =
                crate::linear_scan::allocate(intervals.clone(), &excluded, &selected);

            let pressure = register_pressure(&intervals, selected.insts.len());
            assert_eq!(
                pressure.len(),
                selected.insts.len(),
                "{src:?}: pressure report length must equal the real instruction count"
            );

            let is_spill = |iv: &&Interval| {
                matches!(
                    assignment.get(&iv.value),
                    Some(crate::linear_scan::Location::Spill(_))
                )
            };
            let spilled_gpr = intervals
                .iter()
                .filter(|iv| iv.reg_class == RegClass::Gpr)
                .filter(is_spill)
                .count() as u32;
            let spilled_xmm = intervals
                .iter()
                .filter(|iv| iv.reg_class == RegClass::Xmm)
                .filter(is_spill)
                .count() as u32;

            let peak_gpr = pressure.iter().map(|p| p.gpr).max().unwrap_or(0);
            let peak_xmm = pressure.iter().map(|p| p.xmm).max().unwrap_or(0);

            // SPILL_AWARE_ALLOCATABLE_GPR/XMM, NOT the wider ALLOCATABLE_GPR/XMM --
            // that's the pool allocate() actually scans against since Phase 8c.
            // On THIS corpus every program spills zero values (consistent with
            // linear_scan.rs's own `run_produces_only_reg_locations_never_spill_
            // for_the_corpus`), so `spilled_gpr`/`spilled_xmm` are always 0 here
            // and the bound is comfortably slack (measured: peak GPR 4 vs pool
            // 12, peak XMM 7 vs pool 14) -- this test's job is confirming the
            // bound HOLDS with real numbers, not that it's tight; a separate,
            // randomized high-pressure exercise during design review (not part
            // of this corpus) is what measured the bound as tight (zero slack)
            // on 186/200 programs that actually spill.
            let gpr_pool = crate::linear_scan::SPILL_AWARE_ALLOCATABLE_GPR.len() as u32;
            let xmm_pool = crate::linear_scan::SPILL_AWARE_ALLOCATABLE_XMM.len() as u32;
            assert!(
                peak_gpr <= gpr_pool + spilled_gpr,
                "{src:?}: peak GPR pressure {peak_gpr} exceeds pool size + spills"
            );
            assert!(
                peak_xmm <= xmm_pool + spilled_xmm,
                "{src:?}: peak XMM pressure {peak_xmm} exceeds pool size + spills"
            );
        }
    }

    /// Shared corpus, copied VERBATIM from `crates/forge-regalloc/src/linear_scan.rs`'s
    /// `test_corpus()` -- see verify.rs's identical duplication for the established
    /// reasoning (this project's convention, not an oversight).
    fn test_corpus() -> Vec<&'static str> {
        vec![
            "3.14159 * r * r",
            "sin(x) + cos(y)",
            "(n * 2654435761) >> 16",
            "x / y",
            "x + 1",
            "fma(a, b, c)",
            "base + i * 8",
            "let t = a - b in if t > 0.0 then t else -t",
            "if a > b then (if a > c then a else c) else b",
            "(if a > b then a else b) + a",
            "sqrt(x * x + y * y)",
            "abs(x) + floor(y) + ceil(z)",
            "(n >> 1) % 7 + (n >> 1) / 7",
            "if a > b then (a * c) + (b * c) else a - b",
            "if a > b then (a - b) - (a + b) else c - a",
            "if a > b then fma(a, b, c) else a * c",
            "if x > y then (x * y) + (x - y) else x / y",
            "if x > y then fma(x, y, z) * x else fma(y, x, z) - y",
        ]
    }

    fn front_end(src: &str) -> forge_ir::Function {
        let (tokens, diags) = forge_syntax::lexer::lex(src);
        assert!(diags.is_empty(), "lex errors for {src:?}: {diags:?}");
        let (ast, diags) = forge_syntax::parser::parse(&tokens);
        assert!(diags.is_empty(), "parse errors for {src:?}: {diags:?}");
        let typed = forge_syntax::typeck::typecheck(forge_syntax::resolve::resolve(ast))
            .unwrap_or_else(|e| panic!("type errors for {src:?}: {e:?}"));
        forge_ir::lower::lower(&typed)
    }
```

- [ ] **Step 7: Run to verify everything passes**

Run: `cargo test -p forge-regalloc --lib pressure::tests`
Expected: PASS (7 tests total).

- [ ] **Step 8: Re-export from `lib.rs`**

Add to `crates/forge-regalloc/src/lib.rs`:

```rust
pub use pressure::{register_pressure, PressurePoint};
```

- [ ] **Step 9: Run the full workspace suite, clippy, and fmt**

Run: `cargo test --workspace 2>&1 | tail -40`
Expected: PASS. `forge-regalloc` should show 79 tests (72 from Task 1 + 7 new).

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -60`
Expected: clean.

Run: `cargo fmt --check`
Expected: clean. If it reports diffs, run `cargo fmt` and re-check.

- [ ] **Step 10: Commit**

```bash
git add crates/forge-regalloc/src/pressure.rs crates/forge-regalloc/src/lib.rs
git commit -m "feat(forge-regalloc): add register_pressure diagnostic report"
```

---

## Self-review notes (already applied above, recorded for the implementer's context)

- **Spec coverage**: `verify_allocation` covers all of the design doc's overlap/exemption/spill-slot behavior including both handoff directions and the spill no-exemption case; `register_pressure` covers the dense sweep-line, class independence, and clamp behavior. `evict_and_assign`'s deferred victim case and any Phase 11 "wire into every compile" integration are explicitly out of scope per the design doc — no task here attempts either.
- **Type consistency check**: `verify_allocation(intervals: &[Interval], assignment: &HashMap<Value, Location>) -> Result<(), String>` and `register_pressure(intervals: &[Interval], program_length: usize) -> Vec<PressurePoint>` are used identically in every step and test above — confirmed no drift between each function's own definition and its call sites in the corpus-wide tests.
- **Placeholder scan**: no task above contains a TBD, a "handle appropriately," or an unshown code block — every step's code is the literal text to write. The `test_corpus()`/`front_end()` duplication between Task 1 and Task 2 is intentional (see "Before you start"), not copy-paste drift to be caught and flagged.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-08-09-phase-8d-verification-reporting-plan.md`. Per this project's established cadence, this plan is next sent to a dispatched subagent for its own execution-based review before subagent-driven implementation begins.
