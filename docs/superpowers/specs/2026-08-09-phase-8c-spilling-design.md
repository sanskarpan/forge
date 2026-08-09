# Design: forge Phase 8c — Spilling

**Status:** Approved for planning
**Scope:** Per `docs/superpowers/specs/2026-08-09-phase-8-decomposition-design.md`, CHECKLIST.md Phase 8 bullets 11-14: `spill_at_interval`'s victim selection, the furthest-endpoint-weighted-by-use-density heuristic, spill slot allocation with reuse, and reload/store insertion. Lives in `crates/forge-regalloc`.
**Input:** `Interval::spill_weight` (currently ALWAYS `0.0` — 8a's design doc explicitly deferred populating it to this slice), `LinearScan`'s `spill_at_interval` (currently an `unimplemented!()` stub — 8b's design doc explicitly deferred it here), and 8b's `Location::Spill(u32)` variant (defined but never constructed until now).
**Out of scope**: real byte emission (still the deferred final-emission task's job, same boundary as every prior Phase 7/8 sub-slice) and `evict_and_assign`'s victim-reassignment case (8b left this an explicit `unimplemented!()`; this slice's spill machinery is the "likely correct approach" 8b's own note anticipated, but wiring `evict_and_assign` to call into it is a deliberate choice made explicitly below, not assumed).

## Why this design leads with the point-in-time-vs-lifetime warning, not as a formality

Phase 8b's final holistic review closed with an explicit, unusually direct prediction for this exact slice: *"Spilling is made of this hazard. Spill-slot reuse asks 'is this slot free?' — free WHEN? Reload insertion asks 'is this value in a register?' — at WHICH position?"* Both 8a (the `fixed`-register whole-lifetime-pin bug) and 8b (the naive hint-interference check) failed by answering exactly these questions wrong once each, at real cost (multiple correction rounds). This design answers both questions explicitly, up front, rather than discovering the wrong answer by execution three rounds in:

- **Spill slots are freed the identical way registers are** — an interval's slot becomes reusable once `current_start` has moved PAST its `end` (the same inclusive-boundary `end < current_start` rule 8b's `expire_old_intervals` already established), never based on a snapshot of "what's free right now" divorced from position. Reusing 8b's exact expiry mechanism (parameterized over slots instead of registers) is the concrete way this design avoids re-deriving the same bug from scratch.
- **A spilled value is never "in a register" for any part of its own recorded interval — that interval describes its life in a SPILL SLOT.** Reload need is a SEPARATE, LOCAL, single-instruction fact (a value must be readable from SOME register at the exact instruction that uses it), resolved by a fixed reservation, not a new allocation decision — detailed below. This sidesteps the recursive "the reload itself needs an interval, which itself might need to spill" problem entirely, by construction, not by hoping it doesn't come up.

## The central design decision: reload via reserved scratch registers, not recursive interval-splitting

Textbook linear-scan-with-spilling (and this project's own PROMPT.md sketch, which stops at `spill_at_interval`'s victim selection and says nothing about reload placement) typically treats each reload as its own tiny interval that competes for a register the same way any other interval does — which can itself require spilling something else, recursively. This is real complexity with no natural termination proof without care.

**This design avoids it entirely**: a small, fixed number of registers per class are RESERVED exclusively for reload/store traffic and removed from the pool `LinearScan` ever hands out to ordinary intervals. A spilled value's reload therefore never competes for a register — its register is a compile-time-known constant, always available, because nothing else can ever be assigned it.

```rust
/// Registers reserved exclusively for spill reload/store traffic --
/// NEVER handed out by pick_register to an ordinary interval. Removed
/// from the pool LinearScan scans over (see SPILL_AWARE_ALLOCATABLE_*
/// below), not from `linear_scan::ALLOCATABLE_GPR`/`ALLOCATABLE_XMM`
/// themselves, which stay exactly as 8b shipped them (still the
/// authoritative "which PhysRegs exist and are encodable" answer).
pub const SCRATCH_GPR: [PhysReg; 2] = [PhysReg::R14, PhysReg::R15];
pub const SCRATCH_XMM: [PhysReg; 2] = [PhysReg::Xmm14, PhysReg::Xmm15];
```

**Why 2 per class, not 1**: every current binary `MachineInst` (Add/Sub/Mul/Div/etc.) has two operands (`lhs`, `rhs`); both could independently be spilled at once (e.g. `spilled_a / spilled_b`). Two reserved registers per class covers this — `lhs` (or the sole operand of a unary op) always reloads into `SCRATCH_*[0]`, `rhs` into `SCRATCH_*[1]`, a fixed positional assignment requiring no per-position bookkeeping at all. The destructive 2-address convention (`dst` reuses `lhs`'s register) composes for free: if `lhs` was spilled and reloaded into `SCRATCH_*[0]`, the instruction executes with its result already in `SCRATCH_*[0]`, and — if `dst` is ALSO spilled — the store-after-definition step stores directly from `SCRATCH_*[0]`, no extra register needed. `IntDiv`/`IntRem`/`CallLibm`'s more exotic register needs (`rax`/`rdx`/ABI argument registers) are already handled entirely by emission-time copies per 8a's design and don't interact with this mechanism at all — a spilled operand feeding one of those still just reloads into a scratch register first, then the existing emission-time fixup takes it from there exactly as if it had never been spilled.

**Why `R14`/`R15` and `Xmm14`/`Xmm15` specifically**: arbitrary but principled — the LAST two entries of `ALLOCATABLE_GPR`/`ALLOCATABLE_XMM` (8b's declared order), so removing them shrinks the pool from the end rather than creating a hole in the middle, and callers reading `ALLOCATABLE_GPR[..12]`-style code can see the shrinkage directly. No ABI or encoding reason favors these two over any other pair; picked for order-tidiness only.

```rust
pub const SPILL_AWARE_ALLOCATABLE_GPR: &[PhysReg] = &ALLOCATABLE_GPR[..12]; // 14 - 2 reserved
pub const SPILL_AWARE_ALLOCATABLE_XMM: &[PhysReg] = &ALLOCATABLE_XMM[..14]; // 16 - 2 reserved
```

`allocate()` (8b) is amended to scan against these narrower pools instead of the raw `ALLOCATABLE_GPR`/`ALLOCATABLE_XMM` — the ONLY change to 8b's already-shipped `allocate()`/`LinearScan` call sites this slice makes; `LinearScan` itself is generic over whatever pool it's handed (it already takes `allocatable: &'a [PhysReg]` as a constructor argument, per 8b's design — no structural change needed there, only which constant gets passed in).

**Consequence, stated explicitly**: this shrinks 8b's real register budget by 2 per class before spilling is even considered. Measured against 8b's own corpus (max simultaneous liveness 4 GPR / 7 XMM), this has zero observable effect — nowhere near 12 or 14. It also means this slice's OWN tests cannot rely on the existing corpus to exercise spilling at all (max pressure 7 is nowhere near a 12/14-register pool) — hand-built high-pressure fixtures are required, the same "synthetic values, not corpus-derived" pattern 8b already used for `evict_and_assign`'s hand-built `fixed` fixtures.

## `spill_weight` — the field 8a deliberately left at `0.0`

```rust
/// spill_weight = (number of real reads) / (interval length), matching
/// PROMPT.md's own formula ("uses / length -- spill the cheapest").
/// Computed once, up front, for every interval -- NOT lazily inside
/// spill_at_interval, since the heuristic needs to compare ALL currently
/// active intervals' weights against each other, and re-deriving it
/// per-comparison would be both wasteful and a subtle footgun (computing
/// it fresh at two different call sites risks the two computations
/// silently drifting, exactly the "shared pure function, not two
/// independent implementations" discipline this project has enforced
/// since Phase 7b's match_scaled_index).
pub fn populate_spill_weights(selected: &SelectedFunction, intervals: &mut [Interval]) {
    let mut use_counts: HashMap<Value, u32> = HashMap::new();
    for inst in &selected.insts {
        for used in reads_of(inst) {
            *use_counts.entry(used).or_insert(0) += 1;
        }
    }
    for iv in intervals.iter_mut() {
        let uses = use_counts.get(&iv.value).copied().unwrap_or(0);
        let length = (iv.end - iv.start).max(1); // avoid a length-0 divide;
                                                   // a single-point interval
                                                   // still has a real length
                                                   // of "at least 1" for
                                                   // this formula's purposes
        iv.spill_weight = uses as f32 / length as f32;
    }
}
```

**Called where**: inside `allocate()` (8b), once, on the full `Vec<Interval>` BEFORE partitioning by class — `reads_of` is already `pub(crate)` in `liveness.rs` and usable here; partitioning happens after, so weights are correct regardless of which class ends up needing them. This is the one place 8c actually touches 8b's `allocate()` beyond swapping in the narrower pools.

**A φ-merged group's members all share the SAME `[start, end]` by construction (8a), but do NOT necessarily share the same use count** — a φ's own destination might be read many times after the join point while an individual incoming value is read zero times outside the merge itself. `populate_spill_weights` computes weight PER VALUE, not per group, which is correct: if 8b's `pick_register` ever fails to co-locate a φ-group's members (a routine, expected outcome per 8a's design), spilling ONE member of the group should weigh that member's OWN use pattern, not the whole group's — spilling the whole group together isn't even representable in this model (each member is a fully independent `Interval` post-merge), and shouldn't be attempted.

## `spill_at_interval` — victim selection, mirroring PROMPT.md's sketch with 8a/8b's corrections applied

```rust
/// Called when `pick_register` returns `None` for interval `i` -- no
/// free, non-excluded register exists in the current class's pool.
/// Picks the ACTIVE interval (same class, since spilling an XMM value
/// can't free a GPR) with the worst score -- `end / spill_weight`,
/// PROMPT.md's own formula, weighting toward "blocks a register for a
/// long time AND isn't used much" -- and either:
/// - if the victim's `end` is LATER than `i`'s own `end`: spill the
///   VICTIM (it was going to cost more to keep than `i` will), hand its
///   now-free register to `i`.
/// - otherwise: spill `i` itself (keeping the victim, which dies sooner
///   anyway, is strictly better).
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
        self.spill(victim);
        self.active.retain(|&j| j != victim);
        self.assign(i, Location::Reg(reg));
        self.active.push(i);
        self.active.sort_by_key(|&j| self.intervals[j].end);
    } else {
        self.spill(i);
    }
}
```

**The `.expect()` on an empty `active` list is a real, load-bearing assertion, not defensive noise**: if `pick_register` found nothing (implying the whole class pool is either occupied or excluded for `i`) but `active` is ALSO empty for this class, that's a contradiction — an empty `active` list means every register in the pool is genuinely free, so `pick_register`'s fallback loop should have found one UNLESS every single pool register happens to be excluded specifically for `i` (a real, if unusual, possibility once `excluded_registers`-style per-instruction constraints exist for more than `IntDiv`/`IntRem`'s `rhs`). This can't currently happen (only `rhs` gets excluded, and never from the WHOLE pool), but the assertion documents the invariant loudly rather than letting a future change silently produce a wrong answer (`.expect()` here follows the same "caller/data bugs must fail loudly in release too" precedent as Phase 6a's `bind()` and every subsequent `assert!` in this project).

## `spill()` — assigning a real spill slot, with reuse

```rust
/// Assigns interval `i` a spill slot, reusing an already-vacated slot if
/// one is available AT `i`'s OWN start position, allocating a fresh one
/// otherwise. Removes `i` from `active` if present (a spilled interval
/// never occupies a register, so it has nothing left to track there).
fn spill(&mut self, i: usize) {
    self.active.retain(|&j| j != i);
    self.expire_old_spill_slots(self.intervals[i].start);
    let slot = self.free_slots.pop().unwrap_or_else(|| {
        let s = self.next_slot;
        self.next_slot += 1;
        s
    });
    self.spilled.push(i); // tracked separately from `active` -- see below
    self.assign(i, Location::Spill(slot));
}

/// The spill-slot analogue of `expire_old_intervals` -- IDENTICAL
/// inclusive-boundary reasoning (a slot is reusable once `current_start`
/// has moved PAST its occupant's `end`, i.e. `end < current_start`;
/// still reserved when `end >= current_start`, including the
/// touching-at-one-point case). `self.spilled` must be kept sorted by
/// `end` for this to be a cheap prefix scan, exactly like `active`.
fn expire_old_spill_slots(&mut self, current_start: u32) {
    while let Some(&j) = self.spilled.first() {
        if self.intervals[j].end >= current_start {
            break;
        }
        self.spilled.remove(0);
        if let Some(Location::Spill(slot)) = self.location_of(j) {
            self.free_slots.push(slot);
        }
    }
}
```

**New `LinearScan` fields**: `spilled: Vec<usize>` (interval indices currently occupying a spill slot, sorted by `end` — the direct analogue of `active`), `free_slots: Vec<u32>` (a stack of vacated slot numbers available for reuse), `next_slot: u32` (the next never-before-used slot number, monotonically increasing). `run()` gains one line: `self.expire_old_spill_slots(self.intervals[i].start);` alongside its existing `self.expire_old_intervals(...)` call, at the top of the loop — spill slots expire on the exact same schedule as registers, checked at the exact same point, using the exact same boundary rule, because the underlying question ("is this resource still needed by its current occupant at this position") is identical in shape for both resource kinds.

**Slot numbering is GLOBAL, not per-class**: a `u32` slot index is a byte offset multiplier into the stack frame, and both GPR-class and XMM-class spilled values need 8 bytes each (this language's `i64`/`bool` and `f64` values are both 8 bytes; no smaller/larger spill footprint exists yet) — so slots are drawn from ONE shared numbering space across both `LinearScan` instances `allocate()` runs (GPR pass, then XMM pass). This means `allocate()` must thread `next_slot`/`free_slots` state BETWEEN the two per-class `LinearScan` instances, not reset it for each — a real, easy-to-miss detail: naively constructing two independent `LinearScan`s (as 8b's `allocate()` already does for `active`/`free_regs`, correctly, since registers ARE class-scoped) would each start slot numbering at 0, and TWO different values — one GPR-spilled, one XMM-spilled — could be assigned the SAME slot number while both are genuinely live simultaneously, silently corrupting one when final emission writes both to the same stack offset. `allocate()`'s signature and body change to carry a shared slot-allocation state across both passes:

```rust
pub fn allocate(
    intervals: Vec<Interval>,
    excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>,
) -> (HashMap<Value, Location>, u32) {
    // ^ return type gains a u32: the total number of spill slots used,
    //   which the deferred final-emission task needs to size the stack
    //   frame (feeding prologue::emit_prologue's spill_bytes parameter --
    //   this is the FIRST real producer of that value; Phase 7d built it
    //   parameterized specifically awaiting this).
    let mut assignment = HashMap::new();
    let mut slot_state = SpillSlotState::default(); // { free_slots: Vec<u32>, next_slot: u32 }
    for (class, pool) in [
        (RegClass::Gpr, SPILL_AWARE_ALLOCATABLE_GPR),
        (RegClass::Xmm, SPILL_AWARE_ALLOCATABLE_XMM),
    ] {
        let class_intervals: Vec<Interval> =
            intervals.iter().filter(|iv| iv.reg_class == class).cloned().collect();
        let mut scan = LinearScan::new(class_intervals, excluded_registers, pool, slot_state);
        scan.run();
        assignment.extend(scan.assignment);
        slot_state = scan.into_slot_state(); // carry forward into the next class's pass
    }
    (assignment, slot_state.next_slot * 8) // total bytes, 8 per slot
}
```

This is a real, deliberate change to `allocate()`'s public signature (adding the `u32` return) and a real new piece of cross-pass state-threading — flagged explicitly here because it's exactly the kind of thing that looks like an internal implementation detail but is actually load-bearing: get it wrong (reset slot numbering per class) and two independently-correct-looking `LinearScan` runs silently produce a corrupt combined allocation, with no single-class test able to catch it (each class's own intervals would look perfectly fine in isolation).

`SpillSlotState` itself is a small, plain carrier — the SAME two fields `LinearScan` already needs internally, just extracted so they can move between instances:

```rust
#[derive(Default)]
struct SpillSlotState {
    free_slots: Vec<u32>,
    next_slot: u32,
}
```

`LinearScan::new` gains this as a 4th constructor argument (alongside `intervals`, `excluded_registers`, `allocatable`), storing its two fields directly as `LinearScan`'s own `free_slots`/`next_slot` fields (no separate storage — `SpillSlotState` only exists as a transit shape between `LinearScan` instances, not a field ON `LinearScan` itself). `into_slot_state(self) -> SpillSlotState` is the mirror-image extraction, called once per class pass in `allocate()`, threading the real state forward instead of each `LinearScan::new` implicitly starting fresh at `next_slot: 0`.

## `evict_and_assign`'s deferred victim case — NOT wired up in this slice, and here's why that's a deliberate choice, not an oversight

8b's own note anticipated 8c's spill machinery as "the likely correct approach" for `evict_and_assign`'s still-`unimplemented!()` victim-reassignment case. This design deliberately does NOT wire that up: `Interval::fixed` still has no real producer (confirmed unchanged since 8a — nothing in `build_intervals` sets it), so there remains nothing to correctness-test a `evict_and_assign`-calls-`spill` integration against. Wiring it now would be exactly the kind of speculative, untestable-with-real-data work this project has consistently avoided (Phase 7d's `emit_prologue`/`emit_epilogue` parameterized-but-untested-until-fed-real-data being the closest precedent, except THAT slice's functions at least had hand-built synthetic tests exercising every branch — `evict_and_assign`'s victim path already does too, via its `#[should_panic]` test, and changing that test's expectation to "now calls spill() instead of panicking" without any real caller ever producing this shape is speculative generality, not driven by an actual need). Left as `unimplemented!()`, unchanged, with its own existing test unchanged — a real "not yet needed" call, revisited only if a future `MachineInst` variant or ABI concern actually produces a `fixed` interval.

## Testing

- `populate_spill_weights` on a hand-built `SelectedFunction`/`Vec<Interval>`: a value used 4 times in a 2-position-long interval gets weight `4.0/2.0 = 2.0`; a value used once across a 10-position interval gets `1.0/10.0 = 0.1` — confirms the "used often in a tight window scores high, rarely-used-and-long-lived scores low" property PROMPT.md's own comment describes, and confirms the actual arithmetic, not just the ordering.
- `spill_at_interval`'s two branches, both hand-built (mirroring the corpus-pressure reality that NEITHER branch is reachable from the current real corpus, per the SPILL_AWARE pool-shrinkage note above): (a) victim's `end` > current's `end` → victim spilled, current gets the freed register, `active` correctly updated; (b) victim's `end` <= current's `end` → current itself spilled, victim untouched.
- `spill`/`expire_old_spill_slots`: two non-overlapping spilled intervals get the SAME slot number (reuse); two overlapping spilled intervals get DIFFERENT slot numbers (no corruption) — the direct analogue of 8b's `expire_old_intervals` boundary tests, same touching-at-one-point case (`[0,2]` and `[2,4]` spilled — must NOT share a slot, symmetric with the register case).
- **The cross-class slot-numbering test — this project's newest instance of the "looks like an implementation detail, is actually load-bearing" pattern**: force BOTH a GPR-class and an XMM-class spill (via deliberately oversized hand-built interval sets exceeding `SPILL_AWARE_ALLOCATABLE_GPR`/`_XMM`'s reduced pools) with genuinely overlapping ranges, and confirm they get DIFFERENT slot numbers — this is the one test that would fail if `allocate()` naively reset `next_slot`/`free_slots` per class instead of threading them through.
- `SCRATCH_GPR`/`SCRATCH_XMM` are correctly excluded from `SPILL_AWARE_ALLOCATABLE_GPR`/`_XMM` (disjoint sets, union recovers the original 14/16).
- An end-to-end hand-built-high-pressure test: construct (not via the front-end, since no real program reaches this) an `Interval` set exceeding `SPILL_AWARE_ALLOCATABLE_GPR`'s 12-register pool, run `allocate()`, confirm at least one `Location::Spill` appears, confirm the returned byte count is `(max concurrent spills) * 8`, and confirm the SAME corrected no-overlap property test from 8b (disjoint-or-legitimate-handoff) STILL holds when `Location::Spill` entries are included (extend the property: two `Spill(n)` locations sharing the same `n` must also be disjoint-or-touching, mirroring the register case exactly).
- Re-run 8b's ENTIRE existing corpus-wide test suite (`run_never_shares_a_register_...`, `run_honors_a_non_trivial_fraction_of_hints`, `run_produces_only_reg_locations_never_spill_for_the_corpus` — this last one's NAME becomes slightly inaccurate once `SPILL_AWARE_*` pools are in use and MUST be re-confirmed still passing with the narrower pools, not just assumed unaffected, since shrinking the pool by 2 is exactly the kind of change that could theoretically tip some corpus program into needing a spill it didn't need before) against the corrected `allocate()` — regression coverage, not new coverage, but essential given `allocate()`'s signature and pool arguments both changed.

## Exit criteria

1. `SCRATCH_GPR`/`SCRATCH_XMM` (2 registers each) and `SPILL_AWARE_ALLOCATABLE_GPR`/`_XMM` (the remainder) exist and are disjoint-and-union-complete with 8b's original constants.
2. `populate_spill_weights` computes `uses/length` correctly and is called once, up front, inside `allocate()` before class-partitioning.
3. `spill_at_interval` is fully implemented (no longer `unimplemented!()`), matching PROMPT.md's victim-selection formula, with the `.expect()` invariant documented and tested.
4. `spill`/`expire_old_spill_slots` correctly reuse slots only once genuinely expired (inclusive boundary, same rule as `expire_old_intervals`), tracked via new `spilled`/`free_slots`/`next_slot` fields on `LinearScan`.
5. Slot numbering is GLOBAL across both class passes (threaded through `allocate()`, not reset per class) — the cross-class test in Testing passes.
6. `allocate()`'s signature changes to return `(HashMap<Value, Location>, u32)`, the `u32` being the total spill-frame byte count — the first real producer of the value `prologue::emit_prologue`/`emit_epilogue`'s `spill_bytes` parameter has been waiting for since Phase 7d.
7. `evict_and_assign`'s victim case remains `unimplemented!()`, deliberately not wired to `spill()` in this slice, with the reasoning stated explicitly (no real `Interval::fixed` producer to test against).
8. All of 8b's existing tests still pass against the new `SPILL_AWARE_*` pools and the new `allocate()` signature (call-site updates only, no behavior regressions).
9. Tests cover every item in Testing above, including the cross-class slot-numbering regression and the extended no-overlap property covering `Location::Spill`.
10. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
