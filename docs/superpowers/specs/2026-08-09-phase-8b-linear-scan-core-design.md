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
        // location_of returns a Location, not a PhysReg directly -- only
        // the Reg variant ever occupies a slot in free_regs (Spill never
        // does; 8b never produces it, but this stays correct once 8c does).
        if let Some(Location::Reg(r)) = self.location_of(j) {
            self.free_regs.insert(r);
        }
    }
}
```

## `pick_register` — CORRECTED: the naive interference check makes every hint permanently unusable

**This section was rewritten after execution-based design review found the original rule was a real, structural bug — not a style nit.** The original text below is preserved as a record of what was wrong and why, since the same misunderstanding is easy to re-derive independently:

> *(Original, WRONG reasoning)*: "a hint is a preference among interference-free candidates... honor the hint only if the candidate register is currently free (`self.free_regs.contains(reg)`)."

Measured against the real corpus, this rule honors **zero out of 81** real hints, always. The reason is a direct, non-obvious consequence of 8a's own inclusive-range correction: for a two-address hint (`dst = Add(lhs, rhs)`), `lhs`'s interval ends at exactly the same position `dst`'s interval starts (`lhs.end == dst.start` — `lhs` is read by the very instruction that defines `dst`). Under the inclusive overlap predicate `a.start <= b.end && b.start <= a.end`, this ALWAYS evaluates as "overlapping" — so `lhs`'s register is never in `free_regs` when `dst` is processed, and the naive rule refuses the hint every single time. The exact same thing happens for every φ-group anchor (all group members share an IDENTICAL range by construction, so the anchor's register is never free when a non-anchor member is processed). This is 8a's bug class again, just showing up as an over-conservative CHECK instead of an unsatisfiable CONSTRAINT: a same-instruction "this value dies feeding that value's definition" relationship is a point-in-time fact (x86's own 2-address destructive instructions read-then-overwrite one register atomically), but the naive whole-range overlap test can't distinguish it from two genuinely, simultaneously-live, unrelated values that merely happen to touch at one position.

**Corrected rule**: a hint is honored not just when its target's register is in `free_regs`, but ALSO when its target's register is currently held by an active interval that is EXACTLY the hint target itself AND whose `end` exactly equals this interval's `start` — the structural signature of a legitimate same-instruction reuse, not a coincidence. When this fires, the register isn't freed and re-taken (no bookkeeping churn, no chance of a race with something else grabbing it in between) — ownership is transferred directly from the hint target's `active` entry to the new interval's, and `active` is re-sorted to reflect the new interval's `end`. For anything else — a coincidentally-touching but UNRELATED interval, or the hint target's register genuinely still needed after `dst.start` (which can't happen for a real hint per 8a's backward-pointing/last-use invariant, but isn't assumed) — no special case applies, and the normal free-register fallback is used.

```rust
fn pick_register(&mut self, i: usize, allocatable: &[PhysReg]) -> Option<PhysReg> {
    let iv = self.intervals[i].clone();
    let excluded = self.excluded_at(iv.value);

    if let Some(hinted_value) = iv.hint {
        // Case 1: the hint target's register is already free (it expired
        // normally, e.g. this interval doesn't immediately follow it).
        if let Some(Location::Reg(reg)) = self.assignment.get(&hinted_value) {
            if self.free_regs.contains(reg) && !excluded.contains(reg) {
                return Some(*reg);
            }
        }
        // Case 2: the hint target is STILL `active` (not yet expired by
        // the normal path) but its interval ends exactly where this one
        // starts -- the legitimate same-instruction reuse case. Transfer
        // ownership directly: remove the hint target from `active`
        // without touching `free_regs` at all (the register never
        // becomes "free" in the general sense -- it goes straight from
        // one owner to the next).
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

    // Fall back to any free, non-excluded register. Deterministic order
    // (iterate `allocatable` in its declared order, not free_regs' HashSet
    // iteration order) -- otherwise register assignment, and therefore
    // emitted machine code, would be nondeterministic across runs on
    // identical input, exactly the class of bug 8a's own "deterministic
    // return order" fix closed for build_intervals' output.
    allocatable.iter().find(|r| self.free_regs.contains(r) && !excluded.contains(r)).copied()
}
```

**This is not a complete fix for φ-group co-location** — measured on the corpus, this rule honors 57/81 hints (up from 0/81), and the remaining 24 are two-address hints whose `lhs` genuinely outlives `dst`'s start (correctly refused — no unsafe reuse there) and φ-group hints where the anchor and a non-anchor member are BOTH live simultaneously beyond the touching-point case (e.g. both read again later, or the anchor itself isn't literally adjacent to the non-anchor in scan order). Those cases are NOT bugs to fix in 8b — per 8a's own design doc, un-co-located φ-group members are an expected, routine outcome (nested `if`s provably can't always co-locate even at zero register pressure) whose resolution is the deferred final-emission task's parallel-copy insertion, not this slice's job.

**Aggregating `excluded_registers` over an interval's whole range**: `excluded_registers()` returns `HashMap<(usize, Value), Vec<PhysReg>>`, keyed per INSTRUCTION POSITION (8a's point-in-time correction). 8b has no interval splitting — one register serves the interval's whole `[start, end]` — so a register excluded at ANY position within that range must be excluded for the WHOLE interval, or the value could still end up in a register that's unsafe at the one position that mattered. Every exclusion position is guaranteed (by construction — `excluded_registers` only ever keys on a `MachineInst`'s own operand at that `MachineInst`'s own position, which is always within that operand's interval) to lie inside its value's `[start, end]`, so a simple per-`Value` union — with NO reference to `intervals` needed at all — is both necessary and sufficient:

```rust
fn precompute_excluded(excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>) -> HashMap<Value, HashSet<PhysReg>> {
    let mut out: HashMap<Value, HashSet<PhysReg>> = HashMap::new();
    for (&(_, value), regs) in excluded_registers {
        out.entry(value).or_default().extend(regs.iter().copied());
    }
    out
}

// excluded_at returns an EMPTY set (not a missing-key panic) for any
// Value with no exclusion entry -- the overwhelming common case.
fn excluded_at(&self, value: Value) -> &HashSet<PhysReg> {
    self.excluded.get(&value).unwrap_or(&EMPTY_EXCLUSION_SET)
}
```

**Deferred, explicitly out of scope, not silently missed**: `MachineInst::Shl`/`Shr`/`Sar`'s `rhs` operand has the exact same x86 hardware shape as `idiv`'s divisor conceptually (`shl`'s shift-amount operand must be in `Cl`/`Rcx`) — but `excluded_registers()` (8a) doesn't cover it, and this design doesn't add coverage for it either. This is DIFFERENT from `idiv`'s divisor case in one crucial way: a shift instruction doesn't DESTROY its `rhs` value the way `cqo`/`idiv` destroys the dividend's high bits — `rhs` can safely be relocated into `Cl` by an emission-time `mov` immediately before the shift, exactly like `idiv`'s `lhs`/dividend fixup (already resolved this way in 8a's design). So this is sub-problem 1 of 8a's idiv-clobber resolution (a third-party value sitting in a needed register gets displaced and restored by emission, not by the allocator), not sub-problem 2 (a value that's destroyed and can't be recovered) — correctly out of scope for 8b's allocation-time exclusion mechanism, but recorded here explicitly so it isn't silently lost between slices.

## `evict_and_assign` for `Interval::fixed` — CORRECTED: the eviction path had two real bugs and is now deliberately narrower

8a's design doc is explicit: `Interval::fixed` is ALWAYS `None` for anything `build_intervals` currently produces (every real "fixed register" case was corrected onto emission-time copies or `excluded_registers` instead). This mechanism is therefore CHECKLIST-required plumbing with no current real producer — the same "parameterized, tested with hand-picked synthetic values" pattern Phase 7d used for `emit_prologue`/`emit_epilogue` before Phase 8 existed to feed them real data.

**Design review found the original eviction-reassignment path was wrong in two ways**, both confirmed by execution against hand-built fixtures: (1) the evicted "victim" interval was removed from `active` and given a new register but never re-inserted into `active` and never had its new register removed from `free_regs` — producing a genuine double-booked-register bug on the very next interval processed; (2) even once that leak is fixed, choosing the victim's replacement register from the CURRENT `free_regs` snapshot is unsound, because `free_regs` reflects availability at the CURRENT scan position, not across the victim's own `[start, end]` — an interval that already expired earlier can have freed a register that the victim's own (earlier-starting) interval would conflict with over part of its range. This is, once again, exactly 8a's bug class: treating a point-in-time snapshot as if it were valid for a whole interval's lifetime.

**Resolution: don't attempt reassignment at all.** Since `Interval::fixed` has no real producer to correctness-test this path against, and a truly correct victim-reassignment needs information (which registers are free across the VICTIM's whole original range, from the scan's start, not just now) this slice doesn't have cheaply, the honest scope is: `evict_and_assign` handles ONLY the case where `phys` is already free (no victim) or the occupant can be safely displaced because IT ALSO has nowhere else it needs to be captured (which — as `Interval::fixed` has no real producer — never happens in practice). Any eviction that would require a genuine reassignment is deferred with a clear panic, exactly like `spill_at_interval`:

```rust
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
    self.active.sort_by_key(|&j| self.intervals[j].end); // maintain the sorted-by-end invariant
}
```

**Testing note**: since `build_intervals` never produces `fixed: Some(_)`, this function's tests MUST hand-construct `Vec<Interval>` fixtures directly (not go through the front-end/`select`/`build_intervals` pipeline) — the only way to exercise it at all in this slice. Cover both the no-victim success path and the victim-requires-eviction `#[should_panic]` path (confirming the deferred-work message, not just that SOME panic occurs).

