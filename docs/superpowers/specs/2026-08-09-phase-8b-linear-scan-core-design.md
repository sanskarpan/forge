# Design: forge Phase 8b — Linear Scan Core

**Status:** Approved for planning
**Scope:** Per `docs/superpowers/specs/2026-08-09-phase-8-decomposition-design.md`, CHECKLIST.md Phase 8 bullets 6-10 + 16: sort intervals by start, `active` list sorted by end, `expire_old_intervals`, `pick_register` (hint-then-free, with an interference check — see below), fixed-register eviction, separate GPR/XMM class allocation (via running the single-class loop twice, the decomposition doc's stated default). Lives in `crates/forge-regalloc`.
**Input:** `Vec<Interval>` from `build_intervals` (8a), plus `excluded_registers` (8a/Task 6) for `IntDiv`/`IntRem`'s `rhs`.
**Out of scope (deferred to 8c)**: spilling. This slice assumes registers are always available for anything `build_intervals` can currently produce (real corpus programs never exhaust the register file — see "Scope-limiting note" below) and stubs the spill path with a clear deferred-work panic, mirroring this project's established pattern (Phase 7a's `Call`/float-`Rem` panics).

## Why this design starts from 8a's review history, not from a blank slate

8a went through four rounds of post-implementation correction, all catching the SAME class of mistake: a constraint that only holds at one program point was modeled (and initially built) as if it held for a value's whole lifetime, producing genuinely unsatisfiable data. The final review's honest process assessment (recorded in CHECKLIST.md's Phase 8a bullet 10 annotation) drew three concrete lessons for whoever designs 8b next — this design applies all three directly, not as an afterthought:

1. **Write the invariant property tests before the implementation.** This design's own Testing section leads with the properties (no two overlapping intervals share a register; `active` stays sorted by end; every interval gets a `Location`; every honored hint was interference-checked first), stated as corpus-wide assertions, not fixture-by-fixture pins — 8a's own example-based tests missed three separate forward-hint bugs across two review rounds; only a property test caught the whole class at once.
2. **Ask "can this even be satisfied?" as a design question, not an implementation afterthought.** Checked explicitly below for the interference-check-before-hint rule (a hint is a preference among interference-free candidates, never an override — 8a's design doc already flags that φ-groups routinely can't be fully co-located even at zero pressure).
3. **Be suspicious of constraints expressed as lifetime properties.** 8b mostly consumes data 8a already corrected onto point-in-time semantics (`excluded_registers` is keyed per instruction position, not per interval). The one place THIS slice could repeat the mistake is `evict_and_assign` for `Interval::fixed` — checked explicitly below.

## `Location`

SPEC.md's §7 pseudocode references `Location` (`assignment: FxHashMap<Value, Location>`, `Location::Reg(ra)` in the verifier snippet) but never defines it. Defined here, since 8b is the first slice needing it:

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Location {
    Reg(PhysReg),
    /// Stack slot index. Phase 8c's concern entirely -- 8b never
    /// constructs this variant; it exists now only so `Location`'s shape
    /// is settled before 8c needs to extend the same enum, and so 8b's
    /// own `assignment` map's value type doesn't need to change later.
    Spill(u32),
}
```

## New dependency requirement: `PhysReg` needs `Hash`

`crates/forge-x64/src/reg.rs`'s `PhysReg` currently derives `Clone, Copy, PartialEq, Eq, Debug` — no `Hash`. 8b needs `PhysReg` as a `HashSet`/`HashMap` key (`free_regs: HashSet<PhysReg>`, and `Location::Reg(PhysReg)` inside `HashMap<Value, Location>` doesn't itself need `PhysReg: Hash`, but a free-register pool does). This is a one-line, additive, backward-compatible amendment to already-shipped Phase 6 code — add `Hash` to the derive list — same category of change as 8a's `SelectedFunction::block_starts` addition to Phase 7's `machine_inst/mod.rs`. No existing behavior changes; `#[derive(Hash)]` on a field-less-variant enum is purely mechanical.

## Allocatable register pools

Not every `PhysReg` variant is a candidate for allocation:
- **GPR**: all 16 minus `Rsp` (stack pointer, never a virtual register's home) and `Rbp` (frame pointer, same reasoning as `prologue::SYSV_CALLEE_SAVED` already excluding it) — **14 allocatable GPRs**.
- **XMM**: `Xmm0` through `Xmm15` only. `Xmm16`-`Xmm31` "need EVEX to reach and can't be used by anything built so far" (`reg.rs`'s own doc comment on the full enum) — nothing in this codebase can encode an EVEX-prefixed instruction yet, so handing one of these out would produce unencodable output. **16 allocatable XMM registers.**

```rust
pub const ALLOCATABLE_GPR: &[PhysReg] = &[
    PhysReg::Rax, PhysReg::Rcx, PhysReg::Rdx, PhysReg::Rbx,
    PhysReg::Rsi, PhysReg::Rdi, PhysReg::R8, PhysReg::R9,
    PhysReg::R10, PhysReg::R11, PhysReg::R12, PhysReg::R13,
    PhysReg::R14, PhysReg::R15,
]; // Rsp, Rbp excluded.

pub const ALLOCATABLE_XMM: &[PhysReg] = &[
    PhysReg::Xmm0, PhysReg::Xmm1, PhysReg::Xmm2, PhysReg::Xmm3,
    PhysReg::Xmm4, PhysReg::Xmm5, PhysReg::Xmm6, PhysReg::Xmm7,
    PhysReg::Xmm8, PhysReg::Xmm9, PhysReg::Xmm10, PhysReg::Xmm11,
    PhysReg::Xmm12, PhysReg::Xmm13, PhysReg::Xmm14, PhysReg::Xmm15,
]; // Xmm16-31 excluded -- unencodable without EVEX, which nothing here builds.
```

## The corrected `expire_old_intervals` boundary — a direct consequence of 8a's inclusive-range fix

PROMPT.md's sketch (`if self.intervals[j].end > current_start { break; }`) was written assuming `[start, end)` half-open ranges. 8a's design corrected `Interval`'s actual semantics to INCLUSIVE `[start, end]` — a value is still live AT its `end` position. Applying PROMPT.md's half-open boundary condition unmodified to inclusive data would expire (and free the register of) an interval one position too early, corrupting a value still genuinely live at the new interval's `start`.

**Corrected condition**: an active interval `j` expires (frees its register) once the new interval's `start` has moved PAST `j`'s `end` — i.e., `intervals[j].end < current_start`. It stays active (loop breaks, since `active` is sorted by end) while `intervals[j].end >= current_start` — this correctly keeps `j` active when `j.end == current_start` (the two intervals touch at exactly one shared position, which IS an overlap under the inclusive convention, so `j`'s register must not be freed yet).

```rust
fn expire_old_intervals(&mut self, current_start: u32) {
    while let Some(&j) = self.active.first() {
        if self.intervals[j].end >= current_start {
            break;
        }
        self.active.remove(0);
        self.free_regs.insert(self.location_of(j).expect("active interval always has a Location"));
    }
}
```

## `pick_register` — hint resolution WITH a mandatory interference check

**The rule 8a's review explicitly warned about**: a hint is a preference among interference-free candidates, never an override. Honoring a hint without checking whether the candidate register is actually free at this point would silently corrupt whichever value already holds it.

```rust
fn pick_register(&self, i: usize, allocatable: &[PhysReg]) -> Option<PhysReg> {
    let iv = &self.intervals[i];
    let excluded = self.excluded_at(i); // union of excluded_registers() entries
                                          // over every position in [iv.start, iv.end]
                                          // where iv.value appears as a key -- see below

    // Hint: only usable if it resolves to a register that is BOTH
    // currently free (self.free_regs) AND not excluded for this interval.
    // A hint pointing at an interval not yet assigned (shouldn't happen --
    // 8a's corpus-wide property test guarantees hints point backward in
    // scan order, so the hinted value is always already processed) is
    // treated as absent, not as an error -- defensive, not load-bearing.
    if let Some(hinted_value) = iv.hint {
        if let Some(Location::Reg(reg)) = self.assignment.get(&hinted_value) {
            if self.free_regs.contains(reg) && !excluded.contains(reg) {
                return Some(*reg);
            }
        }
    }

    // Fall back to any free, non-excluded register. Deterministic order
    // (iterate `allocatable` in its declared order, not free_regs' HashSet
    // iteration order) -- otherwise register assignment, and therefore
    // emitted machine code, would be nondeterministic across runs on
    // identical input, exactly the class of bug 8a's own "deterministic
    // return order" fix closed for build_intervals' output.
    allocatable.iter().find(|r| self.free_regs.contains(r) && !excluded.contains(r)).copied()
}
```

**Aggregating `excluded_registers` over an interval's whole range**: `excluded_registers()` returns `HashMap<(usize, Value), Vec<PhysReg>>`, keyed per INSTRUCTION POSITION (8a's point-in-time correction). 8b has no interval splitting — one register serves the interval's whole `[start, end]` — so a register excluded at ANY position within that range must be excluded for the WHOLE interval, or the value could still end up in a register that's unsafe at the one position that mattered. `excluded_at(i)` computes this union once (or precomputes it for all intervals up front, before the scan loop starts, since `excluded_registers()`'s output doesn't change during allocation):

```rust
fn precompute_excluded(intervals: &[Interval], excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>) -> HashMap<Value, HashSet<PhysReg>> {
    let mut out: HashMap<Value, HashSet<PhysReg>> = HashMap::new();
    for (&(_, value), regs) in excluded_registers {
        out.entry(value).or_default().extend(regs.iter().copied());
    }
    out
}
```
(A position-keyed entry for a `Value` not covered by any `Interval` in this function — shouldn't happen given 8a's own coverage invariant — is simply ignored; nothing to exclude for.)

## `evict_and_assign` for `Interval::fixed` — built per spec, but honestly scoped

8a's design doc is explicit: `Interval::fixed` is ALWAYS `None` for anything `build_intervals` currently produces (every real "fixed register" case was corrected onto emission-time copies or `excluded_registers` instead). This mechanism is therefore CHECKLIST-required plumbing with no current real producer — the same "parameterized, tested with hand-picked synthetic values" pattern Phase 7d used for `emit_prologue`/`emit_epilogue` before Phase 8 existed to feed them real data.

```rust
fn evict_and_assign(&mut self, i: usize, phys: PhysReg) {
    // If some OTHER active interval currently holds `phys`, it must be
    // evicted -- fixed registers are non-negotiable (CHECKLIST bullet 10).
    if let Some(&victim) = self.active.iter().find(|&&j| self.location_of(j) == Some(Location::Reg(phys))) {
        self.active.retain(|&j| j != victim);
        self.free_regs.remove(&phys);
        // The evicted interval needs a NEW home. Try any other free,
        // non-excluded register for its class first (cheap, common case);
        // if none is free, this is a genuine spill-on-eviction scenario --
        // explicitly out of this slice's scope (no real Interval::fixed
        // producer exists yet to exercise this path with real data; a
        // hand-constructed test forcing it hits a clear, deferred-work
        // panic rather than silently producing a wrong allocation).
        let victim_class_pool = self.allocatable_for(self.intervals[victim].reg_class);
        match self.pick_register(victim, victim_class_pool) {
            Some(new_reg) => self.assign(victim, Location::Reg(new_reg)),
            None => unimplemented!(
                "eviction forcing a spill ships in Phase 8c -- no real Interval::fixed \
                 producer exists yet to reach this path outside a hand-constructed test"
            ),
        }
    }
    self.free_regs.remove(&phys);
    self.assign(i, Location::Reg(phys));
    self.active.push(i);
    self.active.sort_by_key(|&j| self.intervals[j].end); // maintain the sorted-by-end invariant
}
```

**Testing note**: since `build_intervals` never produces `fixed: Some(_)`, this function's tests MUST hand-construct `Vec<Interval>` fixtures directly (not go through the front-end/`select`/`build_intervals` pipeline) — the only way to exercise it at all in this slice.

## `spill_at_interval` — explicitly stubbed, not built

```rust
fn spill_at_interval(&mut self, _i: usize) {
    unimplemented!("spilling ships in Phase 8c -- see docs/superpowers/specs/2026-08-09-phase-8-decomposition-design.md")
}
```

**Scope-limiting note for 8b's test corpus**: every test program in this slice's test suite must be checked (by construction, not by luck) to have at most `ALLOCATABLE_GPR.len()` (14) simultaneously-live `Gpr`-class values and at most `ALLOCATABLE_XMM.len()` (16) simultaneously-live `Xmm`-class values at any single program point — i.e., `pick_register` never returns `None` for anything in this slice's own corpus. This is easy to satisfy (this project's whole language surface is small expressions; nothing in the existing test corpus from 8a's own `build_intervals_holds_its_invariants_across_the_whole_language_corpus` list comes close to 14 simultaneously-live GPR values), but must be verified, not assumed — a test that accidentally exercises the spill path would hit the `unimplemented!()` panic and fail loudly, which is the correct, safe failure mode (not a silent wrong allocation), but the test SUITE itself should not rely on that panic as its own pass condition.

## Dual-class allocation: run the single-class loop twice

Per the decomposition doc's stated default: partition `Vec<Interval>` by `reg_class` before scanning, run the identical scan loop once per partition (each with its own `active`/`free_regs`, seeded from `ALLOCATABLE_GPR`/`ALLOCATABLE_XMM` respectively), and merge both partitions' `assignment` maps into one final `HashMap<Value, Location>`. No φ-group or hint ever crosses a class boundary — a φ's destination and every incoming value share the SAME `Ty` (a φ is only well-typed when all its incoming values match; confirmed by re-checking `forge-syntax`'s typeck, which requires both `if`/`else` arms to unify to one type before lowering), and two-address hints (`dst -> lhs`) are always same-`Ty` by construction (every arithmetic `MachineInst` variant's operands and result share one type). So splitting by class before scanning never orphans a hint that would have resolved across the split.

## `LinearScan` struct and `run()`

```rust
pub struct LinearScan<'a> {
    intervals: Vec<Interval>,               // sorted by (start, end, value) -- same key as 8a's own sort
    active: Vec<usize>,                     // indices into `intervals`, sorted by end
    free_regs: HashSet<PhysReg>,
    assignment: HashMap<Value, Location>,
    excluded: HashMap<Value, HashSet<PhysReg>>, // precomputed once, see above
    allocatable: &'a [PhysReg],
}

pub fn allocate(intervals: Vec<Interval>, excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>) -> HashMap<Value, Location> {
    let mut assignment = HashMap::new();
    for (class, pool) in [(RegClass::Gpr, ALLOCATABLE_GPR), (RegClass::Xmm, ALLOCATABLE_XMM)] {
        let class_intervals: Vec<Interval> = intervals.iter().filter(|iv| iv.reg_class == class).cloned().collect();
        let mut scan = LinearScan::new(class_intervals, excluded_registers, pool);
        scan.run();
        assignment.extend(scan.assignment);
    }
    assignment
}
```

`run()` follows PROMPT.md's sketch exactly (sort by start [already done via the constructor accepting pre-sorted intervals, or sorting internally with 8a's exact tie-break key], loop: expire, fixed-eviction-or-pick-register, assign-or-spill), with the two corrections above (`expire_old_intervals`'s inclusive boundary, `pick_register`'s interference-checked hint resolution).

## Testing

Property tests FIRST (per the process lesson above), over the SAME real-front-end corpus 8a's own `build_intervals_holds_its_invariants_across_the_whole_language_corpus`/`every_hint_points_backward_in_8bs_scan_order` tests already use (reuse the corpus list, don't re-invent it):

