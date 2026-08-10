# Phase 8c — Spilling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement real spill-slot allocation for `crates/forge-regalloc`'s linear scan allocator — victim selection (`spill_at_interval`), a use-density spill-weight heuristic (`populate_spill_weights`), spill slot assignment with reuse (`spill`), and 2 reserved scratch registers per class for reload/store traffic — closing out CHECKLIST.md Phase 8 bullets 11-14.

**Architecture:** Everything lives in the single existing file `crates/forge-regalloc/src/linear_scan.rs` (Phase 8b's file — this slice amends it, doesn't split it). Two new constant pairs (`SCRATCH_GPR`/`SCRATCH_XMM`, `SPILL_AWARE_ALLOCATABLE_GPR`/`_XMM`) shrink the pools `LinearScan` scans over. `spill_at_interval` (currently `unimplemented!()`) becomes real victim-selection logic, calling a new `spill()` method that assigns spill slots via an order-independent `slot_end: Vec<u32>` high-water-mark (never a scan-cursor-relative expiry list — that mechanism was tried, proven wrong by execution during design review, and removed from the design entirely). `allocate()` gains a `selected: &SelectedFunction` parameter (to call the new `populate_spill_weights`) and changes its return type to `(HashMap<Value, Location>, u32)`, threading `slot_end` between its GPR and XMM passes so slot numbers stay globally unique across both register classes.

**Tech Stack:** Rust, `crates/forge-regalloc` (this crate), depends on `forge-ir` (`Value`) and `forge-x64` (`PhysReg`, `SelectedFunction`, `MachineInst`, `select`).

**Design doc:** `docs/superpowers/specs/2026-08-09-phase-8c-spilling-design.md` — execution-verified (confirmed by an independent review that compiled and ran every corrected code path: const-eval of the new pool constants, the `slot_end` mechanism including its B4/B5 regression scenarios, B6's exclusion check, B2's destructuring fix). Treat every code block referenced below as verified, not merely proposed.

---

## Before you start

Read `crates/forge-regalloc/src/linear_scan.rs` in full — every task below modifies this one file, and several steps depend on exact current code (line numbers will drift after edits, so match by function/test name, not line number). In particular:

- `LinearScan::new`'s current signature is `(intervals: Vec<Interval>, excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>, allocatable: &'a [PhysReg])` — **14 total call sites** in the file (13 inside `#[cfg(test)] mod tests`, plus 1 inside `allocate()` itself) call this with exactly 3 arguments. Every one of them needs a 4th argument once Task 2 adds `slot_end` — Task 2 handles the 13 test-module sites (12 of them get the mechanical fix directly; the 13th, `spill_at_interval_panics_with_a_clear_deferral_message`, is deleted outright in Task 3), and Task 5 handles `allocate()`'s own call site as part of its larger rewrite.
- `allocate`'s current signature is `(intervals: Vec<Interval>, excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>) -> HashMap<Value, Location>`. **4 existing test functions** call it with 2 arguments and use the return value directly as a `HashMap` — every one of them needs updating once Task 5 changes the signature and return type: `run_allocates_a_straight_line_chain_via_transfers`, `run_never_shares_a_register_between_genuinely_conflicting_values`, `run_honors_a_non_trivial_fraction_of_hints`, `run_produces_only_reg_locations_never_spill_for_the_corpus`.
- The existing test helper `iv(value: u32, start: u32, end: u32, class: RegClass) -> Interval` (private to the test module) stays exactly as-is and gets reused throughout this plan's new tests.
- `reads_of(inst: &MachineInst) -> Vec<Value>` is `pub(crate)` in `crates/forge-regalloc/src/liveness.rs` — already importable from `linear_scan.rs` as `crate::liveness::reads_of`.

