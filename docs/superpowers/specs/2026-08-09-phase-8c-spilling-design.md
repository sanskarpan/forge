# Design: forge Phase 8c — Spilling

**Status:** Approved for planning
**Scope:** Per `docs/superpowers/specs/2026-08-09-phase-8-decomposition-design.md`, CHECKLIST.md Phase 8 bullets 11-14: `spill_at_interval`'s victim selection, the furthest-endpoint-weighted-by-use-density heuristic, spill slot allocation with reuse, and reload/store insertion. Lives in `crates/forge-regalloc`.
**Input:** `Interval::spill_weight` (currently ALWAYS `0.0` — 8a's design doc explicitly deferred populating it to this slice), `LinearScan`'s `spill_at_interval` (currently an `unimplemented!()` stub — 8b's design doc explicitly deferred it here), and 8b's `Location::Spill(u32)` variant (defined but never constructed until now).
**Out of scope**: real byte emission (still the deferred final-emission task's job, same boundary as every prior Phase 7/8 sub-slice) and `evict_and_assign`'s victim-reassignment case (8b left this an explicit `unimplemented!()`; this slice's spill machinery is the "likely correct approach" 8b's own note anticipated, but wiring `evict_and_assign` to call into it is a deliberate choice made explicitly below, not assumed).

## Why this design leads with the point-in-time-vs-lifetime warning, not as a formality

Phase 8b's final holistic review closed with an explicit, unusually direct prediction for this exact slice: *"Spilling is made of this hazard. Spill-slot reuse asks 'is this slot free?' — free WHEN? Reload insertion asks 'is this value in a register?' — at WHICH position?"* Both 8a (the `fixed`-register whole-lifetime-pin bug) and 8b (the naive hint-interference check) failed by answering exactly these questions wrong once each, at real cost (multiple correction rounds). This design answers both questions explicitly, up front, rather than discovering the wrong answer by execution three rounds in:

- **A spill slot becomes reusable once the interval about to occupy it starts after the slot's current occupant ends** — the same inclusive-boundary reasoning 8b's `expire_old_intervals` established for registers (`end >= current_start` still means "in use"), but phrased against the CANDIDATE interval's own start rather than a shared scan cursor, so the answer is correct regardless of scan order. (An earlier draft of this design instead ported `expire_old_intervals`'s cursor-relative expiry mechanism directly, parameterized over slots instead of registers — execution proved that literal port wrong twice over, B4 and B5 below; the `spill()` section has the corrected, cursor-free mechanism actually used.)
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
pub const SCRATCH_GPR: [PhysReg; 2] = [PhysReg::R10, PhysReg::R11];
pub const SCRATCH_XMM: [PhysReg; 2] = [PhysReg::Xmm14, PhysReg::Xmm15];
```

**Why 2 per class, not 1**: every current binary `MachineInst` (Add/Sub/Mul/Div/etc.) has two operands (`lhs`, `rhs`); both could independently be spilled at once (e.g. `spilled_a / spilled_b`). Two reserved registers per class covers this — `lhs` (or the sole operand of a unary op) always reloads into `SCRATCH_*[0]`, `rhs` into `SCRATCH_*[1]`, a fixed positional assignment requiring no per-position bookkeeping at all. The destructive 2-address convention (`dst` reuses `lhs`'s register) composes for free: if `lhs` was spilled and reloaded into `SCRATCH_*[0]`, the instruction executes with its result already in `SCRATCH_*[0]`, and — if `dst` is ALSO spilled — the store-after-definition step stores directly from `SCRATCH_*[0]`, no extra register needed.

**`IntDiv`/`IntRem`'s interaction with this mechanism, stated precisely (an earlier draft of this doc overclaimed "no interaction at all" — execution-based review caught this)**: `Rax`/`Rdx` themselves remain part of `SPILL_AWARE_ALLOCATABLE_GPR` (they are not scratch-reserved), so `pick_register` can still hand them to an ordinary interval. When `idiv` executes, whatever ordinary values happen to occupy `Rax`/`Rdx` at that point must be displaced and restored — this is 8a's already-accepted "idiv third-party clobber" sub-problem 1, unchanged by spilling. What spilling ADDS to that existing picture: if a displaced occupant's own emission-time save target were naively "the same 2 scratch registers a spilled operand might also need at the very same instruction," 3 GPRs could be wanted simultaneously (rhs's own reload, plus displacing BOTH of rax/rdx's occupants). This is resolvable at emission time via ordinary stack `push`/`pop` for the displaced occupants (not the reload mechanism's scratch registers at all — displacement and reload are two independent needs that happen to occur at the same instruction, not one need competing with itself) — but it needs saying explicitly rather than asserted away, since the two mechanisms sharing an instruction is a real interaction, even though they don't share REGISTERS.

**Why `R10`/`R11` for GPR (corrected — an earlier draft picked `R14`/`R15`, which was factually wrong to call ABI-neutral)**: execution-based review found `R14`/`R15` are BOTH members of `prologue::SYSV_CALLEE_SAVED` — reserving them as scratch would force every spilling function's prologue/epilogue to `push`/`pop` a pair of registers used only transiently, purely because of which two happened to be picked, not because anything requires it. `R10`/`R11` are caller-saved (not in `SYSV_CALLEE_SAVED`), so no such cost. For XMM, `Xmm14`/`Xmm15` remain the choice: ALL XMM registers are caller-saved under System V (SPEC.md §7, no XMM callee-saved set exists at all), so there is no equivalent cost differential to correct for on that side — but this means `Xmm14`/`Xmm15` (like every XMM register) are destroyed by any `CallLibm`, which matters only in that a reload/store sequence must not straddle a libm call without re-reloading afterward (already true of registers generally; not a new constraint from choosing these two).

```rust
// R10/R11 sit at indices 8-9 of ALLOCATABLE_GPR (Rax, Rcx, Rdx, Rbx, Rsi,
// Rdi, R8, R9, R10, R11, R12, R13, R14, R15), NOT the last two entries --
// `.split_at(12).0` (an earlier draft used this, copying the XMM pattern
// below without checking) would keep R10/R11 IN the pool while also
// claiming they're scratch-reserved, a direct contradiction. An explicit
// literal is the only construction that is correct regardless of which
// two registers scratch picks, so that's what this uses.
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
// correct here (unlike the GPR case above).
pub const SPILL_AWARE_ALLOCATABLE_XMM: &[PhysReg] = ALLOCATABLE_XMM.split_at(14).0; // 16 - 2 reserved
```
(`&ALLOCATABLE_GPR[..12]` — the form an earlier draft used — does NOT compile in a `const` context; slice indexing isn't yet const-stable. `.split_at(N).0` does compile, confirmed by execution, but is only actually CORRECT when the excluded elements are contiguous at one end of the source array — true for XMM here, false for GPR once scratch moved to R10/R11, hence the explicit literal above.)

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

**Called where**: inside `allocate()` (8b), once, on the full `Vec<Interval>` BEFORE partitioning by class — `reads_of` is already `pub(crate)` in `liveness.rs` and usable here; partitioning happens after, so weights are correct regardless of which class ends up needing them. This requires `allocate()` to receive the `SelectedFunction` it's allocating for, which 8b's original signature didn't take (nothing before this needed the MachineInst stream itself, only the `Vec<Interval>` derived from it) — `allocate()`'s signature gains a `selected: &SelectedFunction` parameter for exactly this call (see the `allocate()` code in the `spill()` section below for the full updated signature).

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
        // B6 (execution-based review): the victim's register is not
        // automatically legal for `i` -- `i` may carry its own
        // per-instruction exclusion (e.g. it's an IntDiv/IntRem `rhs`
        // excluded from Rax/Rdx per 8a's `excluded_registers`). Handing
        // over an excluded register here would silently violate that
        // constraint, reachable with real data and invisible to any test
        // that doesn't specifically combine spilling with exclusion.
        // Falling back to spilling `i` itself is always safe -- it
        // leaves the victim exactly as it was, a known-valid assignment.
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

**The `.expect()` on an empty `active` list is a real, load-bearing assertion, not defensive noise**: if `pick_register` found nothing (implying the whole class pool is either occupied or excluded for `i`) but `active` is ALSO empty for this class, that's a contradiction — an empty `active` list means every register in the pool is genuinely free, so `pick_register`'s fallback loop should have found one UNLESS every single pool register happens to be excluded specifically for `i` (a real, if unusual, possibility once `excluded_registers`-style per-instruction constraints exist for more than `IntDiv`/`IntRem`'s `rhs`). This can't currently happen (only `rhs` gets excluded, and never from the WHOLE pool), but the assertion documents the invariant loudly rather than letting a future change silently produce a wrong answer (`.expect()` here follows the same "caller/data bugs must fail loudly in release too" precedent as Phase 6a's `bind()` and every subsequent `assert!` in this project).

## `spill()` — assigning a real spill slot, with reuse

**This section was rewritten wholesale after execution-based review** (findings labeled B4/B5/B7 in that review). The ORIGINAL design here used an `active`-shaped tracked list (`spilled: Vec<usize>`, `free_slots: Vec<u32>`, `next_slot: u32`, an `expire_old_spill_slots` expiry step mirroring `expire_old_intervals`). Execution proved that mechanism wrong in two independent, serious ways, both instances of the SAME point-in-time-vs-lifetime bug class this project has now hit at least four times (see the opening section):

- **B4 — a freed slot is only "free from the current scan cursor onward," not free across the interval about to occupy it.** `spill_at_interval`'s victim-reassignment branch can spill a victim whose `start` is far EARLIER than the scan position currently being processed. `free_slots.pop()` at that moment hands back a slot that is free relative to the CURRENT position, not relative to the victim's own `[start, end]` — reproduced concretely: `X([0,6])` assigned `Spill(0)`, later `G([5,300])` also assigned `Spill(0)` by the old mechanism, overlapping at positions 5-6 and silently corrupting one when both are eventually stored to the same stack offset.
- **B5 — threading `free_slots` (not just `next_slot`) between the GPR and XMM passes causes CROSS-CLASS collisions.** A slot freed relative to the GPR pass's cursor position gets handed to the XMM pass, which restarts its own cursor at 0 — reproduced concretely: GPR `X([0,6])` gets `Spill(0)`, XMM `Y([0,1000])` also gets `Spill(0)`, genuinely overlapping. This is a bug the design's OWN prescribed fix for the (correctly-identified) cross-class risk introduced, not one it solved.
- **B7 — `self.spilled` was never actually kept sorted**, despite the prose above claiming it must be, so `expire_old_spill_slots`'s prefix-scan could strand a low-`end` slot behind a high-`end` one and never free it.

**The replacement below is immune to all three simultaneously**, because it compares a candidate slot only against the INTERVAL'S OWN start — never a scan cursor, never a per-pass "currently free" snapshot — and therefore needs no expiry step, no `spilled` list, no `free_slots` stack, and no `next_slot` counter at all:

```rust
/// Assigns interval `i` a spill slot. `slot_end[s]` records the highest
/// `end` any interval placed in slot `s` has ever had; a slot is safe to
/// reuse for interval `i` iff `slot_end[s] < i.start` -- the same
/// inclusive-boundary rule `expire_old_intervals` uses for registers
/// (`end >= current_start` still means "in use"), just phrased against
/// the requesting interval's own start instead of a scan cursor. This is
/// deliberately order-independent: it gives the same, correct answer
/// whether `i` is the interval currently being scanned or a victim from
/// deep in `active` with a much earlier `start`, which is exactly the
/// property the original `free_slots`/`next_slot` design lacked (B4/B5).
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

**New `LinearScan` field**: `slot_end: Vec<u32>` only — replaces the ORIGINAL design's `spilled`/`free_slots`/`next_slot` trio entirely. `run()` needs no new call anywhere in its loop for this — `slot_end` carries no cursor-relative state to expire, so there is nothing to check at the top of each iteration; `spill()` is self-contained.

**Slot numbering is GLOBAL, not per-class, and this is still true and still load-bearing** with the new mechanism, for the identical underlying reason as before: a `u32` slot index is a byte-offset multiplier into the stack frame, and GPR- and XMM-class spilled values both need 8 bytes (this language's `i64`/`bool` and `f64` values are both 8 bytes; no smaller/larger spill footprint exists yet), so both classes must draw from ONE shared `slot_end` vector across `allocate()`'s GPR pass and XMM pass, not two independently-zeroed ones — otherwise a GPR-spilled and an XMM-spilled value, both genuinely live at once, could land on the same slot number exactly as B5 reproduced above. `allocate()` threads `slot_end` forward between the two per-class `LinearScan` instances:

```rust
pub fn allocate(
    intervals: Vec<Interval>,
    excluded_registers: &HashMap<(usize, Value), Vec<PhysReg>>,
    selected: &SelectedFunction,
) -> (HashMap<Value, Location>, u32) {
    // ^ two signature changes from 8b: `selected` (populate_spill_weights
    //   needs it -- see the `spill_weight` section above) and the return
    //   type gaining a u32, the total spill-frame byte count, which the
    //   deferred final-emission task needs to size the stack frame
    //   (feeding prologue::emit_prologue's spill_bytes parameter -- this
    //   is the FIRST real producer of that value; Phase 7d built it
    //   parameterized specifically awaiting this).
    let mut intervals = intervals;
    populate_spill_weights(selected, &mut intervals);

    let mut assignment = HashMap::new();
    let mut slot_end: Vec<u32> = Vec::new();
    for (class, pool) in [
        (RegClass::Gpr, SPILL_AWARE_ALLOCATABLE_GPR),
        (RegClass::Xmm, SPILL_AWARE_ALLOCATABLE_XMM),
    ] {
        let class_intervals: Vec<Interval> =
            intervals.iter().filter(|iv| iv.reg_class == class).cloned().collect();
        let mut scan = LinearScan::new(class_intervals, excluded_registers, pool, slot_end);
        scan.run();
        // B2 (execution-based review): an earlier draft did
        // `assignment.extend(scan.assignment)` and THEN
        // `scan.into_slot_state()` -- a by-value method call needing the
        // WHOLE `scan`, which no longer compiles once `scan.assignment`
        // has already been partially moved out (E0382). Destructuring
        // both fields out of `scan` in a single statement avoids ever
        // having a whole-`self` method call after a partial move.
        let LinearScan { assignment: class_assignment, slot_end: next_slot_end, .. } = scan;
        assignment.extend(class_assignment);
        slot_end = next_slot_end;
    }
    (assignment, slot_end.len() as u32 * 8) // total bytes, 8 per slot
}
```

This is a real, deliberate change to `allocate()`'s public signature (a new `selected` parameter, and the added `u32` return) and a real piece of cross-pass state-threading — flagged explicitly here because it's exactly the kind of thing that looks like an internal implementation detail but is actually load-bearing: get it wrong (reset `slot_end` per class) and two independently-correct-looking `LinearScan` runs silently produce a corrupt combined allocation, with no single-class test able to catch it (each class's own intervals would look perfectly fine in isolation).

`LinearScan::new` gains `slot_end: Vec<u32>` as a 4th constructor argument (alongside `intervals`, `excluded_registers`, `allocatable`), storing it directly as `LinearScan`'s own `slot_end` field — no separate carrier type is needed (the earlier `SpillSlotState` struct this draft used is gone; a bare `Vec<u32>` is the whole state, so wrapping it added a type with no behavior of its own).

## `evict_and_assign`'s deferred victim case — NOT wired up in this slice, and here's why that's a deliberate choice, not an oversight

8b's own note anticipated 8c's spill machinery as "the likely correct approach" for `evict_and_assign`'s still-`unimplemented!()` victim-reassignment case. This design deliberately does NOT wire that up: `Interval::fixed` still has no real producer (confirmed unchanged since 8a — nothing in `build_intervals` sets it), so there remains nothing to correctness-test a `evict_and_assign`-calls-`spill` integration against. Wiring it now would be exactly the kind of speculative, untestable-with-real-data work this project has consistently avoided (Phase 7d's `emit_prologue`/`emit_epilogue` parameterized-but-untested-until-fed-real-data being the closest precedent, except THAT slice's functions at least had hand-built synthetic tests exercising every branch — `evict_and_assign`'s victim path already does too, via its `#[should_panic]` test, and changing that test's expectation to "now calls spill() instead of panicking" without any real caller ever producing this shape is speculative generality, not driven by an actual need). Left as `unimplemented!()`, unchanged, with its own existing test unchanged — a real "not yet needed" call, revisited only if a future `MachineInst` variant or ABI concern actually produces a `fixed` interval.

## Testing

- `populate_spill_weights` on a hand-built `SelectedFunction`/`Vec<Interval>`: a value used 4 times in a 2-position-long interval gets weight `4.0/2.0 = 2.0`; a value used once across a 10-position interval gets `1.0/10.0 = 0.1` — confirms the "used often in a tight window scores high, rarely-used-and-long-lived scores low" property PROMPT.md's own comment describes, and confirms the actual arithmetic, not just the ordering.
- `spill_at_interval`'s two branches, both hand-built (mirroring the corpus-pressure reality that NEITHER branch is reachable from the current real corpus, per the SPILL_AWARE pool-shrinkage note above): (a) victim's `end` > current's `end` → victim spilled, current gets the freed register, `active` correctly updated; (b) victim's `end` <= current's `end` → current itself spilled, victim untouched.
- **B6 regression — `spill_at_interval`'s reassignment branch respects exclusion**: hand-build a victim holding an excluded register (construct `excluded_registers` so `i`'s value excludes exactly the register the victim currently holds) and confirm `i` is spilled instead of ever receiving that register, and that the victim is left untouched (still active, still in its original register) — this is the one scenario the pre-fix code would have gotten silently wrong.
- **B4 regression — reusing a slot across a victim's own earlier start**: hand-build a scan where interval `X` (`start=0, end=6`) gets spilled first, then later, while the scan cursor sits well past position 6, a SECOND interval `G` (`start=5, end=300`) is spilled via `spill_at_interval`'s victim-reassignment branch (i.e. `G`'s own `start` is far behind the current cursor). Confirm `X` and `G` do NOT receive the same slot number, since `[0,6]` and `[5,300]` genuinely overlap — this is the exact scenario the original `free_slots`-based mechanism got wrong (it consulted "free right now," not "free across `G`'s own range").
- `spill`'s slot reuse, via `slot_end` directly: two NON-overlapping spilled intervals (e.g. `[0,2]` then `[3,5]`) get the SAME slot number; two overlapping or touching-at-one-point spilled intervals (`[0,2]` and `[2,4]`) get DIFFERENT slot numbers — the direct analogue of 8b's `expire_old_intervals` boundary tests, same inclusive-boundary reasoning, now expressed as "reuse iff `slot_end[s] < start`" rather than an expiry step.
- **B5 regression, folded into the cross-class test below** — the SAME cross-class scenario that broke `free_slots` threading (a GPR spill and an XMM spill with genuinely overlapping ranges) must now get DIFFERENT slot numbers under the `slot_end`-threading mechanism.
- **The cross-class slot-numbering test — this project's newest instance of the "looks like an implementation detail, is actually load-bearing" pattern**: force BOTH a GPR-class and an XMM-class spill (via deliberately oversized hand-built interval sets exceeding `SPILL_AWARE_ALLOCATABLE_GPR`/`_XMM`'s reduced pools) with genuinely overlapping ranges, and confirm they get DIFFERENT slot numbers — this is the one test that would fail if `allocate()` naively reset `slot_end` per class instead of threading it through.
- `SCRATCH_GPR`/`SCRATCH_XMM` are correctly excluded from `SPILL_AWARE_ALLOCATABLE_GPR`/`_XMM` (disjoint sets, union recovers the original 14/16).
- An end-to-end hand-built-high-pressure test: construct (not via the front-end, since no real program reaches this) an `Interval` set exceeding `SPILL_AWARE_ALLOCATABLE_GPR`'s 12-register pool, run `allocate()`, confirm at least one `Location::Spill` appears, confirm the returned byte count is **`>=` `(max concurrent spills) * 8` AND a multiple of 8** (NOT exact equality — first-fit slot selection isn't guaranteed to find the theoretically-optimal minimum slot count even with the `slot_end` fix, so the byte count is a valid-but-not-necessarily-tight upper bound; asserting exact equality would make the test fragile to an allocation-order change that is still entirely correct), and confirm the SAME corrected no-overlap property test from 8b (disjoint-or-legitimate-handoff) STILL holds when `Location::Spill` entries are included (extend the property: two `Spill(n)` locations sharing the same `n` must also be disjoint-or-touching, mirroring the register case exactly).
- Re-run 8b's ENTIRE existing corpus-wide test suite (`run_never_shares_a_register_...`, `run_honors_a_non_trivial_fraction_of_hints`, `run_produces_only_reg_locations_never_spill_for_the_corpus` — this last one's NAME becomes slightly inaccurate once `SPILL_AWARE_*` pools are in use and MUST be re-confirmed still passing with the narrower pools, not just assumed unaffected, since shrinking the pool by 2 is exactly the kind of change that could theoretically tip some corpus program into needing a spill it didn't need before) against the corrected `allocate()` — regression coverage, not new coverage, but essential given `allocate()`'s signature and pool arguments both changed.

## Exit criteria

1. `SCRATCH_GPR`/`SCRATCH_XMM` (2 registers each) and `SPILL_AWARE_ALLOCATABLE_GPR`/`_XMM` (the remainder) exist and are disjoint-and-union-complete with 8b's original constants — including for GPR, where the excluded pair (`R10`/`R11`) is NOT at the end of `ALLOCATABLE_GPR`'s declared order, so this must be checked by actual set membership, not merely "the pool has 12 entries."
2. `populate_spill_weights` computes `uses/length` correctly and is called once, up front, inside `allocate()` before class-partitioning, using the newly-added `selected: &SelectedFunction` parameter.
3. `spill_at_interval` is fully implemented (no longer `unimplemented!()`), matching PROMPT.md's victim-selection formula, with the `.expect()` invariant documented and tested, AND its reassignment branch consults `excluded_at` before handing the victim's register to `i` (B6), falling back to spilling `i` itself when that register is excluded.
4. `spill` reuses a slot only when that slot's recorded `slot_end[s] < i.start` (comparing against the INTERVAL's own start, never a scan cursor or per-pass snapshot) — tracked via a single new `slot_end: Vec<u32>` field on `LinearScan`, with no `spilled` list, `free_slots` stack, or `next_slot` counter (all three removed from the design after B4/B5/B7).
5. Slot numbering is GLOBAL across both class passes (`slot_end` threaded through `allocate()`, not reset per class) — the cross-class test in Testing passes, including the B5 overlapping-ranges regression specifically.
6. `allocate()`'s signature changes to accept `selected: &SelectedFunction` and to return `(HashMap<Value, Location>, u32)`, the `u32` being the total spill-frame byte count — the first real producer of the value `prologue::emit_prologue`/`emit_epilogue`'s `spill_bytes` parameter has been waiting for since Phase 7d. `allocate()`'s body correctly destructures each `LinearScan` (moving `assignment` and `slot_end` out via one destructuring statement, never via a whole-`self` consuming method after a partial move) — B2's compile error does not reproduce.
7. `evict_and_assign`'s victim case remains `unimplemented!()`, deliberately not wired to `spill()` in this slice, with the reasoning stated explicitly (no real `Interval::fixed` producer to test against).
8. All of 8b's existing tests still pass against the new `SPILL_AWARE_*` pools and the new `allocate()` signature (call-site updates only, no behavior regressions).
9. Tests cover every item in Testing above, including the B4 (slot-reuse-across-an-earlier-start), B5 (cross-class overlapping-range collision), and B6 (exclusion-respecting reassignment) regressions specifically, and the extended no-overlap property covering `Location::Spill`.
10. `cargo test --workspace` green, `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