## `spill_at_interval` — explicitly stubbed, not built

```rust
fn spill_at_interval(&mut self, _i: usize) {
    unimplemented!("spilling ships in Phase 8c -- see docs/superpowers/specs/2026-08-09-phase-8-decomposition-design.md")
}
```

**Scope-limiting note for 8b's test corpus**: every test program in this slice's test suite must be checked (by construction, not by luck) to have at most `ALLOCATABLE_GPR.len()` (14) simultaneously-live `Gpr`-class values and at most `ALLOCATABLE_XMM.len()` (16) simultaneously-live `Xmm`-class values at any single program point — i.e., `pick_register` never returns `None` for anything in this slice's own corpus. This is easy to satisfy (this project's whole language surface is small expressions; nothing in the existing test corpus from 8a's own `build_intervals_holds_its_invariants_across_the_whole_language_corpus` list comes close to 14 simultaneously-live GPR values), but must be verified, not assumed — a test that accidentally exercises the spill path would hit the `unimplemented!()` panic and fail loudly, which is the correct, safe failure mode (not a silent wrong allocation), but the test SUITE itself should not rely on that panic as its own pass condition.

## Dual-class allocation: run the single-class loop twice

Per the decomposition doc's stated default: partition `Vec<Interval>` by `reg_class` before scanning, run the identical scan loop once per partition (each with its own `active`/`free_regs`, seeded from `ALLOCATABLE_GPR`/`ALLOCATABLE_XMM` respectively), and merge both partitions' `assignment` maps into one final `HashMap<Value, Location>`. No φ-group or hint ever crosses a class boundary — verified two ways, not one: (a) `forge-syntax`'s typeck rejects mismatched `if`/`else` arm types before lowering ever runs, so an `if`-sourced φ's incoming values always share one `Ty`; (b) `forge-ir`'s `Builder::new_phi` (the OTHER φ source — minted for a variable read across a block join, not just `if`/`else` expressions, and today always collapsed away by `try_remove_trivial_phi` before it would reach `build_intervals`, but worth grounding independently of (a) since a future construct could produce a real one) carries the variable's single declared `Ty` through `fill_phi_operands` by construction, not by re-deriving it from each incoming value — so it can't observe a mismatch either, structurally. Two-address hints (`dst -> lhs`/`dst -> src`) are always same-`Ty` by construction too (every binary/unary `MachineInst` variant's operands and result share one type; confirmed against `machine_inst/mod.rs`'s construction sites). So splitting by class before scanning never orphans a hint that would have resolved across the split.