- **No two overlapping intervals share a `Location::Reg`** (the inclusive-range overlap predicate: `a.start <= b.end && b.start <= a.end`) — this is literally what 8d's independent verifier will ALSO check later, built independently; 8b having its own copy of this property test now is not redundant with 8d, it's a regression net for THIS slice while 8d doesn't exist yet.
- **`active` remains sorted by `end` after every `expire_old_intervals`/`assign` call** — a direct invariant check, not just an outcome check.
- **Every interval in the input ends up with exactly one entry in the returned `assignment` map** (modulo the `unimplemented!` spill path, which the scope-limiting note above guarantees the real corpus never reaches).
- **Every honored hint was interference-free at the moment it was honored** — instrument `pick_register` (or reconstruct after the fact from `assignment` + `intervals`) to confirm no hint-driven assignment collided with an already-active interval.
- Hand-constructed synthetic-`Interval` tests (bypassing `build_intervals` entirely, per the note above) for: `evict_and_assign`'s eviction-succeeds case (a `fixed` interval displaces a non-fixed active one, which finds a new free register) and `evict_and_assign`'s eviction-needs-spill case (`#[should_panic]`, confirming the deferred-work message).
- `expire_old_intervals`'s exact boundary: two hand-built intervals `[0,2]` and `[2,4]` sharing position 2 — confirm they're correctly treated as OVERLAPPING (both active simultaneously at position 2, needing two different registers) — this is the single most important boundary test in this slice, given it's a direct, easy-to-get-backward consequence of 8a's inclusive-range correction.
- `PhysReg`'s new `Hash` derive: a trivial smoke test that `HashSet::from([PhysReg::Rax, PhysReg::Rax]).len() == 1` (confirms the derive actually compiles and works as intended — cheap, but real, since a missing/incorrect derive would otherwise only surface as a confusing downstream compile error far from its cause).
- `ALLOCATABLE_GPR`/`ALLOCATABLE_XMM` contents: exactly 14 and 16 entries respectively, `Rsp`/`Rbp` absent from the GPR list, `Xmm16`-`Xmm31` absent from the XMM list.
- A real end-to-end test: run `allocate()` on `build_intervals`'s output for a handful of real corpus programs and confirm every returned `Location` is `Reg(_)` (never `Spill`, matching the scope-limiting note) and that the no-overlap property holds — this is the first point in Phase 8 where 8a and 8b's outputs are actually wired together and exercised end-to-end.

