# forge Phase 8b Linear Scan Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the linear-scan register assignment loop in `crates/forge-regalloc/src/linear_scan.rs`: `Location`, allocatable register pools, `expire_old_intervals`, `pick_register`, `evict_and_assign`, `spill_at_interval` (stubbed), and `LinearScan`/`allocate()` wiring dual-class allocation — consuming 8a's `build_intervals`/`excluded_registers` output.

**Architecture:** Six tasks. Task 1 amends already-shipped Phase 6 code (`PhysReg` gains `Hash`) plus new constants. Tasks 2-5 build `crates/forge-regalloc/src/linear_scan.rs` incrementally. Task 6 is final verification.

**Tech Stack:** Rust, `forge-ir`, `forge-x64`, `forge-regalloc` (8a's `Interval`/`build_intervals`/`excluded_registers`).

**Design doc:** `docs/superpowers/specs/2026-08-09-phase-8b-linear-scan-core-design.md` — read this first, in full. It went through THREE rounds of execution-based review (mirroring 8a's own four-round history), each finding and fixing a real bug: round 1 found `pick_register`'s original hint mechanism was completely vacuous (honored 0 of 81 real hints, always); round 2 found round 1's fix directly contradicted the design's own "no two overlapping intervals share a register" property (fixed by correctly re-stating that property to exempt legitimate same-instruction coalescing handoffs) plus a compile bug and an under-specified `run()`; round 3 confirmed everything holds and found only editorial issues. Every code block in the design doc as it now stands has been transcribed verbatim into a working prototype and run against the real corpus at least once. Trust it precisely.

**Critical context you must internalize before writing any code**: this design's central, hard-won insight is that under 8a's INCLUSIVE `[start, end]` interval convention, two intervals touching at exactly one point (`a.end == b.start`) are NOT necessarily in conflict — if `b.hint == Some(a.value)`, this is a legitimate coalescing handoff (the x86 2-address-destructive instruction at that position reads `a`'s value and immediately overwrites it with `b`'s), and BOTH sharing one register is the CORRECT, intended outcome, not a bug. The "no two overlapping intervals share a register" property therefore needs a specific, narrow exemption — see the design doc's Testing section for the exact statement. Do not "simplify" this away.

---

## Task 1: `Location`, `PhysReg: Hash`, allocatable register pools

**Files:**
- Modify: `crates/forge-x64/src/reg.rs`
- Create: `crates/forge-regalloc/src/linear_scan.rs`
- Modify: `crates/forge-regalloc/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Create `crates/forge-regalloc/src/linear_scan.rs` with only this content for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phys_reg_hash_derive_works() {
        // Confirms the derive actually compiles and behaves correctly --
        // cheap, but real, since a missing/broken derive would otherwise
        // only surface as a confusing downstream compile error far from
        // its cause.
        let set: std::collections::HashSet<PhysReg> =
            [PhysReg::Rax, PhysReg::Rax].into_iter().collect();
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn allocatable_gpr_excludes_rsp_and_rbp() {
        assert_eq!(ALLOCATABLE_GPR.len(), 14);
        assert!(!ALLOCATABLE_GPR.contains(&PhysReg::Rsp));
        assert!(!ALLOCATABLE_GPR.contains(&PhysReg::Rbp));
    }

    #[test]
    fn allocatable_xmm_excludes_xmm16_through_31() {
        assert_eq!(ALLOCATABLE_XMM.len(), 16);
        for r in ALLOCATABLE_XMM {
            assert!(r.encoding() < 16, "{r:?} has encoding >= 16, unencodable without EVEX");
        }
    }

    #[test]
    fn location_reg_and_spill_are_distinct_and_comparable() {
        assert_ne!(Location::Reg(PhysReg::Rax), Location::Spill(0));
        assert_eq!(Location::Reg(PhysReg::Rax), Location::Reg(PhysReg::Rax));
    }
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-regalloc --lib -- linear_scan 2>&1 | head -60`
Expected: FAIL — compile error (`PhysReg`, `Location`, `ALLOCATABLE_GPR`, `ALLOCATABLE_XMM` unresolved in this module).

- [ ] **Step 3: Add `Hash` to `PhysReg`**

In `crates/forge-x64/src/reg.rs`, change:
```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PhysReg {
```
to:
```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PhysReg {
```

- [ ] **Step 4: Write the implementation**

Prepend this to the TOP of `crates/forge-regalloc/src/linear_scan.rs`, above the `#[cfg(test)]` block from Step 1:

```rust
use forge_ir::Value;
use forge_x64::PhysReg;
use std::collections::{HashMap, HashSet};

/// A virtual register's final storage location, once Phase 8 has assigned
/// one. SPEC.md's §7 pseudocode references `Location` but never defines
/// it -- defined here, since this is the first slice needing it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Location {
    Reg(PhysReg),
    /// Stack slot index. Phase 8c's concern entirely -- this slice never
    /// constructs this variant; it exists now only so `Location`'s shape
    /// is settled before 8c needs to extend the same enum.
    Spill(u32),
}

/// System V AMD64 GPRs available for allocation: all 16 minus `Rsp`
/// (stack pointer, never a virtual register's home) and `Rbp` (frame
/// pointer, same reasoning as `prologue::SYSV_CALLEE_SAVED` already
/// excluding it).
pub const ALLOCATABLE_GPR: &[PhysReg] = &[
    PhysReg::Rax,
    PhysReg::Rcx,
    PhysReg::Rdx,
    PhysReg::Rbx,
    PhysReg::Rsi,
    PhysReg::Rdi,
    PhysReg::R8,
    PhysReg::R9,
    PhysReg::R10,
    PhysReg::R11,
    PhysReg::R12,
    PhysReg::R13,
    PhysReg::R14,
    PhysReg::R15,
];

/// XMM registers available for allocation: Xmm0-15 only. Xmm16-31 need
/// EVEX to reach and nothing in this codebase can encode an
/// EVEX-prefixed instruction yet, so handing one out would produce
/// unencodable output.
pub const ALLOCATABLE_XMM: &[PhysReg] = &[
    PhysReg::Xmm0,
    PhysReg::Xmm1,
    PhysReg::Xmm2,
    PhysReg::Xmm3,
    PhysReg::Xmm4,
    PhysReg::Xmm5,
    PhysReg::Xmm6,
    PhysReg::Xmm7,
    PhysReg::Xmm8,
    PhysReg::Xmm9,
    PhysReg::Xmm10,
    PhysReg::Xmm11,
    PhysReg::Xmm12,
    PhysReg::Xmm13,
    PhysReg::Xmm14,
    PhysReg::Xmm15,
];
```

- [ ] **Step 5: Wire the module into `lib.rs`**

Read `crates/forge-regalloc/src/lib.rs` first, then add (alphabetically among existing `mod`/`pub use` lines):
- `mod linear_scan;`
- `pub use linear_scan::{Location, ALLOCATABLE_GPR, ALLOCATABLE_XMM};`

- [ ] **Step 6: Run the tests and confirm they pass**

Run: `cargo test -p forge-regalloc --lib 2>&1 | tail -60`
Expected: all 4 new tests pass; all pre-existing `forge-regalloc`/`forge-x64` tests still pass (the `Hash` derive is purely additive).

- [ ] **Step 7: Run the FULL workspace test suite**

Run: `cargo test --workspace 2>&1 | tail -60`

- [ ] **Step 8: `cargo fmt` and `cargo clippy --workspace -- -D warnings`, fix anything found**

- [ ] **Step 9: Commit**

```bash
git add crates/forge-x64/src/reg.rs crates/forge-regalloc/src/linear_scan.rs crates/forge-regalloc/src/lib.rs
git commit -m "feat(forge-regalloc): Location, allocatable GPR/XMM register pools; PhysReg gains Hash"
```

---

## Task 2: `LinearScan` struct scaffolding, `precompute_excluded`, `expire_old_intervals`

**Files:**
- Modify: `crates/forge-regalloc/src/linear_scan.rs`

- [ ] **Step 1: Write the failing tests**

Append to `linear_scan.rs`'s existing `#[cfg(test)] mod tests` block (add `use forge_regalloc::Interval` style imports as needed — since this is the SAME crate, use `crate::interval::Interval`/`crate::interval::RegClass`, and `use forge_ir::Value;` if not already in scope via the glob):

```rust
fn iv(value: u32, start: u32, end: u32, class: crate::interval::RegClass) -> crate::interval::Interval {
    crate::interval::Interval {
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
fn precompute_excluded_unions_per_value_across_positions() {
    let mut raw: HashMap<(usize, Value), Vec<PhysReg>> = HashMap::new();
    raw.insert((2, Value(1)), vec![PhysReg::Rax, PhysReg::Rdx]);
    raw.insert((5, Value(1)), vec![PhysReg::Rdx]); // same Value, different position -- must union
    raw.insert((3, Value(2)), vec![PhysReg::Rcx]);

    let excluded = precompute_excluded(&raw);

    let v1: HashSet<PhysReg> = excluded[&Value(1)].clone();
    assert_eq!(v1, [PhysReg::Rax, PhysReg::Rdx].into_iter().collect());
    assert_eq!(excluded[&Value(2)], [PhysReg::Rcx].into_iter().collect());
}

#[test]
fn excluded_at_returns_empty_set_for_unlisted_value() {
    let scan = LinearScan::new(vec![], &HashMap::new(), ALLOCATABLE_GPR);
    assert!(scan.excluded_at(Value(999)).is_empty());
}

#[test]
fn expire_old_intervals_keeps_touching_intervals_active() {
    // [0,2] and [2,4] TOUCH at position 2 -- under 8a's inclusive
    // convention this IS an overlap, so [0,2]'s register must NOT be
    // freed when processing the interval starting at 2.
    let a = iv(0, 0, 2, crate::interval::RegClass::Gpr);
    let b = iv(1, 2, 4, crate::interval::RegClass::Gpr);
    let mut scan = LinearScan::new(vec![a.clone(), b], &HashMap::new(), ALLOCATABLE_GPR);
    scan.assign(0, Location::Reg(PhysReg::Rax));
    scan.active.push(0);

    scan.expire_old_intervals(2); // processing b, which starts at 2

    assert_eq!(scan.active, vec![0], "a must still be active -- it touches at position 2");
    assert!(!scan.free_regs.contains(&PhysReg::Rax));
}

#[test]
fn expire_old_intervals_frees_genuinely_disjoint_intervals() {
    let a = iv(0, 0, 2, crate::interval::RegClass::Gpr);
    let b = iv(1, 3, 4, crate::interval::RegClass::Gpr); // starts at 3, strictly after a.end=2
    let mut scan = LinearScan::new(vec![a.clone(), b], &HashMap::new(), ALLOCATABLE_GPR);
    scan.assign(0, Location::Reg(PhysReg::Rax));
    scan.active.push(0);

    scan.expire_old_intervals(3);

    assert!(scan.active.is_empty());
    assert!(scan.free_regs.contains(&PhysReg::Rax));
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-regalloc --lib -- linear_scan 2>&1 | head -60`
Expected: FAIL — compile error (`LinearScan`, `precompute_excluded`, `expire_old_intervals` unresolved).

- [ ] **Step 3: Write the implementation**

Append to `linear_scan.rs`, ABOVE the `#[cfg(test)]` module (after the Task 1 content):

```rust
use crate::interval::{Interval, RegClass};

/// Excludes a `Value`'s specific registers at SPECIFIC instruction
/// positions (8a's `excluded_registers`, keyed per position for IntDiv/
/// IntRem's rhs), aggregated to whole-`Interval` scope: this allocator
/// has no interval splitting, so one register serves an interval's
/// entire `[start, end]`, meaning a register excluded at ANY position
/// within that range must be excluded for the WHOLE interval. Every
/// exclusion position is guaranteed by construction to lie inside its
/// value's interval, so a plain per-Value union is both necessary and
/// sufficient -- no reference to the intervals themselves is needed here.
fn precompute_excluded(
    excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>,
) -> HashMap<Value, HashSet<PhysReg>> {
    let mut out: HashMap<Value, HashSet<PhysReg>> = HashMap::new();
    for (&(_, value), regs) in excluded_registers {
        out.entry(value).or_default().extend(regs.iter().copied());
    }
    out
}

// `HashSet::new()` is not `const fn`, so this needs a lazily-initialized
// static, not a plain `static _: HashSet<_> = HashSet::new();` (a compile
// error).
static EMPTY_EXCLUSION_SET: std::sync::LazyLock<HashSet<PhysReg>> =
    std::sync::LazyLock::new(HashSet::new);

pub struct LinearScan<'a> {
    intervals: Vec<Interval>,
    active: Vec<usize>,
    free_regs: HashSet<PhysReg>,
    assignment: HashMap<Value, Location>,
    excluded: HashMap<Value, HashSet<PhysReg>>,
    allocatable: &'a [PhysReg],
}

impl<'a> LinearScan<'a> {
    fn new(
        intervals: Vec<Interval>,
        excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>,
        allocatable: &'a [PhysReg],
    ) -> Self {
        LinearScan {
            intervals,
            active: Vec::new(),
            free_regs: allocatable.iter().copied().collect(),
            assignment: HashMap::new(),
            excluded: precompute_excluded(excluded_registers),
            allocatable,
        }
    }

    fn assign(&mut self, i: usize, loc: Location) {
        self.assignment.insert(self.intervals[i].value, loc);
    }

    fn location_of(&self, i: usize) -> Option<Location> {
        self.assignment.get(&self.intervals[i].value).copied()
    }

    /// Returns an EMPTY set (not a missing-key panic) for any `Value`
    /// with no exclusion entry -- the overwhelming common case.
    fn excluded_at(&self, value: Value) -> &HashSet<PhysReg> {
        self.excluded.get(&value).unwrap_or(&EMPTY_EXCLUSION_SET)
    }

    /// An active interval `j` expires (frees its register) once the new
    /// interval's `start` has moved PAST `j`'s `end` -- under 8a's
    /// INCLUSIVE `[start, end]` convention, `j.end == current_start`
    /// means the two intervals touch at exactly one shared position,
    /// which IS an overlap, so `j` must stay active (its register must
    /// NOT be freed yet). This is the inclusive-range-correct boundary --
    /// PROMPT.md's original sketch (`end > current_start`) assumes
    /// half-open ranges and would free `j` one position too early.
    fn expire_old_intervals(&mut self, current_start: u32) {
        while let Some(&j) = self.active.first() {
            if self.intervals[j].end >= current_start {
                break;
            }
            self.active.remove(0);
            // Only the Reg variant ever occupies a slot in free_regs --
            // Spill never does (this slice never produces it anyway).
            if let Some(Location::Reg(r)) = self.location_of(j) {
                self.free_regs.insert(r);
            }
        }
    }
}
```

**IMPORTANT**: the test `excluded_at_returns_empty_set_for_unlisted_value` calls `scan.excluded_at(...)` — confirm `excluded_at` is visible to the test module (it will be, since the test module is a child of this file via `mod tests`, and `excluded_at` is a private inherent method — private items are visible to descendant modules in Rust).

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-regalloc --lib 2>&1 | tail -80`
Expected: all new tests pass; no regressions.

- [ ] **Step 5: Run the FULL workspace test suite, fmt, clippy**

```bash
cargo test --workspace
cargo fmt
cargo clippy --workspace -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/forge-regalloc/src/linear_scan.rs
git commit -m "feat(forge-regalloc): LinearScan scaffolding, precompute_excluded, expire_old_intervals"
```

---

## Task 3: `pick_register`

**Files:**
- Modify: `crates/forge-regalloc/src/linear_scan.rs`

- [ ] **Step 1: Write the failing tests**

Append to the test module:

```rust
#[test]
fn pick_register_case2_transfers_ownership_on_same_instruction_reuse() {
    // lhs.end == dst.start == 2, dst.hint == Some(lhs.value) -- the
    // structural signature of a legitimate two-address handoff.
    let lhs = iv(0, 0, 2, crate::interval::RegClass::Gpr);
    let mut dst = iv(1, 2, 4, crate::interval::RegClass::Gpr);
    dst.hint = Some(Value(0));
    let mut scan =
        LinearScan::new(vec![lhs.clone(), dst], &HashMap::new(), ALLOCATABLE_GPR);
    scan.assign(0, Location::Reg(PhysReg::Rax));
    scan.active.push(0);
    // lhs's register is NOT in free_regs (still "active") -- Case 2 must
    // transfer it directly, not require it to be free first.
    scan.free_regs.remove(&PhysReg::Rax);

    let picked = scan.pick_register(1, ALLOCATABLE_GPR);

    assert_eq!(picked, Some(PhysReg::Rax));
    assert!(scan.active.is_empty(), "lhs must be removed from active by the transfer");
    assert!(
        !scan.free_regs.contains(&PhysReg::Rax),
        "the register must NEVER appear in free_regs during a Case 2 transfer"
    );
}

#[test]
fn pick_register_case1_honors_a_hint_whose_target_already_expired() {
    // A hand-built fixture for the structurally-dead-against-real-data
    // Case 1 path: hint target's interval has ALREADY expired (its
    // register is genuinely free) before this interval starts.
    let mut dst = iv(1, 5, 7, crate::interval::RegClass::Gpr);
    dst.hint = Some(Value(0));
    let mut scan = LinearScan::new(vec![dst], &HashMap::new(), ALLOCATABLE_GPR);
    scan.assignment.insert(Value(0), Location::Reg(PhysReg::Rcx));
    scan.free_regs.remove(&PhysReg::Rcx); // simulate Rcx as the only free reg... no wait, put it back:
    scan.free_regs = ALLOCATABLE_GPR.iter().copied().collect();
    // Rcx genuinely free, target NOT in active (already expired).

    let picked = scan.pick_register(0, ALLOCATABLE_GPR);

    assert_eq!(picked, Some(PhysReg::Rcx));
}

#[test]
fn pick_register_falls_back_to_free_register_when_hint_unusable() {
    // Hint target's interval extends PAST this interval's start -- not a
    // legitimate handoff (shouldn't happen per 8a's own invariants, but
    // confirm the fallback path is taken safely, not a panic/wrong reg).
    let target = iv(0, 0, 10, crate::interval::RegClass::Gpr);
    let mut dst = iv(1, 2, 4, crate::interval::RegClass::Gpr);
    dst.hint = Some(Value(0));
    let mut scan =
        LinearScan::new(vec![target.clone(), dst], &HashMap::new(), ALLOCATABLE_GPR);
    scan.assign(0, Location::Reg(PhysReg::Rax));
    scan.active.push(0);

    let picked = scan.pick_register(1, ALLOCATABLE_GPR);

    // Rax is NOT returned (target.end=10 != dst.start=2, no transfer);
    // falls back to the first free register in ALLOCATABLE_GPR's order.
    assert_ne!(picked, Some(PhysReg::Rax));
    assert_eq!(picked, Some(ALLOCATABLE_GPR[1])); // Rax is index 0 and still occupied
}

#[test]
fn pick_register_respects_exclusions_even_for_a_legitimate_handoff() {
    let lhs = iv(0, 0, 2, crate::interval::RegClass::Gpr);
    let mut dst = iv(1, 2, 4, crate::interval::RegClass::Gpr);
    dst.hint = Some(Value(0));
    let mut raw: HashMap<(usize, Value), Vec<PhysReg>> = HashMap::new();
    raw.insert((2, Value(1)), vec![PhysReg::Rax]); // dst itself excluded from Rax
    let mut scan = LinearScan::new(vec![lhs, dst], &raw, ALLOCATABLE_GPR);
    scan.assign(0, Location::Reg(PhysReg::Rax));
    scan.active.push(0);
    scan.free_regs.remove(&PhysReg::Rax);

    let picked = scan.pick_register(1, ALLOCATABLE_GPR);

    assert_ne!(picked, Some(PhysReg::Rax), "excluded even though it's the hint target's register");
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-regalloc --lib -- pick_register 2>&1 | head -60`
Expected: FAIL — compile error (`pick_register` unresolved).

- [ ] **Step 3: Write the implementation**

Add this method inside `impl<'a> LinearScan<'a> { ... }`, after `expire_old_intervals`:

```rust
    /// Picks a register for interval `i`, honoring its hint where safe.
    /// Case 1: the hint target's register is already free (it expired
    /// normally). Case 2: the hint target is STILL active but its
    /// interval ends exactly where this one starts -- the legitimate
    /// same-instruction-reuse case (x86's own 2-address destructive
    /// instructions read-then-overwrite one register atomically). When
    /// Case 2 fires, ownership transfers directly: the hint target is
    /// removed from `active` WITHOUT ever touching `free_regs` -- the
    /// register never becomes "free" in the general sense, it goes
    /// straight from one owner to the next. Falls back to any free,
    /// non-excluded register (in `allocatable`'s declared order, for
    /// deterministic output) if neither case applies.
    fn pick_register(&mut self, i: usize, allocatable: &[PhysReg]) -> Option<PhysReg> {
        let iv = self.intervals[i].clone();
        let excluded = self.excluded_at(iv.value).clone();

        if let Some(hinted_value) = iv.hint {
            if let Some(Location::Reg(reg)) = self.assignment.get(&hinted_value) {
                if self.free_regs.contains(reg) && !excluded.contains(reg) {
                    return Some(*reg);
                }
            }
            if let Some(pos) = self.active.iter().position(|&j| self.intervals[j].value == hinted_value) {
                let target_end = self.intervals[self.active[pos]].end;
                if target_end == iv.start {
                    if let Some(Location::Reg(reg)) = self.assignment.get(&hinted_value).copied() {
                        if !excluded.contains(&reg) {
                            self.active.remove(pos);
                            return Some(reg);
                        }
                    }
                }
            }
        }

        allocatable.iter().find(|r| self.free_regs.contains(r) && !excluded.contains(r)).copied()
    }
```

**IMPORTANT**: `excluded_at` returns `&HashSet<PhysReg>` borrowed from `self`, but `pick_register` needs `&mut self` for the Case 2 mutation (`self.active.remove(pos)`) — the borrow checker will reject holding `excluded` (borrowed from `self`) across a later `&mut self` call. Fix by cloning it up front (`self.excluded_at(iv.value).clone()`, as shown above) rather than holding the borrow — this is a small, deliberate clone, not a bug; a future optimization could restructure to avoid it, but correctness first.

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-regalloc --lib 2>&1 | tail -100`
Expected: all new tests pass. If `pick_register_case1_...`'s manual `free_regs` reset looks awkward, simplify it during implementation — the point is Rcx must be free and Value(0) must NOT be in `active`.

- [ ] **Step 5: Run the FULL workspace test suite, fmt, clippy**

```bash
cargo test --workspace
cargo fmt
cargo clippy --workspace -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/forge-regalloc/src/linear_scan.rs
git commit -m "feat(forge-regalloc): pick_register with same-instruction-reuse hint transfer (Case 2)"
```

---

## Task 4: `evict_and_assign`, `spill_at_interval`

**Files:**
- Modify: `crates/forge-regalloc/src/linear_scan.rs`

- [ ] **Step 1: Write the failing tests**

Append to the test module:

```rust
#[test]
fn evict_and_assign_no_victim_succeeds() {
    let fixed_iv = {
        let mut v = iv(0, 0, 5, crate::interval::RegClass::Gpr);
        v.fixed = Some(PhysReg::Rax);
        v
    };
    let mut scan = LinearScan::new(vec![fixed_iv], &HashMap::new(), ALLOCATABLE_GPR);

    scan.evict_and_assign(0, PhysReg::Rax);

    assert_eq!(scan.assignment.get(&Value(0)), Some(&Location::Reg(PhysReg::Rax)));
    assert!(!scan.free_regs.contains(&PhysReg::Rax));
    assert_eq!(scan.active, vec![0]);
}

#[test]
#[should_panic(expected = "evicting an active interval")]
fn evict_and_assign_with_a_victim_panics() {
    let occupant = iv(0, 0, 10, crate::interval::RegClass::Gpr);
    let fixed_iv = {
        let mut v = iv(1, 3, 5, crate::interval::RegClass::Gpr);
        v.fixed = Some(PhysReg::Rax);
        v
    };
    let mut scan =
        LinearScan::new(vec![occupant, fixed_iv], &HashMap::new(), ALLOCATABLE_GPR);
    scan.assign(0, Location::Reg(PhysReg::Rax));
    scan.active.push(0);

    scan.evict_and_assign(1, PhysReg::Rax); // must panic -- Rax already occupied
}

#[test]
#[should_panic(expected = "spilling ships in Phase 8c")]
fn spill_at_interval_panics_with_a_clear_deferral_message() {
    let a = iv(0, 0, 2, crate::interval::RegClass::Gpr);
    let mut scan = LinearScan::new(vec![a], &HashMap::new(), ALLOCATABLE_GPR);
    scan.spill_at_interval(0);
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-regalloc --lib -- evict_and_assign spill_at_interval 2>&1 | head -60`
Expected: FAIL — compile error.

- [ ] **Step 3: Write the implementation**

Add these two methods inside `impl<'a> LinearScan<'a> { ... }`:

```rust
    /// Satisfies a `fixed` requirement (`CHECKLIST bullet 10`: "ABI
    /// argument positions and idiv's implicit rax/rdx force eviction of
    /// whoever holds them"). `Interval::fixed` is ALWAYS `None` for
    /// anything `build_intervals` (Phase 8a) currently produces -- this
    /// is CHECKLIST-required plumbing with no real producer yet, the
    /// same "parameterized, tested with synthetic values" pattern Phase
    /// 7d used for emit_prologue/emit_epilogue.
    ///
    /// Deliberately narrow: handles ONLY the no-victim case. A genuine
    /// eviction (some OTHER active interval already holds `phys`) needs
    /// a real reassignment strategy this slice does not have cheaply --
    /// choosing the victim's replacement register from the CURRENT
    /// free_regs snapshot would be unsound (free_regs reflects
    /// availability at the CURRENT scan position, not across the
    /// victim's own [start, end]). Since there's no real producer to
    /// correctness-test a reassignment against, this is deferred with a
    /// clear panic rather than built unsoundly -- exactly like
    /// spill_at_interval below.
    fn evict_and_assign(&mut self, i: usize, phys: PhysReg) {
        if let Some(&victim) = self.active.iter().find(|&&j| self.location_of(j) == Some(Location::Reg(phys))) {
            unimplemented!(
                "evicting an active interval to satisfy a fixed-register requirement needs a \
                 real reassignment strategy (not built -- see the Phase 8b design doc's \
                 'evict_and_assign' section for why this is deliberately deferred rather than \
                 built unsoundly) -- Interval {:?} at Value {:?} would need to be evicted from \
                 {phys:?} to satisfy Interval {i} ({:?})'s fixed requirement, and no real \
                 Interval::fixed producer exists yet to force this path outside a hand-constructed \
                 test, so there is no pressure to solve it correctly before Phase 8c exists to \
                 inform the right approach (likely: treat it as a spill of the victim)",
                victim, self.intervals[victim].value, self.intervals[i].value
            );
        }
        self.free_regs.remove(&phys);
        self.assign(i, Location::Reg(phys));
        self.active.push(i);
        self.active.sort_by_key(|&j| self.intervals[j].end);
    }

    /// Explicitly stubbed, not built -- spilling ships in Phase 8c. This
    /// slice's own test corpus is verified (by construction, via the
    /// real max-simultaneous-liveness numbers measured during design
    /// review: 9 for both classes, against pools of 14/16) never to
    /// reach this path through real `build_intervals` output.
    fn spill_at_interval(&mut self, _i: usize) {
        unimplemented!(
            "spilling ships in Phase 8c -- see docs/superpowers/specs/2026-08-09-phase-8-decomposition-design.md"
        )
    }
```

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test -p forge-regalloc --lib 2>&1 | tail -100`

- [ ] **Step 5: Run the FULL workspace test suite, fmt, clippy**

```bash
cargo test --workspace
cargo fmt
cargo clippy --workspace -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/forge-regalloc/src/linear_scan.rs
git commit -m "feat(forge-regalloc): evict_and_assign (no-victim case), spill_at_interval stub"
```

---

## Task 5: `run()`, `allocate()` (dual-class wiring), end-to-end corpus tests

**Files:**
- Modify: `crates/forge-regalloc/src/linear_scan.rs`
- Modify: `crates/forge-regalloc/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Append to the test module. This is the largest step in this plan — it wires 8a and 8b together for the first time, and includes the property tests this whole design was built around:

```rust
#[test]
fn run_allocates_a_straight_line_chain_via_transfers() {
    // a = x + 1; b = a + 1; c = b + 1 -- three successive two-address
    // handoffs, all through the same register.
    let mut b = forge_ir::builder::Builder::new();
    let entry = b.create_block();
    b.seal_block(entry);
    let x = b.emit(
        entry,
        forge_ir::Inst::Param { index: 0, ty: forge_ir::Ty::I64 },
        forge_ir::Ty::I64,
        forge_syntax::span::Span::new(0, 0),
    );
    let one = b.emit(
        entry,
        forge_ir::Inst::ConstI64(1),
        forge_ir::Ty::I64,
        forge_syntax::span::Span::new(0, 0),
    );
    let a = b.emit(
        entry,
        forge_ir::Inst::Add(x, one),
        forge_ir::Ty::I64,
        forge_syntax::span::Span::new(0, 0),
    );
    let c = b.emit(
        entry,
        forge_ir::Inst::Add(a, one),
        forge_ir::Ty::I64,
        forge_syntax::span::Span::new(0, 0),
    );
    b.f.blocks[entry.0 as usize].term = Some(forge_ir::Terminator::Return(c));

    let selected = forge_x64::select(&b.f);
    let intervals = crate::intervals::build_intervals(&b.f, &selected);
    let excluded = crate::intervals::excluded_registers(&b.f, &selected);

    let assignment = allocate(intervals, &excluded);

    // a and c share x's register via Case 2 handoffs (x -> a -> c).
    let x_loc = assignment[&x];
    let a_loc = assignment[&a];
    let c_loc = assignment[&c];
    assert_eq!(x_loc, a_loc);
    assert_eq!(a_loc, c_loc);
    assert!(matches!(x_loc, Location::Reg(_)));
}

#[test]
fn run_never_shares_a_register_between_genuinely_conflicting_values() {
    for src in test_corpus() {
        let func = front_end(src);
        let selected = forge_x64::select(&func);
        let intervals = crate::intervals::build_intervals(&func, &selected);
        let excluded = crate::intervals::excluded_registers(&func, &selected);
        let by_value: HashMap<Value, &crate::interval::Interval> =
            intervals.iter().map(|iv| (iv.value, iv)).collect();

        let assignment = allocate(intervals.clone(), &excluded);

        for i in 0..intervals.len() {
            for j in (i + 1)..intervals.len() {
                let (a, bb) = (&intervals[i], &intervals[j]);
                let (Some(Location::Reg(ra)), Some(Location::Reg(rb))) =
                    (assignment.get(&a.value), assignment.get(&bb.value))
                else {
                    continue;
                };
                if ra != rb {
                    continue;
                }
                let disjoint = a.end < bb.start || bb.end < a.start;
                let legit_handoff = (a.end == bb.start && bb.hint == Some(a.value))
                    || (bb.end == a.start && a.hint == Some(bb.value));
                assert!(
                    disjoint || legit_handoff,
                    "{src:?}: {:?} and {:?} share {ra:?} but are neither disjoint nor a \
                     legitimate handoff -- ranges ({},{}) and ({},{})",
                    a.value,
                    bb.value,
                    a.start,
                    a.end,
                    bb.start,
                    bb.end
                );
            }
        }
        let _ = by_value;
    }
}

#[test]
fn run_honors_a_non_trivial_fraction_of_hints() {
    let (mut total_hints, mut honored) = (0usize, 0usize);
    for src in test_corpus() {
        let func = front_end(src);
        let selected = forge_x64::select(&func);
        let intervals = crate::intervals::build_intervals(&func, &selected);
        let excluded = crate::intervals::excluded_registers(&func, &selected);
        let assignment = allocate(intervals.clone(), &excluded);

        for iv in &intervals {
            let Some(hinted) = iv.hint else { continue };
            total_hints += 1;
            if let (Some(Location::Reg(a)), Some(Location::Reg(b))) =
                (assignment.get(&iv.value), assignment.get(&hinted))
            {
                if a == b {
                    honored += 1;
                }
            }
        }
    }
    assert!(total_hints > 0, "corpus must produce at least one hint");
    // Calibration target per the design doc: 40-70% of total hints, NOT
    // near zero. Adjust this literal threshold to match the real
    // implementation's measured count once this test actually runs --
    // treat a result near 0 as a real regression, not a threshold to
    // lower quietly.
    assert!(
        honored * 100 >= total_hints * 30,
        "only {honored}/{total_hints} hints honored -- expected at least ~30-40%"
    );
}

#[test]
fn run_produces_only_reg_locations_never_spill_for_the_corpus() {
    for src in test_corpus() {
        let func = front_end(src);
        let selected = forge_x64::select(&func);
        let intervals = crate::intervals::build_intervals(&func, &selected);
        let excluded = crate::intervals::excluded_registers(&func, &selected);
        let assignment = allocate(intervals.clone(), &excluded);

        assert_eq!(assignment.len(), intervals.len(), "{src:?}: every interval must get a Location");
        for loc in assignment.values() {
            assert!(matches!(loc, Location::Reg(_)), "{src:?}: unexpected Spill -- corpus should never need one");
        }
    }
}

/// Shared corpus, matching 8a's own `build_intervals_holds_its_invariants_across_the_whole_language_corpus`
/// / `every_hint_points_backward_in_8bs_scan_order` lists in `crates/forge-regalloc/src/intervals.rs` --
/// read that file and copy its exact program list here rather than inventing a new one, so 8a's and 8b's
/// test corpora stay in sync.
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
    ]
}

/// Same front_end helper shape as crates/forge-regalloc/src/intervals.rs's
/// own test module -- read that file's exact implementation and copy it
/// (lex -> parse -> resolve -> typecheck -> lower) rather than re-deriving
/// the API from scratch.
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

**IMPORTANT**: `intervals.clone()` is used repeatedly above because `allocate()` takes `Vec<Interval>` by value but the tests also need the original `intervals` for their own assertions afterward — confirm `Interval` derives `Clone` (it does, per 8a's design) before assuming this compiles as written.

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p forge-regalloc --lib -- run_ 2>&1 | head -60`
Expected: FAIL — compile error (`run`, `allocate` unresolved).

- [ ] **Step 3: Write the implementation**

Add `run` inside `impl<'a> LinearScan<'a> { ... }`:

```rust
    pub fn run(&mut self) {
        self.intervals.sort_by_key(|iv| (iv.start, iv.end, iv.value.0));
        for i in 0..self.intervals.len() {
            self.expire_old_intervals(self.intervals[i].start);

            if let Some(phys) = self.intervals[i].fixed {
                self.evict_and_assign(i, phys);
                continue;
            }

            match self.pick_register(i, self.allocatable) {
                Some(reg) => {
                    // For a Case 2 (same-instruction-reuse) hint, `reg`
                    // was never in `free_regs` to begin with -- this
                    // `remove` is then a documented no-op, not a bug;
                    // it's still correct and necessary for the ordinary
                    // free-register case.
                    self.free_regs.remove(&reg);
                    self.assign(i, Location::Reg(reg));
                    self.active.push(i);
                    self.active.sort_by_key(|&j| self.intervals[j].end);
                }
                None => self.spill_at_interval(i),
            }
        }
    }
```

Add `allocate` as a free function at the bottom of `linear_scan.rs` (outside the `impl` block, before the `#[cfg(test)]` module):

```rust
/// Runs linear scan once per register class (GPR, then XMM), merging
/// both partitions' assignments into one final map. No hint or φ-group
/// ever crosses a class boundary (every φ's incoming values, and every
/// arithmetic MachineInst's operands/result, share one Ty and therefore
/// one RegClass by construction), so splitting before scanning never
/// orphans a hint that would have resolved across the split.
pub fn allocate(
    intervals: Vec<Interval>,
    excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>,
) -> HashMap<Value, Location> {
    let mut assignment = HashMap::new();
    for (class, pool) in [(RegClass::Gpr, ALLOCATABLE_GPR), (RegClass::Xmm, ALLOCATABLE_XMM)] {
        let class_intervals: Vec<Interval> =
            intervals.iter().filter(|iv| iv.reg_class == class).cloned().collect();
        let mut scan = LinearScan::new(class_intervals, excluded_registers, pool);
        scan.run();
        assignment.extend(scan.assignment);
    }
    assignment
}
```

- [ ] **Step 4: Wire into `lib.rs`**

Update the `pub use linear_scan::{...}` line from Task 1 to also export `allocate`:
```rust
pub use linear_scan::{allocate, Location, ALLOCATABLE_GPR, ALLOCATABLE_XMM};
```

- [ ] **Step 5: Run the tests and iterate**

Run: `cargo test -p forge-regalloc --lib 2>&1 | tail -150`

Expect real iteration here: `test_corpus()`/`front_end()` are transcribed from the design/plan's own text, not independently re-verified against `crates/forge-regalloc/src/intervals.rs`'s CURRENT exact content — read that file directly and reconcile any drift (the corpus list or `front_end`'s exact API calls may have shifted since this plan was written) before trusting these tests compile and pass as literally written. The `run_honors_a_non_trivial_fraction_of_hints` threshold (`30%`) is a starting point — if the real measured percentage on this corpus differs meaningfully from the range described in the design doc's Testing section, investigate why before just adjusting the number (a large drop could indicate a real regression, not just corpus-composition noise).

- [ ] **Step 6: Run the FULL workspace test suite, fmt, clippy**

```bash
cargo test --workspace
cargo fmt
cargo clippy --workspace -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add crates/forge-regalloc/src/linear_scan.rs crates/forge-regalloc/src/lib.rs
git commit -m "feat(forge-regalloc): run()/allocate() dual-class wiring, end-to-end corpus property tests"
```

---

## Task 6: Final verification pass

**Files:** none created — this task only runs checks.

- [ ] **Step 1: Full workspace test run**

Run: `cargo test --workspace 2>&1 | tail -80`. Report exact final counts.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace -- -D warnings` AND `cargo clippy --workspace --all-targets -- -D warnings`.

- [ ] **Step 3: Format check**

Run: `cargo fmt --check`

- [ ] **Step 4: Report exit criteria status**

Confirm all 13 exit criteria from `docs/superpowers/specs/2026-08-09-phase-8b-linear-scan-core-design.md`'s "Exit criteria" section.

## Context for this whole plan

This plan's code is transcribed from a design doc that went through three rounds of execution-based verification — every function body above has been run against real data at least once before this plan was written. The places flagged "IMPORTANT" (the `excluded_at` clone in Task 3, the corpus/API drift check in Task 5) are the only spots where this plan's author could not independently re-verify against the exact current state of `crates/forge-regalloc/src/intervals.rs` at implementation time — resolve them by reading the real file, not by guessing.

Work from: `/Users/sanskar/dev/Research/Projects/JIT-Compiler`