**Handoff to 8c/8d, stated explicitly rather than left implicit**: `LinearScan`'s final `assignment: HashMap<Value, Location>` mixes GPR and XMM registers freely (a function's real callee-saved footprint depends on which specific registers — from BOTH classes' `prologue::SYSV_CALLEE_SAVED` and any future XMM equivalent — actually got handed out). Deriving the real `callee_saved: &[PhysReg]` list `emit_prologue`/`emit_epilogue` (Phase 7d) need from this assignment is NOT this slice's job — it belongs to whichever later slice (8c, 8d, or the final-emission task) actually wires allocator output into Phase 7d's already-shipped, parameterized-and-waiting `emit_prologue`/`emit_epilogue` functions. Recorded here so it isn't silently assumed to already be someone's job.

## `LinearScan` struct and `run()`

SPEC.md's sketch types `assignment` as `FxHashMap<Value, Location>` (`rustc-hash`'s faster hasher); this design uses plain `std::collections::HashMap` since `forge-regalloc` doesn't currently depend on `rustc-hash` and nothing here iterates `assignment` in an order-dependent way that would benefit from it — a deliberate, low-stakes divergence from the sketch, flagged the same way 8a flagged its own sketch deviations rather than silently drifting.

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

`run()` follows PROMPT.md's sketch exactly (sort by start [already done via the constructor accepting pre-sorted intervals, or sorting internally with 8a's exact tie-break key], loop: expire, fixed-eviction-or-pick-register, assign-or-spill), with the corrections above (`expire_old_intervals`'s inclusive boundary; `pick_register`'s corrected hint resolution, now `&mut self` since the same-instruction-reuse case mutates `active` directly; `evict_and_assign`'s narrowed, leak-free scope). `allocatable_for` (referenced in an earlier draft of `evict_and_assign`) does not exist and is not needed — `evict_and_assign`'s corrected form never calls `pick_register` at all.

## Testing

Property tests FIRST (per the process lesson above), over the SAME real-front-end corpus 8a's own `build_intervals_holds_its_invariants_across_the_whole_language_corpus`/`every_hint_points_backward_in_8bs_scan_order` tests already use (reuse the corpus list, don't re-invent it):