## Exit criteria

1. `Location` enum exists (`Reg`/`Spill`), matching what SPEC.md's `assignment: FxHashMap<Value, Location>` needed but never defined.
2. `PhysReg` gains `Hash` in `crates/forge-x64/src/reg.rs`, additive, no other change.
3. `ALLOCATABLE_GPR`/`ALLOCATABLE_XMM` correctly exclude `Rsp`/`Rbp` and `Xmm16`-`31` respectively.
4. `expire_old_intervals` uses the INCLUSIVE-range-correct boundary (`end >= current_start` keeps active, not the half-open `end > current_start` PROMPT.md's sketch literally shows) — tested with an explicit touching-at-one-point case.
5. `pick_register` never honors a hint without confirming the candidate register is both free and non-excluded at that point — tested by the "every honored hint was interference-free" property.
6. `excluded_registers`' point-in-time exclusions are correctly aggregated (unioned) to whole-interval scope before being used as a candidate filter — since 8b has no splitting mechanism.
7. `evict_and_assign` exists and is tested with hand-constructed `fixed`-carrying intervals (since `build_intervals` produces none), including its eviction-needs-spill deferred-panic case.
8. `spill_at_interval` is an explicit, clearly-messaged `unimplemented!()` — not a silent wrong answer, and this slice's own test corpus is verified never to reach it.
9. Dual-class allocation via running the single-class loop twice; no hint or φ-group ever needs to cross the split (justified by `Ty`-uniformity, not just asserted).
10. All Testing-section items covered, property tests included and run FIRST in the actual test-writing order (not appended as an afterthought after fixture tests, per the explicit process lesson from 8a).
11. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
12. No regressions in any existing test, including the `PhysReg` derive addition not breaking anything in `forge-x64` (it shouldn't — adding a derive is additive).