Run `cargo test -p forge-regalloc` once before starting, to confirm your baseline is the expected 44 passing tests (per Phase 8b's shipped state) with none failing.

---

### Task 1: Scratch registers and spill-aware allocatable pools

**Files:**
- Modify: `crates/forge-regalloc/src/linear_scan.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block (near the existing `allocatable_gpr_excludes_rsp_and_rbp` / `allocatable_xmm_excludes_xmm16_through_31` tests):

```rust
    #[test]
    fn scratch_and_spill_aware_pools_are_disjoint_and_union_complete_gpr() {
        let scratch: HashSet<PhysReg> = SCRATCH_GPR.iter().copied().collect();
        let spill_aware: HashSet<PhysReg> = SPILL_AWARE_ALLOCATABLE_GPR.iter().copied().collect();
        let original: HashSet<PhysReg> = ALLOCATABLE_GPR.iter().copied().collect();

        assert_eq!(scratch.len(), 2);
        assert_eq!(spill_aware.len(), 12);
        assert!(
            scratch.is_disjoint(&spill_aware),
            "a register can't be both scratch-reserved and ordinarily allocatable"
        );
        let union: HashSet<PhysReg> = scratch.union(&spill_aware).copied().collect();
        assert_eq!(
            union, original,
            "scratch + spill-aware must reconstruct ALLOCATABLE_GPR exactly"
        );
    }

    #[test]
    fn scratch_and_spill_aware_pools_are_disjoint_and_union_complete_xmm() {
        let scratch: HashSet<PhysReg> = SCRATCH_XMM.iter().copied().collect();
        let spill_aware: HashSet<PhysReg> = SPILL_AWARE_ALLOCATABLE_XMM.iter().copied().collect();
        let original: HashSet<PhysReg> = ALLOCATABLE_XMM.iter().copied().collect();

        assert_eq!(scratch.len(), 2);
        assert_eq!(spill_aware.len(), 14);
        assert!(scratch.is_disjoint(&spill_aware));
        let union: HashSet<PhysReg> = scratch.union(&spill_aware).copied().collect();
        assert_eq!(union, original);
    }

    #[test]
    fn scratch_gpr_is_caller_saved_not_callee_saved() {
        // R10/R11, not R14/R15 -- R14/R15 are members of
        // prologue::SYSV_CALLEE_SAVED, which would force every spilling
        // function's prologue/epilogue to push/pop a pair of registers
        // used only transiently. R10/R11 have no such cost.
        assert_eq!(SCRATCH_GPR, [PhysReg::R10, PhysReg::R11]);
        for r in SCRATCH_GPR {
            assert!(
                !forge_x64::SYSV_CALLEE_SAVED.contains(&r),
                "{r:?} must be caller-saved to avoid an unnecessary push/pop pair"
            );
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p forge-regalloc scratch_and_spill_aware`
Expected: FAIL to compile — `SCRATCH_GPR`, `SCRATCH_XMM`, `SPILL_AWARE_ALLOCATABLE_GPR`, `SPILL_AWARE_ALLOCATABLE_XMM` don't exist yet, and `forge_x64::SYSV_CALLEE_SAVED` isn't imported.

First check whether `SYSV_CALLEE_SAVED` is actually re-exported from `forge_x64`'s crate root:

Run: `grep -n "SYSV_CALLEE_SAVED" /Users/sanskar/dev/Research/Projects/JIT-Compiler/crates/forge-x64/src/lib.rs`

If it's not in that `pub use` list, add it there first (it's `pub const SYSV_CALLEE_SAVED` in `crates/forge-x64/src/prologue.rs` already — Phase 7d shipped it — this step only needs a re-export, not a new definition, if one isn't already present).

- [ ] **Step 3: Add the constants**

Add this block right after the existing `ALLOCATABLE_XMM` constant (after line 60, before the `precompute_excluded` function):

```rust
/// Registers reserved exclusively for spill reload/store traffic -- NEVER
/// handed out by pick_register to an ordinary interval. Removed from the
/// pool LinearScan scans over (SPILL_AWARE_ALLOCATABLE_* below), not from
/// ALLOCATABLE_GPR/ALLOCATABLE_XMM themselves, which stay exactly as 8b
/// shipped them (still the authoritative "which PhysRegs exist and are
/// encodable" answer). R10/R11 (not R14/R15) for GPR: R14/R15 are both
/// members of prologue::SYSV_CALLEE_SAVED, so reserving them would force
/// every spilling function's prologue/epilogue to push/pop a pair of
/// registers used only transiently; R10/R11 are caller-saved, no such
/// cost. For XMM, Xmm14/Xmm15: ALL XMM registers are caller-saved under
/// System V, so there is no equivalent cost differential to correct for.
pub const SCRATCH_GPR: [PhysReg; 2] = [PhysReg::R10, PhysReg::R11];
pub const SCRATCH_XMM: [PhysReg; 2] = [PhysReg::Xmm14, PhysReg::Xmm15];

// R10/R11 sit at indices 8-9 of ALLOCATABLE_GPR's declared order (Rax,
// Rcx, Rdx, Rbx, Rsi, Rdi, R8, R9, R10, R11, R12, R13, R14, R15), NOT the
// last two entries -- `.split_at(12).0` would keep R10/R11 IN the pool
// while also claiming they're scratch-reserved, a direct contradiction.
// An explicit literal is the only construction that's correct regardless
// of which two registers scratch picks.
pub const SPILL_AWARE_ALLOCATABLE_GPR: &[PhysReg] = &[
    PhysReg::Rax,
    PhysReg::Rcx,
    PhysReg::Rdx,
    PhysReg::Rbx,
    PhysReg::Rsi,
    PhysReg::Rdi,
    PhysReg::R8,
    PhysReg::R9,
    PhysReg::R12,
    PhysReg::R13,
    PhysReg::R14,
    PhysReg::R15,
]; // 14 - 2 reserved (R10, R11 excluded)

// Xmm14/Xmm15 ARE the last two entries of ALLOCATABLE_XMM, so split_at is
// correct here (unlike the GPR case above, where the excluded pair isn't
// at the end).
pub const SPILL_AWARE_ALLOCATABLE_XMM: &[PhysReg] = ALLOCATABLE_XMM.split_at(14).0; // 16 - 2 reserved
```

If `SYSV_CALLEE_SAVED` needed a re-export in Step 2, add it to `crates/forge-x64/src/lib.rs`'s existing `pub use` list for `prologue` (check that file for the exact existing `pub use prologue::{...}` line and add `SYSV_CALLEE_SAVED` to it). In practice it's already there (confirmed by an execution-based review of this plan), so this is expected to be a no-op — don't be surprised if there's nothing to change.

Also update `crates/forge-regalloc/src/lib.rs`'s existing re-export line for `linear_scan` — `SCRATCH_GPR`/`SCRATCH_XMM` are referenced only from `#[cfg(test)]` code in this slice (real reload/store byte emission is a later, out-of-scope task per the design doc), so without a re-export `cargo clippy` sees them as genuinely dead code and fails the `-D warnings` gate at the very end of this plan (Task 6 Step 5) — confirmed by execution. Fix it now rather than later: change

```rust
pub use linear_scan::{allocate, Location, ALLOCATABLE_GPR, ALLOCATABLE_XMM};
```
to
```rust
pub use linear_scan::{
    allocate, Location, ALLOCATABLE_GPR, ALLOCATABLE_XMM, SCRATCH_GPR, SCRATCH_XMM,
};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p forge-regalloc scratch`
Expected: PASS (3 tests: `scratch_and_spill_aware_pools_are_disjoint_and_union_complete_gpr`, `scratch_and_spill_aware_pools_are_disjoint_and_union_complete_xmm`, `scratch_gpr_is_caller_saved_not_callee_saved` — `cargo test` takes exactly ONE positional name filter, not several; a single substring like `scratch` matches all 3 at once).

- [ ] **Step 5: Commit**

```bash
cd /Users/sanskar/dev/Research/Projects/JIT-Compiler
git add crates/forge-regalloc/src/linear_scan.rs crates/forge-regalloc/src/lib.rs crates/forge-x64/src/lib.rs
git commit -m "feat(forge-regalloc): add SCRATCH_GPR/XMM and SPILL_AWARE_ALLOCATABLE_GPR/XMM"
```

---

### Task 2: `slot_end` field, constructor threading, and `spill()`

**Files:**
- Modify: `crates/forge-regalloc/src/linear_scan.rs`

- [ ] **Step 1: Add the `slot_end` field and thread it through the constructor**

Change the `LinearScan` struct definition:

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

Change `LinearScan::new`:

```rust
    fn new(
        intervals: Vec<Interval>,
        excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>,
        allocatable: &'a [PhysReg],
        slot_end: Vec<u32>,
    ) -> Self {
        LinearScan {
            intervals,
            active: Vec::new(),
            free_regs: allocatable.iter().copied().collect(),
            assignment: HashMap::new(),
            excluded: precompute_excluded(excluded_registers),
            allocatable,
            slot_end,
        }
    }
```

- [ ] **Step 2: Fix every existing call site (mechanical, no behavior change)**

`LinearScan::new` now takes 4 arguments. Update every 3-argument call site in the file to pass `Vec::new()` as the 4th argument. Use this command to find every one of them first:

Run: `grep -n "LinearScan::new(" crates/forge-regalloc/src/linear_scan.rs`

Expected output lists these locations (both in `allocate()` itself — handled separately in Task 5 — and in the test module): `excluded_at_returns_empty_set_for_unlisted_value`, `expire_old_intervals_keeps_touching_intervals_active`, `expire_old_intervals_frees_genuinely_disjoint_intervals`, `pick_register_case2_transfers_ownership_on_same_instruction_reuse`, `pick_register_case1_honors_a_hint_whose_target_already_expired`, `pick_register_falls_back_to_free_register_when_hint_unusable`, `pick_register_respects_exclusions_even_for_a_legitimate_handoff`, `pick_register_refused_exclusion_does_not_remove_the_target_from_active`, `pick_register_case2_when_active_position_differs_from_interval_index`, `evict_and_assign_no_victim_succeeds`, `evict_and_assign_with_a_victim_panics`, `spill_at_interval_panics_with_a_clear_deferral_message` (this one gets fully replaced in Task 3, skip updating it here — just leave it broken, Task 3 rewrites it), and one inside `active_stays_sorted_by_end_throughout_every_corpus_run` (inside its `for (class, pool) in [...]` loop).

For every one of these EXCEPT `spill_at_interval_panics_with_a_clear_deferral_message` (12 of the 13 named test-module sites — the 13th is deleted outright in Task 3, not edited here), add `, Vec::new()` as a 4th argument to the `LinearScan::new(...)` call. For example:

```rust
        let mut scan = LinearScan::new(vec![], &HashMap::new(), ALLOCATABLE_GPR);
```
becomes
```rust
        let mut scan = LinearScan::new(vec![], &HashMap::new(), ALLOCATABLE_GPR, Vec::new());
```

Do this for each of the 12 sites listed above (excluding the one in `spill_at_interval_panics_with_a_clear_deferral_message`, and excluding `allocate()`'s own call site, which Task 5 rewrites).

- [ ] **Step 3: Run to confirm only the expected two call sites still fail to compile**

Run: `cargo build -p forge-regalloc --tests 2>&1 | grep "error\[" `
Expected: exactly 2 remaining "this function takes 4 arguments but 3 arguments were supplied" errors — one inside `allocate()`, one inside `spill_at_interval_panics_with_a_clear_deferral_message`. Both are handled by later tasks/steps; leave them for now and proceed (the crate won't compile until Task 2 Step 5 and Task 3 are both done — that's expected mid-task state, not a stopping point).

- [ ] **Step 4: Write `spill()`'s failing tests**

Add to the test module:

```rust
    #[test]
    fn spill_reuses_a_slot_for_genuinely_disjoint_intervals() {
        let a = iv(0, 0, 2, crate::interval::RegClass::Gpr);
        let b = iv(1, 3, 5, crate::interval::RegClass::Gpr); // disjoint: starts after a.end
        let mut scan = LinearScan::new(vec![a, b], &HashMap::new(), SPILL_AWARE_ALLOCATABLE_GPR, Vec::new());

        scan.spill(0);
        scan.spill(1);

        assert_eq!(scan.assignment[&Value(0)], Location::Spill(0));
        assert_eq!(
            scan.assignment[&Value(1)],
            Location::Spill(0),
            "disjoint intervals must reuse the same slot"
        );
    }

    #[test]
    fn spill_does_not_reuse_a_slot_for_touching_intervals() {
        // [0,2] and [2,4] TOUCH at position 2 -- under 8a's inclusive
        // convention this IS an overlap (mirrors expire_old_intervals's
        // register boundary exactly), so they must NOT share a slot.
        let a = iv(0, 0, 2, crate::interval::RegClass::Gpr);
        let b = iv(1, 2, 4, crate::interval::RegClass::Gpr);
        let mut scan = LinearScan::new(vec![a, b], &HashMap::new(), SPILL_AWARE_ALLOCATABLE_GPR, Vec::new());

        scan.spill(0);
        scan.spill(1);

        assert_ne!(
            scan.assignment[&Value(0)],
            scan.assignment[&Value(1)],
            "touching intervals must NOT share a slot"
        );
    }

    #[test]
    fn spill_slot_choice_depends_on_the_intervals_own_start_not_call_order() {
        // B4 regression: the original free_slots/next_slot design reused a
        // slot that was only "free from the current scan cursor onward,"
        // not free across a victim interval's own (much earlier) start.
        // X([0,6]) is spilled first; G([5,300]) is spilled second but its
        // OWN start (5) genuinely overlaps X's range -- it must NOT reuse
        // X's slot, regardless of scan order.
        let x = iv(0, 0, 6, crate::interval::RegClass::Gpr);
        let g = iv(1, 5, 300, crate::interval::RegClass::Gpr);
        let mut scan = LinearScan::new(vec![x, g], &HashMap::new(), SPILL_AWARE_ALLOCATABLE_GPR, Vec::new());

        scan.spill(0); // X -> some slot, slot_end for it becomes 6
        scan.spill(1); // G, start=5 -- 6 is NOT < 5, so no reuse

        assert_ne!(
            scan.assignment[&Value(0)],
            scan.assignment[&Value(1)],
            "B4: G's start (5) is behind X's end (6) -- must get a fresh slot"
        );

        // Positive case, same scan: H([10,20]) spilled AFTER X and G --
        // X's slot now has slot_end=6, and 6 < 10, so H correctly reuses it.
        let h = iv(2, 10, 20, crate::interval::RegClass::Gpr);
        scan.intervals.push(h);
        scan.spill(2);

        assert_eq!(
            scan.assignment[&Value(2)],
            scan.assignment[&Value(0)],
            "H starts at 10, well past X's slot's recorded end (6) -- must reuse X's slot"
        );
    }

    #[test]
    fn spill_removes_the_interval_from_active_if_present() {
        let a = iv(0, 0, 10, crate::interval::RegClass::Gpr);
        let mut scan = LinearScan::new(vec![a], &HashMap::new(), SPILL_AWARE_ALLOCATABLE_GPR, Vec::new());
        scan.assign(0, Location::Reg(PhysReg::Rax));
        scan.active.push(0);

        scan.spill(0);

        assert!(scan.active.is_empty());
        assert_eq!(scan.assignment[&Value(0)], Location::Spill(0));
    }
```

- [ ] **Step 5: Run to verify they fail**

Run: `cargo test -p forge-regalloc spill_reuses 2>&1 | tail -20`
Expected: FAIL to compile — `spill` method doesn't exist on `LinearScan` yet. (`cargo test` takes exactly one positional name filter; use separate invocations, e.g. `spill_reuses`, `spill_does_not_reuse`, `spill_slot_choice`, `spill_removes`, if you want to isolate each one instead of relying on the substring `spill_` matching all of Task 2's tests together in Step 7 below.)

- [ ] **Step 6: Implement `spill()`**

Add this method inside `impl<'a> LinearScan<'a>`, right after `evict_and_assign` (before `spill_at_interval`, which Task 3 will replace):

```rust
    /// Assigns interval `i` a spill slot. `slot_end[s]` records the
    /// highest `end` any interval placed in slot `s` has ever had; a slot
    /// is safe to reuse for interval `i` iff `slot_end[s] < i.start` --
    /// the same inclusive-boundary rule `expire_old_intervals` uses for
    /// registers (`end >= current_start` still means "in use"), just
    /// phrased against the requesting interval's own start instead of a
    /// scan cursor. This is deliberately order-independent: it gives the
    /// same, correct answer whether `i` is the interval currently being
    /// scanned or a victim from deep in `active` with a much earlier
    /// `start` -- an earlier draft of this design instead ported
    /// expire_old_intervals's cursor-relative expiry mechanism directly
    /// (parameterized over slots instead of registers, with a `spilled`
    /// list, `free_slots` stack, and `next_slot` counter) and execution
    /// proved that literal port wrong twice over (reusing a slot still
    /// live across a victim's own earlier range; a cross-class free_slots
    /// thread colliding GPR and XMM spills onto the same slot). Comparing
    /// only against the interval's own start avoids both failure modes
    /// and needs no expiry step, no `spilled` list, and no `free_slots`
    /// stack at all.
    fn spill(&mut self, i: usize) {
        self.active.retain(|&j| j != i);
        let (start, end) = (self.intervals[i].start, self.intervals[i].end);
        let slot = match self.slot_end.iter().position(|&e| e < start) {
            Some(s) => s,
            None => {
                self.slot_end.push(0);
                self.slot_end.len() - 1
            }
        };
        self.slot_end[slot] = self.slot_end[slot].max(end);
        self.assign(i, Location::Spill(slot as u32));
    }
```

- [ ] **Step 7: Confirm this step's edits are consistent, but do NOT expect a passing test run yet**

Run: `cargo build -p forge-regalloc --tests 2>&1 | grep "error\["`
Expected: still exactly 2 `E0061` errors — the SAME two left over from Step 3 (`allocate()`'s own stale call site, fixed in Task 5; `spill_at_interval_panics_with_a_clear_deferral_message`, deleted in Task 3). Rust requires the whole crate to compile before ANY test binary can run, so `cargo test -p forge-regalloc spill_reuses` (or any other filter) will FAIL with the same 2 compile errors at this point, not run 4 passing tests — that's expected, not a regression. There is no way to get a green `cargo test` run until Task 3 (which removes one of the 2 remaining errors) and Task 5 (which removes the other) are both done. `spill()`'s correctness is confirmed once the crate compiles again, at the end of Task 5.

- [ ] **Step 8: Commit**

```bash
git add crates/forge-regalloc/src/linear_scan.rs
git commit -m "feat(forge-regalloc): add slot_end-based spill() with order-independent reuse"
```

---

### Task 3: `spill_at_interval` — victim selection with exclusion-safe reassignment

**Files:**
- Modify: `crates/forge-regalloc/src/linear_scan.rs`

- [ ] **Step 1: Delete the old stub and its test**

Delete the current `spill_at_interval` method body (the `unimplemented!(...)` stub) and delete the test `spill_at_interval_panics_with_a_clear_deferral_message` entirely (it tested the stub's deferral message, which no longer exists).

- [ ] **Step 2: Write the failing tests**

Add to the test module (where the deleted test used to be):

```rust
    #[test]
    fn spill_at_interval_spills_the_longer_lived_victim_and_hands_i_its_register() {
        // Branch (a): victim.end (100) > i.end (10) -- spill the victim,
        // give its now-free register to i.
        let victim = iv(0, 0, 100, crate::interval::RegClass::Gpr);
        let current = iv(1, 5, 10, crate::interval::RegClass::Gpr);
        let mut scan = LinearScan::new(
            vec![victim, current],
            &HashMap::new(),
            SPILL_AWARE_ALLOCATABLE_GPR,
            Vec::new(),
        );
        scan.assign(0, Location::Reg(PhysReg::Rax));
        scan.active.push(0);

        scan.spill_at_interval(1);

        assert!(
            matches!(scan.assignment[&Value(0)], Location::Spill(_)),
            "victim must be spilled"
        );
        assert_eq!(
            scan.assignment[&Value(1)],
            Location::Reg(PhysReg::Rax),
            "i must receive the victim's freed register"
        );
        assert_eq!(scan.active, vec![1], "i replaces the victim in active");
    }

    #[test]
    fn spill_at_interval_spills_i_itself_when_the_victim_dies_sooner() {
        // Branch (b): victim.end (5) is NOT > i.end (20) -- spill i,
        // leave the victim untouched.
        let victim = iv(0, 0, 5, crate::interval::RegClass::Gpr);
        let current = iv(1, 3, 20, crate::interval::RegClass::Gpr);
        let mut scan = LinearScan::new(
            vec![victim, current],
            &HashMap::new(),
            SPILL_AWARE_ALLOCATABLE_GPR,
            Vec::new(),
        );
        scan.assign(0, Location::Reg(PhysReg::Rbx));
        scan.active.push(0);

        scan.spill_at_interval(1);

        assert_eq!(
            scan.assignment[&Value(0)],
            Location::Reg(PhysReg::Rbx),
            "victim must be left exactly as it was"
        );
        assert!(
            matches!(scan.assignment[&Value(1)], Location::Spill(_)),
            "i must be spilled instead"
        );
        assert_eq!(scan.active, vec![0], "victim remains the only active interval");
    }

    #[test]
    fn spill_at_interval_respects_exclusion_and_spills_i_instead_of_reassigning() {
        // B6 regression: the victim's register isn't automatically legal
        // for i -- i may carry its own per-instruction exclusion (e.g. an
        // IntDiv/IntRem rhs excluded from Rax/Rdx). Even though the
        // victim's end (100) > i's end (10), handing over Rax must be
        // refused because i's value excludes it.
        let victim = iv(0, 0, 100, crate::interval::RegClass::Gpr);
        let current = iv(1, 5, 10, crate::interval::RegClass::Gpr);
        let mut raw: HashMap<(usize, Value), Vec<PhysReg>> = HashMap::new();
        raw.insert((5, Value(1)), vec![PhysReg::Rax]);
        let mut scan = LinearScan::new(
            vec![victim, current],
            &raw,
            SPILL_AWARE_ALLOCATABLE_GPR,
            Vec::new(),
        );
        scan.assign(0, Location::Reg(PhysReg::Rax));
        scan.active.push(0);

        scan.spill_at_interval(1);

        assert_eq!(
            scan.assignment[&Value(0)],
            Location::Reg(PhysReg::Rax),
            "victim must be left untouched -- it was never a valid source for i"
        );
        assert!(
            matches!(scan.assignment[&Value(1)], Location::Spill(_)),
            "i must be spilled instead of receiving an excluded register"
        );
        assert_eq!(scan.active, vec![0]);
    }

    #[test]
    #[should_panic(expected = "no active interval to spill")]
    fn spill_at_interval_panics_when_active_is_empty_for_the_class() {
        let a = iv(0, 0, 5, crate::interval::RegClass::Gpr);
        let mut scan = LinearScan::new(vec![a], &HashMap::new(), SPILL_AWARE_ALLOCATABLE_GPR, Vec::new());
        // active is empty -- spill_at_interval has nothing to pick a
        // victim from, which should never happen in practice (an empty
        // active list means pick_register should have found a free
        // register), but must fail loudly if it ever does.
        scan.spill_at_interval(0);
    }
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo build -p forge-regalloc --tests 2>&1 | tail -30`
Expected: still fails to compile (the old `spill_at_interval` body is gone, `allocate()`'s stale 3-arg call also still broken) — proceed straight to implementation, there's no useful intermediate green state here since `allocate()` is also mid-edit.

- [ ] **Step 4: Implement `spill_at_interval`**

Replace the (now-empty) `spill_at_interval` method with:

```rust
    /// Called when `pick_register` returns `None` for interval `i` -- no
    /// free, non-excluded register exists in the current class's pool.
    /// Picks the ACTIVE interval (same class, since spilling an XMM value
    /// can't free a GPR) with the worst score -- `end / spill_weight`,
    /// PROMPT.md's own formula, weighting toward "blocks a register for a
    /// long time AND isn't used much" -- and either:
    /// - if the victim's `end` is LATER than `i`'s own `end` AND the
    ///   victim's register isn't excluded for `i`: spill the VICTIM (it
    ///   was going to cost more to keep than `i` will), hand its now-free
    ///   register to `i`.
    /// - otherwise (including when the victim's register IS excluded for
    ///   `i`): spill `i` itself, leaving the victim exactly as it was.
    fn spill_at_interval(&mut self, i: usize) {
        let class = self.intervals[i].reg_class;
        let victim = self
            .active
            .iter()
            .copied()
            .filter(|&j| self.intervals[j].reg_class == class)
            .max_by(|&a, &b| {
                let score = |k: usize| {
                    let iv = &self.intervals[k];
                    iv.end as f32 / iv.spill_weight.max(0.01)
                };
                score(a).partial_cmp(&score(b)).unwrap()
            })
            .expect(
                "no active interval to spill -- pick_register returned None with an empty active \
                 list for this class, which means the class's WHOLE pool is excluded for interval \
                 i specifically (a real allocator bug, not a spill-heuristic gap: spilling cannot \
                 help when there's nothing active to spill, and i itself has nowhere to go either)",
            );

        if self.intervals[victim].end > self.intervals[i].end {
            let reg = self
                .location_of(victim)
                .and_then(|loc| match loc {
                    Location::Reg(r) => Some(r),
                    Location::Spill(_) => None,
                })
                .expect("an active interval must currently hold a real register, not a spill slot");
            if self.excluded_at(self.intervals[i].value).contains(&reg) {
                self.spill(i);
            } else {
                self.spill(victim);
                self.active.retain(|&j| j != victim);
                self.assign(i, Location::Reg(reg));
                self.active.push(i);
                self.active.sort_by_key(|&j| self.intervals[j].end);
            }
        } else {
            self.spill(i);
        }
    }
```

Note: `self.spill(victim)` already removes `victim` from `self.active` internally (Task 2's `spill()` does `self.active.retain(|&j| j != i)`), so the subsequent `self.active.retain(|&j| j != victim)` line is redundant with `spill`'s own internal retain — keep it anyway for this task (it's a harmless no-op double-retain, not a bug, and removing it is an optional cleanup the design doc explicitly flagged as "very minor, optional," not required for correctness). Do not spend extra time on it.

- [ ] **Step 5: Confirm this step's edits are consistent, but do NOT expect a passing test run yet**

Run: `cargo build -p forge-regalloc --tests 2>&1 | grep "error\["`
Expected: exactly 1 remaining `E0061` error — `allocate()`'s own stale 3-argument `LinearScan::new` call, fixed in Task 5. The crate still won't compile, so `cargo test -p forge-regalloc spill_at_interval` will fail with that same error, not run 4 passing tests. This is expected — `spill_at_interval`'s correctness (both branches, the B6 exclusion regression, and the empty-active `#[should_panic(expected = "no active interval to spill")]` panic) is confirmed once the crate compiles again, at the end of Task 5.

- [ ] **Step 6: Commit**

```bash
git add crates/forge-regalloc/src/linear_scan.rs
git commit -m "feat(forge-regalloc): implement spill_at_interval victim selection"
```

---

### Task 4: `populate_spill_weights`

**Files:**
- Modify: `crates/forge-regalloc/src/linear_scan.rs`

- [ ] **Step 1: Add the import**

At the top of the file, change:

```rust
use crate::interval::{Interval, RegClass};
use forge_ir::Value;
use forge_x64::PhysReg;
use std::collections::{HashMap, HashSet};
```

to:

```rust
use crate::interval::{Interval, RegClass};
use crate::liveness::reads_of;
use forge_ir::Value;
use forge_x64::{PhysReg, SelectedFunction};
use std::collections::{HashMap, HashSet};
```

- [ ] **Step 2: Write the failing tests**

Add to the test module:

```rust
    fn selected_fn(insts: Vec<forge_x64::MachineInst>) -> forge_x64::SelectedFunction {
        forge_x64::SelectedFunction {
            insts,
            synthetic_types: HashMap::new(),
            coalescing_hints: HashMap::new(),
            pool: forge_x64::ConstantPool::default(),
            block_starts: vec![],
        }
    }

    #[test]
    fn populate_spill_weights_computes_uses_over_length() {
        // Value(0) is read twice here (as lhs of one IntAdd, rhs of
        // another) -- 2 uses over an interval of length 2 (start=0,
        // end=2) should give weight 1.0.
        let selected = selected_fn(vec![
            forge_x64::MachineInst::IntAdd {
                dst: Value(10),
                lhs: Value(0),
                rhs: Value(11),
            },
            forge_x64::MachineInst::IntAdd {
                dst: Value(12),
                lhs: Value(11),
                rhs: Value(0),
            },
        ]);
        let mut intervals = vec![iv(0, 0, 2, crate::interval::RegClass::Gpr)];

        populate_spill_weights(&selected, &mut intervals);

        assert_eq!(intervals[0].spill_weight, 1.0);
    }

    #[test]
    fn populate_spill_weights_a_value_used_once_across_a_long_interval_scores_low() {
        let selected = selected_fn(vec![forge_x64::MachineInst::IntAdd {
            dst: Value(10),
            lhs: Value(0),
            rhs: Value(11),
        }]);
        let mut intervals = vec![iv(0, 0, 10, crate::interval::RegClass::Gpr)];

        populate_spill_weights(&selected, &mut intervals);

        assert_eq!(intervals[0].spill_weight, 0.1);
    }

    #[test]
    fn populate_spill_weights_a_value_never_read_scores_zero() {
        let selected = selected_fn(vec![]);
        let mut intervals = vec![iv(0, 0, 10, crate::interval::RegClass::Gpr)];

        populate_spill_weights(&selected, &mut intervals);

        assert_eq!(intervals[0].spill_weight, 0.0);
    }
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo build -p forge-regalloc --tests 2>&1 | tail -20`
Expected: fails to compile — `populate_spill_weights` doesn't exist yet (the crate is still mid-edit from Task 2/3's remaining `allocate()` call site, so a full green isn't expected until Task 5 — this is fine, keep going).

- [ ] **Step 4: Implement `populate_spill_weights`**

Add this as a free function, after the `LinearScan` `impl` block closes (right before `pub fn allocate`):

```rust
/// spill_weight = (number of real reads) / (interval length), matching
/// PROMPT.md's own formula ("uses / length -- spill the cheapest").
/// Computed once, up front, for every interval -- NOT lazily inside
/// spill_at_interval, since the heuristic needs to compare ALL currently
/// active intervals' weights against each other, and re-deriving it
/// per-comparison would risk the two computations silently drifting.
pub fn populate_spill_weights(selected: &SelectedFunction, intervals: &mut [Interval]) {
    let mut use_counts: HashMap<Value, u32> = HashMap::new();
    for inst in &selected.insts {
        for used in reads_of(inst) {
            *use_counts.entry(used).or_insert(0) += 1;
        }
    }
    for iv in intervals.iter_mut() {
        let uses = use_counts.get(&iv.value).copied().unwrap_or(0);
        let length = (iv.end - iv.start).max(1); // avoid a length-0 divide
        iv.spill_weight = uses as f32 / length as f32;
    }
}
```

- [ ] **Step 5: Confirm this step's edits are consistent, but do NOT expect a passing test run yet**

Run: `cargo build -p forge-regalloc --tests 2>&1 | grep "error\["`
Expected: still exactly 1 remaining `E0061` error — `allocate()`'s own stale 3-argument `LinearScan::new` call, fixed in Task 5. The crate still doesn't compile at this point, so `cargo test -p forge-regalloc populate_spill_weights` will fail with that error, not run 3 passing tests. This is expected, not a regression — `populate_spill_weights`'s correctness (the `uses/length` arithmetic and the never-read-scores-zero case) is confirmed once the crate compiles again, at the end of Task 5.

- [ ] **Step 6: Commit**

```bash
git add crates/forge-regalloc/src/linear_scan.rs
git commit -m "feat(forge-regalloc): add populate_spill_weights (uses/length)"
```

---

### Task 5: `allocate()` — new signature, SPILL_AWARE pools, `slot_end` threading

**Files:**
- Modify: `crates/forge-regalloc/src/linear_scan.rs`

- [ ] **Step 1: Replace `allocate()`**

Replace the current `allocate` function body entirely:

```rust
/// Runs linear scan once per register class (GPR, then XMM), merging both
/// partitions' assignments into one final map. No hint or φ-group ever
/// crosses a class boundary, so splitting before scanning never orphans a
/// hint that would have resolved across the split. `slot_end` is threaded
/// GLOBALLY across both class passes (not reset per class) -- a `u32`
/// slot index is a byte-offset multiplier into the stack frame, and both
/// GPR- and XMM-class spilled values need 8 bytes, so both classes must
/// draw from ONE shared numbering space or two independently-zeroed
/// passes could assign the SAME slot number to two genuinely-live values.
pub fn allocate(
    intervals: Vec<Interval>,
    excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>,
    selected: &SelectedFunction,
) -> (HashMap<Value, Location>, u32) {
    let mut intervals = intervals;
    populate_spill_weights(selected, &mut intervals);

    let mut assignment = HashMap::new();
    let mut slot_end: Vec<u32> = Vec::new();
    for (class, pool) in [
        (RegClass::Gpr, SPILL_AWARE_ALLOCATABLE_GPR),
        (RegClass::Xmm, SPILL_AWARE_ALLOCATABLE_XMM),
    ] {
        let class_intervals: Vec<Interval> = intervals
            .iter()
            .filter(|iv| iv.reg_class == class)
            .cloned()
            .collect();
        let mut scan = LinearScan::new(class_intervals, excluded_registers, pool, slot_end);
        scan.run();
        // Destructure rather than a consuming method call after a partial
        // move -- `assignment.extend(scan.assignment)` immediately
        // followed by a by-value method needing the WHOLE `scan` is a
        // compile error (E0382) once `scan.assignment` has already been
        // partially moved out. Destructuring both fields in one statement
        // avoids ever having a whole-self method call after a partial move.
        let LinearScan { assignment: class_assignment, slot_end: next_slot_end, .. } = scan;
        assignment.extend(class_assignment);
        slot_end = next_slot_end;
    }
    (assignment, slot_end.len() as u32 * 8) // total bytes, 8 per slot
}
```

- [ ] **Step 2: Fix `LinearScan`'s field visibility for the destructuring**

`allocate` is in the same module as `LinearScan`, so private-field destructuring already works without any `pub(crate)` changes — confirm this compiles in the next step rather than pre-emptively changing visibility.

- [ ] **Step 3: Update the 4 existing test call sites**

In `run_allocates_a_straight_line_chain_via_transfers`, change:

```rust
        let assignment = allocate(intervals, &excluded);
```
to:
```rust
        let (assignment, _bytes) = allocate(intervals, &excluded, &selected);
```

(the `selected` variable already exists in this test, from `let selected = forge_x64::select(&b.f);` earlier in the function — reuse it.)

In `run_never_shares_a_register_between_genuinely_conflicting_values`, change:

```rust
            let assignment = allocate(intervals.clone(), &excluded);
```
to:
```rust
            let (assignment, _bytes) = allocate(intervals.clone(), &excluded, &selected);
```

In `run_honors_a_non_trivial_fraction_of_hints`, change:

```rust
            let assignment = allocate(intervals.clone(), &excluded);
```
to:
```rust
            let (assignment, _bytes) = allocate(intervals.clone(), &excluded, &selected);
```

In `run_produces_only_reg_locations_never_spill_for_the_corpus`, change:

```rust
            let assignment = allocate(intervals.clone(), &excluded);
```
to:
```rust
            let (assignment, bytes) = allocate(intervals.clone(), &excluded, &selected);
```

and add, right after the existing `assert_eq!(assignment.len(), intervals.len(), ...)` line in that same test:

```rust
            assert_eq!(
                bytes, 0,
                "{src:?}: corpus programs never need a spill even under the narrower \
                 SPILL_AWARE pools (max measured simultaneous liveness is 4 GPR / 7 XMM, \
                 nowhere near the 12/14-register reduced pools) -- a nonzero byte count here \
                 means something in this corpus started needing to spill"
            );
```

- [ ] **Step 4: Run the full test suite**

Run: `cargo test -p forge-regalloc 2>&1 | tail -60`
Expected: PASS, all tests (44 pre-existing + this plan's new ones so far). This is the first point since Task 2 Step 3 where the whole crate compiles again — if anything fails, re-check every `LinearScan::new` and `allocate` call site listed in "Before you start" and Task 5 Step 3 was updated correctly.

- [ ] **Step 5: Commit**

```bash
git add crates/forge-regalloc/src/linear_scan.rs
git commit -m "feat(forge-regalloc): thread slot_end through allocate(), use SPILL_AWARE pools"
```

---

### Task 6: High-pressure integration tests (forced spilling, cross-class regression, extended no-overlap)

**Files:**
- Modify: `crates/forge-regalloc/src/linear_scan.rs`

- [ ] **Step 1: Write the B5 cross-class collision regression test**

Add to the test module:

```rust
    #[test]
    fn allocate_threads_slot_end_across_the_class_boundary_and_avoids_cross_class_collisions() {
        // B5 regression: an earlier draft threaded a `free_slots` stack
        // (not just a counter) between the GPR and XMM passes, which let
        // a slot freed relative to the GPR pass's cursor get reused by
        // the XMM pass restarting its own cursor at 0 -- two genuinely
        // overlapping values, one per class, landed on the same slot.
        //
        // 13 GPR intervals all sharing the exact range [0,100] -- the
        // SPILL_AWARE_ALLOCATABLE_GPR pool has only 12 registers, so
        // exactly the 13th (last-processed, tie-broken by Value order)
        // must spill.
        let mut intervals: Vec<Interval> = (0..13)
            .map(|n| iv(n, 0, 100, crate::interval::RegClass::Gpr))
            .collect();
        // 15 XMM intervals, same range [0,100] -- genuinely overlaps the
        // GPR spill above. SPILL_AWARE_ALLOCATABLE_XMM has only 14
        // registers, so exactly one of these must also spill.
        intervals.extend((100..115).map(|n| iv(n, 0, 100, crate::interval::RegClass::Xmm)));

        let selected = selected_fn(vec![]);
        let (assignment, bytes) = allocate(intervals.clone(), &HashMap::new(), &selected);

        let gpr_spill_slot = intervals
            .iter()
            .filter(|iv| iv.reg_class == crate::interval::RegClass::Gpr)
            .find_map(|iv| match assignment[&iv.value] {
                Location::Spill(s) => Some(s),
                Location::Reg(_) => None,
            })
            .expect("13 GPR intervals into a 12-register pool must produce exactly one spill");
        let xmm_spill_slot = intervals
            .iter()
            .filter(|iv| iv.reg_class == crate::interval::RegClass::Xmm)
            .find_map(|iv| match assignment[&iv.value] {
                Location::Spill(s) => Some(s),
                Location::Reg(_) => None,
            })
            .expect("15 XMM intervals into a 14-register pool must produce exactly one spill");

        assert_ne!(
            gpr_spill_slot, xmm_spill_slot,
            "B5: a GPR spill and an XMM spill with genuinely overlapping ranges must not share a slot"
        );
        assert!(bytes >= 2 * 8 && bytes % 8 == 0);
    }
```

- [ ] **Step 2: Run to verify it fails first (confirm it's not vacuous), then passes**

Run: `cargo test -p forge-regalloc allocate_threads_slot_end_across_the_class_boundary -- --nocapture`

This test should already PASS against Task 5's implementation (there's no new production code in this step) — its purpose is regression coverage, not driving new implementation. Confirm it passes now. If you want to double check it's not vacuous, temporarily change `slot_end = next_slot_end;` in `allocate()` back to `slot_end = Vec::new();` (simulating the old per-class-reset bug), re-run this specific test, confirm it FAILS, then revert the temporary change and re-run to confirm it passes again.

- [ ] **Step 3: Write the end-to-end forced-spill + extended no-overlap test**

Add to the test module:

```rust
    #[test]
    fn allocate_spills_under_pressure_with_a_valid_frame_size_and_no_overlapping_slot_reuse() {
        // 20 GPR intervals, all sharing the exact range [0,50] --
        // SPILL_AWARE_ALLOCATABLE_GPR has 12 registers, so exactly 8 must
        // spill. Because every spilled interval shares the SAME range,
        // NONE can reuse another's slot (slot_end[s] < start never holds
        // when start=0 and slot_end is always >= 0) -- so this exercises
        // the "no possible reuse" edge and gives an exact, not just a
        // lower-bound, expected byte count.
        let intervals: Vec<Interval> = (0..20)
            .map(|n| iv(n, 0, 50, crate::interval::RegClass::Gpr))
            .collect();
        let selected = selected_fn(vec![]);

        let (assignment, bytes) = allocate(intervals.clone(), &HashMap::new(), &selected);

        let spilled: Vec<u32> = intervals
            .iter()
            .filter_map(|iv| match assignment[&iv.value] {
                Location::Spill(s) => Some(s),
                Location::Reg(_) => None,
            })
            .collect();
        assert_eq!(spilled.len(), 8, "20 intervals into a 12-register pool must spill exactly 8");
        assert!(bytes >= spilled.len() as u32 * 8, "frame must be at least large enough for every spill");
        assert_eq!(bytes % 8, 0, "frame size must be a whole number of 8-byte slots");

        // Extended no-overlap property, covering Location::Spill: unlike
        // registers (which have a zero-cost same-instruction handoff
        // exemption via pick_register's Case 2), a spill slot has no such
        // mechanism -- spill()'s `slot_end[s] < start` check is a STRICT
        // inequality, so two values sharing a slot must be genuinely,
        // strictly disjoint; touching at one point is never permitted.
        for i in 0..intervals.len() {
            for j in (i + 1)..intervals.len() {
                let (a, b) = (&intervals[i], &intervals[j]);
                let (Location::Spill(sa), Location::Spill(sb)) =
                    (assignment[&a.value], assignment[&b.value])
                else {
                    continue;
                };
                if sa != sb {
                    continue;
                }
                assert!(
                    a.end < b.start || b.end < a.start,
                    "{:?} and {:?} share spill slot {sa} but are not strictly disjoint",
                    a.value,
                    b.value
                );
            }
        }
    }
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p forge-regalloc allocate_spills_under_pressure`
Expected: PASS.

- [ ] **Step 5: Run the entire workspace test suite, clippy, and fmt**

Run: `cargo test --workspace 2>&1 | tail -40`
Expected: PASS, all crates green.

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -60`
Expected: clean, no warnings.

Run: `cargo fmt --check`
Expected: clean. If it reports diffs, run `cargo fmt` and re-check.

- [ ] **Step 6: Commit**

```bash
git add crates/forge-regalloc/src/linear_scan.rs
git commit -m "test(forge-regalloc): B5 cross-class regression + forced-spill integration test"
```

---

## Self-review notes (already applied above, recorded for the implementer's context)

- **Spec coverage**: every code block in the design doc's "central design decision," "spill_weight," "spill_at_interval," and "spill()" sections has a corresponding task above. `evict_and_assign`'s deferred victim case is explicitly OUT of scope (design doc says so directly) — no task touches it, which is correct, not a gap.
- **Type consistency check**: `populate_spill_weights(selected: &SelectedFunction, intervals: &mut [Interval])`, `allocate(intervals: Vec<Interval>, excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>, selected: &SelectedFunction) -> (HashMap<Value, Location>, u32)`, and `LinearScan::new(intervals, excluded_registers, allocatable, slot_end: Vec<u32>)` are used identically across every task above — confirmed no drift between Task 4's function signature and Task 5's call site, or Task 2's constructor and every other task's call sites.
- **Placeholder scan**: no task above contains a TBD, a "handle appropriately," or an unshown code block — every step's code is the literal text to write.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-08-09-phase-8c-spilling-plan.md`. Per this project's established cadence (used for every prior Phase 7/8 sub-slice), this plan is next sent to a dispatched subagent for its own execution-based review — actually building the code in a scratch worktree and running it, the same way the design doc was verified — before subagent-driven implementation begins.