- **No two overlapping intervals share a `Location::Reg`** (the inclusive-range overlap predicate: `a.start <= b.end && b.start <= a.end`) — this is literally what 8d's independent verifier will ALSO check later, built independently; 8b having its own copy of this property test now is not redundant with 8d, it's a regression net for THIS slice while 8d doesn't exist yet.
- **`active` remains sorted by `end` after every `expire_old_intervals`/`assign`/hint-transfer call** — a direct invariant check, not just an outcome check.
- **Every interval in the input ends up with exactly one entry in the returned `assignment` map** (modulo the `unimplemented!` spill path, which the scope-limiting note above guarantees the real corpus never reaches).
- **Every honored hint was interference-free at the moment it was honored, AND a real, non-trivial number of hints actually get honored** — BOTH halves are required, not just the first. A property test asserting only "no violations" would have passed VACUOUSLY against the original, broken `pick_register` (which honored zero hints, so there was nothing to violate) — this exact gap is why the bug wasn't caught by reasoning about the rule in isolation. Assert a concrete lower bound on honored-hint count across the corpus (calibrate the exact number once the real implementation runs; treat a value near 0 as a redesign signal, not a threshold to lower).
- Hand-constructed synthetic-`Interval` tests (bypassing `build_intervals` entirely, per the note above) for: `evict_and_assign`'s no-victim success path, and its victim-requires-eviction `#[should_panic]` path (confirming the deferred-work message, not just that some panic occurs).
- A dedicated test for `pick_register`'s Case 2 (same-instruction reuse): two hand-built intervals where `lhs.end == dst.start` and `lhs`'s value is `dst`'s hint — confirm `dst` gets `lhs`'s exact register, `lhs` is removed from `active` without ever appearing in `free_regs`, and `active` stays correctly sorted afterward. A second test for the negative case: a hint target whose interval extends PAST the hinting interval's start (shouldn't happen per 8a's own invariants, but confirm the fallback path is taken safely, not a panic or a wrong register).
- `expire_old_intervals`'s exact boundary: two hand-built intervals `[0,2]` and `[2,4]` sharing position 2 — confirm they're correctly treated as OVERLAPPING (both active simultaneously at position 2, needing two different registers) — this is the single most important boundary test in this slice, given it's a direct, easy-to-get-backward consequence of 8a's inclusive-range correction.
- `PhysReg`'s new `Hash` derive: a trivial smoke test that `HashSet::from([PhysReg::Rax, PhysReg::Rax]).len() == 1` (confirms the derive actually compiles and works as intended — cheap, but real, since a missing/incorrect derive would otherwise only surface as a confusing downstream compile error far from its cause).
- `ALLOCATABLE_GPR`/`ALLOCATABLE_XMM` contents: exactly 14 and 16 entries respectively, `Rsp`/`Rbp` absent from the GPR list, `Xmm16`-`Xmm31` absent from the XMM list.
- A real end-to-end test: run `allocate()` on `build_intervals`'s output for a handful of real corpus programs and confirm every returned `Location` is `Reg(_)` (never `Spill`, matching the scope-limiting note) and that the no-overlap property holds — this is the first point in Phase 8 where 8a and 8b's outputs are actually wired together and exercised end-to-end.

## Exit criteria

1. `Location` enum exists (`Reg`/`Spill`), matching what SPEC.md's `assignment: FxHashMap<Value, Location>` needed but never defined.
2. `PhysReg` gains `Hash` in `crates/forge-x64/src/reg.rs`, additive, no other change.
3. `ALLOCATABLE_GPR`/`ALLOCATABLE_XMM` correctly exclude `Rsp`/`Rbp` and `Xmm16`-`31` respectively.
4. `expire_old_intervals` uses the INCLUSIVE-range-correct boundary (`end >= current_start` keeps active, not the half-open `end > current_start` PROMPT.md's sketch literally shows) — tested with an explicit touching-at-one-point case. Frees only `Location::Reg` occupants (not a type-mismatched raw `Location`).
5. `pick_register` honors a hint in BOTH the "target already expired normally" case AND the "target is a same-instruction reuse, still nominally active" case (Case 1 and Case 2 above) — NOT just the naive free-register check alone, which was found by execution-based review to honor zero hints ever, on any program. Never honors a hint that would collide with a genuinely, simultaneously-live different value. Tested by BOTH halves of the "interference-free AND non-trivially-often honored" property — a test suite asserting only the first half would have passed against the broken version.
6. `excluded_registers`' point-in-time exclusions are correctly aggregated (unioned) to whole-interval scope before being used as a candidate filter — since 8b has no splitting mechanism. `Shl`/`Shr`/`Sar`'s analogous `Cl`-register requirement is explicitly noted as deferred (fixable by an emission-time copy, unlike idiv's divisor), not silently unhandled.
7. `evict_and_assign` exists, is leak-free (no double-booked register, no interval dropped from `active` — both confirmed real bugs in an earlier draft, found by hand-tracing a 3-interval eviction scenario), and is DELIBERATELY NARROWED to the no-victim case, with any genuine victim-requiring-reassignment scenario hitting an explicit, clearly-messaged `unimplemented!()` rather than an unsound reassignment (an earlier draft's reassignment path was proven wrong by execution: it could choose a register free only at the CURRENT scan position, not free across the victim's whole original range).
8. `spill_at_interval` is an explicit, clearly-messaged `unimplemented!()` — not a silent wrong answer, and this slice's own test corpus is verified never to reach it (confirmed by execution: real max simultaneous liveness across a stress-tested corpus is 9 for both classes, against pools of 14/16).
9. Dual-class allocation via running the single-class loop twice; no hint or φ-group ever needs to cross the split (justified by `Ty`-uniformity from BOTH φ-producing paths in `forge-ir`, not just the `if`/`else` one, and by every arithmetic `MachineInst`'s same-`Ty` operand/result shape).
10. All Testing-section items covered, property tests included and run FIRST in the actual test-writing order (not appended as an afterthought after fixture tests, per the explicit process lesson from 8a).
11. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
12. No regressions in any existing test, including the `PhysReg` derive addition not breaking anything in `forge-x64` (it shouldn't — adding a derive is additive).
13. The `callee_saved`-register-derivation handoff to whichever slice actually wires allocator output into Phase 7d's `emit_prologue`/`emit_epilogue` is stated explicitly (not left to be silently assumed by 8c/8d/the final-emission task).
